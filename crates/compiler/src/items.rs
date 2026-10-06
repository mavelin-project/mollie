//! Compiling items used by a program: ADT layouts, functions and impl blocks.
//!
//! Items are compiled in phases, so that functions can call each other
//! regardless of the order they're compiled in:
//!
//! 1. layouts of ADTs;
//! 2. declarations of functions and functions of impl blocks;
//! 3. vtables of trait impls (static data pointing to the functions);
//! 4. bodies of functions.

use cranelift::{
    codegen::ir,
    jit::JITModule,
    module::{FuncId, Linkage, Module},
    prelude::{FunctionBuilder, FunctionBuilderContext, InstBuilder, IntCC},
};
use indexmap::IndexMap;
use mollie_index::{Idx, IndexBoxedSlice};
use mollie_ir::{MollieType, Struct};
use mollie_typed_ast::{FunctionBody, ModuleLoader, UsedItem};
use mollie_typing::{AdtRef, AdtVariantRef, FuncRef, ImplRef, Type, TypeRef, VFuncRef};

use crate::{
    CompileTypedAST, CompiledAdt, CompiledAdtVariant, FuncCompiler, MethodKey, MolValue,
    allocator::TypeLayout,
    error::{CompileError, CompileResult},
    func::{ARRAY_DATA_OFFSET, ARRAY_LENGTH_OFFSET, FunctionCompiler},
    sandbox::{self, TrapKind},
    types,
};

enum Job {
    Func(FuncRef, Box<[TypeRef]>),
    VTable { hash: u64, impl_ref: ImplRef, args: Box<[TypeRef]> },
    Method(MethodKey, Box<[TypeRef]>),
}

/// What a C wrapper of the host calls (see `FuncCompiler::c_wrapper`).
enum Callee {
    /// A compiled function.
    Direct(FuncId),
    /// A function of a vtable, given with the value as the first arguments of
    /// the wrapper, and called with the value first.
    Method,
    /// A function value with this signature, given with its environment as
    /// the first arguments of the wrapper, and called with it last.
    Closure(ir::Signature),
}

impl<ML: ModuleLoader> FuncCompiler<'_, ML, JITModule> {
    pub(crate) fn compile_used_items(&mut self, items: &[UsedItem]) -> CompileResult<()> {
        for item in items {
            if let UsedItem::Adt(adt_ref, args) = item {
                self.compile_adt(*adt_ref, args)?;
            }
        }

        let mut jobs = Vec::new();

        for item in items {
            match item {
                UsedItem::Func(func_ref, args) => {
                    // Functions used inside generic code are compiled for
                    // each instantiation instead.
                    let tcx = &self.type_context.tcx;

                    if !args.iter().all(|&arg| types::is_concrete(tcx, arg)) {
                        continue;
                    }

                    let key = (*func_ref, types::instance_hash(tcx, args, &[]));

                    if !self.compiler.func_ref_to_func_id.contains_key(&key) {
                        self.declare_func(*func_ref, args)?;

                        jobs.push(Job::Func(*func_ref, args.clone()));
                    }
                }
                UsedItem::VTable(ty, impl_ref, args) => {
                    // Impls used inside generic code are compiled for each
                    // instantiation instead.
                    let tcx = &self.type_context.tcx;

                    if !types::is_concrete(tcx, *ty) || !args.iter().all(|&arg| types::is_concrete(tcx, arg)) {
                        continue;
                    }

                    let hash = tcx.types.hash_of(*ty);

                    if !self.compiler.vtables.contains_key(&(hash, *impl_ref)) {
                        self.declare_vtable(hash, *impl_ref, args)?;

                        jobs.push(Job::VTable {
                            hash,
                            impl_ref: *impl_ref,
                            args: args.clone(),
                        });
                    }
                }
                UsedItem::Method(ty, impl_ref, func, args) => {
                    let tcx = &self.type_context.tcx;

                    if !types::is_concrete(tcx, *ty) || !args.iter().all(|&arg| types::is_concrete(tcx, arg)) {
                        continue;
                    }

                    // Type arguments of the impl, then of the function.
                    let impl_generics = tcx.impl_registry.impls[*impl_ref].generics.len();
                    let key = (
                        tcx.types.hash_of(*ty),
                        *impl_ref,
                        *func,
                        types::instance_hash(tcx, args.get(impl_generics..).unwrap_or_default(), &[]),
                    );

                    if !self.compiler.method_instances.contains_key(&key) {
                        self.declare_method(key, args)?;

                        jobs.push(Job::Method(key, args.clone()));
                    }
                }
                // ADTs are compiled above, and bound impls are resolved into
                // `VTable` items by the typed AST.
                UsedItem::Adt(..) | UsedItem::BoundImpl(..) => {}
            }
        }

        for job in &jobs {
            if let Job::VTable { hash, impl_ref, args } = job {
                self.define_vtable_data(*hash, *impl_ref, args)?;
            }
        }

        for job in jobs {
            match job {
                Job::Func(func_ref, args) => self.define_func(func_ref, &args)?,
                Job::VTable { hash, impl_ref, args } => self.define_vtable(hash, impl_ref, &args)?,
                Job::Method(key, args) => self.define_method(key, &args)?,
            }
        }

        Ok(())
    }

    fn compile_adt(&mut self, adt_ref: AdtRef, args: &[TypeRef]) -> CompileResult<()> {
        let tcx = &mut self.type_context.tcx;

        if args.len() != tcx.def_registry.adt_types[adt_ref].generics || !args.iter().all(|&arg| types::is_concrete(tcx, arg)) {
            return Ok(());
        }

        let ty = tcx.types.get_or_add(Type::Adt(adt_ref, args.into()));
        let hash = tcx.types.hash_of(ty);

        if self.compiler.adt_types.contains_key(&hash) {
            return Ok(());
        }

        // Types of fields with type arguments applied.

        let variants = tcx.def_registry.adt_types[adt_ref]
            .variants
            .values()
            .map(|variant| variant.fields.values().map(|field| (field.ty, field.default_value.clone())).collect::<Vec<_>>())
            .map(|fields| {
                fields
                    .into_iter()
                    .map(|(field_ty, default_value)| (tcx.types.apply_type_args(field_ty, args), default_value))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        // Fields of value types are stored inline, so their layouts come
        // first.
        let nested = variants
            .iter()
            .flatten()
            .filter_map(|&(field_ty, _)| match &tcx.types[field_ty] {
                Type::Adt(nested, nested_args) if tcx.is_value_type(field_ty) => Some((*nested, nested_args.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();

        for (nested, nested_args) in nested {
            self.compile_adt(nested, &nested_args)?;
        }

        let tcx = &self.type_context.tcx;
        let adts = &self.compiler.adt_types;
        let isa = self.compiler.codegen.module.isa();
        let mut size = 0;
        let mut align = 1;
        let mut gc_fields = Vec::new();
        let mut compiled_variants = Vec::with_capacity(variants.len());

        for (variant_index, fields) in variants.into_iter().enumerate() {
            let mut ir_types = Vec::with_capacity(fields.len());

            for (field_ty, default_value) in &fields {
                let ir_type = types::ir_type(tcx, adts, *field_ty, &[], isa)?
                    .ok_or_else(|| CompileError::unsupported(format!("`void` field in `{}`", tcx.display_of(ty))))?;

                ir_types.push((ir_type, default_value.clone()));
            }

            let layout = Struct::new(ir_types);

            size = size.max(layout.size);
            align = align.max(layout.align);

            for (field, (field_ty, _)) in layout.fields.iter().zip(&fields) {
                for (offset, ty, kind) in types::layout_fields(tcx, adts, *field_ty, &[])? {
                    gc_fields.push((AdtVariantRef::new(variant_index), field.offset.cast_unsigned() + offset, ty, kind));
                }
            }

            compiled_variants.push(CompiledAdtVariant {
                fields: layout
                    .fields
                    .into_iter()
                    .zip(fields)
                    .map(|(field, (field_ty, _))| (field, field_ty))
                    .collect::<IndexBoxedSlice<_, _>>(),
            });
        }

        let adt = &tcx.def_registry.adt_types[adt_ref];

        tracing::info!(target: "mollie-compiler/adt_types", size, align, "Compiled `{}`", tcx.display_of(ty));

        let fields = self.compiler.heap.intern_fields(gc_fields);
        let type_layout = self.compiler.heap.intern_layout(TypeLayout {
            fields,
            adt_ty: Some(hash),
            size: size as usize,
            align: align as usize,
            kind: Some(adt.kind),
        });

        self.compiler.adt_types.insert(hash, CompiledAdt {
            type_layout,
            name: adt.name.clone(),
            applied_generics: args.len(),
            variants: compiled_variants.into_iter().collect(),
        });

        Ok(())
    }

    /// A function for the host to call the program `target` (of type
    /// `func_ty`) through, with the C calling convention:
    /// `extern "C" fn(result: *mut R, args...)`. Values of value types are
    /// passed by pointer (C passes small structs in other registers than
    /// compiled code does), and the result is always written through
    /// `result`, in its memory layout.
    pub(crate) fn entry_wrapper(&mut self, target: FuncId, func_ty: TypeRef) -> CompileResult<FuncId> {
        let Type::Func(params, returns) = self.type_context.tcx.types[func_ty].clone() else {
            return Err(CompileError::unsupported("program without a function type"));
        };

        self.c_wrapper(Callee::Direct(target), &params, returns)
    }

    /// A function for the host to call functions of vtables through (taking
    /// the value, then `params`, and returning `returns`), like
    /// [`Self::entry_wrapper`]:
    /// `extern "C" fn(result: *mut R, code: usize, data: *mut u8, args...)`.
    /// Returns its address.
    ///
    /// # Errors
    ///
    /// Returns an error if a type can't be passed or the function can't be
    /// compiled.
    pub fn method_entry(&mut self, params: &[TypeRef], returns: TypeRef) -> CompileResult<usize> {
        // The trait is registered before any program uses its value types:
        // their layouts come first.
        for &ty in params.iter().chain([&returns]) {
            self.compile_value_types(ty, &[])?;
        }

        let id = self.c_wrapper(Callee::Method, params, returns)?;

        self.finish_entry(id)
    }

    /// A function for the host to call function values of type `func_ty`
    /// (closures) through, like [`Self::entry_wrapper`]:
    /// `extern "C" fn(result: *mut R, code: usize, env: *mut u8, args...)`.
    /// Returns its address.
    ///
    /// # Errors
    ///
    /// Returns an error if `func_ty` isn't a function type, a type can't be
    /// passed or the function can't be compiled.
    pub fn callback_entry(&mut self, func_ty: TypeRef) -> CompileResult<usize> {
        let Type::Func(params, returns) = self.type_context.tcx.types[func_ty].clone() else {
            return Err(CompileError::unsupported("callback without a function type"));
        };

        self.compile_value_types(func_ty, &[])?;

        // The convention of calls of function values in compiled code.
        let signature = types::signature(
            &self.type_context.tcx,
            &self.compiler.adt_types,
            func_ty,
            &[],
            self.compiler.isa(),
            self.compiler.codegen.module.make_signature(),
            true,
        )?;
        let id = self.c_wrapper(Callee::Closure(signature), &params, returns)?;

        self.finish_entry(id)
    }

    /// Finalizes the entry `id` and returns its address.
    fn finish_entry(&mut self, id: FuncId) -> CompileResult<usize> {
        self.compiler.codegen.module.finalize_definitions()?;
        self.compiler.register_pending_code();

        Ok(self.compiler.codegen.module.get_finalized_function(id).addr())
    }

    /// The C wrapper of [`Self::entry_wrapper`], [`Self::method_entry`] or
    /// [`Self::callback_entry`], calling `callee`.
    fn c_wrapper(&mut self, callee: Callee, params: &[TypeRef], returns: TypeRef) -> CompileResult<FuncId> {
        let tcx = &self.type_context.tcx;
        let adts = &self.compiler.adt_types;
        let isa = self.compiler.isa();
        let ptr_type = isa.pointer_type();
        let params = params
            .iter()
            .map(|&param| types::ir_type(tcx, adts, param, &[], isa))
            .collect::<CompileResult<Vec<_>>>()?;
        let returns = types::ir_type(tcx, adts, returns, &[], isa)?;
        let mut signature = self.compiler.codegen.module.make_signature();

        signature.params.push(ir::AbiParam::new(ptr_type));

        // Indirect calls take the code and the value (or environment) first,
        // and call the code with the convention of compiled code.
        let (target, mut indirect_signature, env_last) = match callee {
            Callee::Direct(target) => (Some(target), None, false),
            Callee::Method => {
                // The value first, then arguments in chunks.
                let mut method_signature = self.compiler.codegen.module.make_signature();

                method_signature.params.push(ir::AbiParam::new(ptr_type));

                for param in params.iter().flatten() {
                    param.add_to_params(&mut method_signature.params);
                }

                if let Some(returns) = &returns {
                    returns.add_to_params(&mut method_signature.returns);
                }

                (None, Some(method_signature), false)
            }
            Callee::Closure(closure_signature) => (None, Some(closure_signature), true),
        };

        if target.is_none() {
            signature.params.push(ir::AbiParam::new(ptr_type));
            signature.params.push(ir::AbiParam::new(ptr_type));
        }

        for param in params.iter().flatten() {
            match param {
                MollieType::Inline { .. } => signature.params.push(ir::AbiParam::new(ptr_type)),
                param => param.add_to_params(&mut signature.params),
            }
        }

        let module = &mut self.compiler.codegen.module;
        let id = module.declare_anonymous_function(&signature)?;
        let mut ctx = module.make_context();
        let mut fn_builder_ctx = FunctionBuilderContext::new();
        // Memory of the host is aligned for its values, which may be less than
        // the size of a chunk.
        let flags = ir::MemFlagsData::new().with_notrap();

        ctx.func.signature = signature;

        {
            let mut fn_builder = FunctionBuilder::new(&mut ctx.func, &mut fn_builder_ctx);
            let entry_block = fn_builder.create_block();

            fn_builder.append_block_params_for_function_params(entry_block);
            fn_builder.switch_to_block(entry_block);
            fn_builder.seal_block(entry_block);

            let mut block_params = fn_builder.block_params(entry_block).to_vec().into_iter();
            let result = block_params.next().ok_or_else(|| CompileError::unsupported("entry without a result pointer"))?;
            let (code, extra) = if target.is_none() {
                (block_params.next(), block_params.next())
            } else {
                (None, None)
            };
            let mut args = Vec::new();

            if !env_last {
                args.extend(extra);
            }

            for param in params.iter().flatten() {
                match *param {
                    MollieType::Inline { size, .. } => {
                        let pointer = block_params.next().ok_or_else(|| CompileError::unsupported("missing argument"))?;

                        for (ty, offset) in mollie_ir::chunks(size) {
                            args.push(fn_builder.ins().load(ty, flags, pointer, offset.cast_signed()));
                        }
                    }
                    _ => args.extend(block_params.by_ref().take(param.components().len())),
                }
            }

            if env_last {
                args.extend(extra);
            }

            let call = match (target, code, indirect_signature.take()) {
                (Some(target), ..) => {
                    let callee = module.declare_func_in_func(target, fn_builder.func);

                    fn_builder.ins().call(callee, &args)
                }
                (None, Some(code), Some(indirect_signature)) => {
                    let indirect_signature = fn_builder.import_signature(indirect_signature);

                    fn_builder.ins().call_indirect(indirect_signature, code, &args)
                }
                _ => return Err(CompileError::unsupported("indirect entry without its code")),
            };
            let results = fn_builder.inst_results(call).to_vec();

            match returns {
                Some(MollieType::Inline { size, .. }) => {
                    for ((_, offset), value) in mollie_ir::chunks(size).into_iter().zip(results) {
                        fn_builder.ins().store(flags, value, result, offset.cast_signed());
                    }
                }
                Some(ty) => {
                    let mut offset = 0;

                    for (component, value) in ty.components().into_iter().zip(results) {
                        fn_builder.ins().store(flags, value, result, offset);

                        offset += component.bytes().cast_signed();
                    }
                }
                None => (),
            }

            fn_builder.ins().return_(&[]);
            fn_builder.finalize(module.isa().frontend_config());
        }

        self.compiler.define_function(id, &mut ctx)?;

        Ok(id)
    }

    /// A function calling the host function `code` with `context`: it takes
    /// arguments like other functions of type `func_ty` (with `args` for its
    /// generics), passes values of value types as pointers to copies, and
    /// reads the result from the slot the host writes it to. Like other calls
    /// to the host, it records its frame for the garbage collector.
    fn host_wrapper(&mut self, code: usize, context: usize, func_ty: TypeRef, args: &[TypeRef]) -> CompileResult<FuncId> {
        let tcx = &self.type_context.tcx;
        let adts = &self.compiler.adt_types;
        let isa = self.compiler.isa();
        let ptr_type = isa.pointer_type();
        let Type::Func(params, returns) = tcx.types[types::resolve(tcx, func_ty, args)].clone() else {
            return Err(CompileError::unsupported("host function without a function type"));
        };

        let params = params
            .iter()
            .map(|&param| types::ir_type(tcx, adts, param, args, isa))
            .collect::<CompileResult<Vec<_>>>()?;
        let returns = types::ir_type(tcx, adts, returns, args, isa)?;
        let signature = types::signature(tcx, adts, func_ty, args, isa, self.compiler.codegen.module.make_signature(), false)?;
        let mut host_signature = self.compiler.codegen.module.make_signature();

        host_signature.params.push(ir::AbiParam::new(ptr_type));
        host_signature.params.push(ir::AbiParam::new(ptr_type));

        for param in params.iter().flatten() {
            match param {
                MollieType::Inline { .. } => host_signature.params.push(ir::AbiParam::new(ptr_type)),
                param => param.add_to_params(&mut host_signature.params),
            }
        }

        let module = &mut self.compiler.codegen.module;
        let id = module.declare_anonymous_function(&signature)?;
        let mut ctx = module.make_context();
        let mut fn_builder_ctx = FunctionBuilderContext::new();
        let exit_push = self.compiler.runtime.exit_push;
        let exit_pop = self.compiler.runtime.exit_pop;

        ctx.func.signature = signature;

        {
            let mut fn_builder = FunctionBuilder::new(&mut ctx.func, &mut fn_builder_ctx);
            let entry_block = fn_builder.create_block();

            fn_builder.append_block_params_for_function_params(entry_block);
            fn_builder.switch_to_block(entry_block);
            fn_builder.seal_block(entry_block);

            let mut block_params = fn_builder.block_params(entry_block).to_vec().into_iter();
            let slot = |fn_builder: &mut FunctionBuilder, ty: MollieType| {
                fn_builder.create_sized_stack_slot(ir::StackSlotData::new(
                    ir::StackSlotKind::ExplicitSlot,
                    ty.bytes().max(1),
                    u8::try_from(ty.align().max(1).trailing_zeros()).unwrap_or(0),
                ))
            };
            let result_slot = returns.map(|ty| slot(&mut fn_builder, ty));
            let context = fn_builder.ins().iconst(ptr_type, context.cast_signed() as i64);
            let result = match result_slot {
                Some(result_slot) => fn_builder.ins().stack_addr(ptr_type, result_slot, 0),
                None => fn_builder.ins().iconst(ptr_type, 0),
            };
            let mut host_args = vec![context, result];

            for param in params.iter().flatten() {
                let values = block_params.by_ref().take(param.components().len()).collect::<Vec<_>>();

                match *param {
                    // Copied to the stack, the host gets a pointer to the copy.
                    MollieType::Inline { size, .. } => {
                        let copy = slot(&mut fn_builder, *param);

                        for ((_, offset), value) in mollie_ir::chunks(size).into_iter().zip(values) {
                            fn_builder.ins().stack_store(ptr_type, value, copy, offset.cast_signed());
                        }

                        host_args.push(fn_builder.ins().stack_addr(ptr_type, copy, 0));
                    }
                    _ => host_args.extend(values),
                }
            }

            let exit_push = module.declare_func_in_func(exit_push, fn_builder.func);
            let exit_pop = module.declare_func_in_func(exit_pop, fn_builder.func);
            let frame_pointer = fn_builder.ins().get_frame_pointer(ptr_type);
            let host_signature = fn_builder.import_signature(host_signature);
            let code = fn_builder.ins().iconst(ptr_type, code.cast_signed() as i64);

            fn_builder.ins().call(exit_push, &[frame_pointer]);
            fn_builder.ins().call_indirect(host_signature, code, &host_args);
            fn_builder.ins().call(exit_pop, &[]);

            let results = match (returns, result_slot) {
                (Some(MollieType::Inline { size, .. }), Some(result_slot)) => mollie_ir::chunks(size)
                    .into_iter()
                    .map(|(ty, offset)| fn_builder.ins().stack_load(ptr_type, ty, result_slot, offset.cast_signed()))
                    .collect(),
                (Some(ty), Some(result_slot)) => {
                    let mut offset = 0;

                    ty.components()
                        .into_iter()
                        .map(|component| {
                            let value = fn_builder.ins().stack_load(ptr_type, component, result_slot, offset);

                            offset += component.bytes().cast_signed();

                            value
                        })
                        .collect()
                }
                _ => Vec::new(),
            };

            fn_builder.ins().return_(&results);
            fn_builder.finalize(module.isa().frontend_config());
        }

        self.compiler.define_function(id, &mut ctx)?;

        Ok(id)
    }

    /// Signature of the function `vfunc` (of type `func_ty`) of an impl:
    /// functions taking `mut self` also return the changed receiver.
    fn impl_func_signature(&self, impl_ref: ImplRef, vfunc: VFuncRef, func_ty: TypeRef, args: &[TypeRef]) -> CompileResult<ir::Signature> {
        let tcx = &self.type_context.tcx;
        let isa = self.compiler.isa();
        let mut signature = types::signature(
            tcx,
            &self.compiler.adt_types,
            func_ty,
            args,
            isa,
            self.compiler.codegen.module.make_signature(),
            false,
        )?;

        if tcx.impl_registry.mut_self.contains(&(impl_ref, vfunc)) {
            let target = tcx.impl_registry.impls[impl_ref].ty;

            if let Some(ty) = types::ir_type(tcx, &self.compiler.adt_types, target, args, isa)? {
                ty.add_to_params(&mut signature.returns);
            }
        }

        Ok(signature)
    }

    /// Compiles layouts of value types in `ty` (with `args` applied), which
    /// code using values of `ty` needs: e.g. types of parameters of functions
    /// of the host, which may come only from the host.
    pub(crate) fn compile_value_types(&mut self, ty: TypeRef, args: &[TypeRef]) -> CompileResult<()> {
        let tcx = &mut self.type_context.tcx;
        let ty = tcx.types.apply_type_args(ty, args);

        match tcx.types[ty].clone() {
            Type::Adt(adt, adt_args) if tcx.is_value_type(ty) => self.compile_adt(adt, &adt_args),
            Type::Array(element, _) => self.compile_value_types(element, &[]),
            Type::Func(params, returns) => {
                for param in params {
                    self.compile_value_types(param, &[])?;
                }

                self.compile_value_types(returns, &[])
            }
            _ => Ok(()),
        }
    }

    /// Declares an instance of a function, with `args` as type arguments of
    /// its generic parameters.
    fn declare_func(&mut self, func_ref: FuncRef, args: &[TypeRef]) -> CompileResult<()> {
        let func_ty = self.type_context.tcx.def_registry.functions[func_ref].ty;

        self.compile_value_types(func_ty, args)?;

        if let Some(&FunctionBody::Host { code, context }) = self.type_context.functions.get(&func_ref) {
            let id = self.host_wrapper(code, context, func_ty, args)?;
            let hash = types::instance_hash(&self.type_context.tcx, args, &[]);

            self.compiler.func_ref_to_func_id.insert((func_ref, hash), id);

            return Ok(());
        }

        let func = &self.type_context.tcx.def_registry.functions[func_ref];
        let signature = types::signature(
            &self.type_context.tcx,
            &self.compiler.adt_types,
            func.ty,
            args,
            self.compiler.isa(),
            self.compiler.codegen.module.make_signature(),
            false,
        )?;
        let hash = types::instance_hash(&self.type_context.tcx, args, &[]);

        let (name, linkage) = match self.type_context.functions.get(&func_ref) {
            Some(FunctionBody::Local { .. }) if args.is_empty() => (format!("{}#{}", func.name, func_ref.index()), Linkage::Local),
            Some(FunctionBody::Local { .. }) => (format!("{}#{}#{hash:x}", func.name, func_ref.index()), Linkage::Local),
            Some(FunctionBody::Import(_)) if !args.is_empty() => {
                return Err(CompileError::unsupported(format!("generic host function `{}`", func.name)));
            }
            Some(&FunctionBody::Import(name)) => (name.to_owned(), Linkage::Import),
            Some(FunctionBody::BuiltIn(name)) => return Err(CompileError::unsupported(format!("built-in function `{name}`"))),
            Some(FunctionBody::Default { .. }) => return Err(CompileError::unsupported(format!("function `{}` is a default of an impl", func.name))),
            Some(FunctionBody::Host { .. }) => unreachable!("functions of the host are declared above"),
            Some(FunctionBody::Stub) => return Err(CompileError::unsupported(format!("function `{}` is only declared by a stub", func.name))),
            None => return Err(CompileError::unsupported(format!("function `{}` has no body", func.name))),
        };

        let id = self.compiler.codegen.module.declare_function(&name, linkage, &signature)?;

        self.compiler.func_id_to_name.insert(id, name);

        // The host may allocate or call compiled code, so the garbage collector
        // must know where frames of compiled code end.
        let id = if linkage == Linkage::Import {
            self.compiler.exit_wrapper(id, signature)?
        } else {
            id
        };

        self.compiler.func_ref_to_func_id.insert((func_ref, hash), id);

        Ok(())
    }

    fn define_func(&mut self, func_ref: FuncRef, args: &[TypeRef]) -> CompileResult<()> {
        let Some(FunctionBody::Local { ast, entry }) = self.type_context.functions.get(&func_ref) else {
            return Ok(());
        };

        let func = &self.type_context.tcx.def_registry.functions[func_ref];
        let id = self.compiler.func_ref_to_func_id[&(func_ref, types::instance_hash(&self.type_context.tcx, args, &[]))];
        let signature = types::signature(
            &self.type_context.tcx,
            &self.compiler.adt_types,
            func.ty,
            args,
            self.compiler.isa(),
            self.compiler.codegen.module.make_signature(),
            false,
        )?;

        // Readable, for backtraces of traps.
        let display_name = self.type_context.tcx.def_registry.functions[func_ref].name.clone();
        {
            let mut compiler = FunctionCompiler::new(
                id,
                &display_name,
                signature,
                &mut *self.compiler,
                &*self.type_context,
                &mut self.ctx,
                &mut self.fn_builder_ctx,
                args.into(),
            );

            compile_body(&mut compiler, &func.arg_names, func.ty, ast, *entry)?;
            compiler.finalize();
        }

        self.compiler.define_function(id, &mut self.ctx)
    }

    fn declare_vtable(&mut self, hash: u64, impl_ref: ImplRef, args: &[TypeRef]) -> CompileResult<()> {
        let mut vtable = IndexMap::new();
        // Functions with their own generics are compiled for each
        // instantiation (see `declare_method`).
        let functions = self.type_context.tcx.impl_registry.impls[impl_ref]
            .functions
            .iter()
            .filter(|(_, func)| func.generics == 0)
            .map(|(vfunc, func)| (vfunc, func.ty, func.name.clone()))
            .collect::<Vec<_>>();

        for (vfunc, func_ty, func_name) in functions {
            self.compile_value_types(func_ty, args)?;

            let host = match self.type_context.vtables.get(&impl_ref).and_then(|functions| functions.get(&vfunc)) {
                Some(&FunctionBody::Host { code, context }) => Some((code, context)),
                _ => None,
            };

            if let Some((code, context)) = host {
                let id = self.host_wrapper(code, context, func_ty, args)?;

                vtable.insert(vfunc, id);

                continue;
            }

            let signature = self.impl_func_signature(impl_ref, vfunc, func_ty, args)?;

            let (name, linkage) = match self.type_context.vtables.get(&impl_ref).and_then(|functions| functions.get(&vfunc)) {
                // The trait's default, a function instantiated for `Self` and
                // the trait's type arguments of this impl. It's used (and so
                // declared) before the impl, see `TypedAST::use_item`.
                Some(FunctionBody::Default { func, type_args }) => {
                    let (func, type_args) = (*func, type_args.clone());
                    let tcx = &mut self.type_context.tcx;
                    let type_args: Box<[_]> = type_args.iter().map(|&ty| tcx.types.apply_type_args(ty, args)).collect();
                    let key = (func, types::instance_hash(tcx, &type_args, &[]));
                    let id = self
                        .compiler
                        .func_ref_to_func_id
                        .get(&key)
                        .copied()
                        .ok_or_else(|| CompileError::unsupported(format!("default of `{}` wasn't compiled", tcx.def_registry.functions[func].name)))?;

                    vtable.insert(vfunc, id);

                    continue;
                }
                Some(FunctionBody::Local { .. } | FunctionBody::BuiltIn(_)) => (format!("impl{}#{hash:x}::{func_name}", impl_ref.index()), Linkage::Local),
                Some(&FunctionBody::Import(name)) => (name.to_owned(), Linkage::Import),
                Some(FunctionBody::Host { .. }) => unreachable!("functions of the host are declared above"),
                Some(FunctionBody::Stub) => return Err(CompileError::unsupported(format!("function `{func_name}` is only declared by a stub"))),
                None => return Err(CompileError::unsupported(format!("function `{func_name}` of an impl has no body"))),
            };

            let id = self.compiler.codegen.module.declare_function(&name, linkage, &signature)?;

            self.compiler.func_id_to_name.insert(id, name);

            let id = if linkage == Linkage::Import {
                self.compiler.exit_wrapper(id, signature)?
            } else {
                id
            };

            vtable.insert(vfunc, id);
        }

        self.compiler.vtables.insert((hash, impl_ref), vtable);

        Ok(())
    }

    /// Declares an instance of a function of an impl with its own generics.
    /// `args` are type arguments of the impl, then of the function.
    fn declare_method(&mut self, key: MethodKey, args: &[TypeRef]) -> CompileResult<()> {
        let (hash, impl_ref, vfunc, instance) = key;
        let func_ty = self.type_context.tcx.impl_registry.impls[impl_ref].functions[vfunc].ty;

        self.compile_value_types(func_ty, args)?;
        let func = &self.type_context.tcx.impl_registry.impls[impl_ref].functions[vfunc];
        let signature = self.impl_func_signature(impl_ref, vfunc, func.ty, args)?;

        if !matches!(
            self.type_context.vtables.get(&impl_ref).and_then(|functions| functions.get(&vfunc)),
            Some(FunctionBody::Local { .. })
        ) {
            return Err(CompileError::unsupported(format!("generic function `{}` without a body", func.name)));
        }

        let name = format!("impl{}#{hash:x}::{}#{instance:x}", impl_ref.index(), func.name);
        let id = self.compiler.codegen.module.declare_function(&name, Linkage::Local, &signature)?;

        self.compiler.func_id_to_name.insert(id, name);
        self.compiler.method_instances.insert(key, id);

        Ok(())
    }

    fn define_method(&mut self, key: MethodKey, args: &[TypeRef]) -> CompileResult<()> {
        let (_, impl_ref, vfunc, _) = key;
        let Some(FunctionBody::Local { ast, entry }) = self.type_context.vtables.get(&impl_ref).and_then(|functions| functions.get(&vfunc)) else {
            return Ok(());
        };

        let func = &self.type_context.tcx.impl_registry.impls[impl_ref].functions[vfunc];
        let id = self.compiler.method_instances[&key];
        let signature = self.impl_func_signature(impl_ref, vfunc, func.ty, args)?;
        let returns_self = self.type_context.tcx.impl_registry.mut_self.contains(&(impl_ref, vfunc));

        // Readable, for backtraces of traps.
        let display_name = impl_func_display(&self.type_context.tcx, impl_ref, vfunc);
        {
            let mut compiler = FunctionCompiler::new(
                id,
                &display_name,
                signature,
                &mut *self.compiler,
                &*self.type_context,
                &mut self.ctx,
                &mut self.fn_builder_ctx,
                args.into(),
            );

            compiler.returns_self = returns_self;
            compile_body(&mut compiler, &func.arg_names, func.ty, ast, *entry)?;
            compiler.finalize();
        }

        self.compiler.define_function(id, &mut self.ctx)
    }

    /// Defines the vtable of a trait impl: the hash of the implementing type,
    /// followed by pointers to functions in the order of the trait's
    /// functions.
    fn define_vtable_data(&mut self, hash: u64, impl_ref: ImplRef, args: &[TypeRef]) -> CompileResult<()> {
        let Some(trait_ref) = self.type_context.tcx.impl_registry.impls[impl_ref].origin_trait else {
            return Ok(());
        };

        if self.compiler.vtable_data.contains_key(&(hash, impl_ref)) {
            return Ok(());
        }

        let count = self.type_context.tcx.def_registry.traits[trait_ref].functions.len();
        let ptr_size = self.compiler.ptr_type().bytes() as usize;
        let mut contents = vec![0; ptr_size * (count + 1)];

        contents[..ptr_size].copy_from_slice(&hash.to_ne_bytes()[..ptr_size]);

        // Functions of the trait come first in impls, in the trait's order.
        let mut functions = (0..count)
            .map(|index| {
                self.compiler.vtables[&(hash, impl_ref)]
                    .get(&VFuncRef::new(index))
                    .copied()
                    .ok_or_else(|| CompileError::unsupported("impl doesn't implement every function of its trait"))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        // Trait objects of value types point to a boxed copy, while functions
        // of their impls take the value itself: their entries load it first.
        let target = self.type_context.tcx.impl_registry.impls[impl_ref].ty;

        if let Some(MollieType::Inline { size, .. }) = types::ir_type(&self.type_context.tcx, &self.compiler.adt_types, target, args, self.compiler.isa())? {
            for (index, func_id) in functions.iter_mut().enumerate() {
                let mut_self = self.type_context.tcx.impl_registry.mut_self.contains(&(impl_ref, VFuncRef::new(index)));

                *func_id = self.vtable_shim(*func_id, size, mut_self)?;
            }
        }

        let codegen = &mut self.compiler.codegen;

        codegen.data_desc.define(contents.into_boxed_slice());
        codegen.data_desc.set_align(ptr_size as u64);

        for (index, func_id) in functions.into_iter().enumerate() {
            let func_ref = codegen.module.declare_func_in_data(func_id, &mut codegen.data_desc);

            codegen
                .data_desc
                .write_function_addr(u32::try_from((index + 1) * ptr_size).unwrap_or(u32::MAX), func_ref);
        }

        let id = codegen.module.declare_anonymous_data(false, false)?;
        let result = codegen.module.define_data(id, &codegen.data_desc);

        codegen.data_desc.clear();
        result?;

        self.compiler.vtable_data.insert((hash, impl_ref), id);
        self.compiler.trait_to_vtable.insert((hash, trait_ref), id);

        Ok(())
    }

    /// The entry of a vtable for `target`, a function of an impl of a value
    /// type (of `size` bytes): it takes a pointer to the boxed value, loads the
    /// value and calls `target` with it.
    fn vtable_shim(&mut self, target: FuncId, size: u32, mut_self: bool) -> CompileResult<FuncId> {
        let ptr_type = self.compiler.ptr_type();
        let chunks = mollie_ir::chunks(size);
        let module = &mut self.compiler.codegen.module;
        let vm_state = self.compiler.vm_state_data;
        let target_signature = module.declarations().get_function_decl(target).signature.clone();
        let mut signature = module.make_signature();
        // A `mut self` function returns the changed value after its result:
        // it goes back to the box, callers of the trait don't get it.
        let results = target_signature.returns.len() - if mut_self { chunks.len() } else { 0 };

        signature.params.push(ir::AbiParam::new(ptr_type));
        signature.params.extend(target_signature.params.iter().skip(chunks.len()).copied());
        signature.returns.extend_from_slice(&target_signature.returns[..results]);

        let id = module.declare_anonymous_function(&signature)?;
        let mut ctx = module.make_context();
        let mut fn_builder_ctx = FunctionBuilderContext::new();

        ctx.func.signature = signature;

        {
            let mut fn_builder = FunctionBuilder::new(&mut ctx.func, &mut fn_builder_ctx);
            let entry_block = fn_builder.create_block();

            fn_builder.append_block_params_for_function_params(entry_block);
            fn_builder.switch_to_block(entry_block);
            fn_builder.seal_block(entry_block);

            let params = fn_builder.block_params(entry_block).to_vec();
            let mut args = chunks
                .iter()
                .map(|&(ty, offset)| {
                    fn_builder
                        .ins()
                        .load(ty, ir::MemFlagsData::new().with_notrap(), params[0], offset.cast_signed())
                })
                .collect::<Vec<_>>();

            args.extend_from_slice(&params[1..]);

            let target_ref = module.declare_func_in_func(target, fn_builder.func);
            let call = fn_builder.ins().call(target_ref, &args);
            let mut returned = fn_builder.inst_results(call).to_vec();
            let changed = returned.split_off(results);

            if !changed.is_empty() {
                // A function that stopped the program returns zeroes, which
                // must not replace the value.
                let state = module.declare_data_in_func(vm_state, fn_builder.func);
                let state = fn_builder.ins().symbol_value(ptr_type, state);
                let trap = fn_builder.ins().load(ir::types::I32, ir::MemFlagsData::trusted(), state, sandbox::TRAP_OFFSET);
                let store_block = fn_builder.create_block();
                let return_block = fn_builder.create_block();

                fn_builder.ins().brif(trap, return_block, &[], store_block, &[]);
                fn_builder.switch_to_block(store_block);
                fn_builder.seal_block(store_block);

                for ((_, offset), value) in chunks.iter().zip(changed) {
                    fn_builder
                        .ins()
                        .store(ir::MemFlagsData::new().with_notrap(), value, params[0], offset.cast_signed());
                }

                fn_builder.ins().jump(return_block, &[]);
                fn_builder.switch_to_block(return_block);
                fn_builder.seal_block(return_block);
            }

            fn_builder.ins().return_(&returned);
            fn_builder.finalize(module.isa().frontend_config());
        }

        self.compiler.define_function(id, &mut ctx)?;

        Ok(id)
    }

    fn define_vtable(&mut self, hash: u64, impl_ref: ImplRef, args: &[TypeRef]) -> CompileResult<()> {
        let generator = &self.type_context.tcx.impl_registry.impls[impl_ref];

        for (vfunc, func) in generator.functions.iter().filter(|(_, func)| func.generics == 0) {
            let body = self.type_context.vtables.get(&impl_ref).and_then(|functions| functions.get(&vfunc));

            // Imported functions are defined by the host. Their vtable entries
            // are exit wrappers, which are already defined (and have no names).
            // Defaults are functions compiled on their own.
            if matches!(
                body,
                Some(FunctionBody::Import(_) | FunctionBody::Default { .. } | FunctionBody::Host { .. } | FunctionBody::Stub) | None
            ) {
                continue;
            }

            let id = self.compiler.vtables[&(hash, impl_ref)][&vfunc];
            let signature = self.impl_func_signature(impl_ref, vfunc, func.ty, args)?;
            let returns_self = self.type_context.tcx.impl_registry.mut_self.contains(&(impl_ref, vfunc));

            // Readable, for backtraces of traps.
            let display_name = impl_func_display(&self.type_context.tcx, impl_ref, vfunc);
            {
                let mut compiler = FunctionCompiler::new(
                    id,
                    &display_name,
                    signature,
                    &mut *self.compiler,
                    &*self.type_context,
                    &mut self.ctx,
                    &mut self.fn_builder_ctx,
                    args.into(),
                );

                compiler.returns_self = returns_self;

                match body {
                    Some(FunctionBody::Local { ast, entry }) => compile_body(&mut compiler, &func.arg_names, func.ty, ast, *entry)?,
                    Some(&FunctionBody::BuiltIn("push")) => compile_array_push(&mut compiler, args)?,
                    // Strings and arrays both start with their length.
                    Some(&FunctionBody::BuiltIn("string_len" | "array_len")) => compile_length(&mut compiler),
                    Some(&FunctionBody::BuiltIn("string_slice")) => compile_string_slice(&mut compiler),
                    Some(&FunctionBody::BuiltIn("array_truncate")) => compile_array_truncate(&mut compiler),
                    Some(&FunctionBody::BuiltIn(name @ ("f32_sqrt" | "f32_floor" | "f32_ceil" | "f32_trunc" | "f32_abs"))) => {
                        compile_float_op(&mut compiler, name);
                    }
                    Some(&FunctionBody::BuiltIn(name)) if compiler.compiler.builtins.contains_key(name) => compile_runtime_builtin(&mut compiler, name),
                    Some(&FunctionBody::BuiltIn(name)) => return Err(CompileError::unsupported(format!("unknown built-in function `{name}`"))),
                    Some(FunctionBody::Import(_) | FunctionBody::Default { .. } | FunctionBody::Host { .. } | FunctionBody::Stub) | None => {
                        unreachable!("functions of the host and defaults are skipped")
                    }
                }

                compiler.finalize();
            }

            self.compiler.define_function(id, &mut self.ctx)?;
        }

        tracing::info!(
            target: "mollie-compiler/virtual_tables",
            "Compiled `{}` for `{}`",
            generator
                .origin_trait
                .map_or("<impl>", |trait_ref| self.type_context.tcx.def_registry.traits[trait_ref].name.as_str()),
            self.type_context.tcx.display_of(generator.ty),
        );

        Ok(())
    }
}

/// Compiles the body of a function: binds its parameters, compiles the entry
/// block and returns its value.
fn compile_body(
    compiler: &mut FunctionCompiler<'_, JITModule>,
    arg_names: &[String],
    func_ty: TypeRef,
    ast: &mollie_typed_ast::TypedAST,
    entry: mollie_typed_ast::BlockRef,
) -> CompileResult<()> {
    let Type::Func(arg_types, returns) = &compiler.types()[func_ty] else {
        return Err(CompileError::unsupported("function without a function type"));
    };

    let (arg_types, returns) = (arg_types.clone(), *returns);

    compiler.return_ty = Some(returns);
    compiler.bind_params(0, arg_names.iter().map(String::as_str).zip(arg_types.iter().copied()))?;
    compiler.consume_fuel();

    let returned = entry.compile(ast, compiler)?;
    let returned = compiler.coerce(returned, ast[entry].ty, returns)?;

    // Returned before leaving scopes: `mut self` functions return `self` too.
    compiler.return_(&returned);
    compiler.pop_all_frames();

    Ok(())
}

/// `string.len()` (the length in bytes) and `array.len()`.
/// A built-in implemented by a function of the runtime, called with the
/// arguments (see `runtime::builtins`).
fn compile_runtime_builtin(compiler: &mut FunctionCompiler<'_, JITModule>, name: &str) {
    let params = compiler.fn_builder.block_params(compiler.entry_block).to_vec();
    let func_id = compiler.compiler.builtins[name];
    // Functions that allocate may stop the program (when the heap limit is
    // exceeded), the others can't.
    let results = compiler.call(func_id, &params);

    compiler.return_(&results.first().map_or(MolValue::Nothing, |&result| MolValue::Value(result)));
}

/// Operations on floats that are single instructions (exactly rounded, so
/// the same on every platform).
fn compile_float_op(compiler: &mut FunctionCompiler<'_, JITModule>, name: &str) {
    let value = compiler.fn_builder.block_params(compiler.entry_block)[0];
    let ins = compiler.fn_builder.ins();
    let result = match name {
        "f32_sqrt" => ins.sqrt(value),
        "f32_floor" => ins.floor(value),
        "f32_ceil" => ins.ceil(value),
        "f32_trunc" => ins.trunc(value),
        _ => ins.fabs(value),
    };

    compiler.return_(&MolValue::Value(result));
}

/// `array.truncate(length)`: shortens the array (it never grows).
fn compile_array_truncate(compiler: &mut FunctionCompiler<'_, JITModule>) {
    let params = compiler.fn_builder.block_params(compiler.entry_block).to_vec();
    let (array, length) = (params[0], params[1]);
    let ptr_type = compiler.ptr_type();
    let current = compiler
        .fn_builder
        .ins()
        .load(ptr_type, ir::MemFlagsData::trusted(), array, ARRAY_LENGTH_OFFSET);
    let length = compiler.fn_builder.ins().umin(current, length);

    compiler.fn_builder.ins().store(ir::MemFlagsData::trusted(), length, array, ARRAY_LENGTH_OFFSET);
    compiler.return_(&MolValue::Nothing);
}

fn compile_length(compiler: &mut FunctionCompiler<'_, JITModule>) {
    let string = compiler.fn_builder.block_params(compiler.entry_block)[0];
    let ptr_type = compiler.ptr_type();
    let length = compiler
        .fn_builder
        .ins()
        .load(ptr_type, ir::MemFlagsData::trusted(), string, ARRAY_LENGTH_OFFSET);

    compiler.return_(&MolValue::Value(length));
}

/// `string.slice(start, end)`: a new string with bytes from `start` to `end`,
/// trapping if they're out of bounds or not at char boundaries.
fn compile_string_slice(compiler: &mut FunctionCompiler<'_, JITModule>) {
    let params = compiler.fn_builder.block_params(compiler.entry_block).to_vec();
    let slice = compiler.compiler.runtime.str_slice;
    let result = compiler.call(slice, &params)[0];
    let ptr_type = compiler.ptr_type();
    let null = compiler.fn_builder.ins().iconst(ptr_type, 0);
    let is_null = compiler.fn_builder.ins().icmp(IntCC::Equal, result, null);

    compiler.trap_if(is_null, TrapKind::InvalidSlice);
    compiler.return_(&MolValue::Value(result));
}

/// `push` of arrays: grows the array by one element and stores the item.
fn compile_array_push(compiler: &mut FunctionCompiler<'_, JITModule>, args: &[TypeRef]) -> CompileResult<()> {
    let element = *args.first().ok_or_else(|| CompileError::unsupported("`push` without an element type"))?;
    let element_type = compiler.value_type(element)?;
    let params = compiler.fn_builder.block_params(compiler.entry_block).to_vec();
    let array = params[0];
    let item = MolValue::from_values(Some(element_type), &params[1..])?;
    let ptr_type = compiler.ptr_type();
    let length = compiler
        .fn_builder
        .ins()
        .load(ptr_type, ir::MemFlagsData::trusted(), array, ARRAY_LENGTH_OFFSET);
    let new_length = compiler.fn_builder.ins().iadd_imm_u(length, 1);
    let realloc_array = compiler.compiler.runtime.realloc_array;

    // Growing the array may collect garbage, and the item isn't stored yet.
    compiler.track(element, &item);

    // Growing the array may collect garbage (it keeps the array), and stops
    // the program if the heap limit is exceeded.
    compiler.call(realloc_array, &[array, new_length]);

    let data = compiler.fn_builder.ins().load(ptr_type, ir::MemFlagsData::trusted(), array, ARRAY_DATA_OFFSET);
    let offset = compiler.fn_builder.ins().imul_imm_u(length, i64::from(element_type.bytes()));
    let slot = compiler.fn_builder.ins().iadd(data, offset);

    compiler.store_value(element_type, &item, slot, 0)?;
    compiler.return_(&MolValue::Nothing);

    Ok(())
}

/// Readable name of a function of an impl, like `Option<T>::unwrap`.
fn impl_func_display(tcx: &mollie_typing::TyCtxt, impl_ref: ImplRef, vfunc: VFuncRef) -> String {
    let generator = &tcx.impl_registry.impls[impl_ref];

    format!("{}::{}", tcx.display_of(generator.ty), generator.functions[vfunc].name)
}
