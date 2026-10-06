//! `loop`, `break`, `continue` and labels.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

#[test]
fn loops_with_break_and_continue() {
    assert_no_errors(check(
        "let mut i = 0;

loop {
    i += 1;

    if i < 3 { continue; }
    if i > 10 { break; }
}

while i > 0 {
    i -= 1;

    if i == 5 { break; }
}

for value in [1, 2, 3] {
    if value == 2 { continue; }
}",
    ));
}

#[test]
fn loop_produces_the_value_of_break() {
    assert_no_errors(check(
        "let mut i = 0;
let found: i32 = loop {
    i += 1;

    if i * i > 50 { break i; }
};

func first_even(values: i32[]) -> i32 {
    for value in values {
        if value % 2 == 0 { return value; }
    }

    0
}",
    ));
}

#[test]
fn break_values_have_one_type() {
    only_errors(check("let x = loop { if true { break 1; } break true; };"), |error| {
        matches!(error, TypeError::Unexpected { .. })
    });
}

#[test]
fn labels_select_the_loop() {
    assert_no_errors(check(
        "let grid = [[1, 2], [3, 4]];
let mut found = 0;

'rows: for row in grid {
    for cell in row {
        if cell == 3 {
            found = cell;

            break 'rows;
        }

        if cell == 2 { continue 'rows; }
    }
}

let value: i32 = 'search: loop {
    loop {
        break 'search 42;
    }
};",
    ));
}

#[test]
fn break_outside_of_a_loop() {
    let error = single_error(check("break;"));

    assert!(matches!(error, TypeError::BreakOutsideLoop), "{error:?}");
}

#[test]
fn break_cannot_leave_a_closure() {
    let errors = check(
        "loop {
    let f = |x| { break; };
}",
    );

    assert!(errors.0.iter().any(|error| matches!(error, TypeError::BreakOutsideLoop)), "{:?}", errors.0);
}

#[test]
fn unknown_labels_are_reported() {
    let error = single_error(check("loop { break 'outer; }"));

    assert!(matches!(error, TypeError::UnknownLabel { ref name } if name == "outer"), "{error:?}");
}

#[test]
fn only_loop_has_a_value() {
    let error = single_error(check("while true { break 1; }"));

    assert!(matches!(error, TypeError::BreakValueOutsideLoop), "{error:?}");
}
