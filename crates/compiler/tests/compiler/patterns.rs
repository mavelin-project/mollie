//! `match` and patterns.

use crate::{run_i32, run_i32_stressed};

const SHAPE: &str = "enum Shape {
    Circle { radius: i32 },
    Rect { width: i32, height: i32 },
    Empty,
}
";

#[test]
fn match_on_enum_variants() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPE}
func area(shape: Shape) -> i32 {{
    match shape {{
        Circle {{ radius }} => radius * radius * 3,
        Shape::Rect {{ width, height }} => width * height,
        Empty => 0,
    }}
}}

area(Shape::Circle {{ radius: 2 }}) * 10000 + area(Shape::Rect {{ width: 3, height: 4 }}) * 10 + area(Shape::Empty)"
        )),
        12 * 10000 + 12 * 10
    );
}

#[test]
fn arms_are_tried_in_order_with_guards() {
    assert_eq!(
        run_i32(
            "func describe(value: Option<i32>) -> i32 {
    match value {
        Some { value } if value > 10 => 2,
        Some { value } => value,
        None => 0,
    }
}

describe(Option::Some { value: 20 }) * 100 + describe(Option::Some { value: 7 }) * 10 + describe(Option::None)"
        ),
        270
    );
}

#[test]
fn literals_and_wildcards() {
    assert_eq!(
        run_i32(
            "func name(n: i32) -> i32 {
    match n {
        1 => 10,
        3 => 30,
        _ => 0,
    }
}

name(1) + name(3) + name(5)"
        ),
        40
    );
}

#[test]
fn strings_and_booleans() {
    assert_eq!(
        run_i32(
            r#"func code(name: string) -> i32 {
    match name {
        "a" => 1,
        "b" => 2,
        _ => 3,
    }
}

func bit(flag: bool) -> i32 {
    match flag {
        true => 1,
        false => 0,
    }
}

code("b") * 10 + code("z") + bit(true) * 100"#
        ),
        123
    );
}

#[test]
fn bindings_of_whole_values() {
    assert_eq!(
        run_i32(
            "func double_large(n: i32) -> i32 {
    match n {
        large if large > 3 => large * 2,
        small => small,
    }
}

double_large(5) * 10 + double_large(2)"
        ),
        102
    );
}

#[test]
fn nested_patterns() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPE}
func size(shape: Option<Shape>) -> i32 {{
    match shape {{
        Some {{ value: Rect {{ width: 0, height }} }} => height,
        Some {{ value: Rect {{ width, height }} }} => width * height,
        Some {{ value: Circle {{ radius }} }} => radius,
        Some => 1,
        None => 0,
    }}
}}

size(Option::Some {{ value: Shape::Rect {{ width: 0, height: 7 }} }}) * 1000
    + size(Option::Some {{ value: Shape::Rect {{ width: 2, height: 3 }} }}) * 100
    + size(Option::Some {{ value: Shape::Circle {{ radius: 4 }} }}) * 10
    + size(Option::Some {{ value: Shape::Empty }})"
        )),
        7641
    );
}

#[test]
fn struct_patterns() {
    assert_eq!(
        run_i32(
            "struct Point { x: i32, y: i32 }

func classify(point: Point) -> i32 {
    match point {
        Point { x: 0, y: 0 } => 0,
        Point { x: 0 } => 1,
        Point { x, y } => x + y,
    }
}

classify(Point { x: 0, y: 0 }) + classify(Point { x: 0, y: 5 }) * 10 + classify(Point { x: 2, y: 3 }) * 100"
        ),
        510
    );
}

#[test]
fn match_as_a_statement_and_unit_variants_in_is() {
    assert_eq!(
        run_i32(
            "let mut total = 0;
let values = [Option::Some { value: 1 }, Option::None, Option::Some { value: 5 }];

for value in values {
    match value {
        Some { value } => {
            total += value;
        }
        None => {
            total += 100;
        }
    }

    if value is None {
        total += 1000;
    }
}

total"
        ),
        1106
    );
}

#[test]
fn matched_values_survive_collections() {
    assert_eq!(
        run_i32_stressed(&format!(
            "{SHAPE}
func make(i: i32) -> Shape {{
    if i == 0 {{ Shape::Empty }} else {{ Shape::Rect {{ width: i, height: 2 }} }}
}}

let mut total = 0;
let mut i = 0;

while i < 20 {{
    total += match make(i) {{
        Rect {{ width, height }} => {{
            let other = make(width + 1);

            width * height + match other {{
                Rect {{ width }} => width,
                _ => 0,
            }}
        }}
        _ => 0,
    }};
    i += 1;
}}

total"
        )),
        // `i * 2 + (i + 1)` for `i` from 1 to 19.
        (1..20).map(|i| i * 2 + i + 1).sum::<i32>()
    );
}

#[test]
fn prelude_variants_are_known_by_name() {
    // The type of `found()` is known after the pattern: `Some` is
    // `Option::Some`, not a name bound to the value.
    assert_eq!(
        run_i32(
            "let found = || { Option::Some { value: 3 } };
let missing = || { let none: Option<i32> = Option::None; none };
let a = if found() is Some { 1 } else { 0 };
let b = if missing() is Some { 10 } else { 0 };
a + b"
        ),
        1
    );
}

#[test]
fn misspelled_variants_are_errors() {
    let text = crate::compile_error_text(&format!(
        "{SHAPE}func area(shape: Shape) -> i32 {{ match shape {{ Circel => 1, _ => 0 }} }}\narea(Shape::Empty)"
    ));

    assert!(text.contains("Circel"), "{text}");
}
