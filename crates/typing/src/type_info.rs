use crate::{AdtRef, PrimitiveType, TraitRef};

mollie_index::new_idx_type!(TypeInfoRef);

#[derive(Debug, Clone)]
pub enum TypeInfo {
    Primitive(PrimitiveType),
    Array(ArrayTypeInfo),
    Func(FuncTypeInfo),
    Adt(AdtTypeInfo),
    Trait(TraitTypeInfo),
    Unknown(Option<TypeInfoRef>),
    Integer,
    Float,
    Generic(usize),
    Ref(TypeInfoRef),
    Size(usize),
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKindInfo {
    Growable,
    View { writable: bool },
    Fixed(TypeInfoRef),
    Literal(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct ArrayTypeInfo {
    pub element: TypeInfoRef,
    pub kind: ArrayKindInfo,
}

#[derive(Debug, Clone)]
pub struct FuncTypeInfo {
    pub args: Box<[TypeInfoRef]>,
    pub returns: TypeInfoRef,
}

#[derive(Debug, Clone)]
pub struct AdtTypeInfo {
    pub id: AdtRef,
    pub type_args: Box<[TypeInfoRef]>,
}

#[derive(Debug, Clone)]
pub struct TraitTypeInfo {
    pub id: TraitRef,
    pub type_args: Box<[TypeInfoRef]>,
}
