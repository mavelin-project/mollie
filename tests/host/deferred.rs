use crate::{run_i32, run_i32_stressed};

#[test]
fn variables_are_assigned_later() {
    assert_eq!(
        run_i32(
            "func pick(first: bool) -> i32 {
    let x: i32;

    if first { x = 40; } else { x = 1; }

    x
}

let total: i32;
total = pick(true) + pick(false) + 1;
total"
        ),
        42
    );
}

#[test]
fn objects_assigned_later_survive_collections() {
    // Collections happen while the variable holds no object yet.
    assert_eq!(
        run_i32_stressed(
            "let coin: Coin;
let other = Coin { cents: 1 };

coin = Coin { cents: 20 };
coin.doubled() + other.doubled()"
        ),
        42
    );
}
