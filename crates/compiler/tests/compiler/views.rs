use mollie_compiler::sandbox::{Limits, TrapKind};

use crate::{run_bool, run_i32, run_i32_stressed, run_limited};

const SUM: &str = "func sum(values: i32[..]) -> i32 {
    let mut total = 0;

    for value in values { total += value; }

    total
}

func double(mut values: i32[..]) {
    let mut i = 0usize;

    while i < values.len() {
        values[i] *= 2;
        i += 1;
    }
}
";

#[test]
fn views_take_every_array() {
    assert_eq!(
        run_i32(&format!(
            "{SUM}let fixed: i32[3] = [1, 2, 3];
let growable = [10, 20];

sum(fixed) + sum(growable) + sum([100, 200])"
        )),
        6 + 30 + 300
    );
}

#[test]
fn writable_views_change_the_array() {
    assert_eq!(
        run_i32(&format!(
            "{SUM}let mut fixed: i32[3] = [1, 2, 3];
let growable = [10, 20];

double(fixed);
double(growable);
// A temporary just loses the change.
double([5, 5]);

fixed[0] + fixed[2] * 10 + growable[1] * 100"
        )),
        2 + 60 + 4000
    );
}

#[test]
fn methods_of_views_on_every_array() {
    assert_eq!(
        run_i32(
            "let mut fixed: i32[4] = [3, 1, 4, 2];

fixed.sort_by(|a, b| { a - b });

let mut reversed: i32[3] = [1, 2, 3];

reversed.reverse();

let growable = fixed.to_array();

growable.push(5);

let found = if fixed.contains(4) && !fixed.is_empty() { 1 } else { 0 };

fixed[0] * 1000 + fixed[3] * 100 + reversed[0] * 10 + growable.len() as i32 + found * 10000"
        ),
        10000 + 1000 + 400 + 30 + 5
    );
}

#[test]
fn length_of_fixed_arrays() {
    assert!(run_bool("let a: f32[5] = [0.0, 0.0, 0.0, 0.0, 0.0];\na.len() == 5"));
}

#[test]
fn for_loops_over_every_array() {
    assert_eq!(
        run_i32(
            "func count(values: i32[..]) -> i32 {
    let mut found = 0;

    for value in values {
        if value < 0 { continue; }
        if value > 100 { break; }

        found += 1;
    }

    found
}

let fixed: i32[5] = [1, -1, 2, 200, 3];
let mut total = 0;

for value in fixed { total += value; }
for value in [10, 20] { total += value; }

count(fixed) * 1000 + total"
        ),
        2000 + 205 + 30
    );
}

#[test]
fn references_written_through_views_survive_collections() {
    assert_eq!(
        run_i32_stressed(
            "func fill(mut names: string[..], prefix: string) {
    let mut i = 0usize;

    while i < names.len() {
        names[i] = \"${prefix} ${i}\";
        i += 1;
    }
}

func total(names: string[..]) -> i32 {
    let mut length = 0;

    for name in names { length += name.len() as i32; }

    length
}

let mut names: string[4] = [\"\", \"\", \"\", \"\"];
let mut sum = 0;
let mut round = 0;

while round < 10 {
    fill(names, \"name\");
    sum += total(names) + total([\"a ${round}\", \"b ${round}\"]);
    round += 1;
}

sum"
        ),
        // `name i` is 6 bytes, `a r` and `b r` 3 bytes.
        10 * (4 * 6 + 2 * 3)
    );
}

#[test]
fn traps_inside_calls_given_stack_arrays() {
    let source = "func get(names: string[..], index: usize) -> i32 { names[index].len() as i32 }

let names: string[2] = [\"a\", \"bb\"];

get(names, 5)";

    let trap = run_limited::<i32>(source, |types| types.i32, Limits::default(), true);

    assert_eq!(trap.map_err(|trap| trap.kind), Err(TrapKind::OutOfBounds));
    // The next run starts without the trapped call's arrays.
    assert_eq!(
        run_i32_stressed("func get(names: string[..], index: usize) -> i32 { names[index].len() as i32 }\n\nlet names: string[2] = [\"a\", \"b${1}\"];\n\nget(names, 1)"),
        2
    );
}

#[test]
fn variadic_functions() {
    assert_eq!(
        run_i32(
            "func total(base: i32, ..values: i32[..]) -> i32 {
    let mut sum = base;

    for value in values { sum += value; }

    sum
}

func longest(..names: string[..]) -> i32 {
    let mut length = 0;

    for name in names {
        if name.len() as i32 > length { length = name.len() as i32; }
    }

    length
}

total(1) + total(10, 1, 2, 3) * 10 + longest(\"a\", \"bcd\", \"x${12}\") * 1000 + longest()"
        ),
        1 + 160 + 3000
    );
}
