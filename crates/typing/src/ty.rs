use crate::{AdtRef, PrimitiveType, TraitRef};

mollie_index::new_idx_type!(TypeRef);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Primitive(PrimitiveType),
    Array(TypeRef, Option<usize>),
    Adt(AdtRef, Box<[TypeRef]>),
    Trait(TraitRef, Box<[TypeRef]>),
    Func(Box<[TypeRef]>, TypeRef),
    Generic(usize),
    Error,
}
