use cranelift::{
    codegen::ir,
    module::Module,
    prelude::{FloatCC, InstBuilder, IntCC},
};
use mollie_shared::Operator;
use mollie_typed_ast::{ExprRef, TypedAST};
use mollie_typing::{PrimitiveType, Type, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
    sandbox::TrapKind,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles `lhs <operator> rhs`. Assignments produce no value.
    pub fn compile_binary(&mut self, ast: &TypedAST, operator: Operator, lhs: ExprRef, rhs: ExprRef) -> CompileResult<MolValue> {
        match operator {
            Operator::Assign
            | Operator::AddAssign
            | Operator::SubAssign
            | Operator::MulAssign
            | Operator::DivAssign
            | Operator::RemAssign
            | Operator::BitAndAssign
            | Operator::BitOrAssign => {
                self.compile_assignment(ast, operator.lower(), lhs, rhs)?;

                Ok(MolValue::Nothing)
            }
            Operator::And | Operator::Or => self.compile_logical(ast, operator, lhs, rhs).map(MolValue::Value),
            _ => {
                let lhs_value = lhs.compile(ast, self)?;
                let rhs_value = rhs.compile(ast, self)?;

                self.bin_op(&lhs_value, ast[lhs].ty, operator, &rhs_value).map(MolValue::Value)
            }
        }
    }

    /// `&&` and `||`, evaluating `rhs` only when needed.
    fn compile_logical(&mut self, ast: &TypedAST, operator: Operator, lhs: ExprRef, rhs: ExprRef) -> CompileResult<ir::Value> {
        let lhs_value = lhs.compile(ast, self)?.value()?;
        let rhs_block = self.fn_builder.create_block();
        let after_block = self.fn_builder.create_block();
        let result = self.fn_builder.append_block_param(after_block, ir::types::I8);

        // The result of the left side is the result of the whole expression
        // when `rhs` is skipped.
        if operator == Operator::And {
            self.fn_builder
                .ins()
                .brif(lhs_value, rhs_block, &[], after_block, &[ir::BlockArg::Value(lhs_value)]);
        } else {
            self.fn_builder
                .ins()
                .brif(lhs_value, after_block, &[ir::BlockArg::Value(lhs_value)], rhs_block, &[]);
        }

        self.fn_builder.switch_to_block(rhs_block);
        self.fn_builder.seal_block(rhs_block);

        let rhs_value = rhs.compile(ast, self)?.value()?;

        self.fn_builder.ins().jump(after_block, &[ir::BlockArg::Value(rhs_value)]);
        self.fn_builder.switch_to_block(after_block);
        self.fn_builder.seal_block(after_block);

        Ok(result)
    }

    /// Stops the program if an integer division `lhs / rhs` would trap: on
    /// division by zero, and on overflow of a signed division.
    fn check_division(&mut self, lhs: ir::Value, rhs: ir::Value, signed: bool) {
        let ty = self.fn_builder.func.dfg.value_type(rhs);
        let zero = self.iconst(ty, 0);
        let is_zero = self.fn_builder.ins().icmp(IntCC::Equal, rhs, zero);

        self.trap_if(is_zero, TrapKind::DivisionByZero);

        if signed {
            // The minimum value of the type, e.g. `i32::MIN`.
            let min = self.iconst(ty, i64::MIN >> (64 - ty.bits()));
            let minus_one = self.iconst(ty, -1);
            let is_min = self.fn_builder.ins().icmp(IntCC::Equal, lhs, min);
            let is_minus_one = self.fn_builder.ins().icmp(IntCC::Equal, rhs, minus_one);
            let overflows = self.fn_builder.ins().band(is_min, is_minus_one);

            self.trap_if(overflows, TrapKind::ArithmeticOverflow);
        }
    }

    /// Applies a binary operator to two values of type `ty`.
    pub fn bin_op(&mut self, lhs: &MolValue, ty: TypeRef, operator: Operator, rhs: &MolValue) -> CompileResult<ir::Value> {
        let ty_name = self.display(ty);
        let unsupported = || CompileError::unsupported(format!("operator `{operator}` for `{ty_name}`"));

        // Value structs are compared field by field.
        if matches!(lhs, MolValue::Inline(_)) && matches!(operator, Operator::Equal | Operator::NotEqual) {
            let fields = self
                .compiled_adt(ty)?
                .main_variant()
                .fields
                .values()
                .map(|(field, field_ty)| (field.ty, field.offset, *field_ty))
                .collect::<Vec<_>>();
            let mut equal = self.iconst(ir::types::I8, 1);

            for (field_type, offset, field_ty) in fields {
                let lhs = self.read_field(lhs, ty, field_type, offset)?;
                let rhs = self.read_field(rhs, ty, field_type, offset)?;
                let field_equal = self.bin_op(&lhs, field_ty, Operator::Equal, &rhs)?;

                equal = self.fn_builder.ins().band(equal, field_equal);
            }

            return Ok(if operator == Operator::Equal {
                equal
            } else {
                self.fn_builder.ins().bxor_imm_u(equal, 1)
            });
        }

        let Type::Primitive(primitive) = self.types()[self.resolve(ty)] else {
            return Err(unsupported());
        };

        if primitive == PrimitiveType::String {
            let (lhs, rhs) = (lhs.value()?, rhs.value()?);

            return match operator {
                Operator::Add => {
                    let concat = self.compiler.runtime.str_concat;

                    Ok(self.call(concat, &[lhs, rhs])[0])
                }
                Operator::Equal | Operator::NotEqual => {
                    let str_eq = self.compiler.runtime.str_eq;
                    let equal = self.call_unchecked(str_eq, &[lhs, rhs])[0];

                    Ok(if operator == Operator::Equal {
                        equal
                    } else {
                        self.fn_builder.ins().bxor_imm_u(equal, 1)
                    })
                }
                // Ordered by bytes: compared to the result of `compare` (-1,
                // 0 or 1).
                Operator::LessThan | Operator::LessThanEqual | Operator::GreaterThan | Operator::GreaterThanEqual => {
                    let compare = self.compiler.builtins["string_compare"];
                    let order = self.call_unchecked(compare, &[lhs, rhs])[0];
                    let condition = match operator {
                        Operator::LessThan => IntCC::SignedLessThan,
                        Operator::LessThanEqual => IntCC::SignedLessThanOrEqual,
                        Operator::GreaterThan => IntCC::SignedGreaterThan,
                        _ => IntCC::SignedGreaterThanOrEqual,
                    };

                    Ok(self.fn_builder.ins().icmp_imm_s(condition, order, 0))
                }
                _ => Err(unsupported()),
            };
        }

        let (lhs, rhs) = (lhs.value()?, rhs.value()?);

        if matches!(operator, Operator::Div | Operator::Rem) && primitive.is_num() {
            self.check_division(lhs, rhs, primitive.is_int());
        }

        let ins = self.fn_builder.ins();

        Ok(match primitive {
            PrimitiveType::F32 => match operator {
                Operator::Add => ins.fadd(lhs, rhs),
                Operator::Sub => ins.fsub(lhs, rhs),
                Operator::Mul => ins.fmul(lhs, rhs),
                Operator::Div => ins.fdiv(lhs, rhs),
                Operator::Equal => ins.fcmp(FloatCC::Equal, lhs, rhs),
                Operator::NotEqual => ins.fcmp(FloatCC::NotEqual, lhs, rhs),
                Operator::LessThan => ins.fcmp(FloatCC::LessThan, lhs, rhs),
                Operator::LessThanEqual => ins.fcmp(FloatCC::LessThanOrEqual, lhs, rhs),
                Operator::GreaterThan => ins.fcmp(FloatCC::GreaterThan, lhs, rhs),
                Operator::GreaterThanEqual => ins.fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs),
                _ => return Err(unsupported()),
            },
            PrimitiveType::Int(_) => match operator {
                Operator::Add => ins.iadd(lhs, rhs),
                Operator::Sub => ins.isub(lhs, rhs),
                Operator::Mul => ins.imul(lhs, rhs),
                Operator::Div => ins.sdiv(lhs, rhs),
                Operator::Rem => ins.srem(lhs, rhs),
                Operator::Equal => ins.icmp(IntCC::Equal, lhs, rhs),
                Operator::NotEqual => ins.icmp(IntCC::NotEqual, lhs, rhs),
                Operator::LessThan => ins.icmp(IntCC::SignedLessThan, lhs, rhs),
                Operator::LessThanEqual => ins.icmp(IntCC::SignedLessThanOrEqual, lhs, rhs),
                Operator::GreaterThan => ins.icmp(IntCC::SignedGreaterThan, lhs, rhs),
                Operator::GreaterThanEqual => ins.icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs),
                Operator::BitAnd => ins.band(lhs, rhs),
                Operator::BitOr => ins.bor(lhs, rhs),
                _ => return Err(unsupported()),
            },
            PrimitiveType::UInt(_) | PrimitiveType::Bool => match operator {
                Operator::Add => ins.iadd(lhs, rhs),
                Operator::Sub => ins.isub(lhs, rhs),
                Operator::Mul => ins.imul(lhs, rhs),
                Operator::Div => ins.udiv(lhs, rhs),
                Operator::Rem => ins.urem(lhs, rhs),
                Operator::Equal => ins.icmp(IntCC::Equal, lhs, rhs),
                Operator::NotEqual => ins.icmp(IntCC::NotEqual, lhs, rhs),
                Operator::LessThan => ins.icmp(IntCC::UnsignedLessThan, lhs, rhs),
                Operator::LessThanEqual => ins.icmp(IntCC::UnsignedLessThanOrEqual, lhs, rhs),
                Operator::GreaterThan => ins.icmp(IntCC::UnsignedGreaterThan, lhs, rhs),
                Operator::GreaterThanEqual => ins.icmp(IntCC::UnsignedGreaterThanOrEqual, lhs, rhs),
                Operator::BitAnd => ins.band(lhs, rhs),
                Operator::BitOr => ins.bor(lhs, rhs),
                _ => return Err(unsupported()),
            },
            _ => return Err(unsupported()),
        })
    }

    /// Compiles `lhs = rhs`, or `lhs <operator>= rhs` when `operator` is set.
    fn compile_assignment(&mut self, ast: &TypedAST, operator: Option<Operator>, lhs: ExprRef, rhs: ExprRef) -> CompileResult<()> {
        let lhs_ty = ast[lhs].ty;
        let place = self
            .place(ast, lhs)?
            .ok_or_else(|| CompileError::unsupported("assignment to a temporary value"))?;
        let ty = self.value_type(lhs_ty)?;
        let value = rhs.compile(ast, self)?;
        let value = self.coerce(value, ast[rhs].ty, lhs_ty)?;
        let value = match operator {
            Some(operator) => {
                let current = self.read_place(&place, ty)?;

                MolValue::Value(self.bin_op(&current, lhs_ty, operator, &value)?)
            }
            None => value,
        };

        self.write_place(&place, ty, value)
    }
}
