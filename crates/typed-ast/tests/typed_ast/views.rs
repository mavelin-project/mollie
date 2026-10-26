use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

fn invalid_array(error: &TypeError) -> bool {
    matches!(error, TypeError::InvalidArray { .. })
}

#[test]
fn views_are_declared() {
    assert_no_errors(check(
        "func sum(values: i32[..]) -> i32 { values[0] + values.len() as i32 }
func clear(mut values: i32[..]) { values[0] = 0; }

let mut fixed: i32[3] = [1, 2, 3];
let growable = [1, 2];

sum(fixed);
sum(growable);
sum([4, 5]);
clear(fixed);
clear(growable);
fixed.reverse();

let viewed: i32[..] = growable;
let mut writable: i32[..] = growable;
writable[0] = viewed[1];

for value in fixed { sum([value]); }",
    ));
}

#[test]
fn views_can_not_be_stored() {
    only_errors(check("struct Holder { values: i32[..] }"), invalid_array);
    only_errors(check("func keep(values: i32[..]) -> i32[..] { values }"), invalid_array);
    only_errors(check("func keep(values: i32[..]) { let all: i32[..][] = [values]; }"), invalid_array);
    only_errors(check("func keep(values: i32[..]) { let kept = Option::Some { value: values }; }"), invalid_array);
    only_errors(check("func keep(values: i32[..]) { let first = || { values[0] }; }"), invalid_array);
}

#[test]
fn elements_change_only_through_writable_views() {
    only_errors(check("func clear(values: i32[..]) { values[0] = 0; }"), invalid_array);
    only_errors(check("func clear(mut value: i32) {}"), invalid_array);
    only_errors(check("let a: i32[3] = [1, 2, 3];\na.reverse();"), |error| {
        matches!(error, TypeError::AssignToImmutable { name } if name == "a")
    });
    only_errors(check("func clear(mut values: i32[..]) {}\n\nlet a: i32[3] = [1, 2, 3];\nclear(a);"), |error| {
        matches!(error, TypeError::AssignToImmutable { name } if name == "a")
    });
}

#[test]
fn views_can_not_grow() {
    only_errors(check("func grow(values: i32[..]) { values.push(1); }"), |error| {
        matches!(error, TypeError::NonIndexable { name, .. } if name == "push")
    });
}

#[test]
fn variables_can_not_view_fixed_arrays() {
    only_errors(check("let a: i32[3] = [1, 2, 3];\nlet v: i32[..] = a;"), invalid_array);
}

#[test]
fn variadic_parameters() {
    assert_no_errors(check(
        "func format(text: string, ..args: string[..]) -> string { text }

format(\"a\");
format(\"a\", \"b\");
format(\"a\", \"b\", \"c\");",
    ));
    only_errors(check("func f(..args: string[..], last: i32) {}"), invalid_array);
    only_errors(check("func f(..args: string[]) {}"), invalid_array);
    only_errors(check("func f(text: string, ..args: string[..]) {}\n\nf(\"a\", 1);"), |error| {
        matches!(error, TypeError::Unexpected { .. })
    });
}
