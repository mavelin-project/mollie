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
    /// Converts a number or a boolean to a new string.
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
            PrimitiveType::Bool => (runtime.str_from_bool, value),
            _ => return Err(CompileError::unsupported(format!("casting `{from}` to `string`"))),
        };

        Ok(self.call(func, &[arg])[0])
    }

    /// Compiles `${value:spec}`.
    pub fn compile_format(&mut self, ast: &TypedAST, value: ExprRef, spec: FormatSpec) -> CompileResult<MolValue> {
        let compiled = value.compile(ast, self)?.value()?;
        let Type::Primitive(primitive) = self.types()[self.resolve(ast[value].ty)] else {
            return Err(CompileError::unsupported(format!("formatting `{}`", self.display(ast[value].ty))));
        };

        let runtime = self.compiler.runtime;
        // Booleans are formatted as strings.
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
            PrimitiveType::String | PrimitiveType::Bool => (runtime.str_format_str, compiled),
            _ => return Err(CompileError::unsupported(format!("formatting `{primitive}`"))),
        };

        Ok(MolValue::Value(self.call(func, &[arg, spec])[0]))
    }

    /// Compiles `expr as primitive`.
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
            (PrimitiveType::Int(_), PrimitiveType::F32) => ins.fcvt_from_sint(to_ir, value),
            (PrimitiveType::UInt(_) | PrimitiveType::Bool, PrimitiveType::F32) => ins.fcvt_from_uint(to_ir, value),
            // Out-of-range and NaN values saturate instead of trapping.
            (PrimitiveType::F32, PrimitiveType::Int(_)) => ins.fcvt_to_sint_sat(to_ir, value),
            (PrimitiveType::F32, PrimitiveType::UInt(_)) => ins.fcvt_to_uint_sat(to_ir, value),
            _ => return Err(CompileError::unsupported(format!("casting `{from}` to `{to}`"))),
        };

        Ok(MolValue::Value(result))
    }
}
