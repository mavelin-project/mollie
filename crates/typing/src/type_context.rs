use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::{BuildHasher, DefaultHasher, Hash, Hasher, RandomState},
    ops::Index,
};

use indexmap::IndexMap;
use mollie_const::ConstantValue;
use mollie_index::{Idx, IndexVec};
use mollie_shared::{LangItem, Span, pretty_fmt::FmtIteratorExt};

use crate::{
    Adt, AdtRef, AdtVariantRef, Arg, ConstRef, CoreTypes, DiagnosticContext, FuncRef, ImplRef, IntType, ModuleId, PrimitiveType, TraitFuncRef, TraitRef,
    TypeInfo, TypeSolver, UIntType, VFuncRef,
    diagnostic::{Diagnostic, DiagnosticDisplay},
    error::{DefinitionType, TypeError},
    ty::{Type, TypeRef},
};

#[derive(Debug)]
pub struct TypeStorage {
    types: IndexVec<TypeRef, Type>,
    /// Every type, to find it again in constant time.
    index: HashMap<Type, TypeRef>,
    /// Keys of hashes identifying types (see [`TypeStorage::hash_of`]),
    /// random so that programs can't be written to make two types collide.
    hash_keys: RandomState,
    pub core_types: CoreTypes<TypeRef>,
}

impl Default for TypeStorage {
    fn default() -> Self {
        let mut types = IndexVec::default();

        let core_types = CoreTypes {
            void: types.insert(Type::Primitive(PrimitiveType::Void)),
            any: types.insert(Type::Primitive(PrimitiveType::Any)),
            bool: types.insert(Type::Primitive(PrimitiveType::Bool)),
            i8: types.insert(Type::Primitive(PrimitiveType::Int(IntType::I8))),
            i16: types.insert(Type::Primitive(PrimitiveType::Int(IntType::I16))),
            i32: types.insert(Type::Primitive(PrimitiveType::Int(IntType::I32))),
            i64: types.insert(Type::Primitive(PrimitiveType::Int(IntType::I64))),
            isize: types.insert(Type::Primitive(PrimitiveType::Int(IntType::ISize))),
            u8: types.insert(Type::Primitive(PrimitiveType::UInt(UIntType::U8))),
            u16: types.insert(Type::Primitive(PrimitiveType::UInt(UIntType::U16))),
            u32: types.insert(Type::Primitive(PrimitiveType::UInt(UIntType::U32))),
            u64: types.insert(Type::Primitive(PrimitiveType::UInt(UIntType::U64))),
            usize: types.insert(Type::Primitive(PrimitiveType::UInt(UIntType::USize))),
            f32: types.insert(Type::Primitive(PrimitiveType::F32)),
            string: types.insert(Type::Primitive(PrimitiveType::String)),
        };

        let index = types.iter().map(|(key, ty)| (ty.clone(), key)).collect();

        Self {
            types,
            index,
            hash_keys: RandomState::new(),
            core_types,
        }
    }
}

impl TypeStorage {
    pub fn is_likely_same(&self, ty: TypeRef, other: TypeRef) -> bool {
        match (&self.types[ty], &self.types[other]) {
            (Type::Generic(..), _) | (_, Type::Generic(..)) => true,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, size), &Type::Array(other_element, other_size)) => {
                self.is_likely_same(element, other_element)
                    && match (size, other_size) {
                        (None, None | Some(_)) => true,
                        (Some(_), None) => false,
                        (Some(size), Some(other_size)) => size == other_size,
                    }
            }
            (Type::Func(args, returns), Type::Func(other_args, other_returns)) => {
                args.len() == other_args.len()
                    && args
                        .iter()
                        .zip(other_args)
                        .all(|(&field_type, &other_field_type)| self.is_likely_same(field_type, other_field_type))
                    && self.is_likely_same(*returns, *other_returns)
            }
            (Type::Adt(adt, type_args), Type::Adt(other_adt, other_type_args)) => {
                type_args.len() == other_type_args.len()
                    && type_args
                        .iter()
                        .zip(other_type_args)
                        .all(|(&field_type, &other_field_type)| self.is_likely_same(field_type, other_field_type))
                    && adt == other_adt
            }
            (Type::Trait(trait_ref, type_args), Type::Trait(other_trait_ref, other_type_args)) => {
                type_args.len() == other_type_args.len()
                    && type_args
                        .iter()
                        .zip(other_type_args)
                        .all(|(&arg, &other_arg)| self.is_likely_same(arg, other_arg))
                    && trait_ref == other_trait_ref
            }
            _ => false,
        }
    }

    /// Whether `ty` has generic parameters in it.
    /// Whether `ty` has at most `budget` nodes (`Pair<i32, i32>` has 3), with
    /// shared parts counted every time. Stops counting once over the budget,
    /// so it's cheap for huge types.
    pub fn size_within(&self, ty: TypeRef, budget: usize) -> bool {
        fn remaining(storage: &TypeStorage, ty: TypeRef, budget: usize) -> Option<usize> {
            let mut budget = budget.checked_sub(1)?;

            match &storage.types[ty] {
                &Type::Array(element, _) => budget = remaining(storage, element, budget)?,
                Type::Adt(_, args) | Type::Trait(_, args) => {
                    for &arg in args {
                        budget = remaining(storage, arg, budget)?;
                    }
                }
                Type::Func(args, returns) => {
                    for &arg in args {
                        budget = remaining(storage, arg, budget)?;
                    }

                    budget = remaining(storage, *returns, budget)?;
                }
                Type::Primitive(_) | Type::Generic(_) | Type::Error => (),
            }

            Some(budget)
        }

        remaining(self, ty, budget).is_some()
    }

    pub fn has_generics(&self, ty: TypeRef) -> bool {
        match &self.types[ty] {
            Type::Generic(_) => true,
            &Type::Array(element, _) => self.has_generics(element),
            Type::Adt(_, args) | Type::Trait(_, args) => args.iter().any(|&arg| self.has_generics(arg)),
            Type::Func(args, returns) => args.iter().any(|&arg| self.has_generics(arg)) || self.has_generics(*returns),
            Type::Primitive(_) | Type::Error => false,
        }
    }

    pub fn is_same(&self, ty: TypeRef, other: TypeRef) -> bool {
        match (&self.types[ty], &self.types[other]) {
            // Errors are already reported, don't report everything they touch again.
            (Type::Error | Type::Primitive(PrimitiveType::Any), _) | (_, Type::Error | Type::Primitive(PrimitiveType::Any)) => true,
            (Type::Generic(index), Type::Generic(other_index)) => index == other_index,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, size), &Type::Array(other_element, other_size)) => self.is_same(element, other_element) && size == other_size,
            (Type::Func(args, returns), Type::Func(other_args, other_returns)) => {
                args.len() == other_args.len()
                    && args.iter().zip(other_args).all(|(&arg, &other_arg)| self.is_same(arg, other_arg))
                    && self.is_same(*returns, *other_returns)
            }
            (Type::Adt(adt, type_args), Type::Adt(other_adt, other_type_args)) => {
                type_args.len() == other_type_args.len()
                    && type_args.iter().zip(other_type_args).all(|(&arg, &other_arg)| self.is_same(arg, other_arg))
                    && adt == other_adt
            }
            (Type::Trait(trait_ref, type_args), Type::Trait(other_trait_ref, other_type_args)) => {
                type_args.len() == other_type_args.len()
                    && type_args.iter().zip(other_type_args).all(|(&arg, &other_arg)| self.is_same(arg, other_arg))
                    && trait_ref == other_trait_ref
            }
            _ => false,
        }
    }

    /// Checks whether an impl written for `impl_ty` applies to `ty`. Unlike
    /// [`TypeStorage::is_likely_same`], this is directional: generics and
    /// unsized arrays are wildcards only on the impl side.
    /// Like [`TypeStorage::impl_matches`], also binding generics of the impl
    /// to the parts of `ty` they match. A generic matched twice must match the
    /// same type.
    pub fn match_impl(&self, impl_ty: TypeRef, ty: TypeRef, args: &mut [Option<TypeRef>]) -> bool {
        match (&self.types[impl_ty], &self.types[ty]) {
            (&Type::Generic(index), _) => match args.get_mut(index) {
                Some(Some(bound)) => *bound == ty || self.is_same(*bound, ty),
                Some(arg) => {
                    *arg = Some(ty);

                    true
                }
                None => true,
            },
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, size), &Type::Array(other_element, other_size)) => {
                self.match_impl(element, other_element, args) && (size.is_none() || size == other_size)
            }
            (Type::Func(impl_args, returns), Type::Func(other_args, other_returns)) => {
                let (impl_args, returns, other_args, other_returns) = (impl_args.clone(), *returns, other_args.clone(), *other_returns);

                impl_args.len() == other_args.len()
                    && impl_args
                        .iter()
                        .zip(&other_args)
                        .all(|(&arg, &other_arg)| self.match_impl(arg, other_arg, args))
                    && self.match_impl(returns, other_returns, args)
            }
            (Type::Adt(adt, type_args), Type::Adt(other_adt, other_type_args)) if adt == other_adt => {
                let (type_args, other_type_args) = (type_args.clone(), other_type_args.clone());

                type_args.len() == other_type_args.len()
                    && type_args
                        .iter()
                        .zip(&other_type_args)
                        .all(|(&arg, &other_arg)| self.match_impl(arg, other_arg, args))
            }
            (Type::Trait(trait_ref, type_args), Type::Trait(other_trait_ref, other_type_args)) if trait_ref == other_trait_ref => {
                let (type_args, other_type_args) = (type_args.clone(), other_type_args.clone());

                type_args.len() == other_type_args.len()
                    && type_args
                        .iter()
                        .zip(&other_type_args)
                        .all(|(&arg, &other_arg)| self.match_impl(arg, other_arg, args))
            }
            _ => false,
        }
    }

    pub fn impl_matches(&self, impl_ty: TypeRef, ty: TypeRef) -> bool {
        match (&self.types[impl_ty], &self.types[ty]) {
            (Type::Generic(_), _) => true,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, size), &Type::Array(other_element, other_size)) => {
                self.impl_matches(element, other_element) && (size.is_none() || size == other_size)
            }
            (Type::Func(args, returns), Type::Func(other_args, other_returns)) => {
                args.len() == other_args.len()
                    && args.iter().zip(other_args).all(|(&arg, &other_arg)| self.impl_matches(arg, other_arg))
                    && self.impl_matches(*returns, *other_returns)
            }
            (Type::Adt(adt, type_args), Type::Adt(other_adt, other_type_args)) => {
                adt == other_adt
                    && type_args.len() == other_type_args.len()
                    && type_args
                        .iter()
                        .zip(other_type_args)
                        .all(|(&arg, &other_arg)| self.impl_matches(arg, other_arg))
            }
            (Type::Trait(trait_ref, type_args), Type::Trait(other_trait_ref, other_type_args)) => {
                trait_ref == other_trait_ref
                    && type_args.len() == other_type_args.len()
                    && type_args
                        .iter()
                        .zip(other_type_args)
                        .all(|(&arg, &other_arg)| self.impl_matches(arg, other_arg))
            }
            _ => false,
        }
    }

    pub fn get_or_add(&mut self, typo: Type) -> TypeRef {
        if let Some(&key) = self.index.get(&typo) {
            return key;
        }

        let key = self.types.insert(typo.clone());

        self.index.insert(typo, key);

        key
    }

    pub fn apply_type_args(&mut self, ty: TypeRef, type_args: &[TypeRef]) -> TypeRef {
        match self.types[ty].clone() {
            Type::Error | Type::Primitive(_) => ty,
            Type::Array(element, size) => {
                let element = self.apply_type_args(element, type_args);

                self.get_or_add(Type::Array(element, size))
            }
            Type::Adt(adt_ref, mut adt_type_args) => {
                for arg in &mut adt_type_args {
                    *arg = self.apply_type_args(*arg, type_args);
                }

                self.get_or_add(Type::Adt(adt_ref, adt_type_args))
            }
            Type::Trait(trait_ref, mut adt_type_args) => {
                for arg in &mut adt_type_args {
                    *arg = self.apply_type_args(*arg, type_args);
                }

                self.get_or_add(Type::Trait(trait_ref, adt_type_args))
            }
            Type::Func(mut args, returns) => {
                for arg in &mut args {
                    *arg = self.apply_type_args(*arg, type_args);
                }

                let returns = self.apply_type_args(returns, type_args);

                self.get_or_add(Type::Func(args, returns))
            }
            Type::Generic(i) => type_args.get(i).copied().unwrap_or_else(|| self.get_or_add(Type::Error)),
        }
    }

    pub fn hash_value_into<T: Hasher>(&self, state: &mut T, ty: &Type) {
        self.hash_instance_value_into(state, ty, &[]);
    }

    pub fn hash_into<T: Hasher>(&self, state: &mut T, info: TypeRef) {
        self.hash_value_into(state, &self.types[info]);
    }

    pub fn hash_of(&self, info: TypeRef) -> u64 {
        self.hash_of_instance(info, &[])
    }

    /// A hasher for hashes identifying types (with [`TypeStorage::hash_into`]
    /// and the like). Its keys are random, so the hashes differ between
    /// compilers: they only identify types, they're never ordered or shown to
    /// programs.
    pub fn hasher(&self) -> DefaultHasher {
        self.hash_keys.build_hasher()
    }

    /// Hashes `ty` with its generics replaced by `generics`. For concrete
    /// `generics` it's the same as the hash of `apply_type_args(ty, generics)`,
    /// without creating that type.
    pub fn hash_of_instance(&self, ty: TypeRef, generics: &[TypeRef]) -> u64 {
        let mut state = self.hasher();

        self.hash_instance_into(&mut state, ty, generics);

        state.finish()
    }

    pub fn hash_instance_into<T: Hasher>(&self, state: &mut T, ty: TypeRef, generics: &[TypeRef]) {
        self.hash_instance_value_into(state, &self.types[ty], generics);
    }

    fn hash_instance_value_into<T: Hasher>(&self, state: &mut T, ty: &Type, generics: &[TypeRef]) {
        match ty {
            Type::Primitive(primitive_type) => primitive_type.hash(state),
            Type::Func(args, returns) => {
                "fn".hash(state);

                for &arg in args {
                    self.hash_instance_into(state, arg, generics);
                }

                self.hash_instance_into(state, *returns, generics);
            }
            Type::Trait(trait_ref, type_args) => {
                "trait".hash(state);

                trait_ref.hash(state);

                for &type_arg in type_args {
                    self.hash_instance_into(state, type_arg, generics);
                }
            }
            Type::Adt(adt_ref, type_args) => {
                "adt".hash(state);

                adt_ref.hash(state);

                for &type_arg in type_args {
                    self.hash_instance_into(state, type_arg, generics);
                }
            }
            &Type::Array(element, _) => {
                "array".hash(state);

                // Arrays of every size are the same objects at run time (and
                // implement the same impls, like `T[]`'s): only their elements
                // tell them apart.
                self.hash_instance_into(state, element, generics);
            }
            &Type::Generic(i) => {
                if let Some(&ty) = generics.get(i) {
                    self.hash_instance_into(state, ty, &[]);
                } else {
                    "generic".hash(state);

                    i.hash(state);
                }
            }
            Type::Error => "error".hash(state),
        }
    }
}

impl Index<TypeRef> for TypeStorage {
    type Output = Type;

    fn index(&self, index: TypeRef) -> &Self::Output {
        &self.types[index]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicKind {
    SizeOf,
    AlignOf,
    SizeOfValue,
    AlignOfValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleItem {
    SubModule(ModuleId),
    Adt(AdtRef),
    Trait(TraitRef),
    Func(FuncRef),
    Intrinsic(IntrinsicKind, TypeRef),
    /// A constant, evaluated at compile time.
    Const(ConstRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleSpan(pub ModuleId, pub Span);

#[cfg(feature = "ariadne")]
impl ariadne::Span for ModuleSpan {
    type SourceId = ModuleId;

    fn source(&self) -> &Self::SourceId {
        &self.0
    }

    fn start(&self) -> usize {
        self.1.start
    }

    fn end(&self) -> usize {
        self.1.end
    }
}

#[derive(Debug)]
pub struct Module {
    pub parent: Option<ModuleId>,
    pub name: String,
    pub id: ModuleId,
    pub items: IndexMap<String, (ModuleItem, DefinitionType, ModuleSpan)>,
}

impl Module {
    pub fn new<T: Into<String>>(id: ModuleId, name: T) -> Self {
        Self {
            parent: None,
            name: name.into(),
            id,
            items: IndexMap::new(),
        }
    }

    pub fn get_item<T: AsRef<str>>(&self, name: T) -> Option<ModuleItem> {
        self.items.get(name.as_ref()).map(|item| item.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangItemValue {
    Adt(AdtRef),
    AdtVariant(AdtRef, AdtVariantRef),
    Trait(TraitRef),
    TraitFunc(TraitRef, TraitFuncRef),
}

/// A constant of a module (`const NAME: Type = value;`).
#[derive(Debug, Clone)]
pub struct Const {
    pub name: String,
    /// Filled once the constant is evaluated.
    pub ty: TypeRef,
    /// `None` until the constant is evaluated, or if it can't be.
    pub value: Option<ConstantValue>,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub postfix: bool,
    pub name: String,
    /// Number of generic parameters. Generic `i` of the function's type is
    /// its `i`-th parameter.
    pub generics: usize,
    pub arg_names: Vec<String>,
    pub ty: TypeRef,
    // pub kind: VTableFuncKind,
}

#[derive(Debug)]
pub struct TraitFunc {
    pub name: String,
    pub args: Box<[Arg<TypeRef>]>,
    pub returns: TypeRef,
    /// The default implementation: a function whose generic 0 is `Self`,
    /// followed by generics of the trait.
    pub default: Option<FuncRef>,
}

#[derive(Debug)]
pub struct Trait {
    pub name: String,
    pub generics: usize,
    pub functions: IndexVec<TraitFuncRef, TraitFunc>,
}

impl Trait {
    /// Returns the byte offset of the function's pointer inside a vtable, or
    /// `None` if the trait has no function called `name`. The first slot of a
    /// vtable holds the type id, so function pointers start after it.
    pub fn get_func_offset<T: AsRef<str>>(&self, name: T) -> Option<usize> {
        let name = name.as_ref();

        self.functions
            .values()
            .position(|func| func.name == name)
            .map(|index| (index + 1) * size_of::<usize>())
    }
}

// #[derive(Debug, Clone)]
// pub enum VTableFuncKind<S = ()> {
//     Local { ast: TypedAST<SolvedPass>, entry: BlockRef },
//     External(&'static str),
//     Special(S),
// }

#[derive(Debug, Clone)]
pub struct VTableFunc<T = TypeRef> {
    pub trait_func: Option<TraitFuncRef>,
    pub name: String,
    pub arg_names: Vec<String>,
    /// Number of the function's own generic parameters, which come after the
    /// generics of the impl block in its type.
    pub generics: usize,
    pub ty: T,
    // pub kind: VTableFuncKind,
}

/// A bound of a generic parameter: `T: Shape` or `T: Source<i32>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    /// Index of the generic parameter.
    pub generic: usize,
    pub trait_ref: TraitRef,
    pub trait_args: Box<[TypeRef]>,
}

#[derive(Debug)]
pub struct VTableGenerator {
    pub ty: TypeRef,
    pub origin_trait: Option<TraitRef>,
    /// Type arguments of the implemented trait, in terms of `generics` (e.g.
    /// `[T]` for `impl<T> Iterator<T> for ArrayIter<T>`). Empty for inherent
    /// impls.
    pub trait_args: Box<[TypeRef]>,
    pub generics: Box<[TypeRef]>,
    pub bounds: Box<[Bound]>,
    pub functions: IndexVec<VFuncRef, VTableFunc>,
}

#[derive(Debug, Default)]
pub struct DefRegistry {
    pub modules: IndexVec<ModuleId, Module>,
    pub adt_types: IndexVec<AdtRef, Adt>,
    pub functions: IndexVec<FuncRef, Func>,
    pub traits: IndexVec<TraitRef, Trait>,
    pub language_items: HashMap<LangItem, LangItemValue>,
    pub constants: IndexVec<ConstRef, Const>,
    /// Bounds of generic parameters of functions.
    pub func_bounds: HashMap<FuncRef, Box<[Bound]>>,
    /// Default values of parameters of functions, by parameter: functions
    /// taking the parameters before it and returning its value.
    pub func_defaults: HashMap<FuncRef, Box<[Option<FuncRef>]>>,
    /// Value types (`value struct`, `value enum`): copied, not referenced.
    pub value_types: HashSet<AdtRef>,
    /// Modules hidden from programs by [`DefRegistry::restrict`], with their
    /// entries in their parents.
    pub restricted_modules: HashMap<ModuleId, (String, (ModuleItem, DefinitionType, ModuleSpan))>,
    /// Root modules of libraries (like `std`), visible by name from every
    /// module, like Rust's extern crates.
    pub extern_roots: IndexMap<String, ModuleId>,
    /// Module whose items are visible in every module unless shadowed, like
    /// Rust's prelude.
    pub prelude: Option<ModuleId>,
}

impl DefRegistry {
    /// Registers the root module of a library, visible from every module as
    /// `name`.
    pub fn register_extern_root<T: Into<String>>(&mut self, name: T) -> ModuleId {
        let name = name.into();
        let id = self.modules.next_index();
        let id = self.modules.insert(Module {
            parent: None,
            name: name.clone(),
            id,
            items: IndexMap::new(),
        });

        self.extern_roots.insert(name, id);

        id
    }

    /// Looks up `name` as it's written in code of `module`: its own items
    /// (declared or imported), then roots of libraries, then items of the
    /// host ([`ModuleId::ZERO`]), then the prelude.
    ///
    /// Only the first segment of a path is looked up like this, the following
    /// ones are items of the module before them ([`Module::get_item`]).
    pub fn lookup(&self, module: ModuleId, name: &str) -> Option<ModuleItem> {
        self.modules[module]
            .get_item(name)
            .or_else(|| self.extern_roots.get(name).map(|&root| ModuleItem::SubModule(root)))
            .or_else(|| self.modules[ModuleId::ZERO].get_item(name))
            .or_else(|| self.prelude.and_then(|prelude| self.modules[prelude].get_item(name)))
    }

    /// Registers the root module of a program. Every program has its own, so
    /// programs (and new versions of a program, when it's reloaded) can
    /// declare items with the same names. Items of the host are visible in
    /// every program (see [`DefRegistry::lookup`]).
    pub fn register_program_root(&mut self) -> ModuleId {
        let id = self.modules.next_index();

        self.modules.insert(Module {
            parent: None,
            name: String::from("<root>"),
            id,
            items: IndexMap::new(),
        })
    }

    /// Whether `module` is a root, other than the root of a library: the
    /// host module or the root of a program.
    pub fn is_root(&self, module: ModuleId) -> bool {
        self.modules[module].parent.is_none() && !self.extern_roots.values().any(|&root| root == module)
    }

    /// Hides `module` (e.g. a module of host functions) from programs: they
    /// can't name it until it's granted with [`DefRegistry::grant`]. Returns
    /// `false` if the module is the root one or is already restricted.
    pub fn restrict(&mut self, module: ModuleId) -> bool {
        let Some(parent) = self.modules[module].parent else {
            return false;
        };

        let name = self.modules[module].name.clone();

        match self.modules[parent].items.shift_remove(&name) {
            Some(entry) => {
                self.restricted_modules.insert(module, (name, entry));

                true
            }
            None => false,
        }
    }

    /// Whether `name` is a module hidden by [`DefRegistry::restrict`] that
    /// code of `module` would see once it's granted (see
    /// [`DefRegistry::lookup`]).
    pub fn is_restricted(&self, module: ModuleId, name: &str) -> bool {
        self.restricted_modules.iter().any(|(&restricted, (restricted_name, _))| {
            restricted_name == name
                && self.modules[restricted]
                    .parent
                    .is_some_and(|parent| parent == module || parent == ModuleId::ZERO)
        })
    }

    /// Makes a module hidden by [`DefRegistry::restrict`] available to
    /// programs again. Returns `false` if it isn't restricted.
    pub fn grant(&mut self, module: ModuleId) -> bool {
        let (Some((name, entry)), Some(parent)) = (self.restricted_modules.remove(&module), self.modules[module].parent) else {
            return false;
        };

        self.modules[parent].items.insert(name, entry);

        true
    }

    /// Registers the module `name` in the root module.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_module<T: Into<String>>(&mut self, name: T, span: Span) -> Result<ModuleId, Diagnostic> {
        self.register_module_in_module(ModuleId::ZERO, name, span)
    }

    /// Registers the module `name` in `parent`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_module_in_module<T: Into<String>>(&mut self, parent: ModuleId, name: T, span: Span) -> Result<ModuleId, Diagnostic> {
        let name = name.into();

        if let Some(&(_, def_type, one)) = self.modules[parent].items.get(&name) {
            Err(Diagnostic::new(TypeError::AlreadyExists {
                name,
                primary: def_type,
                secondary: DefinitionType::Local,
            })
            .with_primary_span(one.0, one.1)
            .with_secondary_span(parent, span))
        } else {
            let id = self.modules.next_index();
            let id = self.modules.insert(Module {
                parent: Some(parent),
                name: name.clone(),
                id,
                items: IndexMap::new(),
            });

            self.modules[parent]
                .items
                .insert(name, (ModuleItem::SubModule(id), DefinitionType::Local, ModuleSpan(parent, span)));

            Ok(id)
        }
    }

    /// Registers the ADT `ty` in the root module.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_adt(&mut self, ty: Adt, span: Span) -> Result<AdtRef, Diagnostic> {
        self.register_adt_in_module(ModuleId::ZERO, ty, span)
    }

    /// Registers the ADT `ty` in `module`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_adt_in_module(&mut self, module: ModuleId, ty: Adt, span: Span) -> Result<AdtRef, Diagnostic> {
        if let Some(name) = ty.name.clone() {
            if let Some(&(_, def_type, one)) = self.modules[module].items.get(&name) {
                Err(Diagnostic::new(TypeError::AlreadyExists {
                    name,
                    primary: def_type,
                    secondary: DefinitionType::Local,
                })
                .with_primary_span(one.0, one.1)
                .with_secondary_span(module, span))
            } else {
                let type_ref = self.adt_types.insert(ty);

                self.modules[module]
                    .items
                    .insert(name, (ModuleItem::Adt(type_ref), DefinitionType::Local, ModuleSpan(module, span)));

                Ok(type_ref)
            }
        } else {
            // Anonymous ADTs can't be referenced by name, so they don't get a
            // module item.
            Ok(self.adt_types.insert(ty))
        }
    }

    /// Registers the trait in the root module.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_trait(&mut self, r#trait: Trait, span: Span) -> Result<TraitRef, Diagnostic> {
        self.register_trait_in_module(ModuleId::ZERO, r#trait, span)
    }

    /// Registers the trait in `module`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_trait_in_module(&mut self, module: ModuleId, r#trait: Trait, span: Span) -> Result<TraitRef, Diagnostic> {
        if let Some(&(_, def_type, one)) = self.modules[module].items.get(&r#trait.name) {
            Err(Diagnostic::new(TypeError::AlreadyExists {
                name: r#trait.name,
                primary: def_type,
                secondary: DefinitionType::Local,
            })
            .with_primary_span(one.0, one.1)
            .with_secondary_span(module, span))
        } else {
            let trait_ref = self.traits.insert(r#trait);

            self.modules[module].items.insert(
                self.traits[trait_ref].name.clone(),
                (ModuleItem::Trait(trait_ref), DefinitionType::Local, ModuleSpan(module, span)),
            );

            Ok(trait_ref)
        }
    }

    /// Registers the constant in `module`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_const_in_module(&mut self, module: ModuleId, constant: Const, span: Span) -> Result<ConstRef, Diagnostic> {
        if let Some(&(_, def_type, one)) = self.modules[module].items.get(&constant.name) {
            Err(Diagnostic::new(TypeError::AlreadyExists {
                name: constant.name,
                primary: def_type,
                secondary: DefinitionType::Local,
            })
            .with_primary_span(one.0, one.1)
            .with_secondary_span(module, span))
        } else {
            let constant = self.constants.insert(constant);

            self.modules[module].items.insert(
                self.constants[constant].name.clone(),
                (ModuleItem::Const(constant), DefinitionType::Local, ModuleSpan(module, span)),
            );

            Ok(constant)
        }
    }

    /// Registers the function `func` in `module`.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic if an item with the same name already exists in the
    /// module.
    pub fn register_func_in_module(&mut self, module: ModuleId, func: Func, span: Span) -> Result<FuncRef, Diagnostic> {
        if let Some(&(_, def_type, one)) = self.modules[module].items.get(&func.name) {
            Err(Diagnostic::new(TypeError::AlreadyExists {
                name: func.name,
                primary: def_type,
                secondary: DefinitionType::Local,
            })
            .with_primary_span(one.0, one.1)
            .with_secondary_span(module, span))
        } else {
            let func = self.functions.insert(func);

            self.modules[module].items.insert(
                self.functions[func].name.clone(),
                (ModuleItem::Func(func), DefinitionType::Local, ModuleSpan(module, span)),
            );

            Ok(func)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimplifiedType {
    Primitive(PrimitiveType),
    Array, // (TypeRef, Option<usize>)
    Adt(AdtRef),
    Trait(TraitRef),
    Func, // (Box<[TypeRef]>, TypeRef)
}

impl SimplifiedType {
    pub const fn from_type(ty: &Type) -> Option<Self> {
        match ty {
            &Type::Primitive(primitive_type) => Some(Self::Primitive(primitive_type)),
            Type::Array(..) => Some(Self::Array),
            &Type::Adt(adt_ref, _) => Some(Self::Adt(adt_ref)),
            &Type::Trait(trait_ref, _) => Some(Self::Trait(trait_ref)),
            Type::Func(..) => Some(Self::Func),
            _ => None,
        }
    }

    pub const fn from_type_info(ty: &TypeInfo) -> Option<Self> {
        match ty {
            &TypeInfo::Primitive(primitive_type) => Some(Self::Primitive(primitive_type)),
            TypeInfo::Array(..) => Some(Self::Array),
            TypeInfo::Adt(adt_info) => Some(Self::Adt(adt_info.id)),
            TypeInfo::Trait(trait_info) => Some(Self::Trait(trait_info.id)),
            TypeInfo::Func(..) => Some(Self::Func),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError<T> {
    NotFound,
    Ambiguous(Vec<T>),
}

impl<T> LookupError<T> {
    fn unique(mut matches: Vec<T>) -> Result<T, Self> {
        match matches.len() {
            0 => Err(Self::NotFound),
            1 => Ok(matches.remove(0)),
            _ => Err(Self::Ambiguous(matches)),
        }
    }
}

#[derive(Debug, Default)]
pub struct ImplRegistry {
    pub impls: IndexVec<ImplRef, VTableGenerator>,
    /// Bounds of the own generic parameters of functions of impl blocks.
    pub method_bounds: HashMap<(ImplRef, VFuncRef), Box<[Bound]>>,
    /// Default values of parameters (without `self`) of functions of impl
    /// blocks, like [`DefRegistry::func_defaults`]. Their functions take
    /// `self` too, and have generics of the impl followed by the function's.
    pub method_defaults: HashMap<(ImplRef, VFuncRef), Box<[Option<FuncRef>]>>,
    /// Where names of functions written in impl blocks are, for tools.
    pub func_spans: HashMap<(ImplRef, VFuncRef), ModuleSpan>,
    /// Functions of impls of value types taking `mut self`: they give the
    /// changed receiver back to the caller.
    pub mut_self: HashSet<(ImplRef, VFuncRef)>,
    /// Ordered, so candidates (and errors about them) come in the same order
    /// on every run.
    pub concrete_impls: IndexMap<(Option<TraitRef>, SimplifiedType), Vec<ImplRef>>,
    pub blanket_impls: Vec<ImplRef>,
}

impl ImplRegistry {
    /// Registers an impl block, putting it into the concrete bucket of its
    /// target type, or into `blanket_impls` if the target is a bare generic
    /// (`impl<T> Trait for T`).
    pub fn register_impl(&mut self, storage: &TypeStorage, generator: VTableGenerator) -> ImplRef {
        let simple_ty = SimplifiedType::from_type(&storage.types[generator.ty]);
        let trait_ref = generator.origin_trait;
        let impl_ref = self.impls.insert(generator);

        match simple_ty {
            Some(simple_ty) => self.concrete_impls.entry((trait_ref, simple_ty)).or_default().push(impl_ref),
            None => self.blanket_impls.push(impl_ref),
        }

        impl_ref
    }

    /// Impls of `trait_ref` (`None` for inherent impls) that may apply to
    /// `ty`, before matching the full type.
    fn candidates(&self, storage: &TypeStorage, trait_ref: Option<TraitRef>, ty: TypeRef) -> Vec<ImplRef> {
        let simple_ty = SimplifiedType::from_type(&storage.types[ty]);

        simple_ty
            .and_then(|simple_ty| self.concrete_impls.get(&(trait_ref, simple_ty)))
            .into_iter()
            .flatten()
            .copied()
            .chain(
                self.blanket_impls
                    .iter()
                    .copied()
                    .filter(|&impl_ref| self.impls[impl_ref].origin_trait == trait_ref),
            )
            .collect()
    }

    /// Impls of any trait that may apply to `ty`, before matching the full
    /// type.
    fn trait_candidates(&self, storage: &TypeStorage, ty: TypeRef) -> Vec<ImplRef> {
        let simple_ty = SimplifiedType::from_type(&storage.types[ty]);

        self.concrete_impls
            .iter()
            .filter(|((trait_ref, impl_ty), _)| trait_ref.is_some() && Some(*impl_ty) == simple_ty)
            .flat_map(|(_, impls)| impls.iter().copied())
            .chain(
                self.blanket_impls
                    .iter()
                    .copied()
                    .filter(|&impl_ref| self.impls[impl_ref].origin_trait.is_some()),
            )
            .collect()
    }

    /// Type arguments of an impl applied to `ty`, found by matching its
    /// target type (and `Self` for trait impls). `None` for those that can't
    /// be found from the type.
    pub fn impl_args(&self, storage: &TypeStorage, impl_ref: ImplRef, ty: TypeRef) -> Option<Box<[Option<TypeRef>]>> {
        let generator = &self.impls[impl_ref];
        let mut args = vec![None; generator.generics.len()];

        if generator.origin_trait.is_some()
            && let Some(this) = args.first_mut()
        {
            *this = Some(ty);
        }

        storage.match_impl(generator.ty, ty, &mut args).then(|| args.into_boxed_slice())
    }

    /// Whether `ty` implements `trait_ref` (with type arguments `trait_args`,
    /// if they're given). Generic parameters (and errors) are assumed to:
    /// their bounds are checked where the bounds of the code around them are
    /// known. Trait objects implement their trait.
    pub fn satisfies(&self, storage: &TypeStorage, ty: TypeRef, trait_ref: TraitRef, trait_args: Option<&[TypeRef]>) -> bool {
        match &storage.types[ty] {
            Type::Generic(_) | Type::Error => true,
            Type::Trait(object_trait, object_args) if *object_trait == trait_ref => trait_args.is_none_or(|trait_args| {
                object_args.len() == trait_args.len() && object_args.iter().zip(trait_args).all(|(&found, &expected)| storage.is_same(found, expected))
            }),
            _ => self.impl_of(storage, trait_ref, trait_args, ty).is_ok(),
        }
    }

    /// Type arguments of the impl `impl_ref` for `ty`, if it applies to `ty`:
    /// its target matches, and its bounds are satisfied.
    fn applicable_args(&self, storage: &TypeStorage, impl_ref: ImplRef, ty: TypeRef) -> Option<Box<[Option<TypeRef>]>> {
        let args = self.impl_args(storage, impl_ref, ty)?;
        let satisfied = self.impls.get(impl_ref).is_none_or(|generator| {
            generator.bounds.iter().all(|bound| {
                // Arguments of a bound in terms of the impl's generics are only
                // compared once they're known.
                let trait_args = (!bound.trait_args.iter().any(|&arg| storage.has_generics(arg))).then_some(&*bound.trait_args);

                args.get(bound.generic)
                    .copied()
                    .flatten()
                    .is_none_or(|arg| self.satisfies(storage, arg, bound.trait_ref, trait_args))
            })
        });

        satisfied.then_some(args)
    }

    /// Whether the impl applies to `ty`: its target matches, and its bounds
    /// are satisfied.
    fn applies(&self, storage: &TypeStorage, impl_ref: ImplRef, ty: TypeRef) -> bool {
        self.applicable_args(storage, impl_ref, ty).is_some()
    }

    /// The impl of `trait_ref` for `ty`, with type arguments of the trait
    /// `trait_args` if they're given (a type can implement `Source<i32>` and
    /// `Source<string>`).
    ///
    /// # Errors
    ///
    /// Returns [`LookupError::NotFound`] if `ty` doesn't implement the trait,
    /// or [`LookupError::Ambiguous`] if several impls match.
    pub fn impl_of(&self, storage: &TypeStorage, trait_ref: TraitRef, trait_args: Option<&[TypeRef]>, ty: TypeRef) -> Result<ImplRef, LookupError<ImplRef>> {
        let matches = self
            .candidates(storage, Some(trait_ref), ty)
            .into_iter()
            .filter(|&impl_ref| {
                let Some(mut args) = self.applicable_args(storage, impl_ref, ty) else {
                    return false;
                };

                // Arguments of the impl's trait are patterns over its generics,
                // matched with the expected ones.
                let impl_trait_args = &self.impls[impl_ref].trait_args;

                trait_args.is_none_or(|trait_args| {
                    impl_trait_args.len() == trait_args.len()
                        && impl_trait_args
                            .iter()
                            .zip(trait_args)
                            .all(|(&pattern, &expected)| storage.match_impl(pattern, expected, &mut args))
                })
            })
            .collect();

        LookupError::unique(matches)
    }

    fn find_methods(&self, storage: &TypeStorage, impls: Vec<ImplRef>, ty: TypeRef, name: &str) -> Vec<(ImplRef, VFuncRef)> {
        impls
            .into_iter()
            .filter(|&impl_ref| self.applies(storage, impl_ref, ty))
            .filter_map(|impl_ref| {
                self.impls[impl_ref]
                    .functions
                    .iter()
                    .find(|(_, func)| func.name == name)
                    .map(|(func_ref, _)| (impl_ref, func_ref))
            })
            .collect()
    }

    /// The impl of `trait_ref` for `ty`, whatever its type arguments.
    ///
    /// # Errors
    ///
    /// Returns [`LookupError::NotFound`] if `ty` doesn't implement the trait,
    /// or [`LookupError::Ambiguous`] if several impls match.
    pub fn trait_impl_lookup(&self, storage: &TypeStorage, trait_ref: TraitRef, ty: TypeRef) -> Result<ImplRef, LookupError<ImplRef>> {
        self.impl_of(storage, trait_ref, None, ty)
    }

    /// Looks up a method called `name` on `ty`. With `trait_ref == None`,
    /// inherent methods are searched first, then methods of every trait
    /// implemented for `ty`.
    ///
    /// # Errors
    ///
    /// Returns [`LookupError::NotFound`] if `ty` has no such method, or
    /// [`LookupError::Ambiguous`] if several traits have one.
    pub fn method_lookup(
        &self,
        storage: &TypeStorage,
        trait_ref: Option<TraitRef>,
        ty: TypeRef,
        name: &str,
    ) -> Result<(ImplRef, VFuncRef), LookupError<(ImplRef, VFuncRef)>> {
        let matches = self.find_methods(storage, self.candidates(storage, trait_ref, ty), ty, name);

        if trait_ref.is_none() && matches.is_empty() {
            LookupError::unique(self.find_methods(storage, self.trait_candidates(storage, ty), ty, name))
        } else {
            LookupError::unique(matches)
        }
    }
}

#[derive(Debug)]
pub struct TyCtxt {
    pub types: TypeStorage,
    pub impl_registry: ImplRegistry,
    pub def_registry: DefRegistry,
}

impl Default for TyCtxt {
    fn default() -> Self {
        Self::new()
    }
}

impl TyCtxt {
    /// Creates a context with an empty host module ([`ModuleId::ZERO`]).
    pub fn new() -> Self {
        let mut def_registry = DefRegistry::default();

        def_registry.modules.insert(Module::new(ModuleId::ZERO, "<host>"));

        Self {
            types: TypeStorage::default(),
            impl_registry: ImplRegistry::default(),
            def_registry,
        }
    }

    pub fn solver<'a>(&'a mut self, diagnostics: &'a mut DiagnosticContext) -> TypeSolver<'a> {
        TypeSolver::from_context(self, diagnostics)
    }

    pub const fn display_of(&self, ty: TypeRef) -> TypeDisplay<'_> {
        TypeDisplay::new(ty, &self.def_registry.adt_types, &self.def_registry.traits, &self.types)
    }

    pub const fn display_of_module(&self, module: ModuleId) -> ModuleDisplay<'_> {
        ModuleDisplay(module, &self.def_registry.modules)
    }

    pub const fn display_of_diagnostic<'a>(&'a self, diagnostic: &'a Diagnostic) -> DiagnosticDisplay<'a> {
        DiagnosticDisplay::new(diagnostic, self)
    }

    /// Like [`TypeStorage::is_same`], but also treats a type as the same as a
    /// trait it implements.
    pub fn is_same(&self, ty: TypeRef, other: TypeRef) -> bool {
        match (&self.types[ty], &self.types[other]) {
            // Errors are already reported, don't report everything they touch again.
            (Type::Error | Type::Primitive(PrimitiveType::Any), _) | (_, Type::Error | Type::Primitive(PrimitiveType::Any)) => true,
            (Type::Generic(index), Type::Generic(other_index)) => index == other_index,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, size), &Type::Array(other_element, other_size)) => self.is_same(element, other_element) && size == other_size,
            (Type::Func(args, returns), Type::Func(other_args, other_returns)) => {
                args.len() == other_args.len()
                    && args.iter().zip(other_args).all(|(&arg, &other_arg)| self.is_same(arg, other_arg))
                    && self.is_same(*returns, *other_returns)
            }
            (Type::Adt(adt, type_args), Type::Adt(other_adt, other_type_args)) => {
                adt == other_adt
                    && type_args.len() == other_type_args.len()
                    && type_args.iter().zip(other_type_args).all(|(&arg, &other_arg)| self.is_same(arg, other_arg))
            }
            (Type::Trait(trait_ref, type_args), Type::Trait(other_trait_ref, other_type_args)) => {
                trait_ref == other_trait_ref
                    && type_args.len() == other_type_args.len()
                    && type_args.iter().zip(other_type_args).all(|(&arg, &other_arg)| self.is_same(arg, other_arg))
            }
            (Type::Trait(trait_ref, trait_args), _) => self.find_trait_impl(other, *trait_ref, trait_args).is_some(),
            (_, Type::Trait(other_trait_ref, other_trait_args)) => self.find_trait_impl(ty, *other_trait_ref, other_trait_args).is_some(),
            _ => false,
        }
    }

    pub fn inst_adt(&mut self, ty: AdtRef, type_args: &[TypeRef]) -> TypeRef {
        self.types.get_or_add(Type::Adt(ty, type_args.iter().copied().collect()))
    }

    pub fn get_adt_item(&self, item: LangItem) -> Option<AdtRef> {
        self.def_registry
            .language_items
            .get(&item)
            .and_then(|v| if let &LangItemValue::Adt(adt_ref) = v { Some(adt_ref) } else { None })
    }

    pub fn get_adt_variant_item(&self, item: LangItem) -> Option<(AdtRef, AdtVariantRef)> {
        self.def_registry.language_items.get(&item).and_then(|v| {
            if let &LangItemValue::AdtVariant(adt_ref, adt_variant) = v {
                Some((adt_ref, adt_variant))
            } else {
                None
            }
        })
    }

    pub fn get_trait_item(&self, item: LangItem) -> Option<TraitRef> {
        self.def_registry
            .language_items
            .get(&item)
            .and_then(|v| if let &LangItemValue::Trait(trait_ref) = v { Some(trait_ref) } else { None })
    }

    pub fn get_trait_func_item(&self, trait_ref: TraitRef, item: LangItem) -> Option<TraitFuncRef> {
        self.def_registry.language_items.get(&item).and_then(|v| {
            if let &LangItemValue::TraitFunc(func_trait_ref, trait_func) = v
                && func_trait_ref == trait_ref
            {
                Some(trait_func)
            } else {
                None
            }
        })
    }

    /// Finds the impl of `origin_trait` for `ty` (an inherent impl if `None`).
    /// Ambiguous lookups are treated as not found.
    pub fn find_vtable(&self, ty: TypeRef, origin_trait: Option<TraitRef>) -> Option<ImplRef> {
        origin_trait.map_or_else(
            || {
                self.impl_registry
                    .impls
                    .iter()
                    .find(|(_, impl_block)| impl_block.origin_trait.is_none() && self.types.impl_matches(impl_block.ty, ty))
                    .map(|(impl_ref, _)| impl_ref)
            },
            |trait_ref| self.trait_impl_lookup(trait_ref, ty).ok(),
        )
    }

    /// Whether values of `ty` are values of a value type.
    pub fn is_value_type(&self, ty: TypeRef) -> bool {
        matches!(self.types[ty], Type::Adt(adt, _) if self.def_registry.value_types.contains(&adt))
    }

    /// The impl of `trait_ref` with type arguments `trait_args` for `ty`: for
    /// trait objects of `trait_ref<trait_args...>`.
    pub fn find_trait_impl(&self, ty: TypeRef, trait_ref: TraitRef, trait_args: &[TypeRef]) -> Option<ImplRef> {
        // Arguments that aren't known (yet) don't select an impl.
        let trait_args = (!trait_args.iter().any(|&arg| self.types[arg] == Type::Error || self.types.has_generics(arg))).then_some(trait_args);

        self.impl_registry.impl_of(&self.types, trait_ref, trait_args, ty).ok()
    }

    /// Finds a method called `name` on `ty`, looking at inherent impls first,
    /// then at trait impls. Ambiguous lookups are treated as not found.
    pub fn find_vtable_by_func<T: AsRef<str>>(&self, ty: TypeRef, name: T) -> Option<(ImplRef, VFuncRef)> {
        self.method_lookup(None, ty, name.as_ref()).ok()
    }

    /// Finds an impl written for exactly `ty`, to merge impl blocks of the
    /// same type and trait (with the same arguments: `Source<i32>` and
    /// `Source<string>` are different impls).
    pub fn get_vtable(&self, ty: TypeRef, origin_trait: Option<TraitRef>, trait_args: &[TypeRef], bounds: &[Bound]) -> Option<ImplRef> {
        // An impl for a type that failed to resolve must not be merged into
        // another one.
        if self.types[ty] == Type::Error {
            return None;
        }

        self.impl_registry
            .impls
            .iter()
            .find(|(_, impl_block)| {
                impl_block.origin_trait == origin_trait
                    && self.types.is_same(impl_block.ty, ty)
                    && impl_block.trait_args.len() == trait_args.len()
                    && impl_block
                        .trait_args
                        .iter()
                        .zip(trait_args)
                        .all(|(&found, &expected)| self.types.is_same(found, expected))
                    && impl_block.bounds.len() == bounds.len()
                    && impl_block.bounds.iter().zip(bounds).all(|(found, expected)| {
                        found.generic == expected.generic
                            && found.trait_ref == expected.trait_ref
                            && found
                                .trait_args
                                .iter()
                                .zip(&expected.trait_args)
                                .all(|(&found, &expected)| self.types.is_same(found, expected))
                    })
            })
            .map(|(impl_ref, _)| impl_ref)
    }

    pub fn name_of_trait(&self, trait_ref: TraitRef) -> &str {
        self.def_registry.traits[trait_ref].name.as_str()
    }

    pub fn register_impl(&mut self, generator: VTableGenerator) -> ImplRef {
        self.impl_registry.register_impl(&self.types, generator)
    }

    /// Like [`ImplRegistry::trait_impl_lookup`], with the types of this
    /// context.
    ///
    /// # Errors
    ///
    /// Returns [`LookupError::NotFound`] if `ty` doesn't implement the trait,
    /// or [`LookupError::Ambiguous`] if several impls match.
    pub fn trait_impl_lookup(&self, trait_ref: TraitRef, ty: TypeRef) -> Result<ImplRef, LookupError<ImplRef>> {
        self.impl_registry.trait_impl_lookup(&self.types, trait_ref, ty)
    }

    /// Like [`ImplRegistry::method_lookup`], with the types of this context.
    ///
    /// # Errors
    ///
    /// Returns [`LookupError::NotFound`] if `ty` has no such method, or
    /// [`LookupError::Ambiguous`] if several traits have one.
    pub fn method_lookup(&self, trait_ref: Option<TraitRef>, ty: TypeRef, name: &str) -> Result<(ImplRef, VFuncRef), LookupError<(ImplRef, VFuncRef)>> {
        self.impl_registry.method_lookup(&self.types, trait_ref, ty, name)
    }
}

pub struct TypeDisplay<'a> {
    ty: TypeRef,
    adt_types: &'a IndexVec<AdtRef, Adt>,
    traits: &'a IndexVec<TraitRef, Trait>,
    storage: &'a TypeStorage,
}

impl<'a> TypeDisplay<'a> {
    pub const fn new(ty: TypeRef, adt_types: &'a IndexVec<AdtRef, Adt>, traits: &'a IndexVec<TraitRef, Trait>, storage: &'a TypeStorage) -> Self {
        Self {
            ty,
            adt_types,
            traits,
            storage,
        }
    }

    const fn with(&self, ty: TypeRef) -> Self {
        Self {
            ty,
            adt_types: self.adt_types,
            traits: self.traits,
            storage: self.storage,
        }
    }
}

impl fmt::Display for TypeDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.storage[self.ty] {
            Type::Primitive(primitive_type) => primitive_type.fmt(f),
            &Type::Array(element, size) => match size {
                Some(size) => write!(f, "{}[{size}]", self.with(element)),
                None => write!(f, "{}[]", self.with(element)),
            },
            Type::Adt(adt_ref, type_args) => {
                if let Some(name) = &self.adt_types[*adt_ref].name {
                    name.fmt(f)?;
                }

                if type_args.is_empty() {
                    Ok(())
                } else {
                    write!(f, "<{}>", type_args.iter().map(|&ty| self.with(ty)).join(", "))
                }
            }
            Type::Trait(trait_ref, type_args) => {
                self.traits[*trait_ref].name.fmt(f)?;

                if type_args.is_empty() {
                    Ok(())
                } else {
                    write!(f, "<{}>", type_args.iter().map(|&ty| self.with(ty)).join(", "))
                }
            }
            Type::Func(arg_types, returns) => {
                write!(f, "fn({}) -> {}", arg_types.iter().map(|&ty| self.with(ty)).join(", "), self.with(*returns))
            }
            Type::Generic(i) => write!(f, "<generic({i})>"),
            Type::Error => f.write_str("<error>"),
        }
    }
}

pub struct ModuleDisplay<'a>(ModuleId, &'a IndexVec<ModuleId, Module>);

impl fmt::Display for ModuleDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let module = &self.1[self.0];

        match module.parent {
            // Roots: the host module, programs and libraries.
            None => f.write_str(&module.name),
            // Modules of the host are named like they're imported.
            Some(parent) if parent == ModuleId::ZERO => f.write_str(&module.name),
            Some(parent) => write!(f, "{}::{}", Self(parent, self.1), module.name),
        }
    }
}
