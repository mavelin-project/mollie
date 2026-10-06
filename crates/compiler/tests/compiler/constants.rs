//! Constants of modules.

use crate::{run_bool, run_i32};

#[test]
fn numeric_constants() {
    assert_eq!(
        run_i32(
            "const BASE: i32 = 40;
const STEP = BASE / 20;

func next(x: i32) -> i32 { x + STEP }

next(BASE)"
        ),
        42
    );
}

#[test]
fn string_and_struct_constants() {
    assert!(run_bool(
        r#"struct Insets { top: f32, bottom: f32 = 2.0 }

const NAME = "panel";
const PADDING = Insets { top: 1.0 };

NAME == "panel" && PADDING.top + PADDING.bottom == 3.0"#
    ));
}

#[test]
fn array_constants() {
    assert_eq!(run_i32("const PRIMES = [2, 3, 5, 7];\n\nPRIMES[0] * PRIMES[3]"), 14);
}
