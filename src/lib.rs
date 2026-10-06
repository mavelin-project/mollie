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

pub mod host;
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
        Adt, AdtKind, AdtRef, AdtVariant, AdtVariantField, AdtVariantRef, Arg, ArgType, FieldRef, ImplRef, IntType, ModuleId, PrimitiveType, Trait, TraitFunc,
        TraitFuncRef, TraitRef, TyCtxt, Type, TypeRef, UIntType, VFuncRef, VTableFunc, VTableGenerator,
    },
};

/// A pointer to the value of a GC object.
///
/// It doesn't keep the object alive: values passed between the host and
/// compiled code are `GcPtr`s. To keep an object alive while the host holds
/// it, use [`GcPtr::root`].
#[repr(transparent)]
pub struct GcPtr<T>(*mut GcValue<T>);

impl<T> Clone for GcPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for GcPtr<T> {}

/// Runs `f` with the heap of the running program.
///
/// # Panics
///
/// Panics if no program runs on this thread: values of programs can only be
/// created while their program runs (e.g. in host functions or in
/// `Compiler::run`), or with the `*_in` functions taking a heap.
fn in_running_heap<R>(f: impl FnOnce(&Heap) -> R) -> R {
    sandbox::with_current_heap(f).expect("no program is running: use the `*_in` functions with the program's heap")
}

impl<T: 'static> GcPtr<T> {
    /// Allocates a GC object holding `value` in the heap of the running
    /// program. It survives the next collection even without being rooted, so
    /// it can be passed to compiled code.
    ///
    /// # Panics
    ///
    /// Panics if no program runs on this thread (see [`GcPtr::new_in`]).
    pub fn from(value: T) -> Self {
        in_running_heap(|heap| Self::new_in(heap, value))
    }

    /// Allocates a GC object holding `value` in `heap` (see
    /// [`GcPtr::from`]).
    pub fn new_in(heap: &Heap, value: T) -> Self {
        // SAFETY: the layout is interned in the heap.
        unsafe { Self::from_parts_in(heap, value, heap.layout_of::<T>()) }
    }
}

impl<T> GcPtr<T> {
    /// Allocates a GC object holding `value` with the layout of a compiled
    /// type (see `CompiledAdt::type_layout`) in the heap of the running
    /// program.
    ///
    /// # Panics
    ///
    /// Panics if no program runs on this thread.
    ///
    /// # Safety
    ///
    /// `layout` must be a layout of the running program's compiler, describing
    /// `T`.
    pub unsafe fn from_parts(value: T, layout: &'static TypeLayout) -> Self {
        in_running_heap(|heap| unsafe { Self::from_parts_in(heap, value, layout) })
    }

    /// Allocates a GC object holding `value` with `layout` in `heap`.
    ///
    /// # Safety
    ///
    /// `layout` must be a layout of the heap's compiler (or interned in the
    /// heap), describing `T`.
    pub unsafe fn from_parts_in(heap: &Heap, value: T, layout: &'static TypeLayout) -> Self {
        let result_value = unsafe { heap.alloc(layout, false) };

        // The memory is zeroed, not a valid `T`, so it must not be dropped.
        unsafe { result_value.cast::<T>().write(value) };

        Self(result_value.cast())
    }

    /// Keeps the object (of `heap`) alive until the returned guard is
    /// dropped.
    pub fn root(self, heap: &Heap) -> GcRoot<'_, T> {
        heap.root(self.0.cast());

        GcRoot { ptr: self, heap }
    }

    pub const fn ptr(&self) -> *const T {
        self.0.cast()
    }

    pub const fn ptr_mut(&mut self) -> *mut T {
        self.0.cast()
    }

    #[allow(clippy::cast_ptr_alignment)]
    pub fn type_layout(&self) -> &'static TypeLayout {
        unsafe { (*self.0.cast::<u8>().wrapping_sub(HEADER_SIZE).cast::<GcValue<()>>()).layout }
    }

    pub fn adt_variant(&self) -> AdtVariantRef {
        let type_layout = self.type_layout();

        match type_layout.kind {
            // SAFETY: discriminant is always placed at the beginning
            Some(AdtKind::Enum) => AdtVariantRef::new(unsafe { self.0.cast::<usize>().read() }),
            _ => AdtVariantRef::ZERO,
        }
    }

    /// The field `field` of this object (of the ADT `adt`), as an `F`.
    ///
    /// # Panics
    ///
    /// Panics if the object isn't a value of `adt`, or `F` doesn't have the
    /// size of the field.
    #[allow(clippy::cast_sign_loss)]
    pub fn get<F>(&self, adt: &CompiledAdt, field: FieldRef) -> Option<&F> {
        assert_eq!(self.type_layout(), adt.type_layout);

        let field = &adt.variants[self.adt_variant()].fields[field].0;

        assert_eq!(size_of::<F>(), field.ty.bytes() as usize);

        unsafe { self.0.byte_add(field.offset as usize).cast::<F>().as_ref() }
    }

    /// Like [`GcPtr::get`], mutably.
    ///
    /// # Panics
    ///
    /// Panics if the object isn't a value of `adt`, or `F` doesn't have the
    /// size of the field.
    #[allow(clippy::cast_sign_loss)]
    pub fn get_mut<F>(&mut self, adt: &CompiledAdt, field: FieldRef) -> Option<&mut F> {
        assert_eq!(self.type_layout(), adt.type_layout);

        let field = &adt.variants[self.adt_variant()].fields[field].0;

        assert_eq!(size_of::<F>(), field.ty.bytes() as usize);

        unsafe { self.0.byte_add(field.offset as usize).cast::<F>().as_mut() }
    }

    /// Reads a field holding an array (a pointer to a GC array).
    #[allow(clippy::cast_sign_loss)]
    fn array_field(&self, adt: &CompiledAdt, field: FieldRef) -> Option<*mut Array> {
        assert_eq!(self.type_layout(), adt.type_layout);

        let field = &adt.variants[self.adt_variant()].fields[field].0;

        assert_eq!(size_of::<*mut Array>(), field.ty.bytes() as usize);

        let array = unsafe { self.0.byte_add(field.offset as usize).cast::<*mut Array>().read() };

        (!array.is_null()).then_some(array)
    }

    pub fn get_slice<F>(&self, adt: &CompiledAdt, field: FieldRef) -> Option<&[F]> {
        let array = unsafe { &*self.array_field(adt, field)? };

        Some(unsafe { std::slice::from_raw_parts(array.ptr.cast::<F>(), array.length) })
    }

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

/// A Mollie string, as passed between the host and compiled code: a pointer
/// to an immutable string.
///
/// Like [`GcPtr`], it doesn't keep the string alive.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct MolStr(*const Array);

impl MolStr {
    /// Allocates a string holding `text` in the heap of the running program.
    /// It survives the next collection even without being rooted, so it can
    /// be passed to compiled code.
    ///
    /// # Panics
    ///
    /// Panics if no program runs on this thread (see [`MolStr::new_in`]).
    pub fn new(text: &str) -> Self {
        in_running_heap(|heap| Self::new_in(heap, text))
    }

    /// Allocates a string holding `text` in `heap`.
    pub fn new_in(heap: &Heap, text: &str) -> Self {
        Self(compiler::strings::new(heap, text))
    }

    pub const fn ptr(&self) -> *const Array {
        self.0
    }

    pub const fn as_str(&self) -> &str {
        // SAFETY: values of `MolStr` are only created from strings.
        unsafe { compiler::strings::as_str(self.0) }
    }

    /// Keeps the string (of `heap`) alive until the returned guard is
    /// dropped.
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

/// Keeps a GC object alive while the host holds it. It borrows the heap of
/// the object, so its compiler can't be dropped before it.
pub struct GcRoot<'h, T> {
    ptr: GcPtr<T>,
    heap: &'h Heap,
}

impl<T> GcRoot<'_, T> {
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

pub trait MollieTypeOf {
    fn generic_index() -> Option<usize> {
        None
    }

    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef;
}

pub trait MollieMultipleTypeOf {
    fn generic_index() -> Option<usize> {
        None
    }

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
    bool   => Type::Primitive(PrimitiveType::Bool              ),
    MolStr => Type::Primitive(PrimitiveType::String               )
}

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

        context.types.get_or_add(Type::Array(element, None))
    }
}

impl<T: MollieTypeOf, const U: usize> MollieTypeOf for [T; U] {
    fn mollie_type_of(context: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type_of(context);

        context.types.get_or_add(Type::Array(element, Some(U)))
    }
}

mollie_arg_types![A];
mollie_arg_types![A, B];
mollie_arg_types![A, B, C];
mollie_arg_types![A, B, C, D];
mollie_arg_types![A, B, C, D, E];
mollie_arg_types![A, B, C, D, E, F];
mollie_arg_types![A, B, C, D, E, F, G];

pub fn func<Args: MollieMultipleTypeOf, Returns: MollieTypeOf>(context: &mut TyCtxt) -> TypeRef {
    let args = Args::mollie_type_of(context).collect();
    let returns = Returns::mollie_type_of(context);

    context.types.get_or_add(Type::Func(args, returns))
}

#[derive(Debug)]
pub struct AdtBuilder<'a> {
    context: &'a mut TyCtxt,
    name: Option<String>,
    collectable: bool,
    value: bool,
    variants: Vec<(Option<String>, AdtBuilderVariant)>,
    generics: usize,
    kind: AdtKind,
}

impl<'a> AdtBuilder<'a> {
    pub fn new_struct<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self {
            context,
            name: Some(name.into()),
            collectable: true,
            value: false,
            variants: vec![(None, vec![])],
            generics: 0,
            kind: AdtKind::Struct,
        }
    }

    pub fn new_enum<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self {
            context,
            name: Some(name.into()),
            collectable: true,
            value: false,
            variants: vec![],
            generics: 0,
            kind: AdtKind::Enum,
        }
    }

    #[must_use]
    pub const fn non_gc_collectable(mut self) -> Self {
        self.collectable = false;

        self
    }

    /// Makes this a value type (`value struct`, `value enum`): values are
    /// copied instead of referenced, and are stored with the C layout of
    /// their fields.
    #[must_use]
    pub const fn value_type(mut self) -> Self {
        self.value = true;

        self
    }

    #[must_use]
    pub fn variant<T: Into<String>>(mut self, name: T) -> Self {
        self.variants.push((Some(name.into()), vec![(
            String::from("<discriminant>"),
            self.context.types.get_or_add(Type::Primitive(PrimitiveType::UInt(UIntType::USize))),
            None,
        )]));

        self
    }

    #[must_use]
    pub const fn add_generic(mut self) -> Self {
        self.generics += 1;

        self
    }

    /// Adds the field `name` of type `T` with the value `default` when it's
    /// omitted, to the last variant.
    ///
    /// # Panics
    ///
    /// Panics if `T` is a generic parameter the builder doesn't have (see
    /// [`AdtBuilder::add_generic`]).
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

    /// Adds the field `name` of type `T` to the last variant.
    ///
    /// # Panics
    ///
    /// Panics if `T` is a generic parameter the builder doesn't have (see
    /// [`AdtBuilder::add_generic`]).
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

    #[must_use]
    pub fn field_ty<T: Into<String>>(mut self, name: T, ty: TypeRef) -> Self {
        if let Some((_, variant)) = self.variants.last_mut() {
            variant.push((name.into(), ty, None));
        }

        self
    }

    /// Registers the ADT in `module`.
    ///
    /// # Panics
    ///
    /// Panics if an item with the same name already exists in the module.
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

        let name = adt.name.clone().unwrap_or_default();
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

    pub fn finish(self) -> AdtRef {
        self.finish_in_module(ModuleId::ZERO)
    }
}

#[derive(Debug)]
pub struct TraitBuilder<'a> {
    context: &'a mut TyCtxt,
    name: String,
    generics: usize,
    functions: IndexVec<TraitFuncRef, (String, Vec<Arg<TypeRef>>, TypeRef)>,
}

impl<'a> TraitBuilder<'a> {
    pub fn new<T: Into<String>>(context: &'a mut TyCtxt, name: T) -> Self {
        Self {
            context,
            name: name.into(),
            functions: IndexVec::new(),
            generics: 1,
        }
    }

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

        self.functions.push((name.into(), args, returns));

        self
    }

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
        ));

        self
    }

    #[must_use]
    pub const fn add_generic(mut self) -> Self {
        self.generics += 1;

        self
    }

    /// Registers the trait in `module`.
    ///
    /// # Panics
    ///
    /// Panics if an item with the same name already exists in the module.
    pub fn finish_in_module(self, module: ModuleId) -> TraitRef {
        let r#trait = Trait {
            name: self.name,
            generics: self.generics,
            functions: self
                .functions
                .into_iter()
                .map(|(_, (name, args, returns))| TraitFunc {
                    name,
                    args: args.into_boxed_slice(),
                    returns,
                    default: None,
                })
                .collect(),
        };

        let name = r#trait.name.clone();

        self.context
            .def_registry
            .register_trait_in_module(module, r#trait, Span::default())
            .unwrap_or_else(|error| panic!("can't register `{name}`: {:?}", error.error))
    }

    pub fn finish(self) -> TraitRef {
        self.finish_in_module(ModuleId::ZERO)
    }
}

pub struct VTableBuilder<'a> {
    context: &'a mut TypedASTContext,
    target: TypeRef,
    generics: usize,
    origin_trait: Option<(TraitRef, Box<[TypeRef]>)>,
    functions: Vec<(VTableFunc, FunctionBody)>,
}

impl<'a> VTableBuilder<'a> {
    pub const fn new(context: &'a mut TypedASTContext, target: TypeRef) -> Self {
        Self {
            context,
            target,
            generics: 0,
            origin_trait: None,
            functions: Vec::new(),
        }
    }

    #[must_use]
    pub const fn add_generic(mut self) -> Self {
        self.generics += 1;

        self
    }

    /// Makes this an impl of `trait_ref` with type arguments `trait_args`.
    /// Every function of the trait must be added with [`Self::func`].
    ///
    /// Like in trait impls written in Mollie, generic 0 is `Self`, so the
    /// impl's own generics start from 1.
    #[must_use]
    pub fn implements(mut self, trait_ref: TraitRef, trait_args: impl IntoIterator<Item = TypeRef>) -> Self {
        self.origin_trait = Some((trait_ref, trait_args.into_iter().collect()));

        self
    }

    /// Adds a function with the body `body`, taking arguments called
    /// `arg_names` (`self` first for methods) of types `args`.
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
            },
            body,
        ));

        self
    }

    /// Adds a function implemented by the host function `external_name`.
    /// Methods take the receiver as their first argument.
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
            },
            FunctionBody::Import(external_name),
        ));

        self
    }

    /// Registers the impl.
    ///
    /// # Panics
    ///
    /// Panics if a function of the implemented trait is missing.
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
