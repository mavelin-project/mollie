//! `for` loops are lowered through the `IntoIterator` and `Iterator` lang
//! items, declared by `std`.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

/// An iterator and an iterable implementing traits of `std`.
const COUNTER: &str = "struct Counter { current: i32, end: i32 }

impl Iterator<i32> for Counter {
    func next(self) -> Option<i32> {
        if self.current == self.end {
            Option::None
        } else {
            self.current += 1;

            Option::Some { value: self.current }
        }
    }
}

struct Range { start: i32, end: i32 }

impl Iterable<Counter> for Range {
    func iter(self) -> Counter {
        Counter { current: self.start, end: self.end }
    }
}
";

#[test]
fn for_loop_over_iterable() {
    assert_no_errors(check(&format!(
        "{COUNTER}
let mut total = 0;
let range = Range {{ start: 0, end: 3 }};

for item in range {{
    total += item;
}}"
    )));
}

#[test]
fn loop_variable_has_the_item_type() {
    assert_no_errors(check(&format!(
        "{COUNTER}
let range = Range {{ start: 0, end: 3 }};

for item in range {{
    let copy: i32 = item;
}}"
    )));
}

#[test]
fn loop_variable_of_wrong_type_is_reported() {
    only_errors(
        check(&format!(
            "{COUNTER}
let range = Range {{ start: 0, end: 3 }};

for item in range {{
    let copy: bool = item;
}}"
        )),
        |err| matches!(err, TypeError::Unexpected { .. }),
    );
}
