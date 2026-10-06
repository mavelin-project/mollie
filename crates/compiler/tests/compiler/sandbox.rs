//! Recoverable traps and limits of programs.

use std::{cell::Cell, rc::Rc};

use mollie_compiler::{
    error::CompileError,
    sandbox::{Limits, Trap, TrapKind},
};
use mollie_typing::{TypeError, TypeRef};

use crate::{compile_error, compiler, lock, run_i32, run_limited};

/// Runs `source` returning an `i32` with `limits`, returning the kind of the
/// trap that stopped it.
fn trap_of(source: &str, limits: Limits) -> TrapKind {
    match run_limited::<i32>(source, |types| types.i32, limits, false) {
        Ok(value) => panic!("the program must trap, but returned {value}:\n{source}"),
        Err(trap) => trap.kind,
    }
}

#[test]
fn index_out_of_bounds() {
    assert_eq!(trap_of("let a = [1, 2, 3];\na[5]", Limits::default()), TrapKind::OutOfBounds);
}

#[test]
fn division_by_zero() {
    assert_eq!(trap_of("let mut zero = 0;\n10 / zero", Limits::default()), TrapKind::DivisionByZero);
}

#[test]
fn division_overflow() {
    assert_eq!(
        trap_of(
            "let mut min: i32 = -2147483647 - 1;\nlet minus_one = 0 - 1;\nmin / minus_one",
            Limits::default()
        ),
        TrapKind::ArithmeticOverflow
    );
}

#[test]
fn division_without_errors() {
    assert_eq!(run_i32("let mut a = 0 - 7;\nlet b: u8 = 200;\na / 2 + (b / 3) as i32"), -3 + 66);
}

#[test]
fn invalid_string_slice() {
    assert_eq!(trap_of(r#""héllo".slice(1, 2).len() as i32"#, Limits::default()), TrapKind::InvalidSlice);
}

#[test]
fn trap_unwinds_nested_calls_and_closures() {
    assert_eq!(
        trap_of(
            "func inner(items: i32[]) -> i32 { items[10] }
func outer(f: func(i32) -> i32) -> i32 { f(1) + 1 }

outer(|x| { inner([x]) }) + 5",
            Limits::default()
        ),
        TrapKind::OutOfBounds
    );
}

#[test]
fn host_stops_the_program() {
    let trap = run_limited::<i32>("fail();\n1", |types| types.i32, Limits::default(), false).expect_err("the program must trap");

    assert_eq!(trap.kind, TrapKind::Host);
    assert_eq!(trap.message.as_deref(), Some("failed by the host"));
}

#[test]
fn endless_loop_runs_out_of_fuel() {
    let limits = Limits {
        fuel: Some(1000),
        ..Limits::default()
    };

    assert_eq!(trap_of("while true {}\n1", limits), TrapKind::OutOfFuel);
}

#[test]
fn fuel_can_be_refilled() {
    let refills = Rc::new(Cell::new(0));
    let counter = Rc::clone(&refills);
    let limits = Limits {
        fuel: Some(100),
        refuel: Some(Box::new(move || {
            counter.set(counter.get() + 1);

            100
        })),
        ..Limits::default()
    };

    let result = run_limited::<i32>("let mut i = 0;\nwhile i < 1000 { i += 1; }\ni", |types| types.i32, limits, false);

    assert_eq!(result, Ok(1000));
    assert!(refills.get() >= 9, "fuel must have been refilled, got {} refills", refills.get());
}

#[test]
fn deep_recursion_overflows_the_stack() {
    let limits = Limits {
        stack_bytes: Some(64 * 1024),
        ..Limits::default()
    };

    assert_eq!(
        trap_of("func down(n: i32) -> i32 { down(n + 1) + 1 }\ndown(0)", limits),
        TrapKind::StackOverflow
    );
}

#[test]
fn allocations_are_limited() {
    let limits = Limits {
        heap_bytes: Some(64 * 1024),
        ..Limits::default()
    };

    assert_eq!(
        trap_of(
            "struct Point { x: i32, y: i32 }

let points = [Point { x: 0, y: 0 }];

while true {
    points.push(Point { x: 1, y: 1 });
}

1",
            limits
        ),
        TrapKind::OutOfMemory
    );
}

#[test]
fn growing_arrays_is_limited() {
    let limits = Limits {
        heap_bytes: Some(64 * 1024),
        ..Limits::default()
    };

    // Only the array grows: pushing numbers allocates no objects.
    assert_eq!(
        trap_of(
            "let numbers = [0];

while true {
    numbers.push(1);
}

1",
            limits
        ),
        TrapKind::OutOfMemory
    );
}

#[test]
fn pushing_to_temporary_arrays_survives_collections() {
    let result = run_limited::<usize>(
        "let mut total = 0usize;

for i in 0..200 {
    let array = [\"a\", \"b\"];

    array.push(\"item ${i}\");
    // Nothing but the call itself references this array.
    [\"x\"].push(\"y ${i}\");
    total += array.len() + array[2].len();
}

total",
        |types| types.usize,
        Limits::default(),
        true,
    );

    assert_eq!(result, Ok((0..200).map(|i: usize| 3 + 5 + i.to_string().len()).sum()));
}

#[test]
fn garbage_does_not_count_towards_the_heap_limit() {
    let limits = Limits {
        heap_bytes: Some(64 * 1024),
        ..Limits::default()
    };

    let result = run_limited::<i32>(
        "struct Point { x: i32, y: i32 }

let mut i = 0;
let mut total = 0;

while i < 10000 {
    let point = Point { x: 1, y: 2 };

    total += point.y;
    i += 1;
}

total",
        |types| types.i32,
        limits,
        false,
    );

    assert_eq!(result, Ok(20000));
}

#[test]
fn iteration_consumes_fuel() {
    let limits = Limits {
        fuel: Some(50),
        ..Limits::default()
    };

    let source = "let mut total = 0;

for value in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40] {
    total += value;
}

total";

    assert_eq!(trap_of(source, limits), TrapKind::OutOfFuel);
}

#[test]
fn program_can_run_again_after_a_trap() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let mut provider = compiler.start_compiling();

    for (name, source) in [("bad", "let a = [1];\na[1]"), ("good", "40 + 2")] {
        provider
            .compile(name, Vec::<(String, TypeRef)>::new(), Some(i32), source)
            .unwrap_or_else(|error| panic!("{}", error.display(&provider.type_context.tcx)));
    }

    let bad = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("bad") }.expect("`bad` must be compiled");
    let good = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("good") }.expect("`good` must be compiled");

    assert_eq!(
        provider.compiler.run(Limits::default(), || bad()).map_err(|trap| trap.kind),
        Err(TrapKind::OutOfBounds)
    );
    assert_eq!(provider.compiler.run(Limits::default(), || good()), Ok(42));
    assert_eq!(
        provider.compiler.run(Limits::default(), || bad()).map_err(|trap| trap.kind),
        Err(TrapKind::OutOfBounds)
    );
}

#[test]
fn restricted_module_is_not_visible() {
    // The program is told it can't use the module, not that it doesn't exist.
    for source in ["import { reveal } from secret;\nreveal();", "secret::reveal();"] {
        let error = compile_error(source);

        assert!(
            matches!(&error, CompileError::Type(diagnostics) if diagnostics.iter().any(|diagnostic| matches!(&diagnostic.error, TypeError::Unavailable { name } if name == "secret"))),
            "{error:?}"
        );
    }
}

#[test]
fn granted_module_is_visible() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let secret = compiler
        .type_context
        .tcx
        .def_registry
        .restricted_modules
        .keys()
        .copied()
        .next()
        .expect("`secret` must be restricted");

    assert!(compiler.type_context.tcx.def_registry.grant(secret));

    let mut provider = compiler.start_compiling();

    provider
        .compile("main", Vec::<(String, TypeRef)>::new(), Some(i32), "import { reveal } from secret;\nreveal()")
        .unwrap_or_else(|error| panic!("{}", error.display(&provider.type_context.tcx)));

    let main = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("main") }.expect("`main` must be compiled");

    assert_eq!(provider.compiler.run(Limits::default(), || main()), Ok(42));
}

#[test]
fn remainder_by_zero() {
    assert_eq!(trap_of("let zero = 0;\n10 % zero", Limits::default()), TrapKind::DivisionByZero);
}

/// The trap that stops `source` (returning an `i32`).
fn trap_in(source: &str) -> Trap {
    run_limited::<i32>(source, |types| types.i32, Limits::default(), false).expect_err("the program must trap")
}

#[test]
fn traps_are_located() {
    let trap = trap_in("let a = [1, 2, 3];\nlet i = 5;\na[i]");
    let location = trap.location.as_ref().expect("the trap must be located");

    assert_eq!(trap.kind, TrapKind::OutOfBounds);
    assert_eq!((location.line, location.column), (3, 1), "{trap}");
    assert_eq!(trap.backtrace.first().map(|frame| frame.function.as_str()), Some("main"), "{trap}");
}

#[test]
fn traps_in_closures_are_located() {
    let trap = trap_in("let f = |x| { 10 / x };\nf(0)");
    let location = trap.location.as_ref().expect("the trap must be located");

    assert_eq!(trap.kind, TrapKind::DivisionByZero);
    // At the division in the closure.
    assert_eq!((location.line, location.column), (1, 15), "{trap}");
    // The call of the closure is in the backtrace.
    assert!(
        trap.backtrace
            .iter()
            .any(|frame| frame.location.as_ref().is_some_and(|location| location.line == 2)),
        "{trap}"
    );
}

#[test]
fn backtraces_go_through_calls() {
    let trap = trap_in(
        "func first(values: i32[]) -> i32 {
    let none: Option<i32> = None;
    none.unwrap()
}

first([])",
    );
    let lines = trap
        .backtrace
        .iter()
        .filter_map(|frame| frame.location.as_ref())
        .map(|location| location.line)
        .collect::<Vec<_>>();

    assert_eq!(trap.kind, TrapKind::Panic);
    // `unwrap` panics in `std`, called at line 3, called at line 6.
    assert!(trap.backtrace.len() >= 3, "{trap}");
    assert!(lines.contains(&3) && lines.contains(&6), "{trap}");
    assert!(trap.to_string().contains("in first at"), "{trap}");
}
