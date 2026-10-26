use cranelift::{
    codegen::ir,
    module::Module,
    prelude::{InstBuilder, IntCC},
};
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{Expr, ExprRef, LitExpr, TypedAST};
use mollie_typing::{AdtVariantRef, FieldRef, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
    sandbox::TrapKind,
};

#[derive(Debug, Clone)]
pub enum Place {
    Memory { ptr: ir::Value, offset: i32 },
    Var { name: String, ty: TypeRef, offset: u32 },
    Spilled {
        name: String,
        ty: TypeRef,
        slot: ir::Value,
        ptr: ir::Value,
        offset: i32,
    },
}

impl Place {
    fn at(self, offset: i32) -> Self {
        match self {
            Self::Memory { ptr, offset: base } => Self::Memory { ptr, offset: base + offset },
            Self::Var { name, ty, offset: base } => Self::Var {
                name,
                ty,
                offset: base + offset.cast_unsigned(),
            },
            Self::Spilled {
                name,
                ty,
                slot,
                ptr,
                offset: base,
            } => Self::Spilled {
                name,
                ty,
                slot,
                ptr,
                offset: base + offset,
            },
        }
    }
}

impl<M: Module> FunctionCompiler<'_, M> {
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
            Expr::ArrayIndex { target, element } if self.fixed_length(ast[target].ty).is_some() => self.fixed_element_place(ast, expr, target, element).map(Some),
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

    fn fixed_element_place(&mut self, ast: &TypedAST, expr: ExprRef, target: ExprRef, element: ExprRef) -> CompileResult<Place> {
        let target_ty = ast[target].ty;
        let length = self
            .fixed_length(target_ty)
            .ok_or_else(|| CompileError::unsupported(format!("`{}` isn't a fixed array of a known length", self.display(target_ty))))?;

        if let Expr::Lit(LitExpr::Int(index)) = ast[element].value
            && let Ok(index) = usize::try_from(index)
            && index < length
        {
            let (_, offset, _) = self.field_layout(target_ty, AdtVariantRef::ZERO, FieldRef::new(index))?;

            if let Some(place) = self.place(ast, target)? {
                return Ok(place.at(offset));
            }

            let ptr = self.spill(ast, target)?;

            return Ok(Place::Memory { ptr, offset });
        }

        let place = self.place(ast, target)?;
        // A temporary array is stored on the stack once the index is
        // computed: until then, its references are in stack maps.
        let temporary = if place.is_some() {
            None
        } else {
            let value = target.compile(ast, self)?;

            self.track(target_ty, &value);

            Some(value)
        };
        let index = element.compile(ast, self)?.value()?;
        let place = match (place, temporary) {
            // The variable is copied to the slot when the place is read or
            // written (see `Self::spill_var`): code run in between (the value
            // of an assignment) may change it.
            (Some(Place::Var { name, ty, offset }), _) => {
                let Some(MollieType::Inline { size, align }) = self.ir_type(ty)? else {
                    return Err(CompileError::unsupported(format!("fixed array in variable `{name}` that isn't of a value type")));
                };

                let slot = self.stack_slot(size, align);

                Place::Spilled {
                    name,
                    ty,
                    slot,
                    ptr: slot,
                    offset: offset.cast_signed(),
                }
            }
            (Some(place), _) => place,
            (None, value) => Place::Memory {
                ptr: self.spill_value(target_ty, &value.unwrap_or(MolValue::Nothing))?,
                offset: 0,
            },
        };

        let out_of_bounds = self.fn_builder.ins().icmp_imm_u(IntCC::UnsignedGreaterThanOrEqual, index, length.cast_signed() as i64);

        self.trap_if(out_of_bounds, TrapKind::OutOfBounds);

        let stride = self.value_type(ast[expr].ty)?.bytes();
        let scaled = self.fn_builder.ins().imul_imm_u(index, i64::from(stride));

        Ok(match place {
            Place::Memory { ptr, offset } => Place::Memory {
                ptr: self.fn_builder.ins().iadd(ptr, scaled),
                offset,
            },
            Place::Spilled { name, ty, slot, ptr, offset } => Place::Spilled {
                name,
                ty,
                slot,
                ptr: self.fn_builder.ins().iadd(ptr, scaled),
                offset,
            },
            Place::Var { .. } => unreachable!("variables are copied to the stack above"),
        })
    }

    fn spill_var(&mut self, name: &str, ty: TypeRef, slot: ir::Value) -> CompileResult<u32> {
        let (MolValue::Inline(values), Some(MollieType::Inline { size, .. })) = (self.read_var(name)?, self.ir_type(ty)?) else {
            return Err(CompileError::unsupported(format!("part of variable `{name}` that isn't of a value type")));
        };

        self.store_inline(size, &values, slot, 0);

        Ok(size)
    }

    fn spill(&mut self, ast: &TypedAST, expr: ExprRef) -> CompileResult<ir::Value> {
        let value = expr.compile(ast, self)?;

        self.spill_value(ast[expr].ty, &value)
    }

    fn spill_value(&mut self, ty: TypeRef, value: &MolValue) -> CompileResult<ir::Value> {
        let Some(MollieType::Inline { size, align }) = self.ir_type(ty)? else {
            return Err(CompileError::unsupported(format!("`{}` isn't a value type", self.display(ty))));
        };

        let slot = self.stack_slot(size, align);

        self.store_value(MollieType::Inline { size, align }, value, slot, 0)?;

        Ok(slot)
    }

    fn is_whole_var(&self, place: &Place, ty: MollieType) -> CompileResult<bool> {
        Ok(match *place {
            Place::Var { ty: var_ty, offset, .. } => offset == 0 && self.ir_type(var_ty)? == Some(ty),
            Place::Memory { .. } | Place::Spilled { .. } => false,
        })
    }

    pub fn read_place(&mut self, place: &Place, ty: MollieType) -> CompileResult<MolValue> {
        if self.is_whole_var(place, ty)? {
            let Place::Var { name, .. } = place else { unreachable!() };

            return self.read_var(name);
        }

        match *place {
            Place::Memory { ptr, offset } => Ok(self.load_value(ty, ptr, offset)),
            Place::Spilled {
                ref name,
                ty: var_ty,
                slot,
                ptr,
                offset,
            } => {
                self.spill_var(name, var_ty, slot)?;

                Ok(self.load_value(ty, ptr, offset))
            }
            Place::Var { ref name, ty: var_ty, offset } => {
                let (MolValue::Inline(values), Some(MollieType::Inline { size, .. })) = (self.read_var(name)?, self.ir_type(var_ty)?) else {
                    return Err(CompileError::unsupported(format!("part of variable `{name}` that isn't of a value type")));
                };

                Ok(self.extract(&values, size, offset, ty))
            }
        }
    }

    pub fn write_place(&mut self, place: &Place, ty: MollieType, value: MolValue) -> CompileResult<()> {
        if self.is_whole_var(place, ty)? {
            let Place::Var { name, .. } = place else { unreachable!() };

            return self.assign_var(name, value);
        }

        match *place {
            Place::Memory { ptr, offset } => self.store_value(ty, &value, ptr, offset),
            Place::Spilled {
                ref name,
                ty: var_ty,
                slot,
                ptr,
                offset,
            } => {
                let size = self.spill_var(name, var_ty, slot)?;

                self.store_value(ty, &value, ptr, offset)?;

                // References in the variable are in stack maps.
                let values = self.load_inline(size, slot, 0);

                self.assign_var(name, MolValue::Inline(values))
            }
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
