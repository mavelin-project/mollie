use mollie_shared::Operator;
use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

#[test]
fn arithmetic_and_comparison_on_numbers() {
    let errors = check(
        "let sum: i32 = 1 + 2 * 3;
let ratio: f32 = 1.0 / 2.0;
let less: bool = 1 < 2;
let equal: bool = 1.0 == 2.0;",
    );

    assert_no_errors(errors);
}

#[test]
fn logical_operators_on_bools() {
    let errors = check("let both: bool = true && false || true;");

    assert_no_errors(errors);
}

#[test]
fn strings_can_be_compared() {
    let errors = check("let same: bool = \"a\" == \"b\";");

    assert_no_errors(errors);
}

#[test]
fn arithmetic_on_bools_is_reported() {
    let error = single_error(check("let sum = true + false;"));

    assert!(matches!(error, TypeError::InvalidOperator { operator: Operator::Add, .. }));
}

#[test]
fn logical_operators_on_numbers_are_reported() {
    let error = single_error(check("let both = 1 && 2;"));

    assert!(matches!(error, TypeError::InvalidOperator { operator: Operator::And, .. }));
}

#[test]
fn structs_have_no_operators() {
    let error = single_error(check(
        "struct Point {}
let sum = Point {} + Point {};",
    ));

    assert!(matches!(error, TypeError::InvalidOperator { operator: Operator::Add, .. }));
}

#[test]
fn compound_assignment_follows_its_operator() {
    let error = single_error(check(
        "let mut flag = true;
flag += true;",
    ));

    assert!(matches!(error, TypeError::InvalidOperator {
        operator: Operator::AddAssign,
        ..
    }));
}

#[test]
fn operands_of_different_types_are_reported() {
    let errors = check("let sum = 1 + 1.0;");

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn remainder_of_floats_is_reported() {
    let error = single_error(check("let x = 1.5 % 2.0;"));

    assert!(matches!(error, TypeError::InvalidOperator { operator: Operator::Rem, .. }), "{error:?}");
}
