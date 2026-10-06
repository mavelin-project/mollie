use std::ops::Index;

use mollie_const::ConstantValue;
use mollie_index::{Idx, IndexBoxedSlice};

use crate::{AdtVariantRef, FieldRef, ty::TypeRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum AdtKind {
    Struct,
    View,
    Enum,
}

#[derive(Debug)]
pub struct AdtVariantField {
    pub name: String,
    pub ty: TypeRef,
    pub default_value: Option<ConstantValue>,
}

#[derive(Debug)]
pub struct AdtVariant {
    pub name: Option<String>,
    pub discriminant: usize,
    pub fields: IndexBoxedSlice<FieldRef, AdtVariantField>,
}

#[derive(Debug)]
pub struct Adt {
    pub name: Option<String>,
    pub collectable: bool,
    pub kind: AdtKind,
    pub generics: usize,
    pub variants: IndexBoxedSlice<AdtVariantRef, AdtVariant>,
}

impl Index<(AdtVariantRef, FieldRef)> for Adt {
    type Output = AdtVariantField;

    fn index(&self, (variant, field): (AdtVariantRef, FieldRef)) -> &Self::Output {
        &self.variants[variant].fields[field]
    }
}

impl Index<FieldRef> for Adt {
    type Output = AdtVariantField;

    fn index(&self, field: FieldRef) -> &Self::Output {
        &self.variants[AdtVariantRef::ZERO].fields[field]
    }
}
