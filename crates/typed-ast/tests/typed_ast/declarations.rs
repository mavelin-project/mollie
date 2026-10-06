use mollie_typing::{TypeError, TypeErrorValue};

use crate::{assert_no_errors, check, single_error};

#[test]
fn struct_construction_and_field_access() {
    let errors = check(
        "struct Point { x: f32, y: f32 }
let p = Point { x: 1.0, y: 2.0 };
let x: f32 = p.x;",
    );

    assert_no_errors(errors);
}

#[test]
fn constructing_wrong_struct_is_rejected() {
    let error = single_error(check(
        "struct Point { x: f32, y: f32 }
struct Vector { x: f32, y: f32 }
let p: Point = Vector { x: 1.0, y: 2.0 };",
    ));

    assert!(matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn missing_field_is_reported() {
    let error = single_error(check(
        "struct Point { x: f32, y: f32 }
let p = Point { x: 1.0 };",
    ));

    assert!(matches!(error, TypeError::MissingField { ref name, .. } if name == "y"));
}

#[test]
fn unknown_field_is_reported() {
    let error = single_error(check(
        "struct Point { x: f32 }
let p = Point { x: 1.0, z: 2.0 };",
    ));

    assert!(matches!(error, TypeError::NoField { ref name, .. } if name == "z"));
}

#[test]
fn field_with_default_value_can_be_omitted() {
    let errors = check(
        "struct Config { size: f32 = 10.0, name: string }
let config = Config { name: \"main\" };",
    );

    assert_no_errors(errors);
}

#[test]
fn mistyped_default_value_is_reported_once() {
    let error = single_error(check("struct Config { size: f32 = 1 }"));

    assert!(matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn default_value_can_use_adt_declared_later_and_generics() {
    let errors = check(
        "struct Slot<T> { item: Option<T> = Option::None }
enum Option<T> { Some { value: T }, None }
let slot: Slot<i32> = Slot {};",
    );

    assert_no_errors(errors);
}

#[test]
fn enum_variants_can_be_constructed() {
    let errors = check(
        "enum Shape { Circle { radius: f32 }, Empty }
let circle = Shape::Circle { radius: 1.0 };
let empty = Shape::Empty;",
    );

    assert_no_errors(errors);
}

#[test]
fn variant_without_its_fields_is_reported() {
    let error = single_error(check(
        "enum Shape { Circle { radius: f32 }, Empty }
let circle = Shape::Circle;",
    ));

    assert!(matches!(error, TypeError::MissingField { ref name, .. } if name == "radius"));
}

#[test]
fn duplicate_declaration_is_reported() {
    let error = single_error(check(
        "struct Point {}
struct Point {}",
    ));

    assert!(matches!(error, TypeError::AlreadyExists { ref name, .. } if name == "Point"));
}

#[test]
fn type_can_be_used_before_its_declaration() {
    let errors = check(
        "struct Line { start: Point, end: Point }
struct Point { x: f32, y: f32 }
let line = Line { start: Point { x: 0.0, y: 0.0 }, end: Point { x: 1.0, y: 1.0 } };",
    );

    assert_no_errors(errors);
}

#[test]
fn recursive_struct_does_not_loop_forever() {
    let errors = check(
        "struct Node { next: Node }
func visit(node: Node) {}",
    );

    assert_no_errors(errors);
}

#[test]
fn adt_is_not_a_value() {
    let error = single_error(check(
        "struct Point {}
let p = Point;",
    ));

    assert!(matches!(error, TypeError::Unexpected {
        expected: TypeErrorValue::Value,
        found: TypeErrorValue::Adt(..)
    }));
}

#[test]
fn field_and_method_with_the_same_name() {
    // `value.name` is the field, `value.name()` calls the method.
    assert_no_errors(check(
        "struct Counter { count: i32 }
impl Counter {
    func count(self) -> bool { self.count > 0 }
}
let counter = Counter { count: 1 };
let field: i32 = counter.count;
let method: bool = counter.count();",
    ));
}

#[test]
fn field_holding_a_function_is_called() {
    assert_no_errors(check(
        "struct Button { on_click: func(i32) -> i32 }
let button = Button { on_click: |x| { x * 2 } };
let result: i32 = button.on_click(2);",
    ));
}

#[test]
fn calling_a_field_that_is_not_a_function_is_reported() {
    let errors = check(
        "struct Point { x: i32 }
let point = Point { x: 1 };
point.x();",
    );

    assert!(!errors.0.is_empty());
}
