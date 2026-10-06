//! `return`, `?` and `panic`.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

#[test]
fn early_return() {
    assert_no_errors(check(
        "func sign(x: i32) -> i32 {
    if x > 0 {
        return 1;
    }

    if x < 0 { return 0 - 1; }

    0
}

func nothing(x: i32) {
    if x > 0 {
        return;
    }
}",
    ));
}

#[test]
fn returned_value_has_the_return_type() {
    only_errors(check("func f() -> i32 { return true; }"), |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn return_from_closure() {
    assert_no_errors(check(
        "func apply(f: func(i32) -> i32) -> i32 { f(1) }
let value = apply(|x| {
    if x > 0 { return x * 2; }

    x
});",
    ));
}

#[test]
fn return_outside_of_function() {
    let errors = check("struct S { x: i32 = return 1 }");

    assert!(errors.0.iter().any(|error| matches!(error, TypeError::ReturnOutsideFunction)), "{:?}", errors.0);
}

#[test]
fn question_mark_on_result_and_option() {
    assert_no_errors(check(
        "func parse(text: string) -> Result<i32, string> {
    if text == \"1\" { Ok { value: 1 } } else { Err { error: \"not a number\" } }
}

func double(text: string) -> Result<i32, string> {
    let value = parse(text)?;

    Ok { value: value * 2 }
}

func first(values: i32[]) -> Option<i32> {
    if values.len() == 0 { None } else { Some { value: values[0] } }
}

func sum_of_firsts(a: i32[], b: i32[]) -> Option<i32> {
    Some { value: first(a)? + first(b)? }
}",
    ));
}

#[test]
fn question_mark_binds_tighter_than_operators() {
    assert_no_errors(check(
        "struct Holder { value: i32 }

func get() -> Option<Holder> { None }

func value() -> Option<i32> {
    Some { value: -get()?.value + 1 }
}",
    ));
}

#[test]
fn question_mark_needs_a_matching_return_type() {
    let error = single_error(check(
        "func first(values: i32[]) -> Option<i32> { None }

func wrong(values: i32[]) -> Result<i32, string> {
    Ok { value: first(values)? }
}",
    ));

    assert!(matches!(error, TypeError::NotTryable { .. }), "{error:?}");
}

#[test]
fn question_mark_on_other_values() {
    let error = single_error(check("func f() -> Option<i32> { Some { value: 1? } }"));

    assert!(matches!(error, TypeError::NotTryable { .. }), "{error:?}");
}

#[test]
fn panic_fits_anywhere() {
    assert_no_errors(check(
        "func get(value: Option<i32>) -> i32 {
    match value {
        Some { value } => value,
        None => panic(\"no value\"),
    }
}

func fail() {
    panic(\"failed\");
}

let a: i32 = Option::Some { value: 1 }.unwrap();
let b: i32 = Option::Some { value: 1 }.expect(\"a value\");",
    ));
}

#[test]
fn panic_takes_a_message() {
    only_errors(check("panic(1);"), |error| matches!(error, TypeError::Unexpected { .. }));
    only_errors(check("panic();"), |error| matches!(error, TypeError::ArgumentCountMismatch { .. }));
}
