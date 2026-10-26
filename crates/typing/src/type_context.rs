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
    Adt, AdtKind, AdtRef, AdtVariantRef, Arg, ConstRef, CoreTypes, DiagnosticContext, FieldRef, FuncRef, ImplRef, IntType, ModuleId, PrimitiveType,
    TraitFuncRef, TraitRef, TypeInfo, TypeSolver, UIntType, VFuncRef,
    diagnostic::{Diagnostic, DiagnosticDisplay},
    error::{DefinitionType, TypeError},
    ty::{ArrayKind, Type, TypeRef},
};

#[derive(Debug)]
pub struct TypeStorage {
    types: IndexVec<TypeRef, Type>,
    index: HashMap<Type, TypeRef>,
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
            f64: types.insert(Type::Primitive(PrimitiveType::F64)),
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
    pub fn kinds_same(&self, kind: ArrayKind, other: ArrayKind, same: impl Fn(&Self, TypeRef, TypeRef) -> bool) -> bool {
        match (kind, other) {
            (ArrayKind::Fixed(size), ArrayKind::Fixed(other_size)) => same(self, size, other_size),
            (kind, other) => kind == other,
        }
    }

    pub fn kinds_match(&self, kind: ArrayKind, other: ArrayKind, same: impl Fn(&Self, TypeRef, TypeRef) -> bool) -> bool {
        match (kind, other) {
            (ArrayKind::View { writable: false }, _)
            | (ArrayKind::View { writable: true }, ArrayKind::Growable | ArrayKind::Fixed(_) | ArrayKind::View { writable: true }) => true,
            (kind, other) => self.kinds_same(kind, other, same),
        }
    }

    pub fn is_likely_same(&self, ty: TypeRef, other: TypeRef) -> bool {
        match (&self.types[ty], &self.types[other]) {
            (Type::Generic(..), _) | (_, Type::Generic(..)) => true,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, kind), &Type::Array(other_element, other_kind)) => {
                self.is_likely_same(element, other_element) && self.kinds_match(kind, other_kind, Self::is_likely_same)
            }
            (Type::Size(size), Type::Size(other_size)) => size == other_size,
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
                Type::Size(_) | Type::Primitive(_) | Type::Generic(_) | Type::Error => (),
            }

            Some(budget)
        }

        remaining(self, ty, budget).is_some()
    }

    pub fn has_generics(&self, ty: TypeRef) -> bool {
        match &self.types[ty] {
            Type::Generic(_) => true,
            &Type::Array(element, kind) => self.has_generics(element) || matches!(kind, ArrayKind::Fixed(size) if self.has_generics(size)),
            Type::Adt(_, args) | Type::Trait(_, args) => args.iter().any(|&arg| self.has_generics(arg)),
            Type::Func(args, returns) => args.iter().any(|&arg| self.has_generics(arg)) || self.has_generics(*returns),
            Type::Primitive(_) | Type::Size(_) | Type::Error => false,
        }
    }

    pub fn is_same(&self, ty: TypeRef, other: TypeRef) -> bool {
        match (&self.types[ty], &self.types[other]) {
            // Errors are already reported, don't report everything they touch again.
            (Type::Error | Type::Primitive(PrimitiveType::Any), _) | (_, Type::Error | Type::Primitive(PrimitiveType::Any)) => true,
            (Type::Generic(index), Type::Generic(other_index)) => index == other_index,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            (&Type::Array(element, kind), &Type::Array(other_element, other_kind)) => {
                self.is_same(element, other_element) && self.kinds_same(kind, other_kind, Self::is_same)
            }
            (Type::Size(size), Type::Size(other_size)) => size == other_size,
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
            (&Type::Array(element, kind), &Type::Array(other_element, other_kind)) => {
                self.match_impl(element, other_element, args)
                    && match (kind, other_kind) {
                        // Impls of views are for every array, since any array
                        // can be viewed.
                        (ArrayKind::View { .. }, _) | (ArrayKind::Growable, ArrayKind::Growable) => true,
                        (ArrayKind::Fixed(size), ArrayKind::Fixed(other_size)) => self.match_impl(size, other_size, args),
                        _ => false,
                    }
            }
            (Type::Size(size), Type::Size(other_size)) => size == other_size,
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
            (&Type::Array(element, kind), &Type::Array(other_element, other_kind)) => {
                self.impl_matches(element, other_element)
                    && match (kind, other_kind) {
                        // Impls of views are for every array, since any array
                        // can be viewed.
                        (ArrayKind::View { .. }, _) | (ArrayKind::Growable, ArrayKind::Growable) => true,
                        (ArrayKind::Fixed(size), ArrayKind::Fixed(other_size)) => self.impl_matches(size, other_size),
                        _ => false,
                    }
            }
            (Type::Size(size), Type::Size(other_size)) => size == other_size,
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
            Type::Error | Type::Primitive(_) | Type::Size(_) => ty,
            Type::Array(element, kind) => {
                let element = self.apply_type_args(element, type_args);
                let kind = match kind {
                    ArrayKind::Fixed(size) => ArrayKind::Fixed(self.apply_type_args(size, type_args)),
                    kind => kind,
                };

                self.get_or_add(Type::Array(element, kind))
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

    pub fn hasher(&self) -> DefaultHasher {
        self.hash_keys.build_hasher()
    }

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
            &Type::Array(element, kind) => {
                "array".hash(state);

                // Growable arrays and views are the same at run time (a
                // pointer to an array), unlike fixed arrays of each size.
                match kind {
                    ArrayKind::Growable => "growable".hash(state),
                    // Writable and read-only views are the same at run time
                    // too.
                    ArrayKind::View { .. } => "view".hash(state),
                    ArrayKind::Fixed(size) => {
                        "fixed".hash(state);
                        self.hash_instance_into(state, size, generics);
                    }
                }

                self.hash_instance_into(state, element, generics);
            }
            Type::Size(size) => {
                "size".hash(state);
                size.hash(state);
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

#[derive(Debug, Clone)]
pub struct Const {
    pub name: String,
    pub ty: TypeRef,
    pub value: Option<ConstantValue>,
}

#[derive(Debug, Clone)]
pub struct Func {
    pub postfix: bool,
    pub name: String,
    pub generics: usize,
    pub arg_names: Vec<String>,
    pub ty: TypeRef,
}

#[derive(Debug)]
pub struct TraitFunc {
    pub name: String,
    pub args: Box<[Arg<TypeRef>]>,
    pub returns: TypeRef,
    pub default: Option<FuncRef>,
}

#[derive(Debug)]
pub struct Trait {
    pub name: String,
    pub generics: usize,
    pub functions: IndexVec<TraitFuncRef, TraitFunc>,
}

impl Trait {
    pub fn get_func_offset<T: AsRef<str>>(&self, name: T) -> Option<usize> {
        let name = name.as_ref();

        self.functions
            .values()
            .position(|func| func.name == name)
            .map(|index| (index + 1) * size_of::<usize>())
    }
}

#[derive(Debug, Clone)]
pub struct VTableFunc<T = TypeRef> {
    pub trait_func: Option<TraitFuncRef>,
    pub name: String,
    pub arg_names: Vec<String>,
    pub generics: usize,
    pub ty: T,
    pub accessor: Option<Accessor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Accessor {
    Getter,
    Setter,
}

impl Accessor {
    pub fn setter_name(name: &str) -> String {
        format!("set {name}")
    }

    pub fn property_name(self, name: &str) -> &str {
        match self {
            Self::Getter => name,
            Self::Setter => name.strip_prefix("set ").unwrap_or(name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub generic: usize,
    pub trait_ref: TraitRef,
    pub trait_args: Box<[TypeRef]>,
}

#[derive(Debug)]
pub struct VTableGenerator {
    pub ty: TypeRef,
    pub origin_trait: Option<TraitRef>,
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
    pub func_bounds: HashMap<FuncRef, Box<[Bound]>>,
    pub func_defaults: HashMap<FuncRef, Box<[Option<FuncRef>]>>,
    pub variadic_functions: HashSet<FuncRef>,
    pub value_types: HashSet<AdtRef>,
    pub restricted_modules: HashMap<ModuleId, (String, (ModuleItem, DefinitionType, ModuleSpan))>,
    pub extern_roots: IndexMap<String, ModuleId>,
    pub prelude: Option<ModuleId>,
    pub restricted_roots: IndexMap<String, ModuleId>,
    pub packages: HashMap<ModuleId, String>,
    pub public_items: HashSet<(ModuleId, String)>,
    pub public_fields: HashSet<(AdtRef, FieldRef)>,
    pub adt_modules: HashMap<AdtRef, ModuleId>,
    pub trait_modules: HashMap<TraitRef, ModuleId>,
}

impl DefRegistry {
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

    pub fn lookup(&self, module: ModuleId, name: &str) -> Option<ModuleItem> {
        self.modules[module]
            .get_item(name)
            .or_else(|| self.extern_roots.get(name).map(|&root| ModuleItem::SubModule(root)))
            .or_else(|| self.modules[ModuleId::ZERO].get_item(name))
            .or_else(|| self.prelude.and_then(|prelude| self.visible_item(module, prelude, name)))
    }

    pub fn visible_item(&self, from: ModuleId, module: ModuleId, name: &str) -> Option<ModuleItem> {
        self.modules[module]
            .get_item(name)
            .filter(|_| self.can_use(from, module, self.public_items.contains(&(module, name.to_owned()))))
    }

    pub fn root_of(&self, mut module: ModuleId) -> ModuleId {
        while let Some(parent) = self.modules[module].parent {
            module = parent;
        }

        module
    }

    pub fn same_package(&self, a: ModuleId, b: ModuleId) -> bool {
        let (a, b) = (self.root_of(a), self.root_of(b));

        a == b || matches!((self.packages.get(&a), self.packages.get(&b)), (Some(a), Some(b)) if a == b)
    }

    pub fn can_use(&self, from: ModuleId, owner: ModuleId, public: bool) -> bool {
        public || self.root_of(owner) == ModuleId::ZERO || self.same_package(from, owner)
    }

    pub fn can_use_field(&self, from: ModuleId, adt: AdtRef, field: FieldRef) -> bool {
        let Some(&owner) = self.adt_modules.get(&adt) else {
            return true;
        };

        self.adt_types[adt].kind == AdtKind::Enum || self.can_use(from, owner, self.public_fields.contains(&(adt, field)))
    }

    pub fn restrict_root(&mut self, name: &str) -> bool {
        match self.extern_roots.shift_remove(name) {
            Some(root) => {
                self.restricted_roots.insert(name.to_owned(), root);

                true
            }
            None => false,
        }
    }

    pub fn grant_root(&mut self, name: &str) -> bool {
        match self.restricted_roots.shift_remove(name) {
            Some(root) => {
                self.extern_roots.insert(name.to_owned(), root);

                true
            }
            None => false,
        }
    }

    pub fn register_program_root(&mut self) -> ModuleId {
        let id = self.modules.next_index();

        self.modules.insert(Module {
            parent: None,
            name: String::from("<root>"),
            id,
            items: IndexMap::new(),
        })
    }

    pub fn is_root(&self, module: ModuleId) -> bool {
        self.modules[module].parent.is_none() && !self.extern_roots.values().any(|&root| root == module)
    }

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

    pub fn is_restricted(&self, module: ModuleId, name: &str) -> bool {
        self.restricted_roots.contains_key(name) || self.restricted_modules.iter().any(|(&restricted, (restricted_name, _))| {
            restricted_name == name
                && self.modules[restricted]
                    .parent
                    .is_some_and(|parent| parent == module || parent == ModuleId::ZERO)
        })
    }

    pub fn grant(&mut self, module: ModuleId) -> bool {
        let (Some((name, entry)), Some(parent)) = (self.restricted_modules.remove(&module), self.modules[module].parent) else {
            return false;
        };

        self.modules[parent].items.insert(name, entry);

        true
    }

    pub fn register_module<T: Into<String>>(&mut self, name: T, span: Span) -> Result<ModuleId, Diagnostic> {
        self.register_module_in_module(ModuleId::ZERO, name, span)
    }

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

    pub fn register_adt(&mut self, ty: Adt, span: Span) -> Result<AdtRef, Diagnostic> {
        self.register_adt_in_module(ModuleId::ZERO, ty, span)
    }

    pub fn register_adt_in_module(&mut self, module: ModuleId, ty: Adt, span: Span) -> Result<AdtRef, Diagnostic> {
        let name = ty.name.clone();

        if let Some(&(_, def_type, one)) = self.modules[module].items.get(&name) {
            return Err(Diagnostic::new(TypeError::AlreadyExists {
                name,
                primary: def_type,
                secondary: DefinitionType::Local,
            })
            .with_primary_span(one.0, one.1)
            .with_secondary_span(module, span));
        }

        let type_ref = self.adt_types.insert(ty);

        self.adt_modules.insert(type_ref, module);
        self.modules[module]
            .items
            .insert(name, (ModuleItem::Adt(type_ref), DefinitionType::Local, ModuleSpan(module, span)));

        Ok(type_ref)
    }

    pub fn register_trait(&mut self, r#trait: Trait, span: Span) -> Result<TraitRef, Diagnostic> {
        self.register_trait_in_module(ModuleId::ZERO, r#trait, span)
    }

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

            self.trait_modules.insert(trait_ref, module);
            self.modules[module].items.insert(
                self.traits[trait_ref].name.clone(),
                (ModuleItem::Trait(trait_ref), DefinitionType::Local, ModuleSpan(module, span)),
            );

            Ok(trait_ref)
        }
    }

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
    Array,
    Adt(AdtRef),
    Trait(TraitRef),
    Func,
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
    pub method_bounds: HashMap<(ImplRef, VFuncRef), Box<[Bound]>>,
    pub method_defaults: HashMap<(ImplRef, VFuncRef), Box<[Option<FuncRef>]>>,
    pub func_spans: HashMap<(ImplRef, VFuncRef), ModuleSpan>,
    pub mut_self: HashSet<(ImplRef, VFuncRef)>,
    pub variadic: HashSet<(ImplRef, VFuncRef)>,
    pub public_methods: HashSet<(ImplRef, VFuncRef)>,
    pub concrete_impls: IndexMap<(Option<TraitRef>, SimplifiedType), Vec<ImplRef>>,
    pub blanket_impls: Vec<ImplRef>,
}

impl ImplRegistry {
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

    pub fn satisfies(&self, storage: &TypeStorage, ty: TypeRef, trait_ref: TraitRef, trait_args: Option<&[TypeRef]>) -> bool {
        match &storage.types[ty] {
            Type::Generic(_) | Type::Error => true,
            Type::Trait(object_trait, object_args) if *object_trait == trait_ref => trait_args.is_none_or(|trait_args| {
                object_args.len() == trait_args.len() && object_args.iter().zip(trait_args).all(|(&found, &expected)| storage.is_same(found, expected))
            }),
            _ => self.impl_of(storage, trait_ref, trait_args, ty).is_ok(),
        }
    }

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

    fn applies(&self, storage: &TypeStorage, impl_ref: ImplRef, ty: TypeRef) -> bool {
        self.applicable_args(storage, impl_ref, ty).is_some()
    }

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

    pub fn trait_impl_lookup(&self, storage: &TypeStorage, trait_ref: TraitRef, ty: TypeRef) -> Result<ImplRef, LookupError<ImplRef>> {
        self.impl_of(storage, trait_ref, None, ty)
    }

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

    pub fn is_same(&self, ty: TypeRef, other: TypeRef) -> bool {
        match (&self.types[ty], &self.types[other]) {
            // Errors are already reported, don't report everything they touch again.
            (Type::Error | Type::Primitive(PrimitiveType::Any), _) | (_, Type::Error | Type::Primitive(PrimitiveType::Any)) => true,
            (Type::Generic(index), Type::Generic(other_index)) => index == other_index,
            (Type::Primitive(primitive_type), Type::Primitive(other_primitive_type)) => primitive_type == other_primitive_type,
            // The direction is checked by unification already: either array
            // may be used as the other (any array as a view).
            (&Type::Array(element, kind), &Type::Array(other_element, other_kind)) => {
                let same_size = |types: &TypeStorage, size, other_size| types.is_same(size, other_size);

                self.is_same(element, other_element)
                    && (self.types.kinds_match(kind, other_kind, same_size) || self.types.kinds_match(other_kind, kind, same_size))
            }
            (Type::Size(size), Type::Size(other_size)) => size == other_size,
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

    pub fn is_value_type(&self, ty: TypeRef) -> bool {
        match self.types[ty] {
            Type::Adt(adt, _) => self.def_registry.value_types.contains(&adt),
            // Fixed arrays are values too.
            Type::Array(_, ArrayKind::Fixed(_)) => true,
            _ => false,
        }
    }

    pub fn find_trait_impl(&self, ty: TypeRef, trait_ref: TraitRef, trait_args: &[TypeRef]) -> Option<ImplRef> {
        // Arguments that aren't known (yet) don't select an impl.
        let trait_args = (!trait_args.iter().any(|&arg| self.types[arg] == Type::Error || self.types.has_generics(arg))).then_some(trait_args);

        self.impl_registry.impl_of(&self.types, trait_ref, trait_args, ty).ok()
    }

    pub fn find_vtable_by_func<T: AsRef<str>>(&self, ty: TypeRef, name: T) -> Option<(ImplRef, VFuncRef)> {
        self.method_lookup(None, ty, name.as_ref()).ok()
    }

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

    pub fn trait_impl_lookup(&self, trait_ref: TraitRef, ty: TypeRef) -> Result<ImplRef, LookupError<ImplRef>> {
        self.impl_registry.trait_impl_lookup(&self.types, trait_ref, ty)
    }

    pub fn method_lookup(&self, trait_ref: Option<TraitRef>, ty: TypeRef, name: &str) -> Result<(ImplRef, VFuncRef), LookupError<(ImplRef, VFuncRef)>> {
        self.impl_registry.method_lookup(&self.types, trait_ref, ty, name)
    }

    pub fn can_use_method(&self, from: ModuleId, impl_ref: ImplRef, func: VFuncRef) -> bool {
        let registry = &self.def_registry;

        if let Some(trait_ref) = self.impl_registry.impls[impl_ref].origin_trait {
            return registry.trait_modules.get(&trait_ref).is_none_or(|&owner| {
                let public = registry.public_items.contains(&(owner, registry.traits[trait_ref].name.clone()));

                registry.can_use(from, owner, public)
            });
        }

        self.impl_registry
            .func_spans
            .get(&(impl_ref, func))
            .is_none_or(|&ModuleSpan(owner, _)| registry.can_use(from, owner, self.impl_registry.public_methods.contains(&(impl_ref, func))))
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
            &Type::Array(element, kind) => match kind {
                ArrayKind::Growable => write!(f, "{}[]", self.with(element)),
                ArrayKind::View { writable } => write!(f, "{}{}[..]", if writable { "mut " } else { "" }, self.with(element)),
                // A generic size is `_` (any).
                ArrayKind::Fixed(size) => match self.storage[size] {
                    Type::Size(size) => write!(f, "{}[{size}]", self.with(element)),
                    _ => write!(f, "{}[_]", self.with(element)),
                },
            },
            Type::Size(size) => write!(f, "{size}"),
            Type::Adt(adt_ref, type_args) => {
                self.adt_types[*adt_ref].name.fmt(f)?;

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
