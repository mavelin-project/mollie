use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI32, Ordering},
};

use mollie::{
    GcPtr, MolStr,
    compiler::{
        Compiler,
        sandbox::{Limits, TrapKind},
    },
    host::{CompilerExt, GcArray, Host, Running, ScriptCallback, ScriptObject, Slice, SliceMut},
    host_value_type,
    stub::{host_stub, host_stub_with},
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
    let total = Arc::new(AtomicI32::new(0));
    let counted = Arc::clone(&total);

    Host::new(&mut compiler).function_named("add", &["amount"], move |amount: i32| counted.fetch_add(amount, Ordering::Relaxed) + amount);

    assert_eq!(run(&mut compiler, "add(2);\nadd(amount: 3)"), Ok(5));
    assert_eq!(total.load(Ordering::Relaxed), 5);
}

#[test]
fn strings_both_ways() {
    let _guard = lock();
    let mut compiler = compiler();

    Host::new(&mut compiler).function("greet", |name: MolStr| format!("hi, {name}"));

    assert_eq!(run(&mut compiler, "greet(\"ann\").len() as i32"), Ok(7));
}

#[repr(C)]
struct Settings {
    name: MolStr,
    level: i32,
    loud: bool,
}

#[test]
fn fields_of_host_types_have_defaults() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.object::<Settings>("Settings")
        .field::<MolStr>("name")
        .field_default::<i32>("level", 3)
        .field_default::<bool>("loud", true)
        .finish();
    host.function(
        "level_of",
        |settings: GcPtr<Settings>| if settings.loud { settings.level * 10 } else { settings.level },
    );

    assert_eq!(
        run(
            &mut compiler,
            "level_of(Settings { name: \"a\" }) + level_of(Settings { name: \"b\", level: 5, loud: false })"
        ),
        Ok(35)
    );
    // Editors know the defaults too.
    assert!(host_stub(&compiler).to_string().contains("level: i32 = 3, loud: bool = true"));
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
    host.function("dot", |a: Vec2, b: Vec2| a.y.mul_add(b.y, a.x * b.x));

    assert_eq!(
        run(
            &mut compiler,
            "let v = scale(Vec2 { x: 1.0, y: 2.0 }, 3.0);\n(v.x * 10.0 + v.y + dot(v, Vec2 { x: 1.0, y: 0.0 })) as i32"
        ),
        Ok(30 + 6 + 3)
    );
}

#[test]
fn fixed_arrays_cross_the_boundary() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.function("sum", |values: [f32; 3]| values.iter().sum::<f32>());
    host.function("pair", |a: i32, b: i32| [a, b]);

    assert_eq!(
        run(&mut compiler, "let p = pair(4, 5);
let values: f32[3] = [1.0, 2.0, 3.0];
p[0] * 10 + p[1] + sum(values) as i32 * 100"),
        Ok(45 + 600)
    );
}

#[test]
fn variadic_functions_of_the_host() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.function_named("join", &["separator", "..parts"], |separator: MolStr, parts: Slice<MolStr>| {
        parts.iter().map(MolStr::as_str).collect::<Vec<_>>().join(separator.as_str())
    });
    host.function_named("sum", &["..values"], |values: Slice<f32>| values.iter().sum::<f32>());

    assert_eq!(
        run(
            &mut compiler,
            "let named = \"c${1}\";\njoin(\", \", \"a\", \"bb\", named).len() as i32 * 100 + join(\"-\").len() as i32 * 10 + sum(1.5, 2.5) as i32 + sum() as i32"
        ),
        // `a, bb, c1`, nothing, then 4.
        Ok(9 * 100 + 4)
    );
    assert!(host_stub(&compiler).to_string().contains("extern func join(separator: string, ..parts: string[..]) -> string;"));
}

#[test]
fn hosts_fill_writable_views() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    host.function_named("squares", &["from", "values"], |from: i32, mut values: SliceMut<i32>| {
        i32::try_from(values.copy_from((from..).map(|value| value * value))).unwrap_or(0)
    });
    host.function_named("double", &["values"], |mut values: SliceMut<f32>| {
        for index in 0..values.len() {
            let value = values.get(index).unwrap_or_default();

            values.set(index, value * 2.0);
        }
    });

    // Fixed arrays get their elements back, growable ones are changed in
    // place.
    assert_eq!(
        run(&mut compiler, "let mut fixed: i32[3] = [0, 0, 0];
let count = squares(2, fixed);
let grown: f32[] = [1.5, 2.5];
double(grown);
fixed[0] + fixed[1] * 10 + fixed[2] * 100 + count * 10000 + grown[1] as i32 * 100000"),
        Ok(4 + 90 + 1600 + 30000 + 500000)
    );
    assert!(host_stub(&compiler).to_string().contains("extern func double(mut values: f32[..]);"));
}

#[test]
fn hosts_stop_programs_with_formatted_messages() {
    let _guard = lock();
    let mut compiler = compiler();

    Host::new(&mut compiler).function("check", |level: i32| {
        if level > 15 {
            mollie::raise!("level {level} is over 15");
        }

        level
    });

    let trap = run(&mut compiler, "check(3) + check(20)").expect_err("the program must stop");

    assert_eq!(trap.kind, TrapKind::Host);
    assert_eq!(trap.message.as_deref(), Some("level 20 is over 15"));
    // Outside of a program, nothing is stopped.
    assert!(!mollie::host::raise!("not running"));
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
    let kept = Arc::new(Mutex::new(None::<ScriptCallback<(Vec2,), Box3>>));
    let keeping = Arc::clone(&kept);
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
        *keeping.lock().expect("not poisoned") = Some(f);
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
    let callback = kept.lock().expect("not poisoned").take().expect("kept by the program");

    assert_eq!(
        callback.call_with_limits(&compiler, (Vec2 { x: 5.0, y: 6.0 },), Limits::default()),
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

#[test]
#[should_panic(expected = "don't match the layout")]
fn enums_must_match_their_rust_type() {
    let mut compiler = compiler();

    // `height` is an `f64`: `Rect` takes 24 bytes, `Shape` 16. (A missing
    // `f32` would fit in the padding: only sizes and alignments are checked.)
    Host::new(&mut compiler)
        .enum_::<Shape>("Shape")
        .variant("Circle")
        .field::<f32>("radius")
        .variant("Rect")
        .field::<f32>("width")
        .field::<u64>("height")
        .finish();
}

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

struct AreaTrait;

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

    assert_eq!(shape.call::<_, i32>(&compiler, "area", (2,), Limits::default()), Ok(18));
    // Another signature, or a function the trait doesn't have.
    assert!(shape.call::<_, bool>(&compiler, "area", (2,), Limits::default()).is_err());
    assert!(shape.call::<_, i32>(&compiler, "volume", (2,), Limits::default()).is_err());
    // Given back to the program.
    assert_eq!(
        compiler
            .script_fn::<(ScriptObject<AreaTrait>,), i32>("measure")
            .expect("compiled")
            .call((shape,), Limits::default()),
        Ok(90)
    );
}

struct BehaviourTrait;

fn behaviour_compiler() -> Compiler<()> {
    let mut compiler = compiler();

    Host::new(&mut compiler)
        .trait_::<BehaviourTrait>("Behaviour")
        .method_default::<(i32,), i32>("used", &["times"], "{ times }")
        .method_default::<(), bool>("ticks", &[], "{ false }")
        // Defaults can call the other functions on `self` (overridden or not).
        .method_default::<(), i32>("speed", &[], "{ if self.ticks() { 2 } else { 0 } }")
        .finish();

    compiler
}

#[test]
fn host_traits_have_defaults() {
    let _guard = lock();
    let mut compiler = behaviour_compiler();

    for (name, source) in [
        ("plain", "struct Plain {}\nimpl Behaviour for Plain {}\nPlain {}"),
        (
            "ticking",
            "struct Ticking { rate: i32 }
impl Behaviour for Ticking {
    func ticks(self) -> bool { true }
    func used(self, times: i32) -> i32 { times * self.rate }
}
Ticking { rate: 3 }",
        ),
    ] {
        compiler
            .compile_script::<(), ScriptObject<BehaviourTrait>>(name, &[], source)
            .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));
    }

    let make = |name: &str| {
        compiler
            .script_fn::<(), ScriptObject<BehaviourTrait>>(name)
            .expect("compiled")
            .call((), Limits::default())
            .expect("runs")
    };
    let plain = make("plain");
    let ticking = make("ticking");

    // Defaults.
    assert_eq!(plain.call::<_, i32>(&compiler, "used", (4,), Limits::default()), Ok(4));
    assert_eq!(plain.call::<_, bool>(&compiler, "ticks", (), Limits::default()), Ok(false));
    assert_eq!(plain.call::<_, i32>(&compiler, "speed", (), Limits::default()), Ok(0));
    // Overridden, and a default using an overridden function.
    assert_eq!(ticking.call::<_, i32>(&compiler, "used", (4,), Limits::default()), Ok(12));
    assert_eq!(ticking.call::<_, bool>(&compiler, "ticks", (), Limits::default()), Ok(true));
    assert_eq!(ticking.call::<_, i32>(&compiler, "speed", (), Limits::default()), Ok(2));
}

#[test]
#[should_panic(expected = "the default of `ticks` of `Broken` has errors")]
fn defaults_of_host_traits_are_checked() {
    let _guard = lock();
    let mut compiler = compiler();

    Host::new(&mut compiler)
        .trait_::<BehaviourTrait>("Broken")
        .method_default::<(), bool>("ticks", &[], "{ 1 }")
        .finish();
}

#[test]
fn stubs_have_defaults_of_host_traits() {
    let _guard = lock();
    let compiler = behaviour_compiler();
    let stub = host_stub(&compiler);
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
    assert!(stub.root.contains("func ticks(self) -> bool { false }"), "stub:\n{stub}");

    let i32 = tools.tcx.types.core_types.i32;

    // Functions with defaults can be left out.
    tools.process(
        (),
        "struct Plain {}
impl Behaviour for Plain {
    func ticks(self) -> bool { true }
}
let value: Behaviour = Plain {};

value.speed()",
        Vec::<(String, mollie::typing::TypeRef)>::new(),
        i32,
    );

    assert!(tools.diagnostics.is_empty(), "{:?}\n\nstub:\n{stub}", errors(&tools));
}

struct ShapeTrait;

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
        dot.call::<_, Vec2>(&compiler, "moved", (Vec2 { x: 0.5, y: -1.0 },), Limits::default()),
        Ok(Vec2 { x: 1.5, y: 1.0 })
    );

    let bounds = dot
        .call::<_, Box3>(&compiler, "bounds", (Vec2 { x: 3.0, y: 4.0 }, 7), Limits::default())
        .expect("runs");

    assert_eq!(bounds, Box3 {
        x: 1.5,
        y: 1.0,
        width: 3.0,
        height: 4.0,
        depth: 7.0
    });
    assert_eq!(dot.call::<_, i32>(&compiler, "count", (), Limits::default()), Ok(1));
}

#[test]
fn host_traits_are_types_in_programs() {
    let _guard = lock();
    let mut compiler = area_compiler();

    // `Area` written in the program is the trait the host registered.
    compiler
        .compile_script::<(), ScriptObject<AreaTrait>>(
            "make",
            &[],
            "struct Square { side: i32 }
impl Area for Square { func area(self, scale: i32) -> i32 { self.side * self.side * scale } }
func pick(side: i32) -> Area { Square { side } }
let shapes: Area[] = [pick(2)];
shapes[0]",
        )
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    let shape = compiler
        .script_fn::<(), ScriptObject<AreaTrait>>("make")
        .expect("compiled")
        .call((), Limits::default())
        .expect("runs");

    assert_eq!(shape.call::<_, i32>(&compiler, "area", (3,), Limits::default()), Ok(12));
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

    assert_eq!(counter.call::<_, i32>(&compiler, "area", (1,), Limits::default()), Ok(1));
    assert_eq!(counter.call::<_, i32>(&compiler, "area", (2,), Limits::default()), Ok(3));
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

    assert_eq!(array.to_vec(&compiler), [1, 2, 3]);
    assert_eq!(array.get(&compiler, 3), None);
    assert!(array.set(&compiler, 0, 10));
    assert!(!array.set(&compiler, 5, 0));
    assert!(array.push(&compiler, 4));
    assert_eq!(array.len(&compiler), 4);
    // Without access to the program (outside of it, with no compiler).
    assert_eq!(array.len(&Running), 0);
    assert_eq!(
        compiler
            .script_fn::<(GcArray<i32>,), i32>("sum")
            .expect("compiled")
            .call((array,), Limits::default()),
        Ok(10 + 2 + 3 + 4)
    );
}

#[test]
fn stubs_are_written_only_when_they_change() {
    let _guard = lock();
    let project = std::env::temp_dir().join(format!("mollie-stub-{}", std::process::id()));
    let mut compiler = compiler();

    Host::new(&mut compiler).module("graphics").function("width", || 1);

    assert_eq!(compiler.write_host_stub(&project).ok(), Some(true));
    // Nothing changed: editors aren't disturbed.
    assert_eq!(compiler.write_host_stub(&project).ok(), Some(false));
    assert!(project.join(".mollie/host/graphics.mol").exists());

    // Another host without `graphics`: its file goes.
    let mut other = self::compiler();

    Host::new(&mut other).module("audio").function("volume", || 1);

    assert_eq!(other.write_host_stub(&project).ok(), Some(true));
    assert!(!project.join(".mollie/host/graphics.mol").exists());
    assert!(project.join(".mollie/host/audio.mol").exists());

    std::fs::remove_dir_all(&project).ok();
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
fn stubs_declare_arguments_of_programs() {
    let _guard = lock();
    let compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let stub = host_stub_with(&compiler, &[("limit", i32)]);

    assert!(stub.root.contains("extern let mut limit: i32;"), "stub:\n{stub}");

    // Tools know the arguments, without the host.
    let mut tools = TypedASTContext::default();

    tools.load_host_stub(&stub.root, &stub.modules);

    let i32 = tools.tcx.types.core_types.i32;

    tools.process((), "limit += 1;\nlimit * 2", Vec::<(String, mollie::typing::TypeRef)>::new(), i32);

    let errors = tools
        .diagnostics
        .errors
        .values()
        .map(|error| tools.tcx.display_of_diagnostic(error).to_string())
        .collect::<Vec<_>>();

    assert!(errors.is_empty(), "{errors:?}\n\nstub:\n{stub}");
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

#[test]
fn arrays_of_plain_values_cross_the_boundary() {
    let _guard = lock();
    let mut compiler = compiler();
    let mut host = Host::new(&mut compiler);

    // A new array as a result, and an array copied from an argument.
    host.function_named("squares", &["count"], |count: i32| (1..=count).map(|value| value * value).collect::<Vec<i32>>());
    host.function_named("sum", &["values"], |values: Vec<f32>| values.iter().sum::<f32>());
    host.function_named("flags", &[], || vec![true, false, true]);

    assert_eq!(
        run(
            &mut compiler,
            "let squares = squares(4);
let flags = flags();

squares.push(100);

let mut total = 0;

for square in squares {
    total += square;
}

let on = if flags[0] && !flags[1] && flags[2] { 1000 } else { 0 };

total + sum([1.5, 2.5]) as i32 * 10000 + on"
        ),
        Ok(1 + 4 + 9 + 16 + 100 + 40_000 + 1000)
    );
}

#[test]
fn properties_of_the_host() {
    let _guard = lock();
    let mut compiler = compiler();
    let volume = Arc::new(AtomicI32::new(3));
    let mut host = Host::new(&mut compiler);

    host.object::<Counter>("Counter").field::<i32>("count").finish();
    host.methods::<GcPtr<Counter>>()
        // Computed from the object.
        .property("doubled", |counter: GcPtr<Counter>| counter.count * 2)
        // Kept by the host.
        .property("volume", {
            let volume = Arc::clone(&volume);

            move |_: GcPtr<Counter>| volume.load(Ordering::Relaxed)
        })
        .setter("volume", {
            let volume = Arc::clone(&volume);

            move |_: GcPtr<Counter>, value: i32| volume.store(value, Ordering::Relaxed)
        })
        .finish();

    assert_eq!(
        run(
            &mut compiler,
            "let counter = Counter { count: 21 };

counter.volume = counter.volume * 10;
counter.volume += 1;

counter.doubled * 100 + counter.volume"
        ),
        Ok(42 * 100 + 31)
    );
    assert_eq!(volume.load(Ordering::Relaxed), 31);

    // Stubs declare them, for editors.
    let stub = host_stub(&compiler).root;

    assert!(stub.contains("extern get func doubled(self) -> i32;"), "{stub}");
    assert!(stub.contains("extern set func volume(self, value: i32);"), "{stub}");

    // Tools read them back.
    let mut tools = TypedASTContext::default();

    tools.load_host_stub(&stub, &[]);

    let i32 = tools.tcx.types.core_types.i32;

    tools.process(
        (),
        "let counter = Counter { count: 21 };\ncounter.volume += 1;\ncounter.doubled + counter.volume",
        Vec::<(String, mollie::typing::TypeRef)>::new(),
        i32,
    );

    assert!(tools.diagnostics.is_empty(), "stub:\n{stub}");
}
