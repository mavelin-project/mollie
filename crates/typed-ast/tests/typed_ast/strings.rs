use mollie_shared::Operator;
use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

#[test]
fn strings_are_concatenated() {
    assert_no_errors(check(r#"let greeting: string = "hi, " + "ann";"#));
}

#[test]
fn compound_concatenation() {
    assert_no_errors(check(
        r#"let mut text = "a";
text += "b";"#,
    ));
}

#[test]
fn strings_are_not_concatenated_with_numbers() {
    only_errors(check(r#"let text = "a" + 1;"#), |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn strings_are_not_subtracted() {
    let error = single_error(check(r#"let text = "a" - "b";"#));

    assert!(matches!(error, TypeError::InvalidOperator { operator: Operator::Sub, .. }), "{error:?}");
}

#[test]
fn template_is_a_string() {
    assert_no_errors(check(
        r#"let name = "ann";
let count = 3;
let text: string = "${name} has ${count} items, ${count > 1}";"#,
    ));
}

#[test]
fn template_without_text_is_a_string() {
    assert_no_errors(check(r#"let text: string = "${1.5}";"#));
}

#[test]
fn structs_cannot_be_interpolated() {
    let error = single_error(check(
        r#"struct Point { x: i32 }
let point = Point { x: 1 };
let text = "${point}";"#,
    ));

    assert!(matches!(error, TypeError::NotFormattable { .. }), "{error:?}");
}

#[test]
fn syntax_errors_in_templates_are_reported() {
    let errors = check(r#"let text = "${1 +}";"#);

    assert!(!errors.0.is_empty());
}
