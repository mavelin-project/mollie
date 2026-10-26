#![allow(missing_docs)]
#![allow(clippy::result_large_err)]

pub mod allocator;
pub mod error;
mod expr;
pub mod func;
mod inline;
mod inlining;
mod items;
pub mod math;
mod nans;
mod runtime;
pub mod sandbox;
pub mod strings;
pub mod types;

use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::HashMap,
    fmt, mem,
    ops::{Deref, DerefMut},
    panic::{self, AssertUnwindSafe},
    ptr::{self, NonNull},
    time::{Duration, Instant},
};

pub use cranelift;
use cranelift::{
    codegen::{
        Context,
        ir::{self, Function},
        print_errors::pretty_error,
    },
    jit::JITModule,
    module::{DataId, FuncId, Linkage, Module, ModuleError, ModuleResult},
    prelude::{AbiParam, Configurable, FunctionBuilder, FunctionBuilderContext, InstBuilder, Variable, isa::TargetIsa, settings, types as ir_types},
};
pub use indexmap::IndexMap;
use mollie_index::{Idx, IndexBoxedSlice};
use mollie_ir::{CodeGenerator, Field, MollieType, Symbol};
use mollie_typed_ast::{BlockRef, ModuleLoader, Stmt, StmtRef, TypedAST, TypedASTContext};
use mollie_typing::{AdtVariantRef, FieldRef, FuncRef, ImplRef, ModuleId, TraitRef, Type, TypeRef, VFuncRef};

use crate::{
    allocator::{Heap, HeapStats, TypeLayout, TypeLayoutField},
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
    sandbox::{Limits, Trap, TrapSites, VmState},
};

#[derive(Debug)]
pub struct CompiledAdtVariant {
    pub fields: IndexBoxedSlice<FieldRef, (Field, TypeRef)>,
}

#[derive(Debug)]
pub struct CompiledAdt {
    pub type_layout: &'static TypeLayout,
    pub name: Option<String>,
    pub applied_generics: usize,
    pub variants: IndexBoxedSlice<AdtVariantRef, CompiledAdtVariant>,
}

impl CompiledAdt {
    pub fn main_variant(&self) -> &CompiledAdtVariant {
        &self.variants[AdtVariantRef::ZERO]
    }
}

#[derive(Debug, Clone)]
pub enum Var {
    Regular(Variable),
    Fat(Variable, Variable),
    Inline(Vec<Variable>),
}

pub type VTable = IndexMap<VFuncRef, FuncId>;

pub type MethodKey = (u64, ImplRef, VFuncRef, u64);

#[derive(Debug, Clone, Copy)]
pub struct Runtime {
    pub alloc: FuncId,
    pub alloc_array: FuncId,
    pub realloc_array: FuncId,
    pub str_eq: FuncId,
    pub str_concat: FuncId,
    pub str_slice: FuncId,
    pub str_from_int: FuncId,
    pub str_from_uint: FuncId,
    pub str_from_f32: FuncId,
    pub str_from_f64: FuncId,
    pub str_from_bool: FuncId,
    pub str_format_int: FuncId,
    pub str_format_uint: FuncId,
    pub str_format_f32: FuncId,
    pub str_format_f64: FuncId,
    pub str_format_str: FuncId,
    pub out_of_fuel: FuncId,
    pub panic: FuncId,
    pub exit_push: FuncId,
    pub exit_pop: FuncId,
    pub stack_array_push: FuncId,
    pub stack_array_pop: FuncId,
}

pub type ElementKey = (MollieType, Vec<(u32, MollieType, TypeLayoutField)>);

pub type StackMaps = Vec<(u32, Box<[u32]>)>;

pub struct CompilerInner<M: Module = JITModule> {
    pub codegen: CodeGenerator<M>,
    pub heap: Box<Heap>,
    pub poisoned: bool,
    pub timings: CompileTimings,
    pub trap_sites: Box<TrapSites>,
    pub runtime: Runtime,
    pub vm_state_data: DataId,
    pub adt_types: IndexMap<u64, CompiledAdt>,
    pub vtables: IndexMap<(u64, ImplRef), VTable>,
    pub method_instances: IndexMap<MethodKey, FuncId>,
    pub vtable_data: IndexMap<(u64, ImplRef), DataId>,
    pub trait_to_vtable: IndexMap<(u64, TraitRef), DataId>,
    pub trampolines: IndexMap<FuncId, FuncId>,
    pub element_layouts: IndexMap<ElementKey, &'static TypeLayout>,
    pub strings: IndexMap<String, DataId>,
    pub exit_wrappers: IndexMap<FuncId, FuncId>,
    pub pending_code: Vec<(FuncId, usize, StackMaps)>,

    pub name_to_func_id: IndexMap<String, FuncId>,
    pub builtins: IndexMap<&'static str, FuncId>,
    pub name_to_entry: IndexMap<String, FuncId>,
    pub program_types: IndexMap<String, TypeRef>,
    pub func_id_to_name: IndexMap<FuncId, String>,
    pub func_id_to_func: IndexMap<FuncId, Function>,
    pub inline_bodies: HashMap<FuncId, Function>,
    pub pending_bodies: IndexMap<FuncId, (Function, bool)>,
    pub fuel: bool,
    pub func_ref_to_func_id: IndexMap<(FuncRef, u64), FuncId>,
}

pub struct Compiler<ML: ModuleLoader, M: Module = JITModule> {
    pub inner: CompilerInner<M>,
    pub type_context: TypedASTContext,
    pub module_loader: ML,
    pub host_data: Vec<Box<dyn Any + Send>>,
    pub host_types: HostTypes,
    pub program_signatures: HashMap<String, TypeId>,
    pub capabilities: HashMap<String, ModuleId>,
    pub host_defaults: HashMap<FuncRef, String>,
}

impl<M: Module> CompilerInner<M> {
    pub fn isa(&self) -> &dyn TargetIsa {
        self.codegen.module.isa()
    }

    pub fn ptr_type(&self) -> ir::Type {
        self.isa().pointer_type()
    }

    pub fn find_adt<T: AsRef<str>>(&self, name: T) -> Option<&CompiledAdt> {
        let name = name.as_ref();

        self.adt_types
            .values()
            .find(|&adt| adt.applied_generics == 0 && adt.name.as_deref() == Some(name))
    }

    pub fn get_adt(&self, hash: u64) -> Option<&CompiledAdt> {
        self.adt_types.get(&hash)
    }

    pub fn get_adt_variant(&self, hash: u64, variant: AdtVariantRef) -> Option<&CompiledAdtVariant> {
        self.adt_types.get(&hash).and_then(|adt| adt.variants.get(variant))
    }

    pub fn define_function(&mut self, id: FuncId, ctx: &mut Context) -> CompileResult<()> {
        let started = Instant::now();
        let module = &self.codegen.module;

        nans::canonicalize(&mut ctx.func, |func| module.declarations().get_function_decl(func).linkage == Linkage::Import);

        let defined = self.codegen.module.define_function(id, ctx);

        self.timings.machine_code += started.elapsed();
        self.timings.functions += 1;

        if let Err(error) = defined {
            return Err(match error {
                ModuleError::Compilation(error) => CompileError::Codegen(pretty_error(&ctx.func, error)),
                error => CompileError::Module(Box::new(error)),
            });
        }

        // Stack maps tell the garbage collector where GC references are while a
        // call is in progress.
        if let Some(compiled) = ctx.compiled_code() {
            let stack_maps = compiled
                .buffer
                .user_stack_maps()
                .iter()
                .map(|(return_addr, _, stack_map)| (*return_addr, stack_map.entries().map(|(_, offset)| offset).collect()))
                .collect();

            self.pending_code.push((id, compiled.code_buffer().len(), stack_maps));
        }

        self.func_id_to_func.insert(id, mem::replace(&mut ctx.func, Function::new()));

        Ok(())
    }

    pub fn define_code(&mut self, id: FuncId, ctx: &mut Context, inlinable: bool) {
        self.pending_bodies.insert(id, (mem::replace(&mut ctx.func, Function::new()), inlinable));
    }

    pub fn define_pending_code(&mut self) -> CompileResult<()> {
        let mut pending = mem::take(&mut self.pending_bodies);
        let mut ctx = self.codegen.module.make_context();

        for id in inlining::callees_first(&pending) {
            let Some((func, inlinable)) = pending.swap_remove(&id) else {
                continue;
            };

            ctx.clear();
            ctx.func = func;

            let globals = ctx.func.global_values.len();
            let mut inliner = inlining::Inliner::new(&self.inline_bodies);

            ctx.inline(&mut inliner)
                .map_err(|error| CompileError::Codegen(pretty_error(&ctx.func, error)))?;
            inliner.fix_symbols(&mut ctx.func, globals);

            if inlinable && inlining::instructions(&ctx.func) <= inlining::MAX_BODY {
                self.inline_bodies.insert(id, ctx.func.clone());
            }

            self.define_function(id, &mut ctx)?;
        }

        Ok(())
    }

    pub fn exit_wrapper(&mut self, target: FuncId, signature: ir::Signature) -> CompileResult<FuncId> {
        if let Some(&wrapper) = self.exit_wrappers.get(&target) {
            return Ok(wrapper);
        }

        let id = self.codegen.module.declare_anonymous_function(&signature)?;
        let mut ctx = self.codegen.module.make_context();
        let mut fn_builder_ctx = FunctionBuilderContext::new();
        let ptr_type = self.ptr_type();

        ctx.func.signature = signature;

        {
            let mut fn_builder = FunctionBuilder::new(&mut ctx.func, &mut fn_builder_ctx);
            let entry_block = fn_builder.create_block();

            fn_builder.append_block_params_for_function_params(entry_block);
            fn_builder.switch_to_block(entry_block);
            fn_builder.seal_block(entry_block);

            let params = fn_builder.block_params(entry_block).to_vec();
            let exit_push = self.codegen.module.declare_func_in_func(self.runtime.exit_push, fn_builder.func);
            let exit_pop = self.codegen.module.declare_func_in_func(self.runtime.exit_pop, fn_builder.func);
            let target_ref = self.codegen.module.declare_func_in_func(target, fn_builder.func);
            let frame_pointer = fn_builder.ins().get_frame_pointer(ptr_type);

            fn_builder.ins().call(exit_push, &[frame_pointer]);

            let call = fn_builder.ins().call(target_ref, &params);
            let results = fn_builder.inst_results(call).to_vec();

            fn_builder.ins().call(exit_pop, &[]);
            fn_builder.ins().return_(&results);
            fn_builder.finalize(self.codegen.module.isa().frontend_config());
        }

        self.define_function(id, &mut ctx)?;
        self.exit_wrappers.insert(target, id);

        Ok(id)
    }

    fn signature_of<P: IntoIterator<Item = ir::Type>, R: IntoIterator<Item = ir::Type>>(&self, params: P, returns: R) -> ir::Signature {
        let mut signature = self.codegen.module.make_signature();

        signature.params.extend(params.into_iter().map(AbiParam::new));
        signature.returns.extend(returns.into_iter().map(AbiParam::new));

        signature
    }

    fn define_vm_state(&mut self) -> ModuleResult<DataId> {
        let state = VmState::UNLIMITED;
        let mut contents = vec![0; size_of::<VmState>()];
        let heap = ptr::from_ref::<Heap>(&self.heap).addr();
        let sites = ptr::from_ref::<TrapSites>(&self.trap_sites).addr();

        contents[sandbox::FUEL_OFFSET as usize..][..size_of::<i64>()].copy_from_slice(&state.fuel.to_ne_bytes());
        contents[sandbox::HEAP_OFFSET as usize..][..size_of::<usize>()].copy_from_slice(&heap.to_ne_bytes());
        contents[sandbox::SITES_OFFSET as usize..][..size_of::<usize>()].copy_from_slice(&sites.to_ne_bytes());

        let codegen = &mut self.codegen;

        codegen.data_desc.define(contents.into_boxed_slice());
        codegen.data_desc.set_align(align_of::<VmState>() as u64);

        let id = codegen.module.declare_anonymous_data(true, false)?;
        let result = codegen.module.define_data(id, &codegen.data_desc);

        codegen.data_desc.clear();
        result.map(|()| id)
    }

    pub fn string_object(&mut self, value: &str) -> Result<DataId, Box<ModuleError>> {
        if let Some(&id) = self.strings.get(value) {
            return Ok(id);
        }

        let ptr_size = self.ptr_type().bytes() as usize;
        let bytes = if value.is_empty() {
            None
        } else {
            Some(self.codegen.static_data(value.as_bytes())?)
        };

        // `length`, `capacity` and `ptr` of the array.
        let mut contents = vec![0; 3 * ptr_size];

        contents[..ptr_size].copy_from_slice(&value.len().to_ne_bytes()[..ptr_size]);
        contents[ptr_size..2 * ptr_size].copy_from_slice(&value.len().to_ne_bytes()[..ptr_size]);

        let codegen = &mut self.codegen;

        codegen.data_desc.define(contents.into_boxed_slice());
        codegen.data_desc.set_align(ptr_size as u64);

        if let Some(bytes) = bytes {
            let bytes = codegen.module.declare_data_in_data(bytes, &mut codegen.data_desc);

            codegen.data_desc.write_data_addr(u32::try_from(2 * ptr_size).unwrap_or(u32::MAX), bytes, 0);
        }

        let id = codegen.module.declare_anonymous_data(false, false)?;
        let result = codegen.module.define_data(id, &codegen.data_desc);

        codegen.data_desc.clear();
        result?;

        self.strings.insert(value.to_owned(), id);

        Ok(id)
    }
}

impl<ML: ModuleLoader, M: Module> Compiler<ML, M> {
    pub const fn set_fuel(&mut self, fuel: bool) {
        self.inner.fuel = fuel;
    }

    fn import_fn<P: IntoIterator<Item = ir::Type>, R: IntoIterator<Item = ir::Type>>(&mut self, name: &str, params: P, returns: R) -> ModuleResult<FuncId> {
        let signature = self.inner.signature_of(params, returns);
        let id = self.inner.codegen.module.declare_function(name, Linkage::Import, &signature)?;

        self.inner.func_id_to_name.insert(id, name.to_owned());
        self.inner.name_to_func_id.insert(name.to_owned(), id);

        Ok(id)
    }

    fn import_wrapped(&mut self, name: &str, params: &[ir::Type], returns: &[ir::Type]) -> CompileResult<FuncId> {
        let target = self.import_fn(name, params.iter().copied(), returns.iter().copied())?;
        let signature = self.inner.signature_of(params.iter().copied(), returns.iter().copied());

        self.inner.exit_wrapper(target, signature)
    }
}

impl<ML: ModuleLoader> Compiler<ML> {
    pub fn with_symbols<I: IntoIterator<Item = Symbol>>(module_loader: ML, symbols: I) -> CompileResult<Self> {
        let mut flag_builder = settings::builder();

        for (name, value) in [
            ("use_colocated_libcalls", "false"),
            ("opt_level", "speed"),
            ("is_pic", "false"),
            ("preserve_frame_pointers", "true"),
            // NaNs have the same bits on every platform where programs can
            // see them, which is cheaper than canonicalizing every result
            // (see `nans.rs`).
            ("enable_nan_canonicalization", "false"),
            // Values of value types are returned as several values (`Option`
            // of a trait object is three), more than fit in registers. Only
            // compiled code calls such functions: host functions write their
            // results to a slot, and programs can't return value types.
            ("enable_multi_ret_implicit_sret", "true"),
        ] {
            flag_builder.set(name, value).expect("invalid Cranelift setting");
        }

        let type_context = TypedASTContext::default();

        let codegen = CodeGenerator::new(symbols.into_iter().chain(runtime::symbols()), settings::Flags::new(flag_builder));
        let ptr_type = codegen.module.isa().pointer_type();
        // Placeholders, replaced right below.
        let placeholder = FuncId::from_u32(0);

        let mut compiler = Self {
            inner: CompilerInner {
                codegen,
                heap: Box::new(Heap::new()),
                poisoned: false,
                timings: CompileTimings::default(),
                trap_sites: Box::default(),
                vm_state_data: DataId::from_u32(0),
                runtime: Runtime {
                    alloc: placeholder,
                    alloc_array: placeholder,
                    realloc_array: placeholder,
                    str_eq: placeholder,
                    str_concat: placeholder,
                    str_slice: placeholder,
                    str_from_int: placeholder,
                    str_from_uint: placeholder,
                    str_from_f32: placeholder,
                    str_from_f64: placeholder,
                    str_from_bool: placeholder,
                    str_format_int: placeholder,
                    str_format_uint: placeholder,
                    str_format_f32: placeholder,
                    str_format_f64: placeholder,
                    str_format_str: placeholder,
                    out_of_fuel: placeholder,
                    panic: placeholder,
                    exit_push: placeholder,
                    exit_pop: placeholder,
                    stack_array_push: placeholder,
                    stack_array_pop: placeholder,
                },
                adt_types: IndexMap::new(),
                vtables: IndexMap::new(),
                method_instances: IndexMap::new(),
                vtable_data: IndexMap::new(),
                trait_to_vtable: IndexMap::new(),
                trampolines: IndexMap::new(),
                element_layouts: IndexMap::new(),
                strings: IndexMap::new(),
                exit_wrappers: IndexMap::new(),
                pending_code: Vec::new(),
                name_to_func_id: IndexMap::new(),
                name_to_entry: IndexMap::new(),
                builtins: IndexMap::new(),
                program_types: IndexMap::new(),
                func_id_to_name: IndexMap::new(),
                func_id_to_func: IndexMap::new(),
                inline_bodies: HashMap::new(),
                pending_bodies: IndexMap::new(),
                fuel: true,
                func_ref_to_func_id: IndexMap::new(),
            },
            type_context,
            module_loader,
            host_data: Vec::new(),
            host_types: HostTypes::default(),
            program_signatures: HashMap::new(),
            host_defaults: HashMap::new(),
            capabilities: HashMap::new(),
        };

        compiler.inner.vm_state_data = compiler.inner.define_vm_state()?;
        compiler.inner.runtime.exit_push = compiler.import_fn("molexit_push", [ptr_type], [])?;
        compiler.inner.runtime.exit_pop = compiler.import_fn("molexit_pop", [], [])?;
        compiler.inner.runtime.stack_array_push = compiler.import_fn("molstack_push", [ptr_type, ptr_type], [])?;
        compiler.inner.runtime.stack_array_pop = compiler.import_fn("molstack_pop", [], [])?;
        compiler.inner.runtime.realloc_array = compiler.import_wrapped("molrealloc_arr", &[ptr_type, ptr_type], &[ir_types::I8])?;
        compiler.inner.runtime.str_eq = compiler.import_fn("molstr_eq", [ptr_type, ptr_type], [ir_types::I8])?;

        // The host may run compiled code to get more fuel.
        compiler.inner.runtime.out_of_fuel = compiler.import_wrapped("molvm_out_of_fuel", &[], &[])?;
        // It doesn't allocate, so it needs no wrapper.
        compiler.inner.runtime.panic = compiler.import_fn("molvm_panic", [ptr_type], [])?;

        // Functions creating strings allocate, so they go through wrappers too.
        compiler.inner.runtime.str_concat = compiler.import_wrapped("molstr_concat", &[ptr_type, ptr_type], &[ptr_type])?;
        compiler.inner.runtime.str_slice = compiler.import_wrapped("molstr_slice", &[ptr_type, ptr_type, ptr_type], &[ptr_type])?;
        compiler.inner.runtime.str_from_int = compiler.import_wrapped("molstr_from_int", &[ir_types::I64], &[ptr_type])?;
        compiler.inner.runtime.str_from_uint = compiler.import_wrapped("molstr_from_uint", &[ir_types::I64], &[ptr_type])?;
        compiler.inner.runtime.str_from_f32 = compiler.import_wrapped("molstr_from_f32", &[ir_types::F32], &[ptr_type])?;
        compiler.inner.runtime.str_from_f64 = compiler.import_wrapped("molstr_from_f64", &[ir_types::F64], &[ptr_type])?;
        compiler.inner.runtime.str_from_bool = compiler.import_wrapped("molstr_from_bool", &[ir_types::I8], &[ptr_type])?;
        compiler.inner.runtime.str_format_int = compiler.import_wrapped("molstr_format_int", &[ir_types::I64, ir_types::I64], &[ptr_type])?;
        compiler.inner.runtime.str_format_uint = compiler.import_wrapped("molstr_format_uint", &[ir_types::I64, ir_types::I64], &[ptr_type])?;
        compiler.inner.runtime.str_format_f32 = compiler.import_wrapped("molstr_format_f32", &[ir_types::F32, ir_types::I64], &[ptr_type])?;
        compiler.inner.runtime.str_format_f64 = compiler.import_wrapped("molstr_format_f64", &[ir_types::F64, ir_types::I64], &[ptr_type])?;
        compiler.inner.runtime.str_format_str = compiler.import_wrapped("molstr_format_str", &[ptr_type, ir_types::I64], &[ptr_type])?;

        // Built-ins implemented by the runtime. Those allocating go through
        // wrappers recording the caller's frame.
        for builtin in runtime::builtins() {
            let ir_type = |value: runtime::Value| match value {
                runtime::Value::Ptr => ptr_type,
                runtime::Value::I8 => ir_types::I8,
                runtime::Value::I32 => ir_types::I32,
                runtime::Value::I64 => ir_types::I64,
                runtime::Value::F32 => ir_types::F32,
                runtime::Value::F64 => ir_types::F64,
            };
            let params = builtin.params.iter().map(|&value| ir_type(value)).collect::<Vec<_>>();
            let returns = builtin.returns.map(ir_type).into_iter().collect::<Vec<_>>();
            let id = if builtin.allocates {
                compiler.import_wrapped(builtin.symbol, &params, &returns)?
            } else {
                compiler.import_fn(builtin.symbol, params, returns)?
            };

            compiler.inner.builtins.insert(builtin.name, id);
        }

        // Allocations may collect garbage, so they go through wrappers
        // recording the caller's frame.
        let alloc = compiler.import_fn("molalloc", [ptr_type], [ptr_type])?;
        let alloc_signature = compiler.inner.signature_of([ptr_type], [ptr_type]);
        let alloc_array = compiler.import_fn("molalloc_arr", [ptr_type, ptr_type], [ptr_type])?;
        let alloc_array_signature = compiler.inner.signature_of([ptr_type, ptr_type], [ptr_type]);

        compiler.inner.runtime.alloc = compiler.inner.exit_wrapper(alloc, alloc_signature)?;
        compiler.inner.runtime.alloc_array = compiler.inner.exit_wrapper(alloc_array, alloc_array_signature)?;

        Ok(compiler)
    }
}

impl CompilerInner {
    fn register_pending_code(&mut self) {
        for (id, size, stack_maps) in self.pending_code.drain(..) {
            let start = self.codegen.module.get_finalized_function(id).addr();

            self.heap.register_code(start, size, stack_maps);
        }
    }

    pub fn vm_state(&self) -> NonNull<VmState> {
        let (state, _) = self.codegen.module.get_finalized_data(self.vm_state_data);

        NonNull::new(state.cast_mut().cast()).expect("finalized data isn't null")
    }

    pub fn run<R>(&self, limits: Limits, call: impl FnOnce() -> R) -> Result<R, Trap> {
        // SAFETY: the state is the one used by functions of this compiler.
        unsafe { sandbox::run(self.vm_state(), limits, call) }
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn heap_stats(&self) -> HeapStats {
        self.heap.stats()
    }

    pub fn collect_garbage(&self) {
        // SAFETY: compiled code on the stack (if the host is called by it) left
        // through a recorded wrapper, and its arguments are pinned.
        unsafe { self.heap.collect() };
    }

    pub fn collect_garbage_if_due(&self) -> bool {
        // SAFETY: as for `collect_garbage`.
        unsafe { self.heap.collect_if_due() }
    }

    pub fn is_running(&self) -> bool {
        sandbox::current_state() == Some(self.vm_state())
    }

    /// # Safety
    ///
    /// `T` must be a function pointer with the compiled function's exact
    /// signature, as compiled code passes arguments and returns values. It's
    /// valid only while this compiler and its code live.
    pub unsafe fn get_func<T>(&self, name: impl AsRef<str>) -> Option<T> {
        assert_eq!(mem::size_of::<T>(), mem::size_of::<*const u8>());

        self.name_to_func_id.get(name.as_ref()).map(|&func_id| {
            let code = self.codegen.module.get_finalized_function(func_id);

            unsafe { mem::transmute_copy::<*const u8, T>(&code) }
        })
    }

    pub fn get_entry(&self, name: impl AsRef<str>) -> Option<usize> {
        self.name_to_entry
            .get(name.as_ref())
            .map(|&func_id| self.codegen.module.get_finalized_function(func_id).addr())
    }

    /// # Safety
    ///
    /// `T` must be the vtable's functions: function pointers of the trait's
    /// functions, in their order, each with the signature compiled for the type
    /// of `hash`. They're valid only while this compiler and its code live.
    pub unsafe fn get_vtable_ptr<T: Copy>(&self, hash: u64, trait_ref: TraitRef) -> Option<T> {
        let vtable = *self.trait_to_vtable.get(&(hash, trait_ref))?;
        let (vtable_ptr, vtable_size) = self.codegen.module.get_finalized_data(vtable);

        assert_eq!(size_of::<T>() + size_of::<usize>(), vtable_size);

        Some(unsafe { vtable_ptr.byte_add(size_of::<usize>()).cast::<T>().read_unaligned() })
    }
}

const _: () = {
    const fn send<T: Send>() {}

    send::<Compiler<()>>();
};

impl<ML: ModuleLoader, M: Module> Compiler<ML, M> {
    pub fn start_compiling(&mut self) -> FuncCompiler<'_, ML, M> {
        FuncCompiler {
            ctx: self.inner.codegen.module.make_context(),
            fn_builder_ctx: FunctionBuilderContext::new(),
            compiler: &mut self.inner,
            type_context: &mut self.type_context,
            module_loader: &mut self.module_loader,
        }
    }
}

#[derive(Default)]
pub struct HostTypes {
    types: HashMap<TypeId, TypeRef>,
    pub callbacks: RefCell<Vec<(TypeId, TypeRef)>>,
}

impl Deref for HostTypes {
    type Target = HashMap<TypeId, TypeRef>;

    fn deref(&self) -> &Self::Target {
        &self.types
    }
}

impl DerefMut for HostTypes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.types
    }
}

pub struct FuncCompiler<'a, ML: ModuleLoader, M: Module> {
    pub compiler: &'a mut CompilerInner<M>,
    pub type_context: &'a mut TypedASTContext,
    pub module_loader: &'a mut ML,

    ctx: Context,
    fn_builder_ctx: FunctionBuilderContext,
}

impl<ML: ModuleLoader> FuncCompiler<'_, ML, JITModule> {
    pub fn compile<N: Into<String>, I: IntoIterator<Item = (N, TypeRef)>>(
        &mut self,
        name: &str,
        params: I,
        returns: Option<TypeRef>,
        text: &str,
    ) -> CompileResult<FuncId> {
        if self.compiler.poisoned {
            return Err(CompileError::Internal(String::from("the compiler failed before, create a new one")));
        }

        let params = params.into_iter().map(|(name, ty)| (name.into(), ty)).collect::<Vec<_>>();
        // A bug in the compiler must not take the host down with it: the panic
        // becomes an error, and the compiler (whose state may be inconsistent)
        // refuses to compile anything else.
        let result = panic::catch_unwind(AssertUnwindSafe(|| self.compile_program(name, &params, returns, text))).unwrap_or_else(|payload| {
            let message = payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| String::from("the compiler panicked"));

            self.compiler.poisoned = true;

            Err(CompileError::Internal(message))
        });

        if result.is_err() {
            // A function may have been left unfinished, which leaves the
            // builder context in use.
            self.fn_builder_ctx = FunctionBuilderContext::new();
        }

        result
    }

    fn compile_program(&mut self, name: &str, params: &[(String, TypeRef)], returns: Option<TypeRef>, text: &str) -> CompileResult<FuncId> {
        let started = Instant::now();

        self.compiler.timings = CompileTimings::default();

        let returns = returns.unwrap_or(self.type_context.tcx.types.core_types.void);
        let (ast, block) = self.type_context.process(&mut *self.module_loader, text, params.to_vec(), returns);

        self.compiler.timings.type_check = started.elapsed();

        let started = Instant::now();

        if !self.type_context.diagnostics.is_empty() {
            return Err(CompileError::Type(mem::take(&mut self.type_context.diagnostics.errors).into_values().collect()));
        }

        self.compile_used_items(&ast.used_items)?;

        let func_ty = self
            .type_context
            .tcx
            .types
            .get_or_add(Type::Func(params.iter().map(|(_, ty)| *ty).collect(), returns));

        self.compile_value_types(func_ty, &[])?;

        let signature = types::signature(
            &self.type_context.tcx,
            &self.compiler.adt_types,
            func_ty,
            &[],
            self.compiler.isa(),
            self.compiler.codegen.module.make_signature(),
            false,
        )?;
        // Programs are found by name with `get_func`. The function itself is
        // anonymous, so a program can be compiled again with the same name
        // (e.g. when it's reloaded), replacing the previous one.
        let id = self.compiler.codegen.module.declare_anonymous_function(&signature)?;

        {
            let mut compiler = FunctionCompiler::new(
                id,
                name,
                signature,
                &mut *self.compiler,
                &*self.type_context,
                &mut self.ctx,
                &mut self.fn_builder_ctx,
                Box::new([]),
            );

            compiler.return_ty = Some(returns);
            compiler.bind_params(0, params.iter().map(|(name, ty)| (name.as_str(), *ty)))?;
            compiler.consume_fuel();

            let returned = block.compile(&ast, &mut compiler)?;
            let returned = compiler.coerce(returned, ast[block].ty, returns)?;

            compiler.pop_all_frames();
            compiler.return_(&returned);
            compiler.finalize();
        }

        self.compiler.define_code(id, &mut self.ctx, false);

        let entry = self.entry_wrapper(id, func_ty)?;

        self.compiler.timings.code = started.elapsed();

        let started = Instant::now();

        self.compiler.define_pending_code()?;
        self.compiler.codegen.module.finalize_definitions()?;
        self.compiler.register_pending_code();

        self.compiler.timings.finalize = started.elapsed();
        self.compiler.name_to_func_id.insert(name.to_owned(), id);
        self.compiler.name_to_entry.insert(name.to_owned(), entry);
        self.compiler.program_types.insert(name.to_owned(), func_ty);
        self.compiler.func_id_to_name.insert(id, name.to_owned());

        Ok(id)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompileTimings {
    pub type_check: Duration,
    pub code: Duration,
    pub machine_code: Duration,
    pub functions: usize,
    pub finalize: Duration,
}

impl fmt::Display for CompileTimings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "type checking {:.1?}, code {:.1?} (machine code {:.1?} for {} functions), finalizing {:.1?}",
            self.type_check, self.code, self.machine_code, self.functions, self.finalize
        )
    }
}

#[derive(Debug, Clone)]
pub enum MolValue {
    Value(ir::Value),
    FatPtr(ir::Value, ir::Value),
    Inline(Vec<ir::Value>),
    Nothing,
}

pub trait CompileTypedAST<M: Module, T> {
    fn compile(self, ast: &TypedAST, compiler: &mut FunctionCompiler<'_, M>) -> CompileResult<T>;
}

impl<M: Module> CompileTypedAST<M, MolValue> for StmtRef {
    fn compile(self, ast: &TypedAST, compiler: &mut FunctionCompiler<'_, M>) -> CompileResult<MolValue> {
        match &ast[self] {
            &Stmt::Expr(expr) => expr.compile(ast, compiler),
            Stmt::NewVar { name, value, .. } => {
                let compiled_value = value.compile(ast, compiler)?;

                compiler.declare(name.clone(), ast[*value].ty, compiled_value)?;

                Ok(MolValue::Nothing)
            }
        }
    }
}

impl<M: Module> CompileTypedAST<M, MolValue> for BlockRef {
    fn compile(self, ast: &TypedAST, compiler: &mut FunctionCompiler<'_, M>) -> CompileResult<MolValue> {
        compiler.push_frame();

        for &statement in &ast[self].value.stmts {
            statement.compile(ast, compiler)?;
        }

        let returned = match ast[self].value.expr {
            // The value of the block may be converted to its type (to a trait
            // object, for a `let` with that type).
            Some(expr) => {
                let value = expr.compile(ast, compiler)?;

                compiler.coerce(value, ast[expr].ty, ast[self].ty)?
            }
            None => MolValue::Nothing,
        };

        compiler.pop_frame();

        Ok(returned)
    }
}
