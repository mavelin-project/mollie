use std::collections::HashMap;

use mollie_typed_ast::TypedASTContext;
use mollie_typing::{TyCtxt, TypeError, TypeRef};

use crate::{MemoryLoader, assert_no_errors, check, only_errors, single_error};

fn check_with_stub(stub: &str, source: &str) -> (Vec<TypeError>, TyCtxt) {
    let mut context = TypedASTContext::default();
    let void = context.tcx.types.core_types.void;

    context.load_host_stub(stub, &[]);
    context.process(MemoryLoader(HashMap::new()), source, Vec::<(String, TypeRef)>::new(), void);

    (
        context.diagnostics.errors.into_values().map(|diagnostic| *diagnostic.error).collect(),
        context.tcx,
    )
}

fn used_before_assigned(error: &TypeError) -> bool {
    matches!(error, TypeError::UsedBeforeAssigned { name } if name == "x")
}

#[test]
fn variables_are_assigned_later() {
    assert_no_errors(check(
        "let x: i32;
x = 1;
let y = x + 1;",
    ));
}

#[test]
fn variables_are_assigned_in_every_branch() {
    assert_no_errors(check(
        "func pick(first: bool) -> i32 {
    let x: i32;

    if first { x = 1; } else { x = 2; }

    x
}",
    ));
}

#[test]
fn branches_that_return_need_no_assignment() {
    assert_no_errors(check(
        "func pick(first: bool) -> i32 {
    let x: i32;

    if first { x = 1; } else { return 0; }

    x
}",
    ));
}

#[test]
fn variables_used_before_assignment_are_reported() {
    assert!(used_before_assigned(&single_error(check(
        "let x: i32;
let y = x + 1;
x = 1;",
    ))));
}

#[test]
fn variables_assigned_in_one_branch_are_reported() {
    assert!(used_before_assigned(&single_error(check(
        "func pick(first: bool) -> i32 {
    let x: i32;

    if first { x = 1; }

    x
}",
    ))));
}

#[test]
fn compound_assignments_read_the_variable() {
    assert!(used_before_assigned(&single_error(check(
        "let mut x: i32;
x += 1;",
    ))));
}

#[test]
fn assignments_in_while_loops_may_not_happen() {
    assert!(used_before_assigned(&single_error(check(
        "let mut x: i32;
let mut i = 0;

while i < 3 {
    x = i;
    i += 1;
}

let y = x;",
    ))));
}

#[test]
fn loops_give_assignments_before_break() {
    assert_no_errors(check(
        "let mut x: i32;

loop {
    x = 1;
    break;
}

let y = x;",
    ));
}

#[test]
fn closures_see_variables_assigned_before_them() {
    assert!(used_before_assigned(&single_error(check(
        "let x: i32;
let f = || { x + 1 };
x = 1;",
    ))));
    assert_no_errors(check(
        "let x: i32;
x = 1;
let f = || { x + 1 };",
    ));
}

#[test]
fn immutable_variables_are_assigned_once() {
    let error = single_error(check(
        "let x: i32;
x = 1;
x = 2;",
    ));

    assert!(matches!(error, TypeError::AssignToImmutable { ref name } if name == "x"));

    let error = single_error(check(
        "let x: i32;
let mut i = 0;

while i < 3 {
    x = i;
    i += 1;
}",
    ));

    assert!(matches!(error, TypeError::AssignToImmutable { .. }));
}

#[test]
fn variables_need_a_type_or_a_value() {
    let error = single_error(check("let x;"));

    assert!(matches!(error, TypeError::MissingValue { ref name } if name == "x"));
}

#[test]
fn programs_cant_declare_variables_of_the_host() {
    only_errors(check("extern let x: i32;"), |error| matches!(error, TypeError::ExternVariableOutsideStub));
}

#[test]
fn stubs_declare_variables_of_the_host() {
    assert_no_errors(check_with_stub(
        "extern struct Data { count: i32 }
extern let data: Data;
extern let mut total: i32;",
        "total = total + data.count;",
    ));
}

#[test]
fn variables_of_the_host_are_immutable_without_mut() {
    let error = single_error(check_with_stub("extern let limit: i32;", "limit = 2;"));

    assert!(matches!(error, TypeError::AssignToImmutable { ref name } if name == "limit"));
}

#[test]
fn variables_of_the_host_need_a_type() {
    let error = single_error(check_with_stub("extern let data;", ""));

    assert!(matches!(error, TypeError::TypeExpected { ref name } if name == "data"));
}

#[test]
fn variables_of_the_host_have_no_value() {
    let error = single_error(check_with_stub("extern let data: i32 = 1;", ""));

    assert!(matches!(error, TypeError::ExternVariableValue));
}
