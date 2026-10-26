use mollie_compiler::sandbox::{Limits, TrapKind};
use mollie_typing::TypeRef;

use crate::{compiler, lock, run_i32, run_i32_stressed, run_limited};

#[test]
fn small_functions_defined_after_their_callers() {
    // `main` and `outer` are compiled before the functions they call.
    assert_eq!(
        run_i32(
            "func outer(x: i32) -> i32 { middle(x) * 2 }
func middle(x: i32) -> i32 { inner(x) + inner(x + 1) }
func inner(x: i32) -> i32 { x * x }

let mut total = 0;

for i in 0..10 { total += outer(i); }

total"
        ),
        (0..10).map(|i| (i * i + (i + 1) * (i + 1)) * 2).sum::<i32>()
    );
}

#[test]
fn recursion_stays_calls() {
    assert_eq!(
        run_i32(
            "func fib(n: i32) -> i32 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
func even(n: i32) -> bool { if n == 0 { true } else { odd(n - 1) } }
func odd(n: i32) -> bool { if n == 0 { false } else { even(n - 1) } }

let parity = if even(10) && odd(7) { 1 } else { 0 };

fib(15) + parity"
        ),
        611
    );
}

#[test]
fn inlined_functions_keep_objects_alive() {
    // Every allocation collects garbage: objects made by inlined calls must be
    // seen by the collector.
    assert_eq!(
        run_i32_stressed(
            "struct Point { x: i32, y: i32 }

func point(x: i32) -> Point { Point { x: x, y: x * 2 } }
func sum(a: Point, b: Point) -> Point { Point { x: a.x + b.x, y: a.y + b.y } }

let mut total = point(0);

for i in 0..50 { total = sum(total, point(i)); }

total.x + total.y"
        ),
        (0..50).sum::<i32>() * 3
    );
}

#[test]
fn data_of_inlined_functions() {
    // Vtables and strings are data the bodies name: inlined, they must name
    // the same data in their callers, whose other data comes first.
    assert_eq!(
        run_i32(
            r#"trait Shape { func area(self) -> i32; }
struct Square { side: i32 }
impl Shape for Square { func area(self) -> i32 { self.side * self.side } }

func pick(side: i32) -> Shape { Square { side: side } }
func word() -> string { "inlined" }

let first = "caller".len() as i32;
let shapes: Shape[] = [pick(3)];

first * 100 + shapes[0].area() * 10 + word().len() as i32"#
        ),
        600 + 90 + 7
    );
}

#[test]
fn traps_in_inlined_functions_keep_their_frames() {
    let trap = run_limited::<i32>(
        "func at(values: i32[], index: i32) -> i32 { values[index as usize] }
func second(values: i32[]) -> i32 { at(values, 1) }

second([7])",
        |types| types.i32,
        Limits::default(),
        false,
    )
    .expect_err("the index is out of bounds");
    let functions = trap.backtrace.iter().map(|frame| frame.function.as_str()).collect::<Vec<_>>();

    assert_eq!(trap.kind, TrapKind::OutOfBounds);
    assert!(functions.contains(&"at") && functions.contains(&"second"), "{trap}");
}

#[test]
fn programs_without_fuel_ignore_its_limit() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;

    compiler.set_fuel(false);

    let mut provider = compiler.start_compiling();

    if let Err(error) = provider.compile(
        "main",
        Vec::<(String, TypeRef)>::new(),
        Some(i32),
        "func step(i: i32) -> i32 { i + 1 }\nlet mut i = 0;\nwhile i < 1000 { i = step(i); }\ni",
    ) {
        panic!("compilation failed:\n{}", error.display(&provider.type_context.tcx));
    }

    let main = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("main") }.expect("`main` must be compiled");
    let limits = Limits {
        fuel: Some(10),
        ..Limits::default()
    };

    assert_eq!(provider.compiler.run(limits, || main()), Ok(1000));
}
