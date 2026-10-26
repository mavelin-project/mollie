use std::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum IntType {
    ISize,
    I64,
    I32,
    I16,
    I8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum UIntType {
    USize,
    U64,
    U32,
    U16,
    U8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
pub enum PrimitiveType {
    Any,
    Int(IntType),
    UInt(UIntType),
    F32,
    F64,
    Bool,
    String,
    Void,
}

impl PrimitiveType {
    pub const fn is_num(&self) -> bool {
        matches!(self, Self::Int(_) | Self::UInt(_))
    }

    pub const fn is_int(&self) -> bool {
        matches!(self, Self::Int(_))
    }

    pub const fn is_uint(&self) -> bool {
        matches!(self, Self::UInt(_))
    }

    pub const fn is_f32(&self) -> bool {
        matches!(self, Self::F32)
    }

    pub const fn is_float(&self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
}

impl fmt::Display for PrimitiveType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("<any>"),
            Self::Int(IntType::ISize) => f.write_str("isize"),
            Self::Int(IntType::I64) => f.write_str("i64"),
            Self::Int(IntType::I32) => f.write_str("i32"),
            Self::Int(IntType::I16) => f.write_str("i16"),
            Self::Int(IntType::I8) => f.write_str("i8"),
            Self::UInt(UIntType::USize) => f.write_str("usize"),
            Self::UInt(UIntType::U64) => f.write_str("u64"),
            Self::UInt(UIntType::U32) => f.write_str("u32"),
            Self::UInt(UIntType::U16) => f.write_str("u16"),
            Self::UInt(UIntType::U8) => f.write_str("u8"),
            Self::F32 => f.write_str("f32"),
            Self::F64 => f.write_str("f64"),
            Self::Bool => f.write_str("bool"),
            Self::String => f.write_str("string"),
            Self::Void => f.write_str("void"),
        }
    }
}
