use cranelift::{
    codegen::ir,
    module::{FuncId, Module},
    prelude::InstBuilder,
};
use mollie_index::Idx;
use mollie_ir::{MollieType, VTablePtr};
use mollie_typed_ast::{Expr, ExprRef, FunctionBody, TypedAST};
use mollie_typing::{ArrayKind, FuncRef, ImplRef, TraitFuncRef, Type, TypeRef, VFuncRef};

use crate::{
    CompileTypedAST, MolValue,
    allocator::TypeLayoutField,
    error::{CompileError, CompileResult},
    func::{ARRAY_CAPACITY_OFFSET, ARRAY_DATA_OFFSET, ARRAY_LENGTH_OFFSET, FunctionCompiler},
    types,
};

use super::place::Place;

#[allow(clippy::cast_possible_truncation, reason = "a pointer is at most 8 bytes")]
const POINTER_BYTES: u32 = size_of::<usize>() as u32;
const ARRAY_HEADER_SIZE: u32 = 3 * POINTER_BYTES;

pub struct ViewArg {
    write_back: Option<(Place, MollieType)>,
    elements: ir::Value,
    size: u32,
    registered: bool,
}

impl<M: Module> FunctionCompiler<'_, M> {
    pub fn func_id(&self, func: FuncRef, type_args: &[TypeRef]) -> CompileResult<FuncId> {
        let hash = types::instance_hash(&self.type_context.tcx, type_args, &self.generics);

        self.compiler.func_ref_to_func_id.get(&(func, hash)).copied().ok_or_else(|| {
            CompileError::unsupported(format!(
                "function `{}` wasn't compiled",
                self.type_context.tcx.def_registry.functions[func].name
            ))
        })
    }

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

    fn compile_args(
        &mut self,
        ast: &TypedAST,
        args: &[ExprRef],
        params: &[TypeRef],
        values: &mut Vec<ir::Value>,
        views: &mut Vec<ViewArg>,
    ) -> CompileResult<()> {
        for (&arg, &param) in args.iter().zip(params) {
            let value = self.compile_arg(ast, arg, param, views)?;

            values.extend(value.values());
        }

        Ok(())
    }

    fn compile_arg(&mut self, ast: &TypedAST, arg: ExprRef, param: TypeRef, views: &mut Vec<ViewArg>) -> CompileResult<MolValue> {
        let Type::Array(element, ArrayKind::View { writable }) = self.types()[self.resolve(param)] else {
            let value = arg.compile(ast, self)?;

            return self.coerce(value, ast[arg].ty, param);
        };

        let arg_ty = ast[arg].ty;
        let Some(length) = self.fixed_length(arg_ty) else {
            // Growable arrays and views are views already.
            return arg.compile(ast, self);
        };

        let Some(MollieType::Inline { size, align }) = self.ir_type(arg_ty)? else {
            return Err(CompileError::unsupported(format!("`{}` isn't a fixed array", self.display(arg_ty))));
        };

        // A writable view changes the array in its place.
        let place = if writable { self.place(ast, arg)? } else { None };
        let ty = MollieType::Inline { size, align };
        let value = match &place {
            Some(place) => self.read_place(place, ty)?,
            None => arg.compile(ast, self)?,
        };

        let elements_offset = ARRAY_HEADER_SIZE.next_multiple_of(align.max(1));
        let header = self.stack_slot(elements_offset + size, align.max(POINTER_BYTES));
        let elements = self.fn_builder.ins().iadd_imm_u(header, i64::from(elements_offset));
        let length = self.ptr_const(length);

        self.store_value(ty, &value, elements, 0)?;
        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), length, header, ARRAY_LENGTH_OFFSET);
        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), length, header, ARRAY_CAPACITY_OFFSET);
        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), elements, header, ARRAY_DATA_OFFSET);

        // References only the copy may hold (new objects the function stores,
        // or those of a temporary array) are roots while it's viewed.
        let references = types::layout_fields(&self.type_context.tcx, &self.compiler.adt_types, element, &self.generics)?;
        let registered = references.iter().any(|&(.., kind)| kind != TypeLayoutField::Regular);

        if registered {
            let layout = self.element_layout(element)?;
            let layout = self.ptr_const(std::ptr::from_ref(layout).addr());
            let push = self.compiler.runtime.stack_array_push;

            self.call_unchecked(push, &[header, layout]);
        }

        views.push(ViewArg {
            write_back: place.map(|place| (place, ty)),
            elements,
            size,
            registered,
        });

        Ok(MolValue::Value(header))
    }

    fn end_views(&mut self, views: Vec<ViewArg>) -> CompileResult<()> {
        for view in views.into_iter().rev() {
            if view.registered {
                let pop = self.compiler.runtime.stack_array_pop;

                self.call_unchecked(pop, &[]);
            }

            if let Some((place, ty)) = view.write_back {
                let values = self.load_inline(view.size, view.elements, 0);

                self.write_place(&place, ty, MolValue::Inline(values))?;
            }
        }

        Ok(())
    }

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
        let mut views = Vec::new();

        self.compile_args(ast, args, &params[1..], &mut values, &mut views)?;

        let signature = self.fn_builder.import_signature(signature);
        let call = self.fn_builder.ins().call_indirect(signature, code, &values);
        let results = self.fn_builder.inst_results(call).to_vec();

        self.check_trap();
        self.end_views(views)?;

        Ok(results)
    }

    pub fn compile_call(&mut self, ast: &TypedAST, _expr: ExprRef, func: ExprRef, args: &[ExprRef]) -> CompileResult<MolValue> {
        let func_ty = ast[func].ty;
        let Type::Func(params, returns) = self.types()[self.resolve(func_ty)].clone() else {
            return Err(CompileError::unsupported(format!("calling a value of `{}`", self.display(func_ty))));
        };

        let returns_type = self.ir_type(returns)?;
        let mut values = Vec::new();
        let mut views = Vec::new();

        let results = match ast[func].value {
            Expr::Func { func: func_ref, ref type_args } => {
                let func_id = self.func_id(func_ref, type_args)?;

                self.compile_args(ast, args, &params, &mut values, &mut views)?;
                self.call(func_id, &values)
            }
            Expr::VTableIndex {
                target,
                target_ty,
                vtable,
                func: vfunc,
                ref type_args,
            } => {
                if let Some(target) = target
                    && let Some(length) = self.fixed_length(ast[target].ty)
                    && matches!(
                        self.type_context.vtables.get(&vtable).and_then(|functions| functions.get(&vfunc)),
                        Some(FunctionBody::BuiltIn("array_len"))
                    )
                {
                    target.compile(ast, self)?;

                    return Ok(MolValue::Value(self.ptr_const(length)));
                }

                let func_id = self.vfunc_id(target_ty, vtable, vfunc, type_args)?;
                // A `mut self` function returns its changed receiver after its
                // result, which goes back to the place of the receiver (a
                // temporary value just loses the change).
                let mut_self = self.type_context.tcx.impl_registry.mut_self.contains(&(vtable, vfunc));
                let mut write_back = None;
                let mut args = args;
                let params = match target {
                    Some(target) => {
                        let receiver = if mut_self && let Some(place) = self.place(ast, target)? {
                            let ty = self.value_type(ast[target].ty)?;
                            let receiver = self.read_place(&place, ty)?;

                            write_back = Some((place, ty));

                            self.coerce(receiver, ast[target].ty, params[0])?
                        } else {
                            // Fixed arrays are viewed by functions of views.
                            self.compile_arg(ast, target, params[0], &mut views)?
                        };

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

                self.compile_args(ast, args, params, &mut values, &mut views)?;

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

                self.compile_args(ast, args, &params[1..], &mut values, &mut views)?;

                let mut results = self.call(func_id, &values);

                // Through a bound, a `mut self` function changes a copy of the
                // receiver (like through a trait object, it changes the boxed
                // value): the changed receiver it returns isn't used.
                if self.type_context.tcx.impl_registry.mut_self.contains(&(vtable, vfunc)) {
                    results.truncate(returns_type.map_or(0, |ty| ty.components().len()));
                }

                results
            }
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

                self.compile_args(ast, args, &params, &mut values, &mut views)?;
                values.push(env);

                let signature = self.signature(func_ty, true)?;
                let signature = self.fn_builder.import_signature(signature);
                let call = self.fn_builder.ins().call_indirect(signature, code, &values);
                let results = self.fn_builder.inst_results(call).to_vec();

                self.check_trap();

                results
            }
        };

        self.end_views(views)?;

        MolValue::from_values(returns_type, &results)
    }
}
