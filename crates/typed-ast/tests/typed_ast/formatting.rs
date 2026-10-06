//! Format specifiers of interpolated values: `"${value:spec}"`.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

#[test]
fn specifiers_of_values() {
    assert_no_errors(check(
        "let n = 42;
let f = 1.5;
let text: string = \"${n:>4} ${n:04} ${n:x} ${f:.2} ${\"s\":<3} ${true:5}\";",
    ));
}

#[test]
fn precision_is_for_floats() {
    only_errors(check("let n = 4;\nlet text = \"${n:.2}\";"), |error| {
        matches!(error, TypeError::InvalidFormat { .. })
    });
}

#[test]
fn hex_is_for_integers() {
    only_errors(check("let text = \"${1.5:x}\";"), |error| matches!(error, TypeError::InvalidFormat { .. }));
}

#[test]
fn zeros_pad_numbers() {
    only_errors(check("let text = \"${\"s\":04}\";"), |error| matches!(error, TypeError::InvalidFormat { .. }));
}

#[test]
fn values_must_be_formattable() {
    only_errors(check("struct Point { x: i32 }\n\nlet text = \"${Point { x: 1 }:4}\";"), |error| {
        matches!(error, TypeError::InvalidFormat { .. })
    });
}
