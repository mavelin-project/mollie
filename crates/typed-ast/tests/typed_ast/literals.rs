use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

#[test]
fn integer_literal_takes_annotated_type() {
    let errors = check(
        "let small: u8 = 5;
let big: i64 = 5;
let index: usize = 5;",
    );

    assert_no_errors(errors);
}

#[test]
fn integer_literal_is_not_a_float() {
    let errors = check("let value: f32 = 5;");

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn primitive_postfixes_give_literal_their_type() {
    let errors = check(
        "let small: u8 = 5u8;
let whole: f32 = 1f32;
let fraction: f32 = 2.5f32;",
    );

    assert_no_errors(errors);
}

#[test]
fn postfix_type_must_match_annotation() {
    let errors = check("let small: u16 = 5u8;");

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn float_with_integer_postfix_is_reported() {
    let error = single_error(check("let value = 1.5u8;"));

    assert!(matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn postfix_function_is_called_for_integers_and_floats() {
    let errors = check(
        "struct Px { value: f32 }
postfix func px(value: f32) -> Px { Px { value } }
let whole: Px = 10px;
let fraction: Px = 2.5px;",
    );

    assert_no_errors(errors);
}

#[test]
fn unknown_postfix_is_reported() {
    let error = single_error(check("let value = 10zz;"));

    assert!(matches!(error, TypeError::NoFunction { ref name, postfix: true } if name == "zz"));
}

#[test]
fn regular_function_is_not_a_postfix() {
    let error = single_error(check(
        "func zz(value: f32) -> f32 { value }
let value = 1zz;",
    ));

    assert!(matches!(error, TypeError::NotPostfix { ref name } if name == "zz"));
}

#[test]
fn string_and_bool_literals() {
    let errors = check(
        "let name: string = \"mollie\";
let flag: bool = true;",
    );

    assert_no_errors(errors);
}
