//! Value types: `value struct` and `value enum`, copied instead of referenced.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

const POINT: &str = "value struct Point { x: i32, y: i32 }

impl Point {
    func sum(self) -> i32 { self.x + self.y }

    func move_by(mut self, dx: i32) { self.x += dx; }
}
";

#[test]
fn value_types_are_declared() {
    assert_no_errors(check(&format!(
        "{POINT}value enum Shape {{
    Circle {{ radius: f32 }},
    Square {{ side: f32 }},
}}

value struct Pair<T> {{ a: T, b: T }}

let mut p = Point {{ x: 1, y: 2 }};
p.x = 3;
p.move_by(2);

let total: i32 = p.sum();
let pair = Pair {{ a: \"a\", b: \"b\" }};
let shape: Shape = Shape::Circle {{ radius: 1.0 }};
let area = match shape {{
    Circle {{ radius }} => radius * radius,
    Square {{ side }} => side * side,
}};"
    )));
}

#[test]
fn fields_of_immutable_values_cant_change() {
    only_errors(check(&format!("{POINT}let p = Point {{ x: 1, y: 2 }};\np.x = 3;")), |error| {
        matches!(error, TypeError::AssignToImmutable { .. })
    });
}

#[test]
fn fields_of_objects_holding_values_can_change() {
    assert_no_errors(check(&format!(
        "{POINT}struct Holder {{ point: Point }}

let holder = Holder {{ point: Point {{ x: 1, y: 2 }} }};
holder.point.x = 5;

let points = [Point {{ x: 1, y: 2 }}];
points[0].y = 7;
points[0].move_by(1);"
    )));
}

#[test]
fn mut_self_needs_a_mutable_place() {
    only_errors(check(&format!("{POINT}let p = Point {{ x: 1, y: 2 }};\np.move_by(1);")), |error| {
        matches!(error, TypeError::AssignToImmutable { .. })
    });

    // A temporary value just loses the change.
    assert_no_errors(check(&format!("{POINT}Point {{ x: 1, y: 2 }}.move_by(1);")));
}

#[test]
fn self_is_read_only_without_mut_self() {
    only_errors(
        check("value struct Point { x: i32 }\n\nimpl Point {\n    func reset(self) { self.x = 0; }\n}"),
        |error| matches!(error, TypeError::AssignToImmutable { .. }),
    );
}

#[test]
fn mut_self_only_for_value_types() {
    only_errors(
        check("struct Counter { n: i32 }\n\nimpl Counter {\n    func bump(mut self) { self.n += 1; }\n}"),
        |error| matches!(error, TypeError::InvalidMutSelf),
    );
    only_errors(
        check("struct Counter { n: i32 }\ntrait Bump { func bump(self); }\n\nimpl Bump for Counter {\n    func bump(mut self) { self.n += 1; }\n}"),
        |error| matches!(error, TypeError::InvalidMutSelf),
    );
}

#[test]
fn mut_self_in_trait_impls_of_value_types() {
    // Through a trait object, the boxed value changes.
    assert_no_errors(check(
        "trait Bump { func bump(self); }\nvalue struct Counter { n: i32 }\n\nimpl Bump for Counter {\n    func bump(mut self) { self.n += 1; }\n}",
    ));
}

#[test]
fn value_types_cant_contain_themselves() {
    only_errors(check("value struct Node { value: i32, next: Node }"), |error| {
        matches!(error, TypeError::RecursiveValueType { .. })
    });

    only_errors(check("value struct A { b: B }\nvalue struct B { a: A }"), |error| {
        matches!(error, TypeError::RecursiveValueType { .. })
    });

    // Arrays and other types are references.
    assert_no_errors(check("value struct Node { value: i32, next: Node[] }\nstruct Tree { left: Tree[] }"));
}

#[test]
fn value_structs_are_compared_by_fields() {
    assert_no_errors(check(&format!(
        "{POINT}let same: bool = Point {{ x: 1, y: 2 }} == Point {{ x: 1, y: 2 }};
let different: bool = Point {{ x: 1, y: 2 }} != Point {{ x: 2, y: 2 }};"
    )));

    only_errors(check("struct Box { x: i32 }\nlet same = Box { x: 1 } == Box { x: 1 };"), |error| {
        matches!(error, TypeError::InvalidOperator { .. })
    });
}

#[test]
fn value_word_stays_a_name() {
    assert_no_errors(check(
        "struct Wrapper { value: i32 }

func value(value: i32) -> i32 { value }

let value = Wrapper { value: value(1) };",
    ));
}
