mod array;
mod binary;
mod call;
mod cast;
mod closure;
mod construct;
mod field;
mod if_else;
mod literal;
mod pattern;
mod place;
mod r#while;

use cranelift::{codegen::ir::InstBuilder, module::Module};
use mollie_shared::UnaryOperator;
use mollie_typed_ast::{Expr, ExprRef, TypedAST};
use mollie_typing::{ModuleSpan, Type};

pub use self::r#while::LoopTarget;
use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> CompileTypedAST<M, MolValue> for ExprRef {
    fn compile(self, ast: &TypedAST, compiler: &mut FunctionCompiler<'_, M>) -> CompileResult<MolValue> {
        // Errors get the span of the innermost expression they come from.
        let value = compile_expr(self, ast, compiler).map_err(|error| error.or_at(ModuleSpan(ast.module, ast[self].span)))?;

        // Every GC reference produced by an expression must be found by the
        // garbage collector while it's live, including temporaries.
        compiler.track(ast[self].ty, &value);

        Ok(value)
    }
}

fn compile_expr<M: Module>(this: ExprRef, ast: &TypedAST, compiler: &mut FunctionCompiler<'_, M>) -> CompileResult<MolValue> {
    // Traps in the code of the expression are located at it (or at the inner
    // expression they come from).
    let outer = compiler.site.replace(ModuleSpan(ast.module, ast[this].span));
    let result = compile_expr_at(this, ast, compiler);

    compiler.site = outer;

    result
}

fn compile_expr_at<M: Module>(this: ExprRef, ast: &TypedAST, compiler: &mut FunctionCompiler<'_, M>) -> CompileResult<MolValue> {
    // Nested code is handled recursively: the stack grows if needed.
    mollie_shared::limits::grow_stack(move || {
        {
            match &ast[this].value {
                Expr::Lit(literal) => compiler.compile_literal(ast, this, literal),
                Expr::Var(name) => {
                    // Variables of type `void` have no value.
                    if compiler.ir_type(ast[this].ty)?.is_none() {
                        Ok(MolValue::Nothing)
                    } else {
                        compiler.read_var(name)
                    }
                }
                Expr::Array { elements, .. } => compiler.compile_array(ast, this, elements),
                &Expr::IfElse { condition, block, otherwise } => compiler.compile_if(ast, this, condition, block, otherwise),
                &Expr::While { condition, block, id } => compiler.compile_while(ast, id, condition, block),
                &Expr::Loop { block, id } => compiler.compile_loop(ast, this, id, block),
                &Expr::Break { id, value } => compiler.compile_break(ast, id, value, ast[this].ty),
                &Expr::Continue { id } => compiler.compile_continue(id, ast[this].ty),
                &Expr::Block(block) => block.compile(ast, compiler),
                &Expr::Unary { operator, expr } => {
                    let value = expr.compile(ast, compiler)?.value()?;
                    let expr_ty = ast[expr].ty;

                    Ok(MolValue::Value(match operator.value {
                        // Booleans are 0 or 1: `bnot` would make `!true` 0xFE,
                        // which is true too.
                        UnaryOperator::Not => compiler.fn_builder.ins().bxor_imm_u(value, 1),
                        UnaryOperator::Neg if matches!(compiler.type_context.tcx.types[expr_ty], Type::Primitive(primitive) if primitive.is_f32()) => {
                            compiler.fn_builder.ins().fneg(value)
                        }
                        UnaryOperator::Neg => compiler.fn_builder.ins().ineg(value),
                    }))
                }
                Expr::Binary { operator, lhs, rhs } => compiler.compile_binary(ast, operator.value, *lhs, *rhs),
                Expr::Closure { args, captures, body } => compiler.compile_closure(ast, this, args, captures, *body),
                Expr::Call { func, args } => compiler.compile_call(ast, this, *func, args),
                Expr::Construct { variant, fields, .. } => compiler.compile_construct(ast, this, *variant, fields),
                &Expr::AdtIndex { target, field } => compiler.compile_field_access(ast, target, field),
                Expr::VTableIndex {
                    target,
                    target_ty,
                    vtable,
                    func,
                    type_args,
                } => {
                    if target.is_some() {
                        return Err(CompileError::unsupported("method of a value used as a function value"));
                    }

                    // A function value can't give the changed receiver back.
                    if compiler.type_context.tcx.impl_registry.mut_self.contains(&(*vtable, *func)) {
                        return Err(CompileError::unsupported("`mut self` method used as a function value"));
                    }

                    let func_id = compiler.vfunc_id(*target_ty, *vtable, *func, type_args)?;

                    compiler.func_value(func_id, ast[this].ty)
                }
                &Expr::ArrayIndex { target, element } => compiler.compile_array_index(ast, this, target, element),
                Expr::TraitFunc { .. } | Expr::BoundFunc { .. } => Err(CompileError::unsupported("trait function used as a function value")),
                Expr::Func { func, type_args } => {
                    let func_id = compiler.func_id(*func, type_args)?;

                    compiler.func_value(func_id, ast[this].ty)
                }
                &Expr::TypeCast(expr, ty) => compiler.compile_cast(ast, expr, ty),
                &Expr::Format { value, spec } => compiler.compile_format(ast, value, spec),
                Expr::Unreachable => compiler.unreachable(ast[this].ty),
                &Expr::Const(constant) => {
                    let value = compiler.type_context.tcx.def_registry.constants[constant]
                        .value
                        .clone()
                        .ok_or_else(|| CompileError::unsupported("constant without a value"))?;

                    compiler.compile_constant(ast[this].ty, &value)
                }
                &Expr::Return(value) => compiler.compile_return(ast, value, ast[this].ty),
                &Expr::Panic(message) => compiler.compile_panic(ast, message, ast[this].ty),
                Expr::IsPattern { target, pattern } => compiler.compile_is_pattern(ast, *target, pattern),
                Expr::Error(_) => Err(CompileError::unsupported("expression with a type error")),
            }
        }
    })
}
