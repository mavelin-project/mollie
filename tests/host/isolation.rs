//! Programs (addons) are isolated: every compiler has its own heap, with its
//! own limit, and values of one program can't be given to another.

use std::{cell::RefCell, rc::Rc};

use mollie::{
    GcPtr, MolStr,
    compiler::{
        Compiler,
        sandbox::{Limits, Trap, TrapKind},
    },
    host::{CompilerExt, Host, ScriptCallback},
};

use crate::lock;

#[repr(C)]
struct Counter {
    count: i32,
}

#[track_caller]
fn compiler() -> Compiler<()> {
    Compiler::with_symbols((), []).unwrap_or_else(|error| panic!("can't create the compiler: {error}"))
}

#[track_caller]
fn compile<Args: mollie::host::ScriptArgs + 'static, R: mollie::host::ScriptValue>(compiler: &mut Compiler<()>, params: &[&str], source: &str) {
    if let Err(error) = compiler.compile_script::<Args, R>("main", params, source) {
        panic!("compilation failed:\n{}\n\nprogram:\n{source}", error.display(&compiler.type_context.tcx));
    }
}

const ALLOCATING: &str = "struct Point { x: i32, y: i32 }

let points = [Point { x: 0, y: 0 }];
let mut i = 0;

while i < 2000 {
    points.push(Point { x: i, y: i });
    i += 1;
}

points.len() as i32";

#[test]
fn programs_have_their_own_heap_limits() {
    let _guard = lock();
    let (mut small, mut large) = (compiler(), compiler());

    compile::<(), i32>(&mut small, &[], ALLOCATING);
    compile::<(), i32>(&mut large, &[], ALLOCATING);

    let limited = Limits {
        heap_bytes: Some(16 * 1024),
        ..Limits::default()
    };
    let trap = small
        .script_fn::<(), i32>("main")
        .expect("compiled")
        .call((), limited)
        .expect_err("the heap is too small");

    assert_eq!(trap.kind, TrapKind::OutOfMemory);
    // The other program's heap is unaffected.
    assert_eq!(large.script_fn::<(), i32>("main").expect("compiled").call((), Limits::default()), Ok(2001));
    assert!(large.inner.heap_stats().allocated_bytes > small.inner.heap_stats().allocated_bytes);
}

type Kept = Rc<RefCell<Option<ScriptCallback<(i32,), i32>>>>;

/// A compiler whose `keep(f)` stores `f` in `kept`.
fn keeping(kept: &Kept) -> Compiler<()> {
    let mut compiler = compiler();
    let slot = Rc::clone(kept);

    Host::new(&mut compiler).function_named("keep", &["f"], move |f: ScriptCallback<(i32,), i32>| {
        *slot.borrow_mut() = Some(f);
    });

    compile::<(), i32>(&mut compiler, &[], "let base = 40;\nkeep(|x| { x + base });\n0");

    compiler
}

#[test]
fn callbacks_kept_by_the_host_run_in_their_program() {
    let _guard = lock();
    let kept = Kept::default();
    let compiler = keeping(&kept);

    assert_eq!(compiler.script_fn::<(), i32>("main").expect("compiled").call((), Limits::default()), Ok(0));

    // Collections don't free the environment of the kept closure.
    compiler.inner.collect_garbage();

    let callback = kept.borrow();
    let callback = callback.as_ref().expect("the callback is kept");

    assert_eq!(callback.call_with_limits((2,), Limits::default()), Ok(42));
    // Outside of its program, it must get limits.
    assert_eq!(callback.call((2,)).map_err(|trap| trap.kind), Err(TrapKind::Host));
}

#[test]
fn callbacks_of_unloaded_programs_fail() {
    let _guard = lock();
    let kept = Kept::default();
    let compiler = keeping(&kept);

    assert_eq!(compiler.script_fn::<(), i32>("main").expect("compiled").call((), Limits::default()), Ok(0));

    // The program is unloaded: its code and heap are freed.
    drop(compiler);

    let callback = kept.borrow_mut().take().expect("the callback is kept");
    let trap: Trap = callback.call_with_limits((2,), Limits::default()).expect_err("the program is gone");

    assert_eq!(trap.kind, TrapKind::Host);
    // Dropping it doesn't touch the freed heap.
    drop(callback);
}

#[test]
fn objects_of_another_program_are_refused() {
    let _guard = lock();
    let (mut first, mut second) = (compiler(), compiler());

    for compiler in [&mut first, &mut second] {
        Host::new(compiler).object::<Counter>("Counter").field::<i32>("count").finish();

        compile::<(GcPtr<Counter>,), i32>(compiler, &["counter"], "counter.count");
    }

    let counter = GcPtr::new_in(first.inner.heap(), Counter { count: 7 });
    let _root = counter.root(first.inner.heap());

    assert_eq!(
        first
            .script_fn::<(GcPtr<Counter>,), i32>("main")
            .expect("compiled")
            .call((counter,), Limits::default()),
        Ok(7)
    );

    let trap = second
        .script_fn::<(GcPtr<Counter>,), i32>("main")
        .expect("compiled")
        .call((counter,), Limits::default())
        .expect_err("the object belongs to the first program");

    assert_eq!(trap.kind, TrapKind::Host);
}

#[test]
fn strings_are_copied_between_programs() {
    let _guard = lock();
    let (mut first, mut second) = (compiler(), compiler());

    compile::<(), MolStr>(&mut first, &[], "\"from ${1 + 1}\"");
    compile::<(MolStr,), i32>(&mut second, &["text"], "(text + \"!\").len() as i32");

    let text = first
        .script_fn::<(), MolStr>("main")
        .expect("compiled")
        .call((), Limits::default())
        .expect("runs");
    let _root = text.root(first.inner.heap());

    assert_eq!(text.as_str(), "from 2");
    assert_eq!(
        second.script_fn::<(MolStr,), i32>("main").expect("compiled").call((text,), Limits::default()),
        Ok(7)
    );
}

#[test]
fn programs_can_be_reloaded_many_times() {
    let _guard = lock();

    for round in 0..50 {
        let mut compiler = compiler();

        compile::<(), i32>(&mut compiler, &[], ALLOCATING);

        assert_eq!(
            compiler.script_fn::<(), i32>("main").expect("compiled").call((), Limits::default()),
            Ok(2001),
            "round {round}"
        );
        // Dropped here: its heap and code are freed.
    }
}

#[test]
fn capabilities_are_granted_per_program() {
    let _guard = lock();
    let mut compiler = compiler();
    let source = "import { reveal } from secret;\nreveal()";

    Host::new(&mut compiler).capability("secret").function("reveal", || 42);

    // Without the capability, `secret` doesn't exist.
    assert!(compiler.compile_script::<(), i32>("plain", &[], source).is_err());

    compiler
        .compile_script_with::<(), i32>("trusted", &[], source, &["secret"])
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    assert_eq!(compiler.script_fn::<(), i32>("trusted").expect("compiled").call((), Limits::default()), Ok(42));
    // It's hidden again from programs compiled afterwards.
    assert!(compiler.compile_script::<(), i32>("later", &[], source).is_err());
    assert!(compiler.compile_script_with::<(), i32>("unknown", &[], "1", &["missing"]).is_err());
}
