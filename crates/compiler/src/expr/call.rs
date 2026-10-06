use cranelift::{
    codegen::ir,
    module::{FuncId, Module},
    prelude::InstBuilder,
};
use mollie_index::Idx;
use mollie_ir::{MollieType, VTablePtr};
use mollie_typed_ast::{Expr, ExprRef, TypedAST};
use mollie_typing::{FuncRef, ImplRef, TraitFuncRef, Type, TypeRef, VFuncRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
    types,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiled instance of a function with type arguments `type_args`.
    pub fn func_id(&self, func: FuncRef, type_args: &[TypeRef]) -> CompileResult<FuncId> {
        let hash = types::instance_hash(&self.type_context.tcx, type_args, &self.generics);

        self.compiler.func_ref_to_func_id.get(&(func, hash)).copied().ok_or_else(|| {
            CompileError::unsupported(format!(
                "function `{}` wasn't compiled",
                self.type_context.tcx.def_registry.functions[func].name
            ))
        })
    }

    /// Compiled function of an impl block for `target_ty`. Functions with
    /// their own generics are compiled for each of their `type_args`.
    pub fn vfunc_id(&self, target_ty: TypeRef, vtable: ImplRef, func: VFuncRef, type_args: &[TypeRef]) -> CompileResult<FuncId> {
        let hash = self.hash(target_ty);
        let compiled = if self.type_context.tcx.impl_registry.impls[vtable].functions[func].generics == 0 {
            self.compiler.vtables.get(&(hash, vtable)).and_then(|vtable| vtable.get(&func)).copied()
        } else {
            let instance = types::instance_hash(&self.type_context.tcx, type_args, &self.generics);

            self.compiler.method_instances.get(&(hash, vtable, func, instance)).copied()
        };

        compiled.ok_or_else(|| CompileError::unsupported(format!("method of `{}` wasn't compiled", self.display(target_ty))))
    }

    /// Compiles arguments, converting them to the parameter types.
    fn compile_args(&mut self, ast: &TypedAST, args: &[ExprRef], params: &[TypeRef], values: &mut Vec<ir::Value>) -> CompileResult<()> {
        for (&arg, &param) in args.iter().zip(params) {
            let value = arg.compile(ast, self)?;
            let value = self.coerce(value, ast[arg].ty, param)?;

            values.extend(value.values());
        }

        Ok(())
    }

    /// Calls the function `trait_func` of the trait object `(data, vtable)`,
    /// with a pointer to the value as the receiver, and `args` converted to
    /// `params` (the first parameter is the receiver).
    #[allow(clippy::too_many_arguments)]
    fn call_through_vtable(
        &mut self,
        ast: &TypedAST,
        data: ir::Value,
        vtable: ir::Value,
        trait_func: TraitFuncRef,
        params: &[TypeRef],
        returns_type: Option<MollieType>,
        args: &[ExprRef],
    ) -> CompileResult<Vec<ir::Value>> {
        let index = u32::try_from(trait_func.index()).map_err(|_| CompileError::unsupported("too many trait functions"))?;
        let code = VTablePtr::get_func_ptr(self.compiler.codegen.module.isa(), &mut self.fn_builder, vtable, index);
        let ptr_type = self.ptr_type();
        let mut signature = self.compiler.codegen.module.make_signature();

        signature.params.push(ir::AbiParam::new(ptr_type));

        for &param in &params[1..] {
            if let Some(ty) = self.ir_type(param)? {
                ty.add_to_params(&mut signature.params);
            }
        }

        if let Some(ty) = returns_type {
            ty.add_to_params(&mut signature.returns);
        }

        let mut values = vec![data];

        self.compile_args(ast, args, &params[1..], &mut values)?;

        let signature = self.fn_builder.import_signature(signature);
        let call = self.fn_builder.ins().call_indirect(signature, code, &values);
        let results = self.fn_builder.inst_results(call).to_vec();

        self.check_trap();

        Ok(results)
    }

    /// Compiles `func(args...)`.
    pub fn compile_call(&mut self, ast: &TypedAST, _expr: ExprRef, func: ExprRef, args: &[ExprRef]) -> CompileResult<MolValue> {
        let func_ty = ast[func].ty;
        let Type::Func(params, returns) = self.types()[self.resolve(func_ty)].clone() else {
            return Err(CompileError::unsupported(format!("calling a value of `{}`", self.display(func_ty))));
        };

        let returns_type = self.ir_type(returns)?;
        let mut values = Vec::new();

        let results = match ast[func].value {
            // Calls of known functions are direct.
            Expr::Func { func: func_ref, ref type_args } => {
                let func_id = self.func_id(func_ref, type_args)?;

                self.compile_args(ast, args, &params, &mut values)?;
                self.call(func_id, &values)
            }
            Expr::VTableIndex {
                target,
                target_ty,
                vtable,
                func: vfunc,
                ref type_args,
            } => {
                let func_id = self.vfunc_id(target_ty, vtable, vfunc, type_args)?;
                // A `mut self` function returns its changed receiver after its
                // result, which goes back to the place of the receiver (a
                // temporary value just loses the change).
                let mut_self = self.type_context.tcx.impl_registry.mut_self.contains(&(vtable, vfunc));
                let mut write_back = None;
                let mut args = args;
                // The receiver of a method is its first parameter.
                let params = match target {
                    Some(target) => {
                        let receiver = if mut_self && let Some(place) = self.place(ast, target)? {
                            let ty = self.value_type(ast[target].ty)?;
                            let receiver = self.read_place(&place, ty)?;

                            write_back = Some((place, ty));

                            receiver
                        } else {
                            target.compile(ast, self)?
                        };
                        let receiver = self.coerce(receiver, ast[target].ty, params[0])?;

                        values.extend(receiver.values());

                        &params[1..]
                    }
                    // `Type::method(value)`: the first argument is the
                    // receiver, given back like the receiver of a method.
                    None if mut_self && !args.is_empty() => {
                        let first = args[0];
                        let receiver = if let Some(place) = self.place(ast, first)? {
                            let ty = self.value_type(ast[first].ty)?;
                            let receiver = self.read_place(&place, ty)?;

                            write_back = Some((place, ty));

                            receiver
                        } else {
                            first.compile(ast, self)?
                        };
                        let receiver = self.coerce(receiver, ast[first].ty, params[0])?;

                        values.extend(receiver.values());
                        args = &args[1..];

                        &params[1..]
                    }
                    None => &params[..],
                };

                self.compile_args(ast, args, params, &mut values)?;

                let mut results = self.call(func_id, &values);

                if mut_self {
                    let changed = results.split_off(returns_type.map_or(0, |ty| ty.components().len()));

                    if let Some((place, ty)) = write_back {
                        self.write_place(&place, ty, MolValue::Inline(changed))?;
                    }
                }

                results
            }
            // Functions of bounds are called on the impl for the type the
            // generic is instantiated with.
            Expr::BoundFunc {
                target,
                trait_ref,
                func: trait_func,
            } => {
                let target_ty = self.resolve(ast[target].ty);

                // A generic instantiated with a trait object of the bound's
                // trait calls through its vtable.
                if matches!(self.types()[target_ty], Type::Trait(object_trait, _) if object_trait == trait_ref) {
                    let MolValue::FatPtr(data, vtable) = target.compile(ast, self)? else {
                        return Err(CompileError::unsupported("trait method of a value that isn't a trait object"));
                    };

                    let results = self.call_through_vtable(ast, data, vtable, trait_func, &params, returns_type, args)?;

                    return MolValue::from_values(returns_type, &results);
                }

                let tcx = &self.type_context.tcx;
                let vtable = tcx.find_vtable(target_ty, Some(trait_ref)).ok_or_else(|| {
                    CompileError::unsupported(format!(
                        "`{}` doesn't implement `{}`",
                        tcx.display_of(target_ty),
                        tcx.def_registry.traits[trait_ref].name
                    ))
                })?;
                let vfunc = tcx.impl_registry.impls[vtable]
                    .functions
                    .iter()
                    .find(|(_, func)| func.trait_func == Some(trait_func))
                    .map(|(vfunc, _)| vfunc)
                    .ok_or_else(|| CompileError::unsupported("missing function of a trait impl"))?;
                let func_id = self.vfunc_id(target_ty, vtable, vfunc, &[])?;
                let receiver = target.compile(ast, self)?;
                let receiver = self.coerce(receiver, ast[target].ty, params[0])?;

                values.extend(receiver.values());

                self.compile_args(ast, args, &params[1..], &mut values)?;

                let mut results = self.call(func_id, &values);

                // Through a bound, a `mut self` function changes a copy of the
                // receiver (like through a trait object, it changes the boxed
                // value): the changed receiver it returns isn't used.
                if self.type_context.tcx.impl_registry.mut_self.contains(&(vtable, vfunc)) {
                    results.truncate(returns_type.map_or(0, |ty| ty.components().len()));
                }

                results
            }
            // Methods of trait objects are called through the vtable, with a pointer to the value as the receiver.
            Expr::TraitFunc { target, func: trait_func, .. } => {
                let MolValue::FatPtr(data, vtable) = target.compile(ast, self)? else {
                    return Err(CompileError::unsupported("trait method of a value that isn't a trait object"));
                };

                self.call_through_vtable(ast, data, vtable, trait_func, &params, returns_type, args)?
            }
            // Function values take their environment as the last argument.
            _ => {
                let MolValue::FatPtr(code, env) = func.compile(ast, self)? else {
                    return Err(CompileError::unsupported("function value isn't a code and environment pair"));
                };

                self.compile_args(ast, args, &params, &mut values)?;
                values.push(env);

                let signature = self.signature(func_ty, true)?;
                let signature = self.fn_builder.import_signature(signature);
                let call = self.fn_builder.ins().call_indirect(signature, code, &values);
                let results = self.fn_builder.inst_results(call).to_vec();

                self.check_trap();

                results
            }
        };

        MolValue::from_values(returns_type, &results)
    }
}
