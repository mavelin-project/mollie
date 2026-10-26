use std::cmp::Ordering;

use cranelift::{
    codegen::ir,
    module::Module,
    prelude::{InstBuilder, IntCC},
};
use mollie_ir::MollieType;
use mollie_shared::FormatSpec;
use mollie_typed_ast::{ExprRef, TypedAST};
use mollie_typing::{PrimitiveType, Type};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    fn number_to_string(&mut self, value: ir::Value, from: PrimitiveType) -> CompileResult<ir::Value> {
        let runtime = self.compiler.runtime;
        let value_ir = self.fn_builder.func.dfg.value_type(value);
        let ins = self.fn_builder.ins();

        let (func, arg) = match from {
            PrimitiveType::Int(_) if value_ir == ir::types::I64 => (runtime.str_from_int, value),
            PrimitiveType::Int(_) => (runtime.str_from_int, ins.sextend(ir::types::I64, value)),
            PrimitiveType::UInt(_) if value_ir == ir::types::I64 => (runtime.str_from_uint, value),
            PrimitiveType::UInt(_) => (runtime.str_from_uint, ins.uextend(ir::types::I64, value)),
            PrimitiveType::F32 => (runtime.str_from_f32, value),
            PrimitiveType::F64 => (runtime.str_from_f64, value),
            PrimitiveType::Bool => (runtime.str_from_bool, value),
            _ => return Err(CompileError::unsupported(format!("casting `{from}` to `string`"))),
        };

        Ok(self.call(func, &[arg])[0])
    }

    pub fn compile_format(&mut self, ast: &TypedAST, value: ExprRef, spec: FormatSpec) -> CompileResult<MolValue> {
        let compiled = value.compile(ast, self)?.value()?;
        let Type::Primitive(primitive) = self.types()[self.resolve(ast[value].ty)] else {
            return Err(CompileError::unsupported(format!("formatting `{}`", self.display(ast[value].ty))));
        };

        let runtime = self.compiler.runtime;
        let compiled = if primitive == PrimitiveType::Bool {
            self.number_to_string(compiled, primitive)?
        } else {
            compiled
        };
        let value_ir = self.fn_builder.func.dfg.value_type(compiled);
        let spec = self.fn_builder.ins().iconst(ir::types::I64, spec.pack().cast_signed());

        let (func, arg) = match primitive {
            PrimitiveType::Int(_) if value_ir == ir::types::I64 => (runtime.str_format_int, compiled),
            PrimitiveType::Int(_) => (runtime.str_format_int, self.fn_builder.ins().sextend(ir::types::I64, compiled)),
            PrimitiveType::UInt(_) if value_ir == ir::types::I64 => (runtime.str_format_uint, compiled),
            PrimitiveType::UInt(_) => (runtime.str_format_uint, self.fn_builder.ins().uextend(ir::types::I64, compiled)),
            PrimitiveType::F32 => (runtime.str_format_f32, compiled),
            PrimitiveType::F64 => (runtime.str_format_f64, compiled),
            PrimitiveType::String | PrimitiveType::Bool => (runtime.str_format_str, compiled),
            _ => return Err(CompileError::unsupported(format!("formatting `{primitive}`"))),
        };

        Ok(MolValue::Value(self.call(func, &[arg, spec])[0]))
    }

    pub fn compile_cast(&mut self, ast: &TypedAST, expr: ExprRef, to: PrimitiveType) -> CompileResult<MolValue> {
        let value = expr.compile(ast, self)?;
        let Type::Primitive(from) = self.types()[self.resolve(ast[expr].ty)] else {
            return Err(CompileError::unsupported(format!("casting `{}` to `{to}`", self.display(ast[expr].ty))));
        };

        if from == to {
            return Ok(value);
        }

        let value = value.value()?;

        if to == PrimitiveType::String {
            return self.number_to_string(value, from).map(MolValue::Value);
        }

        let to_type = self.type_context.tcx.types.core_types.cast_primitive(to);
        let Some(MollieType::Regular(to_ir)) = self.ir_type(to_type)? else {
            return Err(CompileError::unsupported(format!("casting `{from}` to `{to}`")));
        };

        let from_ir = self.fn_builder.func.dfg.value_type(value);
        // Booleans are unsigned integers here.
        let from_signed = from.is_int();
        let ins = self.fn_builder.ins();

        let result = match (from, to) {
            (PrimitiveType::Int(_) | PrimitiveType::UInt(_) | PrimitiveType::Bool, PrimitiveType::Int(_) | PrimitiveType::UInt(_)) => {
                match from_ir.bytes().cmp(&to_ir.bytes()) {
                    Ordering::Less if from_signed => ins.sextend(to_ir, value),
                    Ordering::Less => ins.uextend(to_ir, value),
                    Ordering::Equal => value,
                    Ordering::Greater => ins.ireduce(to_ir, value),
                }
            }
            (PrimitiveType::Int(_) | PrimitiveType::UInt(_), PrimitiveType::Bool) => ins.icmp_imm_u(IntCC::NotEqual, value, 0),
            // Backends convert integers of 32 and 64 bits: smaller ones are
            // extended first.
            (PrimitiveType::Int(_), target) if target.is_float() && from_ir.bits() < 32 => {
                let wide = ins.sextend(ir::types::I32, value);

                self.fn_builder.ins().fcvt_from_sint(to_ir, wide)
            }
            (PrimitiveType::UInt(_) | PrimitiveType::Bool, target) if target.is_float() && from_ir.bits() < 32 => {
                let wide = ins.uextend(ir::types::I32, value);

                self.fn_builder.ins().fcvt_from_uint(to_ir, wide)
            }
            (PrimitiveType::Int(_), target) if target.is_float() => ins.fcvt_from_sint(to_ir, value),
            (PrimitiveType::UInt(_) | PrimitiveType::Bool, target) if target.is_float() => ins.fcvt_from_uint(to_ir, value),
            // Exact one way; rounded to nearest the other.
            (PrimitiveType::F32, PrimitiveType::F64) => ins.fpromote(to_ir, value),
            (PrimitiveType::F64, PrimitiveType::F32) => ins.fdemote(to_ir, value),
            // Out-of-range and NaN values saturate instead of trapping. Backends
            // convert to integers of 32 and 64 bits: smaller ones saturate to
            // 32 bits, then to their own range.
            (source, PrimitiveType::Int(_)) if source.is_float() && to_ir.bits() < 32 => {
                let wide = ins.fcvt_to_sint_sat(ir::types::I32, value);
                let bits = to_ir.bits();
                let (min, max) = (-(1_i64 << (bits - 1)), (1_i64 << (bits - 1)) - 1);
                let min = self.fn_builder.ins().iconst(ir::types::I32, min);
                let max = self.fn_builder.ins().iconst(ir::types::I32, max);
                let clamped = self.fn_builder.ins().smax(wide, min);
                let clamped = self.fn_builder.ins().smin(clamped, max);

                self.fn_builder.ins().ireduce(to_ir, clamped)
            }
            (source, PrimitiveType::UInt(_)) if source.is_float() && to_ir.bits() < 32 => {
                let wide = ins.fcvt_to_uint_sat(ir::types::I32, value);
                let max = self.fn_builder.ins().iconst(ir::types::I32, (1_i64 << to_ir.bits()) - 1);
                let clamped = self.fn_builder.ins().umin(wide, max);

                self.fn_builder.ins().ireduce(to_ir, clamped)
            }
            (source, PrimitiveType::Int(_)) if source.is_float() => ins.fcvt_to_sint_sat(to_ir, value),
            (source, PrimitiveType::UInt(_)) if source.is_float() => ins.fcvt_to_uint_sat(to_ir, value),
            _ => return Err(CompileError::unsupported(format!("casting `{from}` to `{to}`"))),
        };

        Ok(MolValue::Value(result))
    }
}
