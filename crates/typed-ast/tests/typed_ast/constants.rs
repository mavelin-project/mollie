//! Variables (`let`, `let mut`) and constants of modules (`const`).

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, check_with_modules, only_errors, single_error};

#[test]
fn immutable_variables_cannot_be_assigned() {
    let error = single_error(check("let x = 1;\nx = 2;"));

    assert!(matches!(error, TypeError::AssignToImmutable { ref name } if name == "x"), "{error:?}");
}

#[test]
fn mutable_variables_can_be_assigned() {
    assert_no_errors(check("let mut x = 1;\nx = 2;\nx += 3;"));
}

#[test]
fn constants_of_the_root_module() {
    assert_no_errors(check(
        "const LIMIT: i32 = 10;
const DOUBLE = LIMIT * 2;
const NAME = \"panel\";

let total: i32 = LIMIT + DOUBLE;
let name: string = NAME;",
    ));
}

#[test]
fn constants_can_be_used_before_their_declaration() {
    assert_no_errors(check(
        "const A: i32 = B + 1;
const B: i32 = 2;

func get() -> i32 { A }",
    ));
}

#[test]
fn constants_of_other_modules_are_imported() {
    assert_no_errors(check_with_modules(
        "module limits;
import { MAX } from limits;

let max: i32 = MAX + limits::MIN;",
        &[("limits", "const MAX: i32 = 100;\nconst MIN: i32 = 0;")],
    ));
}

#[test]
fn constant_cycles_are_reported() {
    let errors = check("const A: i32 = B;\nconst B: i32 = A;");

    assert!(errors.0.iter().any(|error| matches!(error, TypeError::ConstCycle { .. })), "{:?}", errors.0);
}

#[test]
fn constants_must_be_evaluable() {
    let errors = check(
        "func get() -> i32 { 1 }

const VALUE: i32 = get();",
    );

    assert!(errors.0.iter().any(|error| matches!(error, TypeError::NonConstantEvaluable)), "{:?}", errors.0);
}

#[test]
fn constants_have_their_annotated_type() {
    only_errors(check("const VALUE: i32 = true;"), |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn constants_cannot_be_declared_in_blocks() {
    let error = single_error(check("func f() { const X: i32 = 1; }"));

    assert!(matches!(error, TypeError::LocalDeclaration), "{error:?}");
}

#[test]
fn constants_are_not_types() {
    only_errors(check("const X: i32 = 1;\nlet y: X = 1;"), |error| matches!(error, TypeError::Unexpected { .. }));
}
