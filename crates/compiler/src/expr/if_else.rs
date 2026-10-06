use cranelift::{codegen::ir, module::Module, prelude::InstBuilder};
use mollie_typed_ast::{BlockRef, ExprRef, TypedAST};
use mollie_typing::TypeRef;

use crate::{CompileTypedAST, MolValue, error::CompileResult, func::FunctionCompiler};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles `if condition { block } else otherwise`. Only `if`s with an
    /// `else` produce a value.
    pub fn compile_if(&mut self, ast: &TypedAST, expr: ExprRef, condition: ExprRef, block: BlockRef, otherwise: Option<ExprRef>) -> CompileResult<MolValue> {
        let result_type = if otherwise.is_some() { self.ir_type(ast[expr].ty)? } else { None };

        let then_block = self.fn_builder.create_block();
        let else_block = otherwise.map(|_| self.fn_builder.create_block());
        let after_block = self.fn_builder.create_block();

        let results = result_type.map_or_else(Vec::new, |ty| {
            ty.components()
                .into_iter()
                .map(|component| self.fn_builder.append_block_param(after_block, component))
                .collect()
        });

        // Variables bound by patterns in the condition are visible in the
        // `then` block.
        self.push_frame();

        let condition = condition.compile(ast, self)?.value()?;

        self.fn_builder.ins().brif(condition, then_block, &[], else_block.unwrap_or(after_block), &[]);

        self.fn_builder.switch_to_block(then_block);
        self.fn_builder.seal_block(then_block);

        let value = block.compile(ast, self)?;

        self.jump_with(after_block, ast[block].ty, ast[expr].ty, value, result_type.is_some())?;
        // Pattern bindings don't keep GC objects alive, so leaving the scope
        // emits no code.
        self.pop_frame();

        if let (Some(otherwise), Some(else_block)) = (otherwise, else_block) {
            self.fn_builder.switch_to_block(else_block);
            self.fn_builder.seal_block(else_block);

            let value = otherwise.compile(ast, self)?;

            self.jump_with(after_block, ast[otherwise].ty, ast[expr].ty, value, result_type.is_some())?;
        }

        self.fn_builder.switch_to_block(after_block);
        self.fn_builder.seal_block(after_block);

        MolValue::from_values(result_type, &results)
    }

    /// Jumps to `block`, passing `value` (converted to `to`) if `with_value`.
    fn jump_with(&mut self, block: ir::Block, from: TypeRef, to: TypeRef, value: MolValue, with_value: bool) -> CompileResult<()> {
        let args = if with_value {
            self.coerce(value, from, to)?.values().into_iter().map(ir::BlockArg::Value).collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        self.fn_builder.ins().jump(block, &args);

        Ok(())
    }
}
