//! Programs behave the same on every machine: NaN bits, recursion limits and
//! where a limited program stops don't depend on the platform.

use std::{cell::Cell, rc::Rc};

use mollie_compiler::sandbox::{Limits, TrapKind};

use crate::{run_f32, run_limited};

#[test]
fn nans_are_canonical() {
    // Computed at run time: `0.0 / 0.0` gives a negative NaN on x86-64 and a
    // positive one on AArch64 without canonicalization.
    let nan = run_f32("let zero = 0.0;\nlet one = 1.0;\n(zero * one) / (zero + zero)");

    assert!(nan.is_nan());
    assert_eq!(nan.to_bits(), 0x7FC0_0000);
}

const DOWN: &str = "func down(n: i32) -> i32 { if n == 0 { 0 } else { down(n - 1) + 1 } }

// The same depth with a much bigger frame.
func wide(n: i32) -> i32 {
    let a = [n, n, n, n, n, n, n, n];
    let b = [a, a, a, a];

    if n == 0 { 0 } else { wide(n - 1) + b[3][7] - n + 1 }
}
";

fn depth_limited(call: &str) -> Result<i32, TrapKind> {
    let limits = Limits {
        call_depth: Some(100),
        ..Limits::default()
    };

    run_limited::<i32>(&format!("{DOWN}{call}"), |types| types.i32, limits, false).map_err(|trap| trap.kind)
}

#[test]
fn recursion_is_limited_by_call_depth() {
    // The program itself is a call: 98 calls of `down` below it fit in 100.
    assert_eq!(depth_limited("down(98)"), Ok(98));
    assert_eq!(depth_limited("down(99)"), Err(TrapKind::StackOverflow));
    // Frame sizes don't matter.
    assert_eq!(depth_limited("wide(98)"), Ok(98));
    assert_eq!(depth_limited("wide(99)"), Err(TrapKind::StackOverflow));
}

#[test]
fn default_call_depth_stops_infinite_recursion() {
    let trap = run_limited::<i32>(
        "func forever(n: i32) -> i32 { forever(n + 1) + 1 }\nforever(0)",
        |types| types.i32,
        Limits::default(),
        false,
    )
    .expect_err("the recursion never ends");

    assert_eq!(trap.kind, TrapKind::StackOverflow);
}

#[test]
fn limited_programs_stop_at_the_same_point() {
    let source = "let mut i = 0;\nwhile true { i += 1; }\ni";
    let stops = (0..3)
        .map(|_| {
            let refuels = Rc::new(Cell::new(0));
            let counted = Rc::clone(&refuels);
            let limits = Limits {
                fuel: Some(1000),
                // Refuels twice, then stops the program.
                refuel: Some(Box::new(move || {
                    counted.set(counted.get() + 1);

                    if counted.get() <= 2 { 500 } else { 0 }
                })),
                ..Limits::default()
            };
            let trap = run_limited::<i32>(source, |types| types.i32, limits, false).expect_err("the loop never ends");

            (trap.kind, refuels.get())
        })
        .collect::<Vec<_>>();

    assert_eq!(stops, [(TrapKind::OutOfFuel, 3); 3]);
}
