use std::sync::atomic::Ordering;

use crate::{RECORDED, run_i32, run_i32_stressed};

#[test]
fn host_functions_are_called() {
    assert_eq!(run_i32("record(42);\n1"), 1);
    assert_eq!(RECORDED.load(Ordering::SeqCst), 42);
}

#[test]
fn objects_live_across_host_calls_survive_collections() {
    assert_eq!(
        run_i32_stressed(
            "struct Point { x: i32, y: i32 }

let mut total = 0;
let mut i = 0;
let kept = Point { x: 7, y: 0 };

while i < 50 {
    record(i);

    let temporary = Point { x: i, y: i };

    total += temporary.x + kept.x;
    i += 1;
}

total"
        ),
        1575
    );
}
