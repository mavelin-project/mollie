use mollie_typing::TypeError;

use crate::{assert_no_errors, check, single_error};

#[test]
fn let_variable_can_be_reassigned() {
    let errors = check(
        "let mut counter = 1;
counter = 2;
counter += 3;",
    );

    assert_no_errors(errors);
}

#[test]
fn const_variable_cannot_be_reassigned() {
    let error = single_error(check(
        "let counter = 1;
counter = 2;",
    ));

    assert!(matches!(error, TypeError::AssignToImmutable { ref name } if name == "counter"));
}

#[test]
fn compound_assignment_to_const_is_reported() {
    let error = single_error(check(
        "let counter = 1;
counter += 2;",
    ));

    assert!(matches!(error, TypeError::AssignToImmutable { .. }));
}

#[test]
fn fields_of_const_variable_can_be_assigned() {
    let errors = check(
        "struct Point { x: i32 }
let point = Point { x: 1 };
point.x = 2;",
    );

    assert_no_errors(errors);
}

#[test]
fn shadowing_const_with_let_allows_assignment() {
    let errors = check(
        "let value = 1;
let mut value = 2;
value = 3;",
    );

    assert_no_errors(errors);
}

#[test]
fn literal_cannot_be_assigned() {
    let error = single_error(check("1 = 2;"));

    assert!(matches!(error, TypeError::NotAssignable));
}

#[test]
fn declaration_inside_block_is_reported() {
    let error = single_error(check("func outer() { struct Inner {} }"));

    assert!(matches!(error, TypeError::LocalDeclaration));
}

#[test]
fn iterating_over_non_iterable_is_reported() {
    let error = single_error(check("for item in 5 {}"));

    assert!(matches!(error, TypeError::NotIterable { .. }));
}

#[test]
fn syntax_error_is_reported() {
    let error = single_error(check("const = ;"));

    assert!(matches!(error, TypeError::Parse { .. }));
}

#[test]
fn if_condition_must_be_bool() {
    let errors = check("if 1 {}");

    assert!(!errors.0.is_empty());
    assert!(errors.0.iter().all(|error| matches!(error, TypeError::Unexpected { .. })), "{errors:#?}");
}
