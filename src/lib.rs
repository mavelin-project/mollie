//! Mollie is a statically typed scripting language for running untrusted code,
//! like addons players download, inside a Rust program. Programs are compiled
//! to machine code, and each one gets its own memory, limits and API. A program
//! does the same thing on every machine, so peers in a multiplayer game can run
//! the same addons and agree on the result.
//!
//! This crate is how a Rust program, the host, uses Mollie. The host gives
//! programs its own functions, types and traits, compiles programs and calls
//! them through [`host`]. Values of programs it holds on to are handles like
//! [`GcPtr`] and [`MolStr`]. Editors learn about the host's API from a
//! [`stub`].

use std::{fmt, iter::empty, ops::Deref};

pub use mollie_compiler as compiler;
pub use mollie_const as constants;
pub use mollie_index as index;
pub use mollie_ir as ir;
pub use mollie_parser as parser;
pub use mollie_shared as shared;
use mollie_shared::Span;
pub use mollie_typed_ast as typed_ast;
pub use mollie_typing as typing;

/// Giving programs the host's functions, types and traits, and compiling and
/// calling programs.
pub mod host;
/// Mollie declarations of what the host registers, for tools like the language
/// server.
pub mod stub;

use self::{
    compiler::{
        CompiledAdt,
        allocator::{Array, GcValue, HEADER_SIZE, Heap, TypeLayout},
        sandbox,
    },
    constants::ConstantValue,
    index::{Idx, IndexBoxedSlice, IndexVec},
    typed_ast::{FunctionBody, TypedASTContext},
    typing::{
        Adt, AdtKind, AdtRef, AdtVariant, AdtVariantField, AdtVariantRef, Arg, ArgType, ArrayKind, FieldRef, Func, ImplRef, IntType, ModuleId, PrimitiveType, Trait, TraitFunc,
        TraitFuncRef, TraitRef, TyCtxt, Type, TypeRef, UIntType, VFuncRef, VTableFunc, VTableGenerator,
    },
};

/// A reference to an object in a program's heap. The object is freed once
/// nothing reachable from the roots refers to it, so a host keeping it past the
/// program's run must root it with [`GcPtr::root`].
#[repr(transparent)]
pub struct GcPtr<T>(*mut GcValue<T>);

impl<T> Clone for GcPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for GcPtr<T> {}

fn in_running_heap<R>(f: impl FnOnce(&Heap) -> R) -> R {
    sandbox::with_current_heap(f).expect("no program is running: use the `*_in` functions with the program's heap")
}

impl<T: 'static> GcPtr<T> {
    /// Moves `value` into the heap of the program running on this thread. It
    /// panics outside of a run, where [`GcPtr::new_in`] is needed instead.
    pub fn from(value: T) -> Self {
        in_running_heap(|heap| Self::new_in(heap, value))
    }

    /// Moves `value` into `heap`.
    pub fn new_in(heap: &Heap, value: T) -> Self {
        // SAFETY: the layout is interned in the heap.
        unsafe { Self::from_parts_in(heap, value, heap.layout_of::<T>()) }
    }
}

impl<T> GcPtr<T> {
    /// # Safety
    ///
    /// The same as for [`GcPtr::from_parts_in`], with the heap of the program
    /// running on this thread.
    pub unsafe fn from_parts(value: T, layout: &'static TypeLayout) -> Self {
        in_running_heap(|heap| unsafe { Self::from_parts_in(heap, value, layout) })
    }

    /// # Safety
    ///
    /// `layout` must be interned in `heap` and describe `T`, with its size, its
    /// alignment and every place where it refers to objects of the heap.
    pub unsafe fn from_parts_in(heap: &Heap, value: T, layout: &'static TypeLayout) -> Self {
        let result_value = unsafe { heap.alloc(layout, false) };

        // The memory is zeroed, not a valid `T`, so it must not be dropped.
        unsafe { result_value.cast::<T>().write(value) };

        Self(result_value.cast())
    }

    /// Keeps the object alive while the root lives.
    pub fn root(self, heap: &Heap) -> GcRoot<'_, T> {
        heap.root(self.0.cast());

        GcRoot { ptr: self, heap }
    }

    /// Its value.
    pub const fn ptr(&self) -> *const T {
        self.0.cast()
    }

    /// Its value.
    pub const fn ptr_mut(&mut self) -> *mut T {
        self.0.cast()
    }

    /// The layout it was allocated with.
    #[allow(clippy::cast_ptr_alignment)]
    pub fn type_layout(&self) -> &'static TypeLayout {
        unsafe { (*self.0.cast::<u8>().wrapping_sub(HEADER_SIZE).cast::<GcValue<()>>()).layout }
    }

    /// The index of its variant if it's an enum, and 0 otherwise.
    pub fn adt_variant(&self) -> AdtVariantRef {
        let type_layout = self.type_layout();

        match type_layout.kind {
            // SAFETY: the discriminant is the first field of every variant.
            Some(AdtKind::Enum) => AdtVariantRef::new(unsafe { self.0.cast::<usize>().read() }),
            _ => AdtVariantRef::ZERO,
        }
    }

    /// Its field `field`, if it's a value of `adt`. It panics if it isn't, or
    /// the field isn't the size of `F`.
    #[allow(clippy::cast_sign_loss)]
    pub fn get<F>(&self, adt: &CompiledAdt, field: FieldRef) -> Option<&F> {
        assert_eq!(self.type_layout(), adt.type_layout);

        let field = &adt.variants[self.adt_variant()].fields[field].0;

        assert_eq!(size_of::<F>(), field.ty.bytes() as usize);

        unsafe { self.0.byte_add(field.offset as usize).cast::<F>().as_ref() }
    }

    /// Its field `field`, if it's a value of `adt`. It panics if it isn't, or
    /// the field isn't the size of `F`.
    #[allow(clippy::cast_sign_loss)]
    pub fn get_mut<F>(&mut self, adt: &CompiledAdt, field: FieldRef) -> Option<&mut F> {
        assert_eq!(self.type_layout(), adt.type_layout);

        let field = &adt.variants[self.adt_variant()].fields[field].0;

        assert_eq!(size_of::<F>(), field.ty.bytes() as usize);

        unsafe { self.0.byte_add(field.offset as usize).cast::<F>().as_mut() }
    }

    #[allow(clippy::cast_sign_loss)]
    fn array_field(&self, adt: &CompiledAdt, field: FieldRef) -> Option<*mut Array> {
        assert_eq!(self.type_layout(), adt.type_layout);

        let field = &adt.variants[self.adt_variant()].fields[field].0;

        assert_eq!(size_of::<*mut Array>(), field.ty.bytes() as usize);

        let array = unsafe { self.0.byte_add(field.offset as usize).cast::<*mut Array>().read() };

        (!array.is_null()).then_some(array)
    }

    /// The elements of the array in its field `field`, which is found as
    /// [`GcPtr::get`] finds it. `None` if the field holds no array.
    pub fn get_slice<F>(&self, adt: &CompiledAdt, field: FieldRef) -> Option<&[F]> {
        let array = unsafe { &*self.array_field(adt, field)? };

        Some(unsafe { std::slice::from_raw_parts(array.ptr.cast::<F>(), array.length) })
    }

    /// The elements of the array in its field `field`, which is found as
    /// [`GcPtr::get`] finds it. `None` if the field holds no array.
    pub fn get_slice_mut<F>(&mut self, adt: &CompiledAdt, field: FieldRef) -> Option<&mut [F]> {
        let array = unsafe { &*self.array_field(adt, field)? };

        Some(unsafe { std::slice::from_raw_parts_mut(array.ptr.cast::<F>(), array.length) })
    }
}

impl<T: fmt::Display> fmt::Display for GcPtr<T> {
    #[track_caller]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<T: fmt::Debug> fmt::Debug for GcPtr<T> {
    #[track_caller]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

impl<T> Deref for GcPtr<T> {
    type Target = T;

    #[track_caller]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.cast::<T>() }
    }
}

/// A string of a program, in its heap.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct MolStr(*const Array);

impl MolStr {
    /// Copies `text` into the heap of the program running on this thread. It
    /// panics outside of a run, where [`MolStr::new_in`] is needed instead.
    pub fn new(text: &str) -> Self {
        in_running_heap(|heap| Self::new_in(heap, text))
    }

    /// Copies `text` into `heap`.
    pub fn new_in(heap: &Heap, text: &str) -> Self {
        Self(compiler::strings::new(heap, text))
    }

    /// Its array of bytes.
    pub const fn ptr(&self) -> *const Array {
        self.0
    }

    /// Its text.
    pub const fn as_str(&self) -> &str {
        // SAFETY: values of `MolStr` are only created from strings.
        unsafe { compiler::strings::as_str(self.0) }
    }

    /// Keeps the string alive while the root lives.
    pub fn root(self, heap: &Heap) -> GcRoot<'_, Array> {
        GcPtr(self.0.cast_mut().cast::<GcValue<Array>>()).root(heap)
    }
}

impl Deref for MolStr {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl fmt::Display for MolStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for MolStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Keeps an object alive until it's dropped, by making it a root of its heap.
pub struct GcRoot<'h, T> {
    ptr: GcPtr<T>,
    heap: &'h Heap,
}

impl<T> GcRoot<'_, T> {
    /// The object.
    pub const fn ptr(&self) -> GcPtr<T> {
        self.ptr
    }
}

impl<T> Deref for GcRoot<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.ptr
    }
}

impl<T> Drop for GcRoot<'_, T> {
    fn drop(&mut self) {
        self.heap.unroot(self.ptr.0.cast());
    }
}

/// A Rust type with a Mollie type (see [`func`]).
pub trait MollieTypeOf {
    /// The generic parameter it stands for, if it's one (see [`Generic`]).
    fn generic_index() -> Option<usize> {
        None
    }

    /// Its Mollie type, made in `context`.
    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef;
}

/// The Mollie types of a function's parameters (see [`func`]).
pub trait MollieMultipleTypeOf {
    /// The generic parameter it stands for, if it's one (see [`Generic`]).
    fn generic_index() -> Option<usize> {
        None
    }

    /// The Mollie types of the parameters, in order.
    fn mollie_type_of(context: &mut TyCtxt) -> impl Iterator<Item = TypeRef>;
}

macro_rules! mollie_types {
    ($($typo:ty => $variant:expr),*) => {
        $(
            impl MollieTypeOf for $typo {
                fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
                    context.types.get_or_add($variant)
                }
            }
        )*
    };
}

macro_rules! mollie_arg_types {
    ($($name:ident),*) => {
        impl<$($name: MollieTypeOf),*> MollieMultipleTypeOf for ($($name),*,) {
            fn mollie_type_of(context: &mut TyCtxt) ->impl Iterator<Item = TypeRef> {
                [$($name::mollie_type_of(context)),*].into_iter()
            }
        }
    };
}

mollie_types! {
    i8     => Type::Primitive(PrimitiveType::Int (IntType ::   I8)),
    u8     => Type::Primitive(PrimitiveType::UInt(UIntType::   U8)),
    i16    => Type::Primitive(PrimitiveType::Int (IntType ::  I16)),
    u16    => Type::Primitive(PrimitiveType::UInt(UIntType::  U16)),
    i32    => Type::Primitive(PrimitiveType::Int (IntType ::  I32)),
    u32    => Type::Primitive(PrimitiveType::UInt(UIntType::  U32)),
    i64    => Type::Primitive(PrimitiveType::Int (IntType ::  I64)),
    u64    => Type::Primitive(PrimitiveType::UInt(UIntType::  U64)),
    isize  => Type::Primitive(PrimitiveType::Int (IntType ::ISize)),
    usize  => Type::Primitive(PrimitiveType::UInt(UIntType::USize)),
    f32    => Type::Primitive(PrimitiveType::F32                ),
    f64    => Type::Primitive(PrimitiveType::F64                ),
    bool   => Type::Primitive(PrimitiveType::Bool              ),
    MolStr => Type::Primitive(PrimitiveType::String               )
}

/// The generic parameter `N`, as a type.
pub struct Generic<const N: usize>;

impl<const N: usize> MollieTypeOf for Generic<N> {
    fn generic_index() -> Option<usize> {
        Some(N)
    }

    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
        context.types.get_or_add(Type::Generic(N))
    }
}

type AdtBuilderVariant = Vec<(String, TypeRef, Option<ConstantValue>)>;

/// The `Any` primitive type.
pub struct AnyType;

impl MollieTypeOf for () {
    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
        context.types.get_or_add(Type::Primitive(PrimitiveType::Void))
    }
}

impl MollieTypeOf for AnyType {
    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
        context.types.get_or_add(Type::Primitive(PrimitiveType::Any))
    }
}

impl MollieMultipleTypeOf for () {
    fn mollie_type_of(_: &mut TyCtxt) -> impl Iterator<Item = TypeRef> {
        empty()
    }
}

impl<T: MollieTypeOf> MollieTypeOf for &[T] {
    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type_of(context);

        context.types.get_or_add(Type::Array(element, ArrayKind::Growable))
    }
}

impl<T: MollieTypeOf, const U: usize> MollieTypeOf for [T; U] {
    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type_of(context);

        // Inline, like Rust's arrays: a fixed array.
        let size = context.types.get_or_add(Type::Size(U));

        context.types.get_or_add(Type::Array(element, ArrayKind::Fixed(size)))
    }
}

mollie_arg_types![A];
mollie_arg_types![A, B];
mollie_arg_types![A, B, C];
mollie_arg_types![A, B, C, D];
mollie_arg_types![A, B, C, D, E];
mollie_arg_types![A, B, C, D, E, F];
mollie_arg_types![A, B, C, D, E, F, G];

/// The Mollie type of functions taking `Args` and returning `Returns`.
pub fn func<Args: MollieMultipleTypeOf, Returns: MollieTypeOf>(context: &mut TyCtxt) -> TypeRef {
    let args = Args::mollie_type_of(context).collect();
    let returns = Returns::mollie_type_of(context);

    context.types.get_or_add(Type::Func(args, returns))
}

/// Makes a struct or an enum of the host.
#[derive(Debug)]
pub struct AdtBuilder<'a> {
    context: &'a mut TyCtxt,
    name: String,
    collectable: bool,
    value: bool,
    variants: Vec<(Option<String>, AdtBuilderVariant)>,
    generics: usize,
    kind: AdtKind,
}

impl<'a> AdtBuilder<'a> {
    fn new(context: &'a mut TyCtxt, name: String, kind: AdtKind, collectable: bool, value: bool) -> Self {
        Self {
            context,
            name,
            collectable,
            value,
            // A struct is its one variant; an enum's are added.
            variants: if kind == AdtKind::Struct { vec![(None, Vec::new())] } else { Vec::new() },
            generics: 0,
            kind,
        }
    }

    /// A struct called `name`, without fields yet. Its values are objects in
    /// the heap.
    pub fn new_struct<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self::new(context, name.into(), AdtKind::Struct, true, false)
    }

    /// A struct called `name`, without fields yet. Its values are copied, and
    /// stored inline where they're used.
    pub fn new_value_struct<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self::new(context, name.into(), AdtKind::Struct, true, true)
    }

    /// An enum called `name`, without variants yet (see
    /// [`AdtBuilder::variant`]). Its values are objects in the heap.
    pub fn new_enum<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self::new(context, name.into(), AdtKind::Enum, true, false)
    }

    /// An enum called `name`, without variants yet (see
    /// [`AdtBuilder::variant`]). Its values are copied, and stored inline where
    /// they're used.
    pub fn new_value_enum<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self::new(context, name.into(), AdtKind::Enum, true, true)
    }

    /// A struct called `name` whose values point to memory the host owns, so
    /// the collector doesn't trace them.
    pub fn new_opaque<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self::new(context, name.into(), AdtKind::Struct, false, false)
    }

    /// Adds a variant called `name`. Fields added after it belong to it.
    #[must_use]
    pub fn variant<T: Into<String>>(mut self, name: T) -> Self {
        self.variants.push((Some(name.into()), vec![(
            String::from("<discriminant>"),
            self.context.types.get_or_add(Type::Primitive(PrimitiveType::UInt(UIntType::USize))),
            None,
        )]));

        self
    }

    /// Adds a generic parameter, which fields refer to as [`Generic`].
    #[must_use]
    pub const fn add_generic(mut self) -> Self {
        self.generics += 1;

        self
    }

    /// Adds a field called `name` of the type of `T`. When a program leaves it
    /// out, it's `default`.
    #[must_use]
    pub fn field_default<T: MollieTypeOf + Into<ConstantValue>>(mut self, name: impl Into<String>, default: T) -> Self {
        if let Some(index) = T::generic_index() {
            assert!(self.generics > index, "pls add generic param");
        }

        if let Some((_, variant)) = self.variants.last_mut() {
            variant.push((name.into(), T::mollie_type_of(self.context), Some(default.into())));
        }

        self
    }

    /// Adds a field called `name` of the type of `T`, which must be given.
    #[must_use]
    pub fn field<T: MollieTypeOf>(mut self, name: impl Into<String>) -> Self {
        if let Some(index) = T::generic_index() {
            assert!(self.generics > index, "pls add generic param");
        }

        if let Some((_, variant)) = self.variants.last_mut() {
            variant.push((name.into(), T::mollie_type_of(self.context), None));
        }

        self
    }

    /// Adds a field called `name` of the type `ty`, which must be given.
    #[must_use]
    pub fn field_ty<T: Into<String>>(self, name: T, ty: TypeRef) -> Self {
        self.field_ty_default(name, ty, None)
    }

    /// Adds a field called `name` of the type `ty`. When a program leaves it
    /// out, it's `default`, and without a default it must be given.
    #[must_use]
    pub fn field_ty_default<T: Into<String>>(mut self, name: T, ty: TypeRef, default: Option<ConstantValue>) -> Self {
        if let Some((_, variant)) = self.variants.last_mut() {
            variant.push((name.into(), ty, default));
        }

        self
    }

    /// Registers it in `module`. It panics if the name is already taken there.
    pub fn finish_in_module(self, module: ModuleId) -> AdtRef {
        let adt = Adt {
            name: self.name,
            collectable: self.collectable,
            kind: self.kind,
            generics: self.generics,
            variants: IndexBoxedSlice::from_iter(self.variants.into_iter().enumerate().map(|(discriminant, (name, fields))| {
                AdtVariant {
                    name,
                    discriminant,
                    fields: fields
                        .into_iter()
                        .map(|(name, ty, default_value)| AdtVariantField { name, ty, default_value })
                        .collect(),
                }
            })),
        };

        let name = adt.name.clone();
        let adt_ref = self
            .context
            .def_registry
            .register_adt_in_module(module, adt, Span::default())
            .unwrap_or_else(|error| panic!("can't register `{name}`: {:?}", error.error));

        if self.value {
            self.context.def_registry.value_types.insert(adt_ref);
        }

        adt_ref
    }

    /// Registers it in the root module (see [`AdtBuilder::finish_in_module`]).
    pub fn finish(self) -> AdtRef {
        self.finish_in_module(ModuleId::ZERO)
    }
}

/// Makes a trait of the host.
#[derive(Debug)]
pub struct TraitBuilder<'a> {
    context: &'a mut TyCtxt,
    name: String,
    generics: usize,
    functions: IndexVec<TraitFuncRef, BuilderFunction>,
}

type BuilderFunction = (String, Vec<Arg<TypeRef>>, TypeRef, bool);

impl<'a> TraitBuilder<'a> {
    /// A trait called `name`, without functions.
    pub fn new<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self {
            context,
            name: name.into(),
            functions: IndexVec::new(),
            // Type parameters of the trait (see `add_generic`). `Self` (the
            // type of `self`, generic 0 in signatures) isn't one, like in
            // traits written in Mollie: `Shape` written in a program is the
            // same type as the host's `Type::Trait(shape, [])`.
            generics: 0,
        }
    }

    /// Adds a function taking `self` and `params`, returning `returns`.
    #[must_use]
    pub fn func<T: Into<String>, I: IntoIterator<Item = (T, TypeRef)>>(mut self, name: T, params: I, returns: TypeRef) -> Self {
        let mut args = vec![Arg {
            name: "self".into(),
            kind: ArgType::This,
            ty: self.context.types.get_or_add(Type::Generic(0)),
        }];

        args.extend(params.into_iter().map(|(name, ty)| Arg {
            name: name.into(),
            kind: ArgType::Regular,
            ty,
        }));

        self.functions.push((name.into(), args, returns, false));

        self
    }

    /// Gives the last function added a default, the function `Trait::name`,
    /// whose body is compiled separately. It panics if no function was added
    /// yet.
    #[must_use]
    pub fn with_default(mut self) -> Self {
        self.functions.last_mut().expect("a function to give a default").3 = true;

        self
    }

    /// Adds a function without `self`, taking `params` and returning `returns`.
    #[must_use]
    pub fn static_func<T: Into<String>, I: IntoIterator<Item = (T, TypeRef)>>(mut self, name: T, params: I, returns: TypeRef) -> Self {
        self.functions.push((
            name.into(),
            params
                .into_iter()
                .map(|(name, ty)| Arg {
                    name: name.into(),
                    kind: ArgType::Regular,
                    ty,
                })
                .collect(),
            returns,
            false,
        ));

        self
    }

    /// Adds a generic parameter. In signatures, `Self` is generic 0 and these
    /// come after it.
    #[must_use]
    pub const fn add_generic(mut self) -> Self {
        self.generics += 1;

        self
    }

    /// Registers it in `module`. It panics if the name is already taken there.
    pub fn finish_in_module(self, module: ModuleId) -> TraitRef {
        let Self {
            context,
            name: trait_name,
            generics,
            functions,
        } = self;
        let functions = functions
            .into_iter()
            .map(|(_, (name, args, returns, has_default))| {
                // Like defaults written in Mollie: a function whose generic 0
                // is `Self`, then come the trait's generics.
                let default = has_default.then(|| {
                    let ty = context.types.get_or_add(Type::Func(args.iter().map(|arg| arg.ty).collect(), returns));

                    context.def_registry.functions.insert(Func {
                        postfix: false,
                        name: format!("{trait_name}::{name}"),
                        generics: generics + 1,
                        arg_names: args.iter().map(|arg| arg.name.clone()).collect(),
                        ty,
                    })
                });

                TraitFunc {
                    name,
                    args: args.into_boxed_slice(),
                    returns,
                    default,
                }
            })
            .collect();
        let r#trait = Trait {
            name: trait_name,
            generics,
            functions,
        };

        let name = r#trait.name.clone();

        context
            .def_registry
            .register_trait_in_module(module, r#trait, Span::default())
            .unwrap_or_else(|error| panic!("can't register `{name}`: {:?}", error.error))
    }

    /// Registers it in the root module (see
    /// [`TraitBuilder::finish_in_module`]).
    pub fn finish(self) -> TraitRef {
        self.finish_in_module(ModuleId::ZERO)
    }
}

/// Makes an impl block of the host, with functions of a type or its
/// implementation of a trait.
pub struct VTableBuilder<'a> {
    context: &'a mut TypedASTContext,
    target: TypeRef,
    generics: usize,
    origin_trait: Option<(TraitRef, Box<[TypeRef]>)>,
    functions: Vec<(VTableFunc, FunctionBody)>,
}

impl<'a> VTableBuilder<'a> {
    /// An impl block for `target`.
    pub const fn new(context: &'a mut TypedASTContext, target: TypeRef) -> Self {
        Self {
            context,
            target,
            generics: 0,
            origin_trait: None,
            functions: Vec::new(),
        }
    }

    /// Adds a generic parameter of the impl.
    #[must_use]
    pub const fn add_generic(mut self) -> Self {
        self.generics += 1;

        self
    }

    /// Makes it the impl of `trait_ref` with the arguments `trait_args`. Every
    /// function of the trait must then be added.
    #[must_use]
    pub fn implements(mut self, trait_ref: TraitRef, trait_args: impl IntoIterator<Item = TypeRef>) -> Self {
        self.origin_trait = Some((trait_ref, trait_args.into_iter().collect()));

        self
    }

    /// Adds a function called `name` that runs `body`. It takes `args`, named
    /// `arg_names`, and returns `returns`.
    #[must_use]
    pub fn func_body<T: Into<String>, I: IntoIterator<Item = TypeRef>>(
        mut self,
        name: T,
        arg_names: Vec<String>,
        args: I,
        returns: TypeRef,
        body: FunctionBody,
    ) -> Self {
        let ty = self.context.tcx.types.get_or_add(Type::Func(args.into_iter().collect(), returns));

        self.functions.push((
            VTableFunc {
                trait_func: None,
                name: name.into(),
                arg_names,
                generics: 0,
                ty,
                accessor: None,
            },
            body,
        ));

        self
    }

    /// Makes the last function added a getter or a setter of a property.
    #[must_use]
    pub fn accessor(mut self, accessor: mollie_typing::Accessor) -> Self {
        if let Some((func, _)) = self.functions.last_mut() {
            func.accessor = Some(accessor);
        }

        self
    }

    /// Adds a function called `name`, taking `args` and returning `returns`,
    /// implemented by the host's function `external_name`.
    #[must_use]
    pub fn func<T: Into<String>, I: IntoIterator<Item = TypeRef>>(mut self, name: T, external_name: &'static str, args: I, returns: TypeRef) -> Self {
        let ty = self.context.tcx.types.get_or_add(Type::Func(args.into_iter().collect(), returns));

        self.functions.push((
            VTableFunc {
                trait_func: None,
                name: name.into(),
                arg_names: Vec::new(),
                generics: 0,
                ty,
                accessor: None,
            },
            FunctionBody::Import(external_name),
        ));

        self
    }

    /// Registers the impl. The functions of its trait come first, in the
    /// trait's order. It panics if one of them wasn't added.
    pub fn finish(mut self) -> ImplRef {
        let (origin_trait, trait_args) = self.origin_trait.take().map_or_default(|(trait_ref, args)| (Some(trait_ref), args));

        // Trait objects call functions by index, so functions of the trait go
        // first and in the trait's order.
        if let Some(trait_ref) = origin_trait {
            let r#trait = &self.context.tcx.def_registry.traits[trait_ref];
            let mut ordered = Vec::with_capacity(self.functions.len());

            for (trait_func, func) in r#trait.functions.iter() {
                let index = self
                    .functions
                    .iter()
                    .position(|(vfunc, _)| vfunc.name == func.name)
                    .unwrap_or_else(|| panic!("`{}::{}` isn't implemented", r#trait.name, func.name));
                let (mut vfunc, body) = self.functions.remove(index);

                vfunc.trait_func = Some(trait_func);
                ordered.push((vfunc, body));
            }

            ordered.append(&mut self.functions);
            self.functions = ordered;
        }

        let generics = (0..self.generics + usize::from(origin_trait.is_some()))
            .map(|generic| self.context.tcx.types.get_or_add(Type::Generic(generic)))
            .collect();
        let (functions, bodies): (Vec<VTableFunc>, Vec<FunctionBody>) = self.functions.into_iter().unzip();

        let impl_ref = self.context.tcx.register_impl(VTableGenerator {
            ty: self.target,
            origin_trait,
            trait_args,
            generics,
            bounds: Box::new([]),
            functions: functions.into_iter().collect(),
        });

        self.context.vtables.insert(
            impl_ref,
            bodies.into_iter().enumerate().map(|(index, body)| (VFuncRef::new(index), body)).collect(),
        );

        impl_ref
    }
}
