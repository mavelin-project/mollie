//! Places: what assignments (and `mut self` functions) change.

use cranelift::{codegen::ir, module::Module};
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{Expr, ExprRef, TypedAST};
use mollie_typing::{AdtVariantRef, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

/// Where a value can be stored.
#[derive(Debug, Clone)]
pub enum Place {
    /// Memory: a field of an object or an element of an array, or a part of
    /// one (a field of a value type stored there).
    Memory { ptr: ir::Value, offset: i32 },
    /// A variable of type `ty`, or a part of it at `offset` (a field of a
    /// value type).
    Var { name: String, ty: TypeRef, offset: u32 },
}

impl Place {
    /// The part of the place at `offset` in it.
    fn at(self, offset: i32) -> Self {
        match self {
            Self::Memory { ptr, offset: base } => Self::Memory { ptr, offset: base + offset },
            Self::Var { name, ty, offset: base } => Self::Var {
                name,
                ty,
                offset: base + offset.cast_unsigned(),
            },
        }
    }
}

impl<M: Module> FunctionCompiler<'_, M> {
    /// The place `expr` refers to, or `None` for a temporary value. Fields of
    /// values of value types are parts of the place of the value; fields of
    /// objects are places in the object.
    pub fn place(&mut self, ast: &TypedAST, expr: ExprRef) -> CompileResult<Option<Place>> {
        match ast[expr].value {
            Expr::Var(ref name) => Ok(Some(Place::Var {
                name: name.clone(),
                ty: ast[expr].ty,
                offset: 0,
            })),
            Expr::AdtIndex { target, field } => {
                let target_ty = ast[target].ty;
                let (_, offset, _) = self.field_layout(target_ty, AdtVariantRef::ZERO, field)?;

                if matches!(self.ir_type(target_ty)?, Some(MollieType::Inline { .. })) {
                    Ok(self.place(ast, target)?.map(|place| place.at(offset)))
                } else {
                    let ptr = target.compile(ast, self)?.value()?;

                    Ok(Some(Place::Memory { ptr, offset }))
                }
            }
            Expr::ArrayIndex { target, element } => {
                let array = target.compile(ast, self)?.value()?;
                let index = element.compile(ast, self)?.value()?;
                let element_type = self.value_type(ast[expr].ty)?;
                let ptr = self.element_addr(array, index, element_type.bytes());

                Ok(Some(Place::Memory { ptr, offset: 0 }))
            }
            _ => Ok(None),
        }
    }

    /// Whether `place` is a whole variable with the representation `ty`.
    fn is_whole_var(&self, place: &Place, ty: MollieType) -> CompileResult<bool> {
        Ok(match *place {
            Place::Var { ty: var_ty, offset, .. } => offset == 0 && self.ir_type(var_ty)? == Some(ty),
            Place::Memory { .. } => false,
        })
    }

    /// The value of representation `ty` in `place`.
    pub fn read_place(&mut self, place: &Place, ty: MollieType) -> CompileResult<MolValue> {
        if self.is_whole_var(place, ty)? {
            let Place::Var { name, .. } = place else { unreachable!() };

            return self.read_var(name);
        }

        match *place {
            Place::Memory { ptr, offset } => Ok(self.load_value(ty, ptr, offset)),
            Place::Var { ref name, ty: var_ty, offset } => {
                let (MolValue::Inline(values), Some(MollieType::Inline { size, .. })) = (self.read_var(name)?, self.ir_type(var_ty)?) else {
                    return Err(CompileError::unsupported(format!("part of variable `{name}` that isn't of a value type")));
                };

                Ok(self.extract(&values, size, offset, ty))
            }
        }
    }

    /// Stores `value` of representation `ty` in `place`.
    pub fn write_place(&mut self, place: &Place, ty: MollieType, value: MolValue) -> CompileResult<()> {
        if self.is_whole_var(place, ty)? {
            let Place::Var { name, .. } = place else { unreachable!() };

            return self.assign_var(name, value);
        }

        match *place {
            Place::Memory { ptr, offset } => self.store_value(ty, &value, ptr, offset),
            Place::Var { ref name, ty: var_ty, offset } => {
                let (MolValue::Inline(mut values), Some(MollieType::Inline { size, .. })) = (self.read_var(name)?, self.ir_type(var_ty)?) else {
                    return Err(CompileError::unsupported(format!("part of variable `{name}` that isn't of a value type")));
                };

                self.insert(&mut values, size, offset, ty, &value)?;
                self.assign_var(name, MolValue::Inline(values))
            }
        }
    }
}
