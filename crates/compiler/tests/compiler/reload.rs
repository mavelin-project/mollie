//! Compiling new versions of a program, like a host reloading a script.

use std::sync::atomic::Ordering;

use mollie_compiler::sandbox::Limits;
use mollie_typing::TypeRef;

use crate::{RECORDED, compiler, lock};

/// `update(state: i32) -> i32` of a version of the program.
type Update = extern "C" fn(i32) -> i32;

#[test]
fn new_versions_of_a_program_replace_old_ones() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let mut provider = compiler.start_compiling();
    let mut load = |source: &str| {
        provider
            .compile("update", [("state", i32)], Some(i32), source)
            .map_err(|error| error.display(&provider.type_context.tcx).to_string())?;

        Ok::<_, String>(unsafe { provider.compiler.get_func::<Update>("update") }.expect("`update` must be compiled"))
    };

    // Both versions declare `Step`, and items of the host (`record`) are
    // visible in both.
    let first = load(
        "struct Step { by: i32 }

record(state);
state + Step { by: 1 }.by",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let second = load(
        "struct Step { by: i32, times: i32 }

func apply(state: i32, step: Step) -> i32 { (state + step.by) * step.times }

record(state);
apply(state, Step { by: 1, times: 10 })",
    )
    .unwrap_or_else(|error| panic!("{error}"));

    // A version with an error isn't loaded, the last one keeps working.
    assert!(load("struct Step {}\nstate + true").is_err());

    let latest = unsafe { provider.compiler.get_func::<Update>("update") }.expect("`update` must be compiled");
    let run = |update: Update, state: i32| provider.compiler.run(Limits::default(), || update(state)).expect("the program must not trap");

    // The state is kept by the host, across versions.
    let state = run(first, 1);
    let state = run(second, state);

    assert_eq!(state, (2 + 1) * 10);
    assert_eq!(RECORDED.load(Ordering::SeqCst), 2);
    assert_eq!(run(latest, 0), 10);
    // Old versions still work (their code is never freed).
    assert_eq!(run(first, 0), 1);
}

#[test]
fn programs_do_not_share_items() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let mut provider = compiler.start_compiling();

    for (name, source) in [
        ("a", "struct Point { x: i32 }\nPoint { x: 1 }.x"),
        ("b", "struct Point { y: i32 }\nPoint { y: 2 }.y"),
    ] {
        provider
            .compile(name, Vec::<(String, TypeRef)>::new(), Some(i32), source)
            .unwrap_or_else(|error| panic!("{}", error.display(&provider.type_context.tcx)));
    }

    // `b` can't see `Point` of `a`.
    assert!(provider.compile("c", Vec::<(String, TypeRef)>::new(), Some(i32), "Point { x: 1 }.x").is_err());

    let a = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("a") }.expect("`a` must be compiled");
    let b = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("b") }.expect("`b` must be compiled");

    assert_eq!(provider.compiler.run(Limits::default(), || a() * 10 + b()), Ok(12));
}
