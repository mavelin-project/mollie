use cranelift::{codegen::ir, module::Module, prelude::InstBuilder};
use mollie_typed_ast::{BlockRef, ExprRef, LoopId, TypedAST};
use mollie_typing::TypeRef;

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

pub struct LoopTarget {
    id: LoopId,
    latch: ir::Block,
    after: ir::Block,
    ty: TypeRef,
}

impl<M: Module> FunctionCompiler<'_, M> {
    pub fn compile_while(&mut self, ast: &TypedAST, id: LoopId, condition: ExprRef, block: BlockRef) -> CompileResult<MolValue> {
        let header_block = self.fn_builder.create_block();
        let body_block = self.fn_builder.create_block();
        let latch_block = self.fn_builder.create_block();
        let after_block = self.fn_builder.create_block();

        self.fn_builder.ins().jump(header_block, &[]);
        self.fn_builder.switch_to_block(header_block);

        // Variables bound by patterns in the condition are visible in the body.
        self.push_frame();

        let condition = condition.compile(ast, self)?.value()?;

        self.fn_builder.ins().brif(condition, body_block, &[], after_block, &[]);
        self.fn_builder.switch_to_block(body_block);
        self.fn_builder.seal_block(body_block);

        let void = self.types().core_types.void;

        self.loops.push(LoopTarget {
            id,
            latch: latch_block,
            after: after_block,
            ty: void,
        });

        block.compile(ast, self)?;

        self.loops.pop();
        self.fn_builder.ins().jump(latch_block, &[]);
        // Pattern bindings don't keep GC objects alive, so leaving the scope
        // emits no code.
        self.pop_frame();
        self.compile_latch(latch_block, header_block);

        self.fn_builder.switch_to_block(after_block);
        self.fn_builder.seal_block(after_block);

        Ok(MolValue::Nothing)
    }

    pub fn compile_loop(&mut self, ast: &TypedAST, expr: ExprRef, id: LoopId, block: BlockRef) -> CompileResult<MolValue> {
        let ty = ast[expr].ty;
        let result_type = self.ir_type(ty)?;
        let body_block = self.fn_builder.create_block();
        let latch_block = self.fn_builder.create_block();
        let after_block = self.fn_builder.create_block();
        let results = result_type.map_or_else(Vec::new, |ty| {
            ty.components()
                .into_iter()
                .map(|component| self.fn_builder.append_block_param(after_block, component))
                .collect()
        });

        self.fn_builder.ins().jump(body_block, &[]);
        self.fn_builder.switch_to_block(body_block);
        self.loops.push(LoopTarget {
            id,
            latch: latch_block,
            after: after_block,
            ty,
        });
        self.push_frame();

        block.compile(ast, self)?;

        self.pop_frame();
        self.loops.pop();
        self.fn_builder.ins().jump(latch_block, &[]);
        self.compile_latch(latch_block, body_block);

        self.fn_builder.switch_to_block(after_block);
        self.fn_builder.seal_block(after_block);

        MolValue::from_values(result_type, &results)
    }

    fn compile_latch(&mut self, latch: ir::Block, start: ir::Block) {
        self.fn_builder.switch_to_block(latch);
        self.fn_builder.seal_block(latch);
        self.consume_fuel();
        self.fn_builder.ins().jump(start, &[]);
        self.fn_builder.seal_block(start);
    }

    fn loop_target(&self, id: LoopId) -> CompileResult<&LoopTarget> {
        self.loops
            .iter()
            .rev()
            .find(|target| target.id == id)
            .ok_or_else(|| CompileError::unsupported("`break` or `continue` outside of its loop"))
    }

    pub fn compile_break(&mut self, ast: &TypedAST, id: LoopId, value: Option<ExprRef>, ty: TypeRef) -> CompileResult<MolValue> {
        let (after, loop_ty) = {
            let target = self.loop_target(id)?;

            (target.after, target.ty)
        };
        let args = match value {
            Some(value) => {
                let compiled = value.compile(ast, self)?;

                self.coerce(compiled, ast[value].ty, loop_ty)?
                    .values()
                    .into_iter()
                    .map(ir::BlockArg::Value)
                    .collect::<Vec<_>>()
            }
            None => Vec::new(),
        };

        self.fn_builder.ins().jump(after, &args);
        self.after_jump(ty)
    }

    pub fn compile_continue(&mut self, id: LoopId, ty: TypeRef) -> CompileResult<MolValue> {
        let latch = self.loop_target(id)?.latch;

        self.fn_builder.ins().jump(latch, &[]);
        self.after_jump(ty)
    }

    fn after_jump(&mut self, ty: TypeRef) -> CompileResult<MolValue> {
        let block = self.fn_builder.create_block();

        self.fn_builder.switch_to_block(block);
        self.fn_builder.seal_block(block);
        self.zero_value(ty)
    }
}
