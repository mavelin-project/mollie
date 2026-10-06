//! Patterns of `is` and `match`.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

const SHAPE: &str = "enum Shape {
    Circle { radius: i32 },
    Rect { width: i32, height: i32 },
    Empty,
}
";

#[test]
fn exhaustive_match_on_enum() {
    assert_no_errors(check(&format!(
        "{SHAPE}
func area(shape: Shape) -> i32 {{
    match shape {{
        Shape::Circle {{ radius }} => radius * radius * 3,
        Shape::Rect {{ width, height }} => width * height,
        Shape::Empty => 0,
    }}
}}"
    )));
}

#[test]
fn variants_can_be_written_without_their_enum() {
    assert_no_errors(check(&format!(
        "{SHAPE}
func area(shape: Shape) -> i32 {{
    match shape {{
        Circle {{ radius }} => radius,
        Rect {{ width }} => width,
        Empty => 0,
    }}
}}"
    )));
}

#[test]
fn missing_variants_are_reported() {
    let error = single_error(check(&format!(
        "{SHAPE}
func area(shape: Shape) -> i32 {{
    match shape {{
        Circle {{ radius }} => radius,
    }}
}}"
    )));

    assert!(
        matches!(&error, TypeError::NonExhaustive { missing } if missing == &["Rect", "Empty"]),
        "{error:?}"
    );
}

#[test]
fn guarded_arms_and_refutable_fields_do_not_cover_variants() {
    let error = single_error(check(&format!(
        "{SHAPE}
func area(shape: Shape) -> i32 {{
    match shape {{
        Circle {{ radius }} if radius > 1 => radius,
        Rect {{ width: 1, height }} => height,
        Empty => 0,
    }}
}}"
    )));

    assert!(
        matches!(&error, TypeError::NonExhaustive { missing } if missing == &["Circle", "Rect"]),
        "{error:?}"
    );
}

#[test]
fn wildcard_and_bindings_cover_everything() {
    assert_no_errors(check(&format!(
        "{SHAPE}
func is_empty(shape: Shape) -> bool {{
    match shape {{
        Empty => true,
        _ => false,
    }}
}}

func size(n: i32) -> i32 {{
    match n {{
        0 => 0,
        other => other * 2,
    }}
}}"
    )));
}

#[test]
fn numbers_need_a_pattern_matching_anything() {
    let error = single_error(check(
        "func name(n: i32) -> string {
    match n {
        0 => \"zero\",
        1 => \"one\",
    }
}",
    ));

    assert!(matches!(&error, TypeError::NonExhaustive { missing } if missing == &["_"]), "{error:?}");
}

#[test]
fn booleans_are_covered_by_both_values() {
    assert_no_errors(check(
        "func flip(value: bool) -> bool {
    match value {
        true => false,
        false => true,
    }
}",
    ));
}

#[test]
fn bindings_have_the_types_of_fields() {
    let errors = check(&format!(
        "{SHAPE}
func radius(shape: Shape) -> bool {{
    match shape {{
        Circle {{ radius }} => radius,
        _ => false,
    }}
}}"
    ));

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn guards_are_booleans() {
    let errors = check(
        "func check(n: i32) -> i32 {
    match n {
        value if value => 1,
        _ => 0,
    }
}",
    );

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn arms_have_the_same_type() {
    let errors = check(
        "func check(n: i32) -> i32 {
    match n {
        0 => 1,
        _ => true,
    }
}",
    );

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn literals_have_the_matched_type() {
    let errors = check(
        "func check(n: i32) -> i32 {
    match n {
        \"zero\" => 1,
        _ => 0,
    }
}",
    );

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn unknown_fields_are_reported() {
    let errors = check(&format!(
        "{SHAPE}
func check(shape: Shape) -> i32 {{
    match shape {{
        Circle {{ diameter }} => diameter,
        _ => 0,
    }}
}}"
    ));

    assert!(
        errors
            .0
            .iter()
            .any(|error| matches!(error, TypeError::NoField { name, .. } if name == "diameter")),
        "{errors:?}"
    );
}

#[test]
fn pattern_of_another_type_is_reported() {
    let errors = check(&format!(
        "{SHAPE}
struct Point {{ x: i32 }}

func check(shape: Shape) -> i32 {{
    match shape {{
        Point {{ x }} => x,
        _ => 0,
    }}
}}"
    ));

    assert!(errors.0.iter().any(|error| matches!(error, TypeError::PatternMismatch { .. })), "{errors:?}");
}

#[test]
fn nested_patterns_use_types_of_fields() {
    assert_no_errors(check(&format!(
        "{SHAPE}
func radius(shape: Option<Shape>) -> i32 {{
    match shape {{
        Some {{ value: Circle {{ radius }} }} => radius,
        Some {{ value: Rect {{ width: 0, height }} }} => height,
        Some => 1,
        None => 0,
    }}
}}"
    )));
}

#[test]
fn structs_can_be_destructured() {
    assert_no_errors(check(
        "struct Point { x: i32, y: i32 }

func on_axis(point: Point) -> bool {
    match point {
        Point { x: 0 } => true,
        Point { y: 0 } => true,
        Point { x, y } => x == y && false,
    }
}",
    ));
}

#[test]
fn unit_variants_in_is_expressions() {
    assert_no_errors(check(
        "func check(value: Option<i32>) -> i32 {
    if value is None {
        0
    } else if value is Some { value } {
        value
    } else {
        1
    }
}",
    ));
}

#[test]
fn match_as_a_statement() {
    assert_no_errors(check(
        "let mut total = 0;
let value: Option<i32> = Option::Some { value: 2 };

match value {
    Some { value } => {
        total += value;
    }
    None => {}
}

total += 1;",
    ));
}
