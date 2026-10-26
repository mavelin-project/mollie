#![allow(missing_docs)]

use std::{
    hash::{Hash, Hasher},
    mem::discriminant,
};

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum ConstantValue {
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    ISize(isize),
    USize(usize),
    F32(f32),
    F64(f64),
    Bool(bool),
    Array(Box<[Self]>),
    String(String),
    Construct {
        ty: usize,
        variant: usize,
        fields: Box<[(usize, Option<Self>)]>,
    },
    Nothing,
}

impl Eq for ConstantValue {}

impl Hash for ConstantValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        discriminant(self).hash(state);
    }
}

impl From<i8> for ConstantValue {
    fn from(value: i8) -> Self {
        Self::I8(value)
    }
}

impl From<u8> for ConstantValue {
    fn from(value: u8) -> Self {
        Self::U8(value)
    }
}

impl From<i16> for ConstantValue {
    fn from(value: i16) -> Self {
        Self::I16(value)
    }
}

impl From<u16> for ConstantValue {
    fn from(value: u16) -> Self {
        Self::U16(value)
    }
}

impl From<i32> for ConstantValue {
    fn from(value: i32) -> Self {
        Self::I32(value)
    }
}

impl From<u32> for ConstantValue {
    fn from(value: u32) -> Self {
        Self::U32(value)
    }
}

impl From<i64> for ConstantValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<u64> for ConstantValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<isize> for ConstantValue {
    fn from(value: isize) -> Self {
        Self::ISize(value)
    }
}

impl From<usize> for ConstantValue {
    fn from(value: usize) -> Self {
        Self::USize(value)
    }
}

impl From<f32> for ConstantValue {
    fn from(value: f32) -> Self {
        Self::F32(value)
    }
}

impl From<f64> for ConstantValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<&str> for ConstantValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for ConstantValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for ConstantValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}
