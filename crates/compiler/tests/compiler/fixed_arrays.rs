use mollie_compiler::sandbox::{Limits, TrapKind};

use crate::{compile_error_text, run_bool, run_f32, run_i32, run_i32_stressed, run_limited};

#[test]
fn literals_become_fixed_arrays_where_expected() {
    assert_eq!(run_i32("let a: i32[3] = [1, 2, 3];\na[0] + a[1] * 10 + a[2] * 100"), 321);
    assert!((run_f32("let v: f32[2] = [0.5, 1.5];\nv[1] - v[0]") - 1.0).abs() < f32::EPSILON);
    assert!(run_bool("let flags: bool[3] = [true, false, true];\nflags[2] && !flags[1]"));
}

#[test]
fn elements_at_indices_known_at_run_time() {
    assert_eq!(
        run_i32(
            "let mut a: i32[4] = [1, 2, 3, 4];
let mut i: usize = 0;
let mut total = 0;

while i < 4 {
    a[i] = a[i] * 10;
    total += a[i];
    i += 1;
}

total + a[3]"
        ),
        100 + 40
    );
}

#[test]
fn values_are_copied() {
    assert_eq!(
        run_i32(
            "let a: i32[3] = [1, 2, 3];
let mut b = a;

b[0] = 10;

let i: usize = 2;
b[i] = 30;

a[0] + a[2] * 10 + b[0] * 100 + b[2] * 1000"
        ),
        1 + 30 + 1000 + 30000
    );
}

#[test]
fn parameters_and_results() {
    assert_eq!(
        run_i32(
            "func sum(values: i32[3]) -> i32 { values[0] + values[1] + values[2] }

func scaled(values: i32[3], by: i32) -> i32[3] {
    let mut result = values;
    let mut i: usize = 0;

    while i < 3 {
        result[i] = values[i] * by;
        i += 1;
    }

    result
}

let a: i32[3] = [1, 2, 3];

sum(scaled(a, 10)) + sum(a)"
        ),
        60 + 6
    );
}

#[test]
fn fields_of_structs_and_value_structs() {
    assert_eq!(
        run_i32(
            "struct Mesh { corners: f32[2][2], ids: i32[3] }
value struct Color { rgb: u8[3], alpha: u8 }

let mesh = Mesh { corners: [[1.0, 2.0], [3.0, 4.0]], ids: [7, 8, 9] };

mesh.ids[1] = 80;
mesh.corners[1][0] = 30.0;

let mut color = Color { rgb: [10, 20, 30], alpha: 255 };
let copy = color;

color.rgb[2] = 40;

let i: usize = 1;

mesh.ids[i] + mesh.corners[i][0] as i32 + color.rgb[2] as i32 + copy.rgb[2] as i32 + color.alpha as i32"
        ),
        80 + 30 + 40 + 30 + 255
    );
}

#[test]
fn arrays_of_fixed_arrays_and_generics() {
    assert_eq!(
        run_i32(
            "let mut grid: i32[3][] = [];

grid.push([1, 2, 3]);
grid.push([4, 5, 6]);
grid[1][2] = 60;

let found: Option<i32[3]> = Option::Some { value: grid[1] };

match found {
    Some { value } => value[0] + value[2] + grid[0][1],
    None => 0,
}"
        ),
        4 + 60 + 2
    );
}

#[test]
fn closures_capture_arrays() {
    assert_eq!(
        run_i32(
            "func make(values: i32[2]) -> func(i32) -> i32 {
    |x| { values[0] * x + values[1] }
}

let f = make([3, 4]);

f(10)"
        ),
        34
    );
}

#[test]
fn arrays_are_compared_by_elements() {
    assert!(run_bool(
        "let a: i32[3] = [1, 2, 3];
let b: i32[3] = [1, 2, 3];
let c: i32[3] = [1, 2, 4];

a == b && a != c"
    ));
}

#[test]
fn references_in_fixed_arrays_survive_collections() {
    assert_eq!(
        run_i32_stressed(
            "struct Named { names: string[2] }

let mut names: string[3] = [\"a\", \"b\", \"c\"];
let all: Named[] = [];
let mut i: usize = 0;

while i < 20 {
    names[i % 3] = \"name ${i}\";
    all.push(Named { names: [\"first ${i}\", \"second ${i}\"] });
    i += 1;
}

let mut total = 0;

for named in all { total += named.names[0].len() as i32 + named.names[1].len() as i32; }

total + names[0].len() as i32 + names[1].len() as i32 + names[2].len() as i32"
        ),
        // `first i` is 7 or 8 bytes, `second i` 8 or 9; the last names are
        // `name 18`, `name 19` and `name 17`.
        (10 * 7 + 10 * 8) + (10 * 8 + 10 * 9) + 3 * 7
    );
}

#[test]
fn index_out_of_bounds() {
    let trap = run_limited::<i32>("let a: i32[3] = [1, 2, 3];\nlet i: usize = 3;\na[i]", |types| types.i32, Limits::default(), false);

    assert_eq!(trap.map_err(|trap| trap.kind), Err(TrapKind::OutOfBounds));
}

#[test]
fn elements_of_immutable_arrays_can_not_change() {
    let error = compile_error_text("let a: i32[3] = [1, 2, 3];\na[0] = 5;");

    assert!(error.contains("immutable variable `a`"), "{error}");
}

#[test]
fn arrays_larger_than_the_limit_are_errors() {
    // 8000 bytes: compiled with `Option`, whose value holds the array.
    let error = compile_error_text("let big: Option<i64[1000]> = Option::None;");

    assert!(error.contains("larger than 4096 bytes"), "{error}");
}

#[test]
fn arrays_of_any_size_by_value() {
    assert_eq!(
        run_i32(
            "func total(values: i32[_]) -> i32 {
    let mut sum = 0;

    for value in values { sum += value; }

    sum * values.len() as i32
}

func last(values: string[_]) -> string { values[values.len() - 1] }

func cleared(values: i32[_]) -> i32 {
    values[0] = 0;

    values[0]
}

let small: i32[2] = [1, 2];
let large: i32[4] = [1, 2, 3, 4];

total(small) * 1000 + total(large) + last([\"a\", \"bcd\"]).len() as i32 * 100000 + cleared(small) + small[0] * 10"
        ),
        3 * 100000 + 6000 + 40 + 10
    );
}

