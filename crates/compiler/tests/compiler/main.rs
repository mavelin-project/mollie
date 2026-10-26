#![allow(missing_docs)]

mod arguments;
mod arithmetic;
mod arrays;
mod bounds;
mod closures;
mod constants;
mod containers;
mod control_flow;
mod defaults;
mod determinism;
mod enums;
mod errors;
mod fixed_arrays;
mod formatting;
mod fuzz;
mod gc;
mod generics;
mod host;
mod inlining;
mod iteration;
mod loops;
mod patterns;
mod ranges;
mod reload;
mod returns;
mod sandbox;
mod std_lib;
mod strings;
mod structs;
mod traits;
mod values;
mod views;

use std::sync::{
    Mutex, MutexGuard, PoisonError,
    atomic::{AtomicI32, AtomicUsize, Ordering},
};

use mollie_compiler::{
    Compiler,
    error::CompileError,
    sandbox::{self as sndbx, Limits, Trap},
};
use mollie_index::Idx;
use mollie_shared::Span;
use mollie_typed_ast::FunctionBody;
use mollie_typing::{ArrayKind, CoreTypes, Func, ModuleId, Type, TypeRef};

static LOCK: Mutex<()> = Mutex::new(());

pub fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

pub static TOUCHED: AtomicUsize = AtomicUsize::new(0);
pub static RECORDED: AtomicI32 = AtomicI32::new(0);

extern "C" fn touch() -> i8 {
    TOUCHED.fetch_add(1, Ordering::SeqCst);

    1
}

extern "C" fn record(value: i32) {
    RECORDED.store(value, Ordering::SeqCst);
}

extern "C" fn fail() {
    assert!(sndbx::raise("failed by the host"), "`fail` must be called by a running program");
}

const unsafe extern "C" fn get_size(array: *const usize) -> usize {
    unsafe { array.read() }
}

const extern "C" fn reveal() -> i32 {
    42
}

pub fn compiler() -> Compiler<()> {
    let mut compiler = Compiler::with_symbols((), [
        ("test_get_size", get_size as *const u8),
        ("test_touch", touch as *const u8),
        ("test_record", record as *const u8),
        ("test_fail", fail as *const u8),
        ("test_reveal", reveal as *const u8),
    ])
    .unwrap_or_else(|error| panic!("can't create the compiler: {error}"));
    let secret = compiler
        .type_context
        .tcx
        .def_registry
        .register_module("secret", Span::default())
        .unwrap_or_else(|error| panic!("can't register `secret`: {:?}", error.error));

    let core_types = compiler.type_context.tcx.types.core_types;
    let any_array = compiler.type_context.tcx.types.get_or_add(Type::Array(core_types.any, ArrayKind::Growable));

    for (module, name, import, args, returns) in [
        (ModuleId::ZERO, "get_size", "test_get_size", vec![any_array], core_types.usize),
        (ModuleId::ZERO, "touch", "test_touch", vec![], core_types.bool),
        (ModuleId::ZERO, "record", "test_record", vec![core_types.i32], core_types.void),
        (ModuleId::ZERO, "fail", "test_fail", vec![], core_types.void),
        (secret, "reveal", "test_reveal", vec![], core_types.i32),
    ] {
        let ty = compiler.type_context.tcx.types.get_or_add(Type::Func(args.into(), returns));
        let func_ref = compiler
            .type_context
            .tcx
            .def_registry
            .register_func_in_module(
                module,
                Func {
                    postfix: false,
                    generics: 0,
                    name: name.to_owned(),
                    arg_names: Vec::new(),
                    ty,
                },
                Span::default(),
            )
            .unwrap_or_else(|error| panic!("can't register `{name}`: {:?}", error.error));

        compiler.type_context.functions.insert(func_ref, FunctionBody::Import(import));
    }

    compiler.type_context.tcx.def_registry.restrict(secret);

    compiler
}

#[track_caller]
pub fn run_limited<R: Copy>(source: &str, returns: impl FnOnce(&CoreTypes<TypeRef>) -> TypeRef, limits: Limits, stress: bool) -> Result<R, Trap> {
    let _guard = lock();
    let mut compiler = compiler();
    let returns = returns(&compiler.type_context.tcx.types.core_types);
    let mut provider = compiler.start_compiling();

    if let Err(error) = provider.compile("main", Vec::<(String, TypeRef)>::new(), Some(returns), source) {
        panic!("compilation failed:\n{}\n\nprogram:\n{source}", error.display(&provider.type_context.tcx));
    }

    let main = unsafe { provider.compiler.get_func::<extern "C" fn() -> R>("main") }.expect("`main` must be compiled");

    // The heap is the compiler's, so stress mode ends with it.
    provider.compiler.heap().set_stress(stress);
    provider.compiler.run(limits, || main())
}

#[track_caller]
pub fn run_with<R: Copy>(source: &str, returns: impl FnOnce(&CoreTypes<TypeRef>) -> TypeRef, stress: bool) -> R {
    run_limited(source, returns, Limits::default(), stress).unwrap_or_else(|trap| panic!("the program trapped: {trap}\n\nprogram:\n{source}"))
}

#[track_caller]
pub fn run_i32(source: &str) -> i32 {
    run_with(source, |types| types.i32, false)
}

#[track_caller]
pub fn run_i32_stressed(source: &str) -> i32 {
    run_with(source, |types| types.i32, true)
}

#[track_caller]
pub fn run_bool(source: &str) -> bool {
    run_with::<i8>(source, |types| types.bool, false) != 0
}

#[track_caller]
pub fn run_f32(source: &str) -> f32 {
    run_with(source, |types| types.f32, false)
}

#[track_caller]
pub fn run_usize(source: &str) -> usize {
    run_with(source, |types| types.usize, false)
}

#[track_caller]
pub fn run_usize_stressed(source: &str) -> usize {
    run_with(source, |types| types.usize, true)
}

#[track_caller]
pub fn run_bool_stressed(source: &str) -> bool {
    run_with::<i8>(source, |types| types.bool, true) != 0
}

pub fn compile_error_text(source: &str) -> String {
    let _guard = lock();
    let mut compiler = compiler();
    let mut provider = compiler.start_compiling();

    match provider.compile("main", Vec::<(String, TypeRef)>::new(), None, source) {
        Ok(_) => panic!("compilation must fail:\n{source}"),
        Err(error) => error.display(&provider.type_context.tcx).to_string(),
    }
}

pub fn compile_error(source: &str) -> CompileError {
    let _guard = lock();
    let mut compiler = compiler();
    let mut provider = compiler.start_compiling();

    match provider.compile("main", Vec::<(String, TypeRef)>::new(), None, source) {
        Ok(_) => panic!("compilation must fail:\n{source}"),
        Err(error) => error,
    }
}
