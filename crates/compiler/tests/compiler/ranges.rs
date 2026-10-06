//! Ranges: `start..end` and `start..=end`.

use crate::{run_i32, run_i32_stressed};

#[test]
fn exclusive_and_inclusive_ranges() {
    assert_eq!(
        run_i32(
            "let mut total = 0;

for i in 0..5 { total += i; }
for i in 10..=12 { total += i; }

total"
        ),
        10 + 33
    );
}

#[test]
fn empty_and_reversed_ranges() {
    assert_eq!(
        run_i32(
            "let mut count = 0;

for i in 3..3 { count += 1; }
for i in 5..2 { count += 1; }
for i in 4..=4 { count += 10; }

count"
        ),
        10
    );
}

#[test]
fn ranges_up_to_the_maximum_value() {
    assert_eq!(
        run_i32(
            "let mut count = 0;

for i in 250u8..=255u8 { count += 1; }

count"
        ),
        6
    );
}

#[test]
fn contains_and_is_empty() {
    assert_eq!(
        run_i32(
            "let range = 2..8;
let mut result = 0;

if range.contains(2) { result += 1; }
if !range.contains(8) { result += 10; }
if (1..=1).contains(1) { result += 100; }
if (3..3).is_empty() { result += 1000; }
if !(3..=3).is_empty() { result += 10000; }

result"
        ),
        11111
    );
}

#[test]
fn ranges_with_labels_and_breaks() {
    assert_eq!(
        run_i32_stressed(
            "let mut found = 0;

'search: for i in 1..20 {
    for j in 1..20 {
        if i * j == 42 && i > 5 {
            found = i * 100 + j;

            break 'search;
        }
    }
}

found"
        ),
        607
    );
}
