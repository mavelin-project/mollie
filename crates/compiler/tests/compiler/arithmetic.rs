use std::sync::atomic::Ordering;

use crate::{TOUCHED, run_bool, run_f32, run_i32, run_with};

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
fn xor_and_shifts() {
    assert_eq!(run_i32("let mut x = 6 ^ 3;\nx <<= 4;\nx ^= 1;\nx >>= 2;\nx + (1 << 3) + (-16 >> 2)"), 20 + 8 - 4);
    // Shifts bind tighter than `&`, `^` and `|`, looser than `+`.
    assert_eq!(run_i32("1 + 2 << 3"), 24);
    assert_eq!(run_i32("1 | 2 ^ 3 & 1"), 3);
    assert_eq!(run_i32("12 & 1 << 2"), 4);
    // Unsigned values shift in zeros, signed ones their sign; amounts wrap
    // around the width.
    assert_eq!(run_i32("((0 - 1) as u32 >> 28) as i32"), 15);
    assert_eq!(run_i32("let x: i64 = 1;\n((x << 40) >> 38) as i32 + (1 << 33)"), 4 + 2);
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

#[test]
fn floats_to_small_integers_saturate() {
    // Backends only convert floats to integers of 32 and 64 bits.
    assert_eq!(
        run_i32(
            "let a = 200.7 as u8;
let b = 300.0 as u8;
let c = -5.0 as u8;
let d = 100.0 as i8;
let e = -1000.0 as i16;
let f = 70000.0 as u16;
let g = (a as f32 + (d as f32)) as i32;
a as i32 + b as i32 * 1000 + c as i32 + d as i32 * 10 + e as i32 + f as i32 + g"
        ),
        200 + 255 * 1000 + 0 + 100 * 10 - 1000 + 65535 + 300
    );
}

fn run_f64(source: &str) -> f64 {
    run_with(source, |types| types.f64, false)
}

#[test]
fn f64_literals_take_their_type() {
    // Not rounded to `f32` on the way.
    assert_eq!(run_f64("let x: f64 = 0.1;\nx * 3.0"), 0.1 * 3.0);
    assert_eq!(run_f64("0.1 as f64"), 0.1);
    assert_eq!(run_f64("0.25f64 + 1.0"), 1.25);
    assert_eq!(run_f64("func half(x: f64) -> f64 { x / 2.0 }\nhalf(0.3)"), 0.3 / 2.0);
    // Floats left undecided are `f32`, methods included.
    assert!((run_f32("let x = 1.75;\nx.floor() + x") - 2.75).abs() < f32::EPSILON);
}

#[test]
fn f64_conversions() {
    assert_eq!(run_f64("(0.1 as f32) as f64"), f64::from(0.1f32));
    assert!((run_f32("let x: f64 = 0.1;\nx as f32") - 0.1).abs() < f32::EPSILON);
    assert_eq!(run_f64("1099511627777i64 as f64"), 1_099_511_627_777.0);
    assert_eq!(run_i32("let x: f64 = -3.9;\nx as i32"), -3);
    assert_eq!(run_i32("let x: f64 = 1000000000.0;\n(x * x) as i32"), i32::MAX);
}

#[test]
fn f64_methods_and_math() {
    assert_eq!(run_f64("let x: f64 = 2.0;\nx.sqrt()"), 2f64.sqrt());
    assert_eq!(run_f64("let x: f64 = -2.5;\nx.abs().floor() + x.ceil() + x.round()"), 2.0 - 2.0 - 3.0);
    assert!((run_f64("PI_F64.sin()")).abs() < 1e-15);
    assert!((run_f64("let x: f64 = 2.0;\nx.pow(10.0)") - 1024.0).abs() < 1e-9);
    assert!((run_f64("let x: f64 = 1.0;\nx.exp().ln()") - 1.0).abs() < 1e-14);
    assert!(run_bool("let a: f64 = 0.1;\nlet b: f64 = 0.2;\na + b > 0.3 && a < b && a.max(b) == b"));
}

#[test]
fn f64_formatting() {
    assert!(run_bool("let x: f64 = 0.1;\n\"${x}\" == \"0.1\" && \"${x:.3}\" == \"0.100\" && (x as string) == \"0.1\""));
}
