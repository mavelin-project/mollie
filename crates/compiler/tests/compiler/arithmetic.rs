use std::sync::atomic::Ordering;

use crate::{TOUCHED, run_bool, run_f32, run_i32};

#[test]
fn integer_arithmetic_follows_precedence() {
    assert_eq!(run_i32("1 + 2 * 3 - 4 / 2"), 5);
}

#[test]
fn signed_division_truncates_towards_zero() {
    assert_eq!(run_i32("let a = -7;\na / 2"), -3);
}

#[test]
fn unsigned_operations_are_unsigned() {
    // As a signed 32-bit integer, 4000000000 would be negative.
    assert!(run_bool(
        "let a: u32 = 4000000000;
let b: u32 = 2;
a / b == 2000000000 && a > b"
    ));
}

#[test]
fn float_arithmetic() {
    assert!((run_f32("1.5 * 2.0 + 0.5") - 3.5).abs() < f32::EPSILON);
}

#[test]
fn comparisons_and_logic() {
    assert!(!run_bool("!true"));
    assert!(!run_bool("!(1 == 1)"));
    assert!(run_bool("!!true"));
    assert!(run_bool("if !true { false } else { true }"));
    assert!(run_bool("1 < 2 && 2 > 3 || 4 == 4"));
    assert!(!run_bool("1 >= 2 || 3 != 3"));
}

#[test]
fn logical_operators_short_circuit() {
    let before = TOUCHED.load(Ordering::SeqCst);

    assert!(run_bool(
        "let a = false && touch();
let b = true || touch();
let c = true && touch();
c && b && a == false"
    ));
    assert_eq!(TOUCHED.load(Ordering::SeqCst) - before, 1);
}

#[test]
fn bitwise_operators() {
    assert_eq!(run_i32("let flags = 12;\n(flags & 4) | 1"), 5);
}

#[test]
fn casts_between_numbers() {
    assert!((run_f32("let x = 7;\n(x as f32) / 2.0") - 3.5).abs() < f32::EPSILON);
    assert_eq!(run_i32("3.9 as i32"), 3);
    // Truncating to `u8` keeps the low bits; widening an unsigned value
    // zero-extends it.
    assert_eq!(run_i32("(-1 as u8) as i32"), 255);
    // Widening a signed value sign-extends it.
    assert_eq!(run_i32("(-1 as i8) as i32"), -1);
}

#[test]
fn remainder() {
    assert_eq!(run_i32("let mut x = 17;\nlet a = x % 5;\nx %= 4;\na * 10 + x + (0 - 7) % 3"), 20 + 1 - 1);
}
