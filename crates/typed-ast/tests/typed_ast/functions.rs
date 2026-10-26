use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

#[test]
fn function_can_be_called_before_its_declaration() {
    let errors = check(
        "let result: i32 = twice(2);
func twice(x: i32) -> i32 { add(x, x) }
func add(a: i32, b: i32) -> i32 { a + b }",
    );

    assert_no_errors(errors);
}

#[test]
fn recursive_function() {
    let errors = check(
        "func factorial(n: i32) -> i32 {
    if n == 0 { 1 } else { n * factorial(n - 1) }
}
let result: i32 = factorial(5);",
    );

    assert_no_errors(errors);
}

#[test]
fn mutually_recursive_functions() {
    let errors = check(
        "func even(n: i32) -> bool {
    if n == 0 { true } else { odd(n - 1) }
}
func odd(n: i32) -> bool {
    if n == 0 { false } else { even(n - 1) }
}
let result: bool = even(4);",
    );

    assert_no_errors(errors);
}

#[test]
fn wrong_argument_count_is_reported() {
    let errors = check(
        "func take(a: i32) {}
take(1, 2);",
    );

    only_errors(errors, |error| {
        matches!(error, TypeError::ArgumentCountMismatch {
            expected: 1,
            found: 2,
            func: _
        })
    });
}

#[test]
fn wrong_argument_type_is_reported() {
    let errors = check(
        "func take(a: i32) {}
take(true);",
    );

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn wrong_return_type_is_reported() {
    let errors = check("func answer() -> i32 { true }");

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn function_bodies_do_not_see_top_level_variables() {
    let error = single_error(check(
        "let secret = 1;
func peek() -> i32 { secret }",
    ));

    assert!(matches!(error, TypeError::NotFound { ref name, .. } if name == "secret"));
}

#[test]
fn unknown_variable_is_reported() {
    let error = single_error(check("let a = missing;"));

    assert!(matches!(error, TypeError::NotFound { ref name, .. } if name == "missing"));
}
