#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::ptr::from_ref;

use cranelift::{
    codegen::{
        Context,
        ir::{self, UserFuncName, condcodes::IntCC},
    },
    jit::JITModule,
    module::{DataId, FuncId, Module},
    prelude::{Block, FunctionBuilder, FunctionBuilderContext, InstBuilder, isa::TargetIsa},
};
use indexmap::IndexMap;
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{ExprRef, TypedAST, TypedASTContext};
use mollie_typing::{AdtVariantRef, FieldRef, ModuleSpan, PrimitiveType, TraitRef, Type, TypeRef, TypeStorage};

use crate::{
    CompileTypedAST, CompiledAdt, CompilerInner, MolValue, Var,
    allocator::{TypeLayout, TypeLayoutField},
    error::{CompileError, CompileResult},
    expr::LoopTarget,
    sandbox::{self, Frame, Location, TrapKind},
    types,
};

/// Offsets of fields of an [`Array`](crate::allocator::Array) value.
pub(crate) const ARRAY_LENGTH_OFFSET: i32 = 0;
pub(crate) const ARRAY_DATA_OFFSET: i32 = 2 * size_of::<usize>() as i32;

#[derive(Debug, Clone)]
pub struct Variable {
    pub value: Var,
    pub ty: TypeRef,
}

pub type VariableFrame = IndexMap<String, Variable>;

pub struct FunctionCompiler<'a, M: Module = JITModule> {
    pub(crate) id: FuncId,
    pub(crate) entry_block: Block,

    pub(crate) compiler: &'a mut CompilerInner<M>,
    pub(crate) fn_builder: FunctionBuilder<'a>,
    pub(crate) type_context: &'a TypedASTContext,
    /// Type arguments of the generic impl this function belongs to.
    pub(crate) generics: Box<[TypeRef]>,

    funcs: IndexMap<FuncId, ir::FuncRef>,
    data: IndexMap<DataId, ir::GlobalValue>,
    frames: Vec<VariableFrame>,
    /// Block returning zeroes, taken when the program is stopped by a trap.
    /// Filled by [`FunctionCompiler::finalize`].
    trap_return: Option<Block>,
    /// Return type of the function, for `return`.
    pub(crate) return_ty: Option<TypeRef>,
    /// Loops around the code being compiled, for `break` and `continue`.
    pub(crate) loops: Vec<LoopTarget>,
    /// Whether the function takes `mut self`, which it returns after its
    /// result.
    pub(crate) returns_self: bool,
    /// Name of the function, for locations of traps.
    name: String,
    /// The code being compiled, for locations of traps.
    pub(crate) site: Option<ModuleSpan>,
}

impl<'a, M: Module> FunctionCompiler<'a, M> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: FuncId,
        name: &str,
        signature: ir::Signature,
        compiler: &'a mut CompilerInner<M>,
        type_context: &'a TypedASTContext,
        ctx: &'a mut Context,
        fn_builder_ctx: &'a mut FunctionBuilderContext,
        generics: Box<[TypeRef]>,
    ) -> Self {
        compiler.codegen.module.clear_context(ctx);

        ctx.func.signature = signature;
        ctx.func.name = UserFuncName::testcase(name);

        let mut fn_builder = FunctionBuilder::new(&mut ctx.func, fn_builder_ctx);
        let entry_block = fn_builder.create_block();

        fn_builder.append_block_params_for_function_params(entry_block);
        fn_builder.switch_to_block(entry_block);
        fn_builder.seal_block(entry_block);

        let mut compiler = Self {
            id,
            entry_block,
            compiler,
            fn_builder,
            type_context,
            generics,
            funcs: IndexMap::new(),
            data: IndexMap::new(),
            frames: vec![VariableFrame::new()],
            trap_return: None,
            return_ty: None,
            loops: Vec::new(),
            returns_self: false,
            name: name.to_owned(),
            site: None,
        };

        compiler.prologue();
        compiler
    }

    /// Checks the stack limit. Fuel is consumed once parameters are bound
    /// (see [`FunctionCompiler::consume_fuel`]): the host may run compiled code
    /// to give more fuel, which may collect garbage, so GC references in
    /// parameters must be described by stack maps by then.
    fn prologue(&mut self) {
        let ptr_type = self.ptr_type();
        let state = self.vm_state();
        let stack_pointer = self.fn_builder.ins().get_stack_pointer(ptr_type);
        let stack_limit = self
            .fn_builder
            .ins()
            .load(ptr_type, ir::MemFlagsData::trusted(), state, sandbox::STACK_LIMIT_OFFSET);
        let overflow = self.fn_builder.ins().icmp(IntCC::UnsignedLessThan, stack_pointer, stack_limit);

        self.trap_if(overflow, TrapKind::StackOverflow);

        // The call depth is the limit programs see: it doesn't depend on the
        // platform, like the stack used does.
        let depth = self
            .fn_builder
            .ins()
            .load(ir::types::I32, ir::MemFlagsData::trusted(), state, sandbox::DEPTH_OFFSET);
        let max_depth = self
            .fn_builder
            .ins()
            .load(ir::types::I32, ir::MemFlagsData::trusted(), state, sandbox::MAX_DEPTH_OFFSET);
        let too_deep = self.fn_builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, depth, max_depth);

        self.trap_if(too_deep, TrapKind::StackOverflow);

        let depth = self.fn_builder.ins().iadd_imm_u(depth, 1);

        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), depth, state, sandbox::DEPTH_OFFSET);
    }

    /// Ends the call started by the prologue, before returning. The depth
    /// isn't restored when the program traps: its run restores the state.
    pub fn leave(&mut self) {
        let state = self.vm_state();
        let depth = self
            .fn_builder
            .ins()
            .load(ir::types::I32, ir::MemFlagsData::trusted(), state, sandbox::DEPTH_OFFSET);
        let depth = self.fn_builder.ins().iadd_imm_s(depth, -1);

        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), depth, state, sandbox::DEPTH_OFFSET);
    }

    /// Finishes building the function. It still has to be defined with
    /// [`CompilerInner::define_function`].
    pub fn finalize(mut self) -> FuncId {
        // Every other block is filled, so the trap block can be filled now.
        if let Some(block) = self.trap_return {
            let returns = self.fn_builder.func.signature.returns.clone();

            self.fn_builder.switch_to_block(block);
            self.fn_builder.seal_block(block);

            let zeroes = returns
                .iter()
                .map(|param| {
                    if param.value_type == ir::types::F32 {
                        self.fn_builder.ins().f32const(0.0)
                    } else if param.value_type == ir::types::F64 {
                        self.fn_builder.ins().f64const(0.0)
                    } else {
                        self.fn_builder.ins().iconst(param.value_type, 0)
                    }
                })
                .collect::<Vec<_>>();

            self.fn_builder.ins().return_(&zeroes);
        }

        let frontend_config = self.compiler.codegen.module.isa().frontend_config();

        self.fn_builder.finalize(frontend_config);

        self.id
    }

    /// Pointer to the [`VmState`](sandbox::VmState) of programs.
    fn vm_state(&mut self) -> ir::Value {
        let data_id = self.compiler.vm_state_data;

        self.data_addr(data_id)
    }

    fn trap_return_block(&mut self) -> Block {
        if let Some(block) = self.trap_return {
            return block;
        }

        let block = self.fn_builder.create_block();

        self.fn_builder.set_cold_block(block);
        self.trap_return = Some(block);

        block
    }

    /// Stops the program with `kind` if `condition` is true (non-zero).
    pub fn trap_if(&mut self, condition: ir::Value, kind: TrapKind) {
        let trap_block = self.fn_builder.create_block();
        let continue_block = self.fn_builder.create_block();

        self.fn_builder.set_cold_block(trap_block);
        self.fn_builder.ins().brif(condition, trap_block, &[], continue_block, &[]);
        self.fn_builder.switch_to_block(trap_block);
        self.fn_builder.seal_block(trap_block);

        let state = self.vm_state();
        let code = self.fn_builder.ins().iconst(ir::types::I32, i64::from(kind as u32));
        let trap_return = self.trap_return_block();

        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), code, state, sandbox::TRAP_OFFSET);
        self.record_site();
        self.fn_builder.ins().jump(trap_return, &[]);
        self.fn_builder.switch_to_block(continue_block);
        self.fn_builder.seal_block(continue_block);
    }

    /// Stops the program, and continues in a block that's never reached,
    /// producing a zero value of `ty` for the code after it.
    ///
    /// # Errors
    ///
    /// Returns an error if `ty` has no representation.
    pub fn unreachable(&mut self, ty: TypeRef) -> CompileResult<MolValue> {
        self.stop(TrapKind::Unreachable);
        self.zero_value(ty)
    }

    /// Stops the program with `kind`. The code after it is never reached.
    fn stop(&mut self, kind: TrapKind) {
        let condition = self.iconst(ir::types::I8, 1);

        self.trap_if(condition, kind);
    }

    /// A zero value of `ty`, for code that's never reached.
    pub(crate) fn zero_value(&mut self, ty: TypeRef) -> CompileResult<MolValue> {
        Ok(match self.ir_type(ty)? {
            None => MolValue::Nothing,
            Some(MollieType::Regular(ty)) => MolValue::Value(self.zero(ty)),
            Some(MollieType::Fat(ty, metadata_ty)) => MolValue::FatPtr(self.zero(ty), self.zero(metadata_ty)),
            Some(MollieType::Inline { size, .. }) => MolValue::Inline(self.zero_inline(size)),
        })
    }

    /// Compiles `return value` (or `return`), producing a value of `ty` for
    /// the code after it, which is never reached.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_return(&mut self, ast: &TypedAST, value: Option<ExprRef>, ty: TypeRef) -> CompileResult<MolValue> {
        let returns = self.return_ty.ok_or_else(|| CompileError::unsupported("`return` outside of a function"))?;
        let value = match value {
            Some(value) => {
                let compiled = value.compile(ast, self)?;

                self.coerce(compiled, ast[value].ty, returns)?
            }
            None => MolValue::Nothing,
        };

        self.return_(&value);

        let after = self.fn_builder.create_block();

        self.fn_builder.switch_to_block(after);
        self.fn_builder.seal_block(after);
        self.zero_value(ty)
    }

    /// Compiles `panic(message)`, producing a value of `ty` for the code after
    /// it, which is never reached.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_panic(&mut self, ast: &TypedAST, message: ExprRef, ty: TypeRef) -> CompileResult<MolValue> {
        let message = message.compile(ast, self)?.value()?;
        let panic = self.compiler.runtime.panic;

        self.call_unchecked(panic, &[message]);
        self.stop(TrapKind::Panic);
        self.zero_value(ty)
    }

    fn zero(&mut self, ty: ir::Type) -> ir::Value {
        if ty == ir::types::F32 {
            self.fn_builder.ins().f32const(0.0)
        } else if ty == ir::types::F64 {
            self.fn_builder.ins().f64const(0.0)
        } else {
            self.fn_builder.ins().iconst(ty, 0)
        }
    }

    /// Returns (with zeroes) if the program was stopped, e.g. by the function
    /// that was just called.
    pub fn check_trap(&mut self) {
        let state = self.vm_state();
        let trap = self
            .fn_builder
            .ins()
            .load(ir::types::I32, ir::MemFlagsData::trusted(), state, sandbox::TRAP_OFFSET);
        let trap_return = self.trap_return_block();
        let record_block = self.fn_builder.create_block();
        let continue_block = self.fn_builder.create_block();

        self.fn_builder.set_cold_block(record_block);
        self.fn_builder.ins().brif(trap, record_block, &[], continue_block, &[]);

        // The call is a frame of the backtrace.
        self.fn_builder.switch_to_block(record_block);
        self.fn_builder.seal_block(record_block);
        self.record_site();
        self.fn_builder.ins().jump(trap_return, &[]);

        self.fn_builder.switch_to_block(continue_block);
        self.fn_builder.seal_block(continue_block);
    }

    /// Adds the code being compiled to the trace of the trap that stopped the
    /// program (see [`VmState::trace`](sandbox::VmState::trace)). When the
    /// trace is full, the last site is replaced: it keeps the deepest calls.
    fn record_site(&mut self) {
        let location = self.site.map(|ModuleSpan(module, span)| Location {
            module: self.type_context.tcx.display_of_module(module).to_string(),
            line: span.range.start_line + 1,
            column: span.range.start_column + 1,
        });
        let site = self.compiler.trap_sites.push(Frame {
            function: self.name.clone(),
            location,
        });
        let ptr_type = self.ptr_type();
        let state = self.vm_state();
        let flags = ir::MemFlagsData::trusted();
        let length = self.fn_builder.ins().load(ir::types::I32, flags, state, sandbox::TRACE_LEN_OFFSET);
        let last = self.iconst(ir::types::I32, i64::from(sandbox::TRACE_CAPACITY as u32 - 1));
        let index = self.fn_builder.ins().umin(length, last);
        let index = self.fn_builder.ins().uextend(ptr_type, index);
        let offset = self.fn_builder.ins().imul_imm_u(index, 4);
        let slot = self.fn_builder.ins().iadd(state, offset);
        let site = self.iconst(ir::types::I32, i64::from(site));

        self.fn_builder.ins().store(flags, site, slot, sandbox::TRACE_OFFSET);

        let next = self.fn_builder.ins().iadd_imm_u(length, 1);
        let capacity = self.iconst(ir::types::I32, i64::from(sandbox::TRACE_CAPACITY as u32));
        let next = self.fn_builder.ins().umin(next, capacity);

        self.fn_builder.ins().store(flags, next, state, sandbox::TRACE_LEN_OFFSET);
    }

    /// Consumes one unit of fuel. When it runs out, the host is asked for
    /// more, or the program is stopped.
    pub fn consume_fuel(&mut self) {
        let state = self.vm_state();
        let fuel = self
            .fn_builder
            .ins()
            .load(ir::types::I64, ir::MemFlagsData::trusted(), state, sandbox::FUEL_OFFSET);
        let one = self.fn_builder.ins().iconst(ir::types::I64, 1);
        let fuel = self.fn_builder.ins().isub(fuel, one);

        self.fn_builder.ins().store(ir::MemFlagsData::trusted(), fuel, state, sandbox::FUEL_OFFSET);

        let zero = self.fn_builder.ins().iconst(ir::types::I64, 0);
        let out_of_fuel = self.fn_builder.ins().icmp(IntCC::SignedLessThanOrEqual, fuel, zero);
        let refuel_block = self.fn_builder.create_block();
        let continue_block = self.fn_builder.create_block();

        self.fn_builder.set_cold_block(refuel_block);
        self.fn_builder.ins().brif(out_of_fuel, refuel_block, &[], continue_block, &[]);
        self.fn_builder.switch_to_block(refuel_block);
        self.fn_builder.seal_block(refuel_block);

        let refuel = self.compiler.runtime.out_of_fuel;

        self.call(refuel, &[]);
        self.fn_builder.ins().jump(continue_block, &[]);
        self.fn_builder.switch_to_block(continue_block);
        self.fn_builder.seal_block(continue_block);
    }

    pub fn isa(&self) -> &dyn TargetIsa {
        self.compiler.codegen.module.isa()
    }

    pub fn ptr_type(&self) -> ir::Type {
        self.compiler.ptr_type()
    }

    pub const fn types(&self) -> &TypeStorage {
        &self.type_context.tcx.types
    }

    pub fn resolve(&self, ty: TypeRef) -> TypeRef {
        types::resolve(&self.type_context.tcx, ty, &self.generics)
    }

    /// The representation of values of `ty` (`None` for `void`), with the
    /// generics of the function applied.
    ///
    /// # Errors
    ///
    /// Returns an error if a value type in `ty` wasn't compiled.
    pub fn ir_type(&self, ty: TypeRef) -> CompileResult<Option<MollieType>> {
        types::ir_type(&self.type_context.tcx, &self.compiler.adt_types, ty, &self.generics, self.isa())
    }

    /// Like [`FunctionCompiler::ir_type`], but `void` is an error.
    ///
    /// # Errors
    ///
    /// Returns an error if `ty` is `void`, or a value type in it wasn't
    /// compiled.
    pub fn value_type(&self, ty: TypeRef) -> CompileResult<MollieType> {
        self.ir_type(ty)?
            .ok_or_else(|| CompileError::unsupported(format!("`void` used as a value of `{}`", self.display(ty))))
    }

    pub fn hash(&self, ty: TypeRef) -> u64 {
        self.types().hash_of_instance(ty, &self.generics)
    }

    pub fn display(&self, ty: TypeRef) -> String {
        self.type_context.tcx.display_of(self.resolve(ty)).to_string()
    }

    /// The signature of functions of type `func_ty` (taking an environment last
    /// with `with_env`).
    ///
    /// # Errors
    ///
    /// Returns an error if `func_ty` isn't a function type, or a value type in
    /// it wasn't compiled.
    pub fn signature(&self, func_ty: TypeRef, with_env: bool) -> CompileResult<ir::Signature> {
        types::signature(
            &self.type_context.tcx,
            &self.compiler.adt_types,
            func_ty,
            &self.generics,
            self.isa(),
            self.compiler.codegen.module.make_signature(),
            with_env,
        )
    }

    pub fn func_ref(&mut self, func_id: FuncId) -> ir::FuncRef {
        if let Some(&func_ref) = self.funcs.get(&func_id) {
            return func_ref;
        }

        let func_ref = self.compiler.codegen.module.declare_func_in_func(func_id, self.fn_builder.func);

        self.funcs.insert(func_id, func_ref);

        func_ref
    }

    /// Address of static data.
    pub fn data_addr(&mut self, data_id: DataId) -> ir::Value {
        let global_value = if let Some(&global_value) = self.data.get(&data_id) {
            global_value
        } else {
            let global_value = self.compiler.codegen.module.declare_data_in_func(data_id, self.fn_builder.func);

            self.data.insert(data_id, global_value);

            global_value
        };

        let ptr_type = self.ptr_type();

        self.fn_builder.ins().symbol_value(ptr_type, global_value)
    }

    /// Calls a function that can stop the program (compiled code, the host
    /// or the runtime when it allocates), returning early if it does.
    pub fn call(&mut self, func_id: FuncId, args: &[ir::Value]) -> Vec<ir::Value> {
        let results = self.call_unchecked(func_id, args);

        self.check_trap();

        results
    }

    /// Calls a function that can't stop the program, like comparing strings,
    /// or whose caller checks it (see [`FunctionCompiler::call`]).
    pub fn call_unchecked(&mut self, func_id: FuncId, args: &[ir::Value]) -> Vec<ir::Value> {
        let func_ref = self.func_ref(func_id);
        let call = self.fn_builder.ins().call(func_ref, args);

        self.fn_builder.inst_results(call).to_vec()
    }

    /// Integer constant of type `ty`, truncating `value` to its width.
    pub fn iconst(&mut self, ty: ir::Type, value: i64) -> ir::Value {
        let bits = ty.bits();
        let value = if bits < 64 { value & ((1 << bits) - 1) } else { value };

        self.fn_builder.ins().iconst(ty, value)
    }

    pub fn ptr_const(&mut self, value: usize) -> ir::Value {
        let ptr_type = self.ptr_type();

        self.iconst(ptr_type, value.cast_signed() as i64)
    }

    /// Allocates a zeroed GC object and returns a pointer to its value.
    pub fn alloc(&mut self, type_layout: &'static TypeLayout) -> ir::Value {
        let layout = self.ptr_const(from_ref(type_layout).addr());
        let alloc = self.compiler.runtime.alloc;

        self.call(alloc, &[layout])[0]
    }

    /// Allocates an array of `length` zeroed elements and returns a pointer to
    /// its value.
    pub fn alloc_array(&mut self, item_layout: &'static TypeLayout, length: ir::Value) -> ir::Value {
        let layout = self.ptr_const(from_ref(item_layout).addr());
        let alloc_array = self.compiler.runtime.alloc_array;

        self.call(alloc_array, &[layout, length])[0]
    }

    pub fn load_value(&mut self, ty: MollieType, ptr: ir::Value, offset: i32) -> MolValue {
        match ty {
            MollieType::Inline { size, .. } => MolValue::Inline(self.load_inline(size, ptr, offset)),
            MollieType::Regular(ty) => MolValue::Value(self.fn_builder.ins().load(ty, ir::MemFlagsData::trusted(), ptr, offset)),
            MollieType::Fat(ty, metadata_ty) => MolValue::FatPtr(
                self.fn_builder.ins().load(ty, ir::MemFlagsData::trusted(), ptr, offset),
                self.fn_builder
                    .ins()
                    .load(metadata_ty, ir::MemFlagsData::trusted(), ptr, offset + ty.bytes().cast_signed()),
            ),
        }
    }

    /// Stores `value` of representation `ty` at `ptr + offset`.
    ///
    /// # Errors
    ///
    /// Returns an error if `value` doesn't have the representation `ty`.
    pub fn store_value(&mut self, ty: MollieType, value: &MolValue, ptr: ir::Value, offset: i32) -> CompileResult<()> {
        match (ty, value) {
            (MollieType::Regular(_), &MolValue::Value(value)) => {
                self.fn_builder.ins().store(ir::MemFlagsData::trusted(), value, ptr, offset);
            }
            (MollieType::Fat(ty, _), &MolValue::FatPtr(value, metadata)) => {
                self.fn_builder.ins().store(ir::MemFlagsData::trusted(), value, ptr, offset);
                self.fn_builder
                    .ins()
                    .store(ir::MemFlagsData::trusted(), metadata, ptr, offset + ty.bytes().cast_signed());
            }
            (MollieType::Inline { size, .. }, MolValue::Inline(values)) => self.store_inline(size, values, ptr, offset),
            (ty, value) => return Err(CompileError::unsupported(format!("can't store {value:?} as {ty:?}"))),
        }

        Ok(())
    }

    /// Compiled layout of an ADT type.
    ///
    /// # Errors
    ///
    /// Returns an error if `ty` isn't an ADT, or wasn't compiled.
    pub fn compiled_adt(&self, ty: TypeRef) -> CompileResult<&CompiledAdt> {
        let hash = self.hash(ty);

        self.compiler
            .adt_types
            .get(&hash)
            .ok_or_else(|| CompileError::unsupported(format!("`{}` wasn't compiled", self.display(ty))))
    }

    /// Representation, offset and type of a field of an ADT.
    ///
    /// # Errors
    ///
    /// Returns an error if `adt_ty` wasn't compiled, or doesn't have the field.
    pub fn field_layout(&self, adt_ty: TypeRef, variant: AdtVariantRef, field: FieldRef) -> CompileResult<(MollieType, i32, TypeRef)> {
        let compiled = self.compiled_adt(adt_ty)?;
        let (field_layout, field_ty) = compiled
            .variants
            .get(variant)
            .and_then(|variant| variant.fields.get(field))
            .ok_or_else(|| CompileError::unsupported(format!("no field #{} in `{}`", field.index(), self.display(adt_ty))))?;

        Ok((field_layout.ty, field_layout.offset, *field_ty))
    }

    /// The GC reference in a value of type `ty`, if any.
    pub fn gc_pointer(&self, ty: TypeRef, value: &MolValue) -> Option<ir::Value> {
        match (&self.types()[self.resolve(ty)], value) {
            (&Type::Adt(adt_ref, _), &MolValue::Value(value)) if self.type_context.tcx.def_registry.adt_types[adt_ref].collectable => Some(value),
            (Type::Array(..) | Type::Primitive(PrimitiveType::String), &MolValue::Value(value))
            | (Type::Trait(..), &MolValue::FatPtr(value, _))
            | (Type::Func(..), &MolValue::FatPtr(_, value)) => Some(value),
            _ => None,
        }
    }

    /// Indices of chunks of an inline value of `ty` (of `size` bytes) that may
    /// be references to GC objects.
    fn pointer_chunks(&self, ty: TypeRef, size: u32) -> CompileResult<Vec<usize>> {
        let pointers = types::inline_pointers(&self.type_context.tcx, &self.compiler.adt_types, ty, &self.generics)?;

        Ok(mollie_ir::chunks(size)
            .into_iter()
            .enumerate()
            .filter(|(_, (_, offset))| pointers.contains(offset))
            .map(|(index, _)| index)
            .collect())
    }

    /// Makes the GC references in `value` (of type `ty`), if any, visible to
    /// the garbage collector: while they're live across a call, they're kept
    /// on the stack and described by the call's stack map.
    pub fn track(&mut self, ty: TypeRef, value: &MolValue) {
        if let Some(ptr) = self.gc_pointer(ty, value) {
            self.fn_builder.declare_value_needs_stack_map(ptr);
        }

        if let MolValue::Inline(values) = value
            && let Ok(Some(MollieType::Inline { size, .. })) = self.ir_type(ty)
            && let Ok(pointers) = self.pointer_chunks(ty, size)
        {
            for index in pointers {
                self.fn_builder.declare_value_needs_stack_map(values[index]);
            }
        }
    }

    fn current_value(&mut self, var: Var) -> MolValue {
        match var {
            Var::Regular(var) => MolValue::Value(self.fn_builder.use_var(var)),
            Var::Fat(var, metadata) => MolValue::FatPtr(self.fn_builder.use_var(var), self.fn_builder.use_var(metadata)),
            Var::Inline(vars) => MolValue::Inline(vars.into_iter().map(|var| self.fn_builder.use_var(var)).collect()),
        }
    }

    pub fn push_frame(&mut self) {
        self.frames.push(VariableFrame::new());
    }

    /// Leaves the current scope.
    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    /// Leaves every scope, at the end of the function.
    pub fn pop_all_frames(&mut self) {
        self.frames.clear();
    }

    pub fn get_var(&self, name: &str) -> Option<Variable> {
        self.frames.iter().rev().find_map(|frame| frame.get(name).cloned())
    }

    /// Declares a variable. If it holds a GC reference, every value it gets is
    /// described by stack maps.
    ///
    /// # Errors
    ///
    /// Returns an error if `value` doesn't have the representation of `ty`.
    pub fn declare(&mut self, name: impl Into<String>, ty: TypeRef, value: MolValue) -> CompileResult<()> {
        let name = name.into();
        let gc_pointer = self.gc_pointer(ty, &value);
        let var = match (self.ir_type(ty)?, value) {
            (Some(MollieType::Regular(ty)), MolValue::Value(value)) => {
                let var = self.fn_builder.declare_var(ty);

                if gc_pointer.is_some() {
                    self.fn_builder.declare_var_needs_stack_map(var);
                }

                self.fn_builder.def_var(var, value);

                Var::Regular(var)
            }
            (Some(MollieType::Fat(ty, metadata_ty)), MolValue::FatPtr(value, metadata)) => {
                let var = self.fn_builder.declare_var(ty);
                let metadata_var = self.fn_builder.declare_var(metadata_ty);

                // The GC reference is the first value of trait objects, and the
                // second one (the environment) of functions.
                match gc_pointer {
                    Some(ptr) if ptr == value => self.fn_builder.declare_var_needs_stack_map(var),
                    Some(_) => self.fn_builder.declare_var_needs_stack_map(metadata_var),
                    None => {}
                }

                self.fn_builder.def_var(var, value);
                self.fn_builder.def_var(metadata_var, metadata);

                Var::Fat(var, metadata_var)
            }
            (Some(MollieType::Inline { size, .. }), MolValue::Inline(values)) => {
                let pointers = self.pointer_chunks(ty, size)?;
                let vars = mollie_ir::chunks(size)
                    .into_iter()
                    .zip(values)
                    .enumerate()
                    .map(|(index, ((chunk_ty, _), value))| {
                        let var = self.fn_builder.declare_var(chunk_ty);

                        if pointers.contains(&index) {
                            self.fn_builder.declare_var_needs_stack_map(var);
                        }

                        self.fn_builder.def_var(var, value);

                        var
                    })
                    .collect();

                Var::Inline(vars)
            }
            // `void` variables have no value.
            (None, MolValue::Nothing) => return Ok(()),
            (ty, value) => return Err(CompileError::unsupported(format!("can't create variable `{name}` of {ty:?} with {value:?}"))),
        };

        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name, Variable { value: var, ty });
        }

        Ok(())
    }

    /// Declares a variable bound by a pattern or captured by a closure.
    ///
    /// # Errors
    ///
    /// Returns an error if `value` doesn't have the representation of `ty`.
    pub fn declare_binding(&mut self, name: impl Into<String>, ty: TypeRef, value: MolValue) -> CompileResult<()> {
        self.declare(name, ty, value)
    }

    /// The value of the variable `name`.
    ///
    /// # Errors
    ///
    /// Returns an error if there's no variable called `name`.
    pub fn read_var(&mut self, name: &str) -> CompileResult<MolValue> {
        self.get_var(name).map_or_else(
            || Err(CompileError::unsupported(format!("unknown variable `{name}`"))),
            |variable| Ok(self.current_value(variable.value)),
        )
    }

    /// Gives the variable `name` the value `value`.
    ///
    /// # Errors
    ///
    /// Returns an error if there's no variable called `name`, or `value`
    /// doesn't have its representation.
    pub fn assign_var(&mut self, name: &str, value: MolValue) -> CompileResult<()> {
        let variable = self
            .get_var(name)
            .ok_or_else(|| CompileError::unsupported(format!("unknown variable `{name}`")))?;

        match (variable.value, value) {
            (Var::Regular(var), MolValue::Value(value)) => self.fn_builder.def_var(var, value),
            (Var::Fat(var, metadata_var), MolValue::FatPtr(value, metadata)) => {
                self.fn_builder.def_var(var, value);
                self.fn_builder.def_var(metadata_var, metadata);
            }
            (Var::Inline(vars), MolValue::Inline(values)) if vars.len() == values.len() => {
                for (var, value) in vars.into_iter().zip(values) {
                    self.fn_builder.def_var(var, value);
                }
            }
            (var, value) => return Err(CompileError::unsupported(format!("can't assign {value:?} to {var:?}"))),
        }

        Ok(())
    }

    /// Declares variables for function parameters, starting from block
    /// parameter `index`. Returns the index of the next block parameter.
    ///
    /// # Errors
    ///
    /// Returns an error if there are fewer block parameters than parameters, or
    /// a value type wasn't compiled.
    pub fn bind_params<'n>(&mut self, mut index: usize, params: impl IntoIterator<Item = (&'n str, TypeRef)>) -> CompileResult<usize> {
        for (name, ty) in params {
            let block_params = self.fn_builder.block_params(self.entry_block).to_vec();
            let value = match self.ir_type(ty)? {
                None => continue,
                Some(MollieType::Regular(_)) => {
                    index += 1;

                    MolValue::Value(block_params[index - 1])
                }
                Some(MollieType::Fat(..)) => {
                    index += 2;

                    MolValue::FatPtr(block_params[index - 2], block_params[index - 1])
                }
                Some(MollieType::Inline { size, .. }) => {
                    let count = mollie_ir::chunks(size).len();

                    index += count;

                    MolValue::Inline(block_params[index - count..index].to_vec())
                }
            };

            self.declare(name, ty, value)?;
        }

        Ok(index)
    }

    pub fn return_(&mut self, value: &MolValue) -> ir::Inst {
        let mut values = value.values();

        // The changed receiver of a `mut self` function goes back to the
        // caller after the result.
        if self.returns_self
            && let Ok(this) = self.read_var("self")
        {
            values.extend(this.values());
        }

        self.leave();
        self.fn_builder.ins().return_(&values)
    }

    /// Converts a value of type `from` to type `to`: concrete values become
    /// trait objects (or `any`), other values stay as they are.
    ///
    /// # Errors
    ///
    /// Returns an error if `from` doesn't implement the trait of `to`, or its
    /// vtable wasn't compiled.
    pub fn coerce(&mut self, value: MolValue, from: TypeRef, to: TypeRef) -> CompileResult<MolValue> {
        let from_is_dynamic = matches!(self.types()[self.resolve(from)], Type::Trait(..) | Type::Primitive(PrimitiveType::Any));
        let target = self.types()[self.resolve(to)].clone();

        match (target, value) {
            (_, value) if from_is_dynamic => Ok(value),
            (Type::Trait(trait_ref, trait_args), MolValue::Value(ptr)) => {
                let vtable = self.vtable_ptr(from, trait_ref, &trait_args)?;

                Ok(MolValue::FatPtr(ptr, vtable))
            }
            (Type::Primitive(PrimitiveType::Any), MolValue::Value(value)) => {
                let metadata = self.ptr_const(0);

                Ok(MolValue::FatPtr(value, metadata))
            }
            // Values of value types are boxed: trait objects and `any` point
            // to a GC copy.
            (Type::Trait(trait_ref, trait_args), value @ MolValue::Inline(_)) => {
                let ptr = self.box_value(from, &value)?;
                let vtable = self.vtable_ptr(from, trait_ref, &trait_args)?;

                Ok(MolValue::FatPtr(ptr, vtable))
            }
            (Type::Primitive(PrimitiveType::Any), value @ MolValue::Inline(_)) => {
                let ptr = self.box_value(from, &value)?;
                let metadata = self.ptr_const(0);

                Ok(MolValue::FatPtr(ptr, metadata))
            }
            (_, value) => Ok(value),
        }
    }

    /// A GC copy of the inline `value` of type `ty`.
    ///
    /// # Errors
    ///
    /// Returns an error if `ty` wasn't compiled.
    pub fn box_value(&mut self, ty: TypeRef, value: &MolValue) -> CompileResult<ir::Value> {
        let layout = self.compiled_adt(ty)?.type_layout;
        let ir_type = self.value_type(ty)?;
        // The value's references must survive the allocation.
        self.track(ty, value);

        let ptr = self.alloc(layout);

        self.store_value(ir_type, value, ptr, 0)?;

        Ok(ptr)
    }

    /// Pointer to the vtable of `trait_ref<trait_args...>` implemented for
    /// `ty`.
    ///
    /// # Errors
    ///
    /// Returns an error if the vtable wasn't compiled.
    pub fn vtable_ptr(&mut self, ty: TypeRef, trait_ref: TraitRef, trait_args: &[TypeRef]) -> CompileResult<ir::Value> {
        let trait_name = self.type_context.tcx.name_of_trait(trait_ref).to_owned();
        let trait_args = trait_args.iter().map(|&arg| self.resolve(arg)).collect::<Vec<_>>();
        let impl_ref = self
            .type_context
            .tcx
            .find_trait_impl(self.resolve(ty), trait_ref, &trait_args)
            .ok_or_else(|| CompileError::unsupported(format!("`{}` doesn't implement `{trait_name}`", self.display(ty))))?;

        let hash = self.hash(ty);
        let data_id = *self
            .compiler
            .vtable_data
            .get(&(hash, impl_ref))
            .ok_or_else(|| CompileError::unsupported(format!("vtable of `{}` for `{trait_name}` wasn't compiled", self.display(ty))))?;

        Ok(self.data_addr(data_id))
    }

    /// A function value (code and environment) calling `func_id`, a function
    /// of type `func_ty` that doesn't take an environment.
    ///
    /// # Errors
    ///
    /// Returns an error if the trampoline taking the environment can't be
    /// compiled.
    pub fn func_value(&mut self, func_id: FuncId, func_ty: TypeRef) -> CompileResult<MolValue> {
        let trampoline = if let Some(&trampoline) = self.compiler.trampolines.get(&func_id) {
            trampoline
        } else {
            let target_params = self.signature(func_ty, false)?.params.len();
            let signature = self.signature(func_ty, true)?;
            let id = self.compiler.codegen.module.declare_anonymous_function(&signature)?;
            let mut ctx = self.compiler.codegen.module.make_context();
            let mut fn_builder_ctx = FunctionBuilderContext::new();

            {
                let mut trampoline = FunctionCompiler::new(
                    id,
                    "trampoline",
                    signature,
                    &mut *self.compiler,
                    self.type_context,
                    &mut ctx,
                    &mut fn_builder_ctx,
                    self.generics.clone(),
                );

                // The last parameter is the environment, which plain functions
                // don't have. The caller of the trampoline checks whether the
                // program was stopped.
                let params = trampoline.fn_builder.block_params(trampoline.entry_block).to_vec();
                let results = trampoline.call_unchecked(func_id, &params[..target_params]);

                trampoline.leave();
                trampoline.fn_builder.ins().return_(&results);
                trampoline.finalize();
            }

            self.compiler.define_function(id, &mut ctx)?;
            self.compiler.trampolines.insert(func_id, id);

            id
        };

        let func_ref = self.func_ref(trampoline);
        let ptr_type = self.ptr_type();
        let code = self.fn_builder.ins().func_addr(ptr_type, func_ref);
        let env = self.ptr_const(0);

        Ok(MolValue::FatPtr(code, env))
    }

    /// Address of an array element, trapping if `index` is out of bounds.
    pub fn element_addr(&mut self, array: ir::Value, index: ir::Value, element_size: u32) -> ir::Value {
        let ptr_type = self.ptr_type();
        let length = self.fn_builder.ins().load(ptr_type, ir::MemFlagsData::trusted(), array, ARRAY_LENGTH_OFFSET);
        let out_of_bounds = self.fn_builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, length);

        self.trap_if(out_of_bounds, TrapKind::OutOfBounds);

        let data = self.fn_builder.ins().load(ptr_type, ir::MemFlagsData::trusted(), array, ARRAY_DATA_OFFSET);
        let offset = self.fn_builder.ins().imul_imm_u(index, i64::from(element_size));

        self.fn_builder.ins().iadd(data, offset)
    }

    /// GC layout of array elements of type `ty`.
    ///
    /// # Errors
    ///
    /// Returns an error if a value type in `ty` wasn't compiled.
    pub fn element_layout(&mut self, ty: TypeRef) -> CompileResult<&'static TypeLayout> {
        let ir_type = self.value_type(ty)?;
        let fields = types::layout_fields(&self.type_context.tcx, &self.compiler.adt_types, ty, &self.generics)?;

        Ok(self.compiler.element_layout(ir_type, fields))
    }

    /// Allocates an array of `values` with element type `element`.
    ///
    /// # Errors
    ///
    /// Returns an error if a value type in `element` wasn't compiled.
    pub fn array_of(&mut self, element: TypeRef, values: &[MolValue]) -> CompileResult<ir::Value> {
        let element_type = self.value_type(element)?;
        let layout = self.element_layout(element)?;
        let length = self.ptr_const(values.len());
        let array = self.alloc_array(layout, length);
        let ptr_type = self.ptr_type();
        let data = self.fn_builder.ins().load(ptr_type, ir::MemFlagsData::trusted(), array, ARRAY_DATA_OFFSET);

        for (index, value) in values.iter().enumerate() {
            let offset = i32::try_from(index * element_type.bytes() as usize).map_err(|_| CompileError::unsupported("array literal is too large"))?;

            self.store_value(element_type, value, data, offset)?;
        }

        Ok(array)
    }
}

impl MolValue {
    pub fn values(&self) -> Vec<ir::Value> {
        match *self {
            Self::Value(value) => vec![value],
            Self::FatPtr(value, metadata) => vec![value, metadata],
            Self::Inline(ref values) => values.clone(),
            Self::Nothing => Vec::new(),
        }
    }

    /// # Errors
    ///
    /// Returns an error if the value isn't a single IR value.
    pub fn value(&self) -> CompileResult<ir::Value> {
        match *self {
            Self::Value(value) => Ok(value),
            ref value => Err(CompileError::unsupported(format!("expected a single value, found {value:?}"))),
        }
    }

    /// Converts IR values to a value with the given representation.
    ///
    /// # Errors
    ///
    /// Returns an error if the number of values doesn't match.
    pub fn from_values(ty: Option<MollieType>, values: &[ir::Value]) -> CompileResult<Self> {
        match (ty, values) {
            (None, _) => Ok(Self::Nothing),
            (Some(MollieType::Regular(_)), &[value]) => Ok(Self::Value(value)),
            (Some(MollieType::Fat(..)), &[value, metadata]) => Ok(Self::FatPtr(value, metadata)),
            (Some(MollieType::Inline { size, .. }), values) if values.len() == mollie_ir::chunks(size).len() => Ok(Self::Inline(values.to_vec())),
            (ty, values) => Err(CompileError::unsupported(format!("got {} value(s) for {ty:?}", values.len()))),
        }
    }
}

impl<M: Module> CompilerInner<M> {
    /// GC layout of array elements with the given representation and
    /// references (see [`types::layout_fields`]).
    pub fn element_layout(&mut self, ir_type: MollieType, references: Vec<(u32, MollieType, TypeLayoutField)>) -> &'static TypeLayout {
        let heap = &self.heap;

        self.element_layouts.entry((ir_type, references.clone())).or_insert_with(|| {
            let fields = heap.intern_fields(
                references
                    .into_iter()
                    .map(|(offset, ty, kind)| (AdtVariantRef::ZERO, offset, ty, kind))
                    .collect(),
            );

            heap.intern_layout(TypeLayout {
                fields,
                adt_ty: None,
                size: ir_type.bytes() as usize,
                align: ir_type.align() as usize,
                kind: None,
            })
        })
    }
}
