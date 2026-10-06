//! Inherent methods implemented by the host.

use crate::{run_i32, run_i32_stressed};

#[test]
fn host_method_is_called() {
    assert_eq!(run_i32("let coin = Coin { cents: 21 };\ncoin.doubled()"), 42);
}

#[test]
fn host_method_results_are_values() {
    assert_eq!(
        run_i32("let a = Coin { cents: 5 };\nconst b = Coin { cents: 1 };\na.doubled() + b.doubled()"),
        12
    );
}

#[test]
fn objects_allocated_by_host_survive_collections() {
    // Each `split` allocates a coin on the host, which is then used by compiled
    // code while other allocations collect garbage.
    assert_eq!(
        run_i32_stressed(
            "let mut total = 0;
let mut i = 0;
let coin = Coin { cents: 64 };

while i < 20 {
    let half = coin.split();
    let quarter = half.split();

    total += half.cents + quarter.doubled();
    i += 1;
}

total"
        ),
        (32 + 32) * 20
    );
}

#[test]
fn strings_cross_the_host_boundary() {
    // Strings created by compiled code are passed to the host, and strings
    // created by the host are used by compiled code while it collects
    // garbage.
    assert_eq!(
        run_i32_stressed(
            r#"let coin = Coin { cents: 5 };
let mut matches = 0;
let mut i = 0;

while i < 10 {
    let text = coin.describe("coin ${i}: ");

    if text == "coin ${i}: 5 cents" {
        matches += 1;
    }

    i += 1;
}

matches"#
        ),
        10
    );
}
