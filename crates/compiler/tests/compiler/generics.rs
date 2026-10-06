//! Generic functions, compiled for each instantiation.

use crate::{run_i32, run_i32_stressed};

const POINT: &str = "struct Point { x: i32, y: i32 }\n";
const HOLDER: &str = "struct Holder<T> { value: T }\n";

#[test]
fn generic_function_with_different_type_arguments() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}func id<T>(value: T) -> T {{ value }}

let point = id(Point {{ x: 3, y: 4 }});
id(10) + point.x + point.y"
        )),
        17
    );
}

#[test]
fn generic_function_calls_generic_functions() {
    assert_eq!(
        run_i32(&format!(
            "{HOLDER}func wrap<T>(value: T) -> Holder<T> {{ Holder {{ value }} }}
func unwrap<T>(holder: Holder<T>) -> T {{ holder.value }}
func round_trip<T>(value: T) -> T {{ unwrap(wrap(value)) }}

round_trip(5) + round_trip(Holder {{ value: 2 }}).value"
        )),
        7
    );
}

#[test]
fn generic_function_with_closure() {
    assert_eq!(
        run_i32(
            "func apply<T>(value: T, f: func(T) -> T) -> T { f(value) }

let factor = 2;
apply(20, |x| { x * factor }) + apply(1, |x| { x + 1 })"
        ),
        42
    );
}

#[test]
fn generic_function_used_as_a_value() {
    assert_eq!(
        run_i32(
            "func id<T>(value: T) -> T { value }
func call(f: func(i32) -> i32, x: i32) -> i32 { f(x) }

call(id, 42)"
        ),
        42
    );
}

#[test]
fn generic_arrays_survive_collections() {
    assert_eq!(
        run_i32_stressed(&format!(
            "{POINT}func fill<T>(value: T, count: i32) -> T[] {{
    let items = [value];
    let mut i = 1;

    while i < count {{
        items.push(value);
        i += 1;
    }}

    items
}}

let points = fill(Point {{ x: 2, y: 3 }}, 50);
let numbers = fill(7, 10);

let mut total = 0;
let mut i = 0;

while i < 50 {{
    total += points[i].x + points[i].y;
    i += 1;
}}

total + numbers[9]"
        )),
        257
    );
}
