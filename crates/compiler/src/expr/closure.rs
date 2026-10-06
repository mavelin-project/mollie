use cranelift::{
    module::Module,
    prelude::{FunctionBuilderContext, InstBuilder},
};
use mollie_index::Idx;
use mollie_ir::Struct;
use mollie_typed_ast::{BlockRef, ExprRef, TypedAST};
use mollie_typing::{AdtKind, AdtVariantRef, Arg, Type, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    allocator::TypeLayout,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
    types,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles a closure into a function value: a pointer to its code and a
    /// pointer to its environment (copies of the captured variables), or null
    /// if it captures nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_closure(
        &mut self,
        ast: &TypedAST,
        expr: ExprRef,
        args: &[Arg<TypeRef>],
        captures: &[(String, TypeRef)],
        body: BlockRef,
    ) -> CompileResult<MolValue> {
        let func_ty = ast[expr].ty;
        let Type::Func(_, returns) = self.types()[self.resolve(func_ty)] else {
            return Err(CompileError::unsupported(format!("closure of `{}`", self.display(func_ty))));
        };

        let capture_types = captures
            .iter()
            .map(|(_, ty)| Ok((self.value_type(*ty)?, None)))
            .collect::<CompileResult<Vec<_>>>()?;
        let environment = Struct::new(capture_types);
        let signature = self.signature(func_ty, true)?;
        let id = self.compiler.codegen.module.declare_anonymous_function(&signature)?;
        let mut ctx = self.compiler.codegen.module.make_context();
        let mut fn_builder_ctx = FunctionBuilderContext::new();

        {
            let mut closure = FunctionCompiler::new(
                id,
                "closure",
                signature,
                &mut *self.compiler,
                self.type_context,
                &mut ctx,
                &mut fn_builder_ctx,
                self.generics.clone(),
            );

            closure.return_ty = Some(returns);

            let env_param = closure.bind_params(0, args.iter().map(|arg| (arg.name.as_str(), arg.ty)))?;
            let env = closure.fn_builder.block_params(closure.entry_block)[env_param];

            // The environment keeps captured values alive while the closure is
            // alive.
            for ((name, ty), field) in captures.iter().zip(&environment.fields) {
                let value = closure.load_value(field.ty, env, field.offset);

                closure.declare_binding(name.clone(), *ty, value)?;
            }

            closure.consume_fuel();

            let returned = body.compile(ast, &mut closure)?;
            let returned = closure.coerce(returned, ast[body].ty, returns)?;

            closure.pop_all_frames();
            closure.return_(&returned);
            closure.finalize();
        }

        self.compiler.define_function(id, &mut ctx)?;

        let env = if captures.is_empty() {
            self.ptr_const(0)
        } else {
            let mut fields = Vec::new();

            for ((_, ty), field) in captures.iter().zip(&environment.fields) {
                for (offset, ty, kind) in types::layout_fields(&self.type_context.tcx, &self.compiler.adt_types, *ty, &self.generics)? {
                    fields.push((AdtVariantRef::ZERO, field.offset.cast_unsigned() + offset, ty, kind));
                }
            }

            let fields = self.compiler.heap.intern_fields(fields);
            let layout = self.compiler.heap.intern_layout(TypeLayout {
                fields,
                adt_ty: None,
                size: environment.size as usize,
                align: environment.align as usize,
                kind: Some(AdtKind::Struct),
            });

            let env = self.alloc(layout);

            for ((name, _), field) in captures.iter().zip(&environment.fields) {
                let value = self.read_var(name)?;

                self.store_value(field.ty, &value, env, field.offset)?;
            }

            env
        };

        let func_ref = self.func_ref(id);
        let ptr_type = self.ptr_type();
        let code = self.fn_builder.ins().func_addr(ptr_type, func_ref);

        Ok(MolValue::FatPtr(code, env))
    }
}
