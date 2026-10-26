use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

#[test]
fn fixed_arrays_are_declared() {
    assert_no_errors(check(
        "struct Mesh { corners: f32[2][4], ids: i32[3] }
value struct Color { rgb: u8[3] }

func sum(values: i32[3]) -> i32 { values[0] + values[1] + values[2] }
func make() -> i32[3] { [1, 2, 3] }

let mut a: i32[3] = make();
a[0] = sum([4, 5, 6]);

let mesh = Mesh { corners: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], ids: a };
mesh.ids[1] = 2;

let mut color = Color { rgb: [1, 2, 3] };
color.rgb[0] = 10;

// Literals nothing decides are growable.
let growable = [1, 2, 3];
growable.push(4);",
    ));
}

#[test]
fn literals_of_other_lengths_are_errors() {
    only_errors(check("let a: i32[3] = [1, 2];"), |error| matches!(error, TypeError::ArgumentCountMismatch { .. }));
}

#[test]
fn growable_and_fixed_arrays_are_different_types() {
    only_errors(check("func sum(values: i32[3]) -> i32 { values[0] }\n\nlet a: i32[] = [1, 2, 3];\nsum(a);"), |error| {
        matches!(error, TypeError::Unexpected { .. })
    });
}

#[test]
fn elements_need_a_mutable_place() {
    only_errors(check("let a: i32[3] = [1, 2, 3];\na[0] = 5;"), |error| {
        matches!(error, TypeError::AssignToImmutable { name } if name == "a")
    });
    only_errors(
        check("value struct Color { rgb: u8[3] }\n\nlet color = Color { rgb: [1, 2, 3] };\ncolor.rgb[0] = 5;"),
        |error| matches!(error, TypeError::AssignToImmutable { name } if name == "color"),
    );
}

#[test]
fn fixed_arrays_can_not_grow() {
    only_errors(check("let mut a: i32[3] = [1, 2, 3];\na.push(4);"), |error| {
        matches!(error, TypeError::NonIndexable { name, .. } if name == "push")
    });
}

#[test]
fn any_size_is_only_for_parameters() {
    only_errors(check("let a: i32[_] = [1, 2, 3];"), |error| matches!(error, TypeError::InvalidArray { .. }));
}

#[test]
fn any_size_takes_only_fixed_arrays() {
    assert_no_errors(check("func first(values: i32[_]) -> i32 { values[0] }\n\nlet a: i32[2] = [1, 2];\nfirst(a);\nfirst([1, 2, 3]);"));
    only_errors(check("func first(values: i32[_]) -> i32 { values[0] }\n\nlet a: i32[] = [1, 2];\nfirst(a);"), |error| {
        matches!(error, TypeError::Unexpected { .. })
    });
    only_errors(check("struct Holder { values: i32[_] }"), |error| matches!(error, TypeError::InvalidArray { .. }));
}

