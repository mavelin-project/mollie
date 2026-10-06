//! The safe host API: functions, types and impls registered from Rust, and
//! programs called with checked signatures.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use mollie::{
    GcPtr, MolStr,
    compiler::{
        Compiler,
        sandbox::{Limits, TrapKind},
    },
    host::{CompilerExt, GcArray, Host, ScriptCallback, ScriptObject},
    host_value_type,
    stub::host_stub,
    typed_ast::TypedASTContext,
};

use crate::lock;

#[repr(C)]
struct Counter {
    count: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Vec2 {
    x: f32,
    y: f32,
}

host_value_type!(Vec2);

fn compiler() -> Compiler<()> {
    Compiler::with_symbols((), []).unwrap_or_else(|error| panic!("can't create the compiler: {error}"))
}

/// Compiles `source` returning an `i32` and runs it.
fn run(compiler: &mut Compiler<()>, source: &str) -> Result<i32, mollie::compiler::sandbox::Trap> {
    if let Err(error) = compiler.compile_script::<(), i32>("main", &[], source) {
        panic!("compilation failed:\n{}\n\nprogram:\n{source}", error.display(&compiler.type_context.tcx));
    }

    compiler
        .script_fn::<(), i32>("main")
        .expect("`main` must be compiled")
        .call((), Limits::default())
}

#[test]
fn closures_with_state() {
    let _guard = lock();
    let mut compiler = compiler();
    let total = Rc::new(Cell::new(0));
    let counted = Rc::clone(&total);

    Host::new(&mut compiler).function_named("add", &["amount"], move |amount: i32| {
        counted.set(counted.get() + amount);

        counted.get()
    });

    assert_eq!(run(&mut compiler, "add(2);\nadd(amount: 3)"), Ok(5));
    assert_eq!(total.get(), 5);
}

#[test]
fn strings_both_ways() {
    let _guard = lock();
    let mut compiler = compiler();

    Host::new(&mut compiler).function("greet", |name: MolStr| format!("hi, {name}"));

    assert_eq!(run(&mut compiler, "greet(\"ann\").len() as i32"), Ok(7));
}

#[test]
fn objects_with_methods() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.object::<Counter>("Counter").field::<i32>("count").finish();
    host.methods::<GcPtr<Counter>>()
        .method_named("bump", &["by"], |mut counter: GcPtr<Counter>, by: i32| {
            // SAFETY: the object is a `Counter`, alive while the method runs.
            unsafe { (*counter.ptr_mut()).count += by };
        })
        .method("get", |counter: GcPtr<Counter>| counter.count)
        .finish();

    assert_eq!(
        run(
            &mut compiler,
            "let counter = Counter { count: 1 };\ncounter.bump(by: 4);\ncounter.get() * 10 + counter.count"
        ),
        Ok(55)
    );
}

#[test]
fn value_types_cross_by_value() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();
    host.function("scale", |v: Vec2, k: f32| Vec2 { x: v.x * k, y: v.y * k });
    host.function("dot", |a: Vec2, b: Vec2| a.x * b.x + a.y * b.y);

    assert_eq!(
        run(
            &mut compiler,
            "let v = scale(Vec2 { x: 1.0, y: 2.0 }, 3.0);\n(v.x * 10.0 + v.y + dot(v, Vec2 { x: 1.0, y: 0.0 })) as i32"
        ),
        Ok(30 + 6 + 3)
    );
}

#[test]
fn panics_stop_the_program() {
    let _guard = lock();
    let mut compiler = compiler();

    Host::new(&mut compiler).function("fail", |code: i32| -> i32 { panic!("failed with {code}") });

    let trap = run(&mut compiler, "fail(7) + 1").expect_err("the program must stop");

    assert_eq!(trap.kind, TrapKind::Host);
    assert_eq!(trap.message.as_deref(), Some("failed with 7"));
}

#[test]
fn signatures_are_checked() {
    let _guard = lock();
    let mut compiler = compiler();

    compiler
        .compile_script::<(i32, i32), i32>("update", &["state", "frame"], "state + frame")
        .expect("the program must compile");

    assert!(compiler.script_fn::<(i32,), i32>("update").is_err());
    assert!(compiler.script_fn::<(i32, i32), bool>("update").is_err());
    assert!(compiler.script_fn::<(), i32>("missing").is_err());

    let update = compiler.script_fn::<(i32, i32), i32>("update").expect("the signature matches");

    assert_eq!(update.call((40, 2), Limits::default()), Ok(42));
}

#[test]
fn callbacks_from_scripts() {
    let _guard = lock();
    let mut compiler = compiler();

    Host::new(&mut compiler).function_named("twice", &["f", "x"], |f: ScriptCallback<(i32,), i32>, x: i32| {
        // A trap of the callback is the program's: it stops when `twice`
        // returns.
        f.call((x,)).and_then(|once| f.call((once,))).unwrap_or(0)
    });

    assert_eq!(run(&mut compiler, "let base = 1;\ntwice(|x| { x * 3 + base }, 2)"), Ok(22));
}

#[test]
fn callbacks_take_and_return_value_types() {
    let _guard = lock();
    let mut compiler = compiler();
    let kept = Rc::new(RefCell::new(None::<ScriptCallback<(Vec2,), Box3>>));
    let keeping = Rc::clone(&kept);
    let mut host = Host::new(&mut compiler);

    host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();
    host.value_type::<Box3>("Box3")
        .field::<f32>("x")
        .field::<f32>("y")
        .field::<f32>("width")
        .field::<f32>("height")
        .field::<f32>("depth")
        .finish();
    // 8 and 20 bytes as arguments, both ways.
    host.function_named("transform", &["f", "v"], |f: ScriptCallback<(Vec2, Box3), Vec2>, v: Vec2| {
        let by = Box3 {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 0.0,
            depth: 2.0,
        };

        f.call((v, by)).and_then(|once| f.call((once, by))).unwrap_or(Vec2 { x: 0.0, y: 0.0 })
    });
    host.function_named("keep", &["f"], move |f: ScriptCallback<(Vec2,), Box3>| {
        *keeping.borrow_mut() = Some(f);
    });

    assert_eq!(
        run(
            &mut compiler,
            "let r = transform(|v, b| { Vec2 { x: v.x + b.width, y: v.y * b.depth } }, Vec2 { x: 1.0, y: 2.0 });

keep(|v| { Box3 { x: v.x, y: v.y, width: 1.0, height: 2.0, depth: 3.0 } });

(r.x + r.y) as i32"
        ),
        // (1 + 10 + 10) + (2 * 2 * 2)
        Ok(29)
    );

    // Called later by the host, in a run of its own.
    let callback = kept.borrow_mut().take().expect("kept by the program");

    assert_eq!(
        callback.call_with_limits((Vec2 { x: 5.0, y: 6.0 },), Limits::default()),
        Ok(Box3 {
            x: 5.0,
            y: 6.0,
            width: 1.0,
            height: 2.0,
            depth: 3.0
        })
    );
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
#[allow(dead_code, reason = "built by scripts")]
enum Shape {
    Circle { radius: f32 },
    Rect { width: f32, height: f32 },
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
#[allow(dead_code, reason = "built by scripts")]
enum Step {
    Stay,
    Move { by: i32 },
}

host_value_type!(Step);

#[test]
fn enums_of_the_host() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.enum_::<Shape>("Shape")
        .variant("Circle")
        .field::<f32>("radius")
        .variant("Rect")
        .field::<f32>("width")
        .field::<f32>("height")
        .finish();
    host.value_enum::<Step>("Step").variant("Stay").variant("Move").field::<i32>("by").finish();
    host.function_named("area", &["shape"], |shape: GcPtr<Shape>| match *shape {
        Shape::Circle { radius } => 3.0 * radius * radius,
        Shape::Rect { width, height } => width * height,
    });
    host.function_named("distance", &["step"], |step: Step| match step {
        Step::Stay => 0,
        Step::Move { by } => by,
    });

    assert_eq!(
        run(
            &mut compiler,
            "(area(Shape::Rect { width: 2.0, height: 3.0 }) + area(Shape::Circle { radius: 1.0 })) as i32 + distance(Step::Move { by: 5 })"
        ),
        Ok(14)
    );
}

// #[test]
// #[should_panic(expected = "don't match the layout")]
// fn enums_must_match_their_rust_type() {
//     let mut compiler = compiler();

//     // `Rect` is missing a field.
//     Host::new(&mut compiler)
//         .enum_::<Shape>("Shape")
//         .variant("Circle")
//         .field::<f32>("radius")
//         .variant("Rect")
//         .field::<f32>("width")
//         .finish();
// }

#[test]
fn declarations_of_the_host() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();
    host.function_named("length", &["v"], |v: Vec2| v.x.hypot(v.y));

    let declarations = host.declarations();

    assert!(declarations.contains("extern value struct Vec2 { x: f32, y: f32 }"), "{declarations}");
    assert!(declarations.contains("extern func length(v: Vec2) -> f32;"), "{declarations}");
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Box3 {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    depth: f32,
}

host_value_type!(Box3);

#[test]
fn value_types_cross_with_programs() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();
    host.value_type::<Box3>("Box3")
        .field::<f32>("x")
        .field::<f32>("y")
        .field::<f32>("width")
        .field::<f32>("height")
        .field::<f32>("depth")
        .finish();

    // 8 bytes of floats (C passes them in a float register) and 20 bytes
    // (more than fits in registers), both ways.
    compiler
        .compile_script::<(Vec2, f32), Vec2>("scale", &["v", "k"], "Vec2 { x: v.x * k, y: v.y * k }")
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));
    compiler
        .compile_script::<(Box3, i32), Box3>(
            "grow",
            &["b", "by"],
            "Box3 { x: b.x, y: b.y, width: b.width + by as f32, height: b.height + by as f32, depth: b.depth * 2.0 }",
        )
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    let scale = compiler.script_fn::<(Vec2, f32), Vec2>("scale").expect("compiled");
    let grow = compiler.script_fn::<(Box3, i32), Box3>("grow").expect("compiled");
    let input = Box3 {
        x: 1.0,
        y: 2.0,
        width: 3.0,
        height: 4.0,
        depth: 5.0,
    };

    assert_eq!(scale.call((Vec2 { x: 1.5, y: -2.0 }, 2.0), Limits::default()), Ok(Vec2 { x: 3.0, y: -4.0 }));
    assert_eq!(
        grow.call((input, 10), Limits::default()),
        Ok(Box3 {
            width: 13.0,
            height: 14.0,
            depth: 10.0,
            ..input
        })
    );
}

/// Stands for the trait `Area`.
enum AreaTrait {}

fn area_compiler() -> Compiler<()> {
    let mut compiler = compiler();

    Host::new(&mut compiler)
        .trait_::<AreaTrait>("Area")
        .method::<(i32,), i32>("area", &["scale"])
        .finish();

    compiler
}

#[test]
fn trait_objects_are_called_by_the_host() {
    let _guard = lock();
    let mut compiler = area_compiler();

    compiler
        .compile_script::<(), ScriptObject<AreaTrait>>(
            "make",
            &[],
            "struct Square { side: i32 }
impl Area for Square { func area(self, scale: i32) -> i32 { self.side * self.side * scale } }
Square { side: 3 }",
        )
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));
    compiler
        .compile_script::<(ScriptObject<AreaTrait>,), i32>("measure", &["shape"], "shape.area(10)")
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    let shape = compiler
        .script_fn::<(), ScriptObject<AreaTrait>>("make")
        .expect("compiled")
        .call((), Limits::default())
        .expect("runs");

    // Collections don't free the value the host holds.
    compiler.inner.collect_garbage();

    assert_eq!(shape.call::<_, i32>("area", (2,), Limits::default()), Ok(18));
    // Another signature, or a function the trait doesn't have.
    assert!(shape.call::<_, bool>("area", (2,), Limits::default()).is_err());
    assert!(shape.call::<_, i32>("volume", (2,), Limits::default()).is_err());
    // Given back to the program.
    assert_eq!(
        compiler
            .script_fn::<(ScriptObject<AreaTrait>,), i32>("measure")
            .expect("compiled")
            .call((shape,), Limits::default()),
        Ok(90)
    );
}

/// Stands for the trait `Shape`.
enum ShapeTrait {}

#[test]
fn trait_functions_take_and_return_value_types() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();
    host.value_type::<Box3>("Box3")
        .field::<f32>("x")
        .field::<f32>("y")
        .field::<f32>("width")
        .field::<f32>("height")
        .field::<f32>("depth")
        .finish();
    // 8 bytes (in a float register for C, a chunk for compiled code) and 20
    // bytes (through memory), as arguments and results.
    host.trait_::<ShapeTrait>("Shape")
        .method::<(Vec2,), Vec2>("moved", &["by"])
        .method::<(Vec2, i32), Box3>("bounds", &["scale", "depth"])
        .method::<(), i32>("count", &[])
        .finish();

    compiler
        .compile_script::<(), ScriptObject<ShapeTrait>>(
            "make",
            &[],
            "value struct Dot { at: Vec2, moves: i32 }
impl Shape for Dot {
    func moved(mut self, by: Vec2) -> Vec2 {
        self.at = Vec2 { x: self.at.x + by.x, y: self.at.y + by.y };
        self.moves += 1;
        self.at
    }

    func bounds(self, scale: Vec2, depth: i32) -> Box3 {
        Box3 { x: self.at.x, y: self.at.y, width: scale.x, height: scale.y, depth: depth as f32 }
    }

    func count(self) -> i32 { self.moves }
}
Dot { at: Vec2 { x: 1.0, y: 2.0 }, moves: 0 }",
        )
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    let dot = compiler
        .script_fn::<(), ScriptObject<ShapeTrait>>("make")
        .expect("compiled")
        .call((), Limits::default())
        .expect("runs");

    assert_eq!(
        dot.call::<_, Vec2>("moved", (Vec2 { x: 0.5, y: -1.0 },), Limits::default()),
        Ok(Vec2 { x: 1.5, y: 1.0 })
    );

    let bounds = dot.call::<_, Box3>("bounds", (Vec2 { x: 3.0, y: 4.0 }, 7), Limits::default()).expect("runs");

    assert_eq!(bounds, Box3 {
        x: 1.5,
        y: 1.0,
        width: 3.0,
        height: 4.0,
        depth: 7.0
    });
    assert_eq!(dot.call::<_, i32>("count", (), Limits::default()), Ok(1));
}

#[test]
fn mut_self_through_trait_objects_changes_the_value() {
    let _guard = lock();
    let mut compiler = area_compiler();

    compiler
        .compile_script::<(), ScriptObject<AreaTrait>>(
            "make",
            &[],
            "value struct Counter { total: i32 }
impl Area for Counter { func area(mut self, scale: i32) -> i32 { self.total += scale; self.total } }
Counter { total: 0 }",
        )
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    let counter = compiler
        .script_fn::<(), ScriptObject<AreaTrait>>("make")
        .expect("compiled")
        .call((), Limits::default())
        .expect("runs");

    assert_eq!(counter.call::<_, i32>("area", (1,), Limits::default()), Ok(1));
    assert_eq!(counter.call::<_, i32>("area", (2,), Limits::default()), Ok(3));
}

#[test]
fn arrays_of_scripts() {
    let _guard = lock();
    let mut compiler = compiler();

    compiler.compile_script::<(), GcArray<i32>>("make", &[], "[1, 2, 3]").expect("compiles");
    compiler
        .compile_script::<(GcArray<i32>,), i32>("sum", &["values"], "let mut total = 0;\nfor value in values { total += value; }\ntotal")
        .expect("compiles");

    let mut array = compiler
        .script_fn::<(), GcArray<i32>>("make")
        .expect("compiled")
        .call((), Limits::default())
        .expect("runs");

    assert_eq!(array.to_vec(), [1, 2, 3]);
    assert_eq!(array.get(3), None);
    assert!(array.set(0, 10));
    assert!(!array.set(5, 0));
    assert!(array.push(4));
    assert_eq!(array.len(), 4);
    assert_eq!(
        compiler
            .script_fn::<(GcArray<i32>,), i32>("sum")
            .expect("compiled")
            .call((array,), Limits::default()),
        Ok(10 + 2 + 3 + 4)
    );
}

#[test]
fn stubs_check_programs_without_the_host() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.function_named("log", &["message"], |_: MolStr| {});
    host.module("graphics");
    host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();
    host.object::<Counter>("Counter").field::<i32>("count").finish();
    host.methods::<GcPtr<Counter>>().method("get", |counter: GcPtr<Counter>| counter.count).finish();
    host.everywhere();
    host.trait_::<AreaTrait>("Area").method::<(i32,), i32>("area", &["scale"]).finish();
    host.capability("secret").function("reveal", || 42);

    let stub = host_stub(&compiler);

    // A tool (like a language server) checks a program without the host.
    let mut tools = TypedASTContext::default();

    tools.load_host_stub(&stub.root, &stub.modules);

    let errors = |tools: &TypedASTContext| {
        tools
            .diagnostics
            .errors
            .values()
            .map(|error| tools.tcx.display_of_diagnostic(error).to_string())
            .collect::<Vec<_>>()
    };

    assert!(tools.diagnostics.is_empty(), "{:?}\n\nstub:\n{stub}", errors(&tools));

    let i32 = tools.tcx.types.core_types.i32;

    tools.process(
        (),
        "import { Vec2, Counter } from graphics;
import { reveal } from secret;

struct Square { side: i32 }

impl Area for Square {
    func area(self, scale: i32) -> i32 { self.side * scale }
}

log(\"checked\");

let v = Vec2 { x: 1.0, y: 2.0 };
let counter = Counter { count: 3 };
let shape: Area = Square { side: 2 };

counter.get() + reveal() + v.x as i32 + shape.area(10)",
        Vec::<(String, mollie::typing::TypeRef)>::new(),
        i32,
    );

    assert!(tools.diagnostics.is_empty(), "{:?}\n\nstub:\n{stub}", errors(&tools));
}

#[test]
fn stubs_are_only_declarations() {
    let _guard = lock();
    let mut compiler = compiler();
    let error = compiler
        .compile_script::<(), i32>("main", &[], "extern func secret() -> i32;\nsecret()")
        .expect_err("`extern` is only for stubs");

    assert!(
        error.display(&compiler.type_context.tcx).to_string().contains("stub"),
        "{}",
        error.display(&compiler.type_context.tcx)
    );

    // A program checked against a stub can't be compiled: the functions are
    // only declared.
    let mut compiler = compiler_with_stub("extern func secret() -> i32;");

    assert!(compiler.compile_script::<(), i32>("main", &[], "secret()").is_err());
}

fn compiler_with_stub(root: &str) -> Compiler<()> {
    let mut compiler = compiler();

    compiler.type_context.load_host_stub(root, &[]);

    compiler
}
