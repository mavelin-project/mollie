//! `loop`, `break`, `continue` and labels.

use mollie_compiler::sandbox::{Limits, TrapKind};

use crate::{run_i32, run_i32_stressed, run_limited};

#[test]
fn loop_with_break_value() {
    assert_eq!(
        run_i32(
            "let mut i = 0;

loop {
    i += 1;

    if i * i > 50 { break i; }
}"
        ),
        8
    );
}

#[test]
fn continue_skips_iterations() {
    assert_eq!(
        run_i32(
            "let mut total = 0;

for value in [1, 2, 3, 4, 5, 6] {
    if value % 2 == 0 { continue; }

    total += value;
}

let mut i = 0;

while i < 10 {
    i += 1;

    if i < 8 { continue; }

    total += 100;
}

total"
        ),
        9 + 300
    );
}

#[test]
fn labeled_break_and_continue() {
    assert_eq!(
        run_i32(
            "let grid = [[1, 2, 3], [4, 5, 6], [7, 8, 9]];
let mut visited = 0;

'rows: for row in grid {
    for cell in row {
        if cell == 2 { continue 'rows; }
        if cell == 8 { break 'rows; }

        visited += cell;
    }
}

'search: for row in grid {
    for cell in row {
        if cell > 4 { break 'search; }
    }
}

let first = 'outer: loop {
    let mut i = 0;

    loop {
        i += 1;

        if i == 3 { break 'outer i * 10; }
    }
};

visited * 100 + first"
        ),
        // 1, then 4, 5, 6, then 7.
        (1 + 4 + 5 + 6 + 7) * 100 + 30
    );
}

#[test]
fn loop_values_survive_collections() {
    assert_eq!(
        run_i32_stressed(
            "struct Point { x: i32 }

let mut i = 0;
let point = loop {
    let candidate = Point { x: i };

    i += 1;

    if i > 20 { break candidate; }
};

point.x"
        ),
        20
    );
}

#[test]
fn endless_loop_runs_out_of_fuel() {
    let limits = Limits {
        fuel: Some(1000),
        ..Limits::default()
    };
    let trap =
        run_limited::<i32>("let mut i = 0;\nloop { i += 1; if i < 0 { continue; } }", |types| types.i32, limits, false).expect_err("the loop must be stopped");

    assert_eq!(trap.kind, TrapKind::OutOfFuel);
}
