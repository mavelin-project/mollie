mod adt;
mod diagnostic;
mod error;
mod primitive_type;
mod solver;
mod ty;
mod type_context;
mod type_info;

use mollie_index::new_idx_type;

pub use self::{
    adt::{Adt, AdtKind, AdtVariant, AdtVariantField},
    diagnostic::{Diagnostic, DiagnosticContext, DiagnosticDisplay},
    error::{DefinitionType, LookupType, SpecialAdtKind, TypeError, TypeErrorValue},
    primitive_type::{IntType, PrimitiveType, UIntType},
    solver::{TypeFrameRef, TypeSolver, UnifyArgs, UnifyError},
    ty::{Type, TypeRef},
    type_context::{
        Bound, Const, DefRegistry, Func, ImplRegistry, IntrinsicKind, LangItemValue, LookupError, Module, ModuleDisplay, ModuleItem, ModuleSpan, Trait,
        TraitFunc, TyCtxt, TypeDisplay, TypeStorage, VTableFunc, VTableGenerator,
    },
    type_info::{AdtTypeInfo, ArrayTypeInfo, FuncTypeInfo, TraitTypeInfo, TypeInfo, TypeInfoRef},
};

new_idx_type!(AdtRef);
new_idx_type!(AdtVariantRef);
new_idx_type!(FieldRef);
new_idx_type!(TraitRef);
new_idx_type!(ImplRef);
new_idx_type!(VFuncRef);
new_idx_type!(FuncRef);
new_idx_type!(ConstRef);
new_idx_type!(TraitFuncRef);
new_idx_type!(ModuleId);
new_idx_type!(TypeErrorRef);

impl TraitFuncRef {
    pub const fn as_vfunc(&self) -> VFuncRef {
        VFuncRef(self.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ArgType {
    This,
    Regular,
}

#[derive(Debug, Clone)]
pub struct Arg<T> {
    pub name: String,
    pub kind: ArgType,
    pub ty: T,
}

#[derive(Debug, Clone, Copy)]
pub struct CoreTypes<T> {
    pub void: T,
    pub any: T,
    pub bool: T,
    pub i8: T,
    pub i16: T,
    pub i32: T,
    pub i64: T,
    pub isize: T,
    pub u8: T,
    pub u16: T,
    pub u32: T,
    pub u64: T,
    pub usize: T,
    pub f32: T,
    pub string: T,
}

impl<T: Copy> CoreTypes<T> {
    pub const fn cast_primitive(&self, primitive: PrimitiveType) -> T {
        match primitive {
            PrimitiveType::Any => self.any,
            PrimitiveType::Int(int_type) => match int_type {
                IntType::ISize => self.isize,
                IntType::I64 => self.i64,
                IntType::I32 => self.i32,
                IntType::I16 => self.i16,
                IntType::I8 => self.i8,
            },
            PrimitiveType::UInt(uint_type) => match uint_type {
                UIntType::USize => self.usize,
                UIntType::U64 => self.u64,
                UIntType::U32 => self.u32,
                UIntType::U16 => self.u16,
                UIntType::U8 => self.u8,
            },
            PrimitiveType::F32 => self.f32,
            PrimitiveType::Bool => self.bool,
            PrimitiveType::String => self.string,
            PrimitiveType::Void => self.void,
        }
    }
}
