//! Ranges: `start..end` and `start..=end`.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

#[test]
fn ranges_are_iterable() {
    assert_no_errors(check(
        "let mut total = 0;

for i in 0..10 { total += i; }
for i in 1..=total { total -= 1; }

let n: u8 = 3;

for i in 0..n {
    let x: u8 = i;
}",
    ));
}

#[test]
fn ranges_are_values() {
    assert_no_errors(check(
        "let range: Range<i32> = 2..8;
let inside: bool = range.contains(4);
let empty: bool = (5..5).is_empty();
let first: i32 = range.start;",
    ));
}

#[test]
fn sides_of_a_range_have_one_type() {
    only_errors(check("let range = 0..true;"), |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn ranges_of_values_without_steps_are_not_iterable() {
    let (errors, _) = check("for value in \"a\"..\"z\" {}");

    assert!(!errors.is_empty());
}
