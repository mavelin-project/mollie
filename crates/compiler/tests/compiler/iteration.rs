use crate::run_i32;

#[test]
fn for_loop_over_an_array() {
    assert_eq!(
        run_i32(
            "let mut total = 0;

for x in [1, 2, 3, 4] {
    total += x;
}

total"
        ),
        10
    );
}

#[test]
fn for_loop_over_a_custom_iterator() {
    assert_eq!(
        run_i32(
            "struct Counter { current: i32, end: i32 }

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

let mut total = 0;
let range = Range { start: 0, end: 3 };

for x in range {
    total += x;
}

total"
        ),
        6
    );
}

#[test]
fn for_loops_over_values_typed_later() {
    // The types of results of closures and generic functions are only known
    // once the function is checked: the loop finds `iter` and `next` then.
    assert_eq!(
        run_i32(
            "func same<T>(value: T) -> T { value }
let make = || { [1, 2, 3] };
let mut total = 0;
for n in make() { total += n; }
for n in same([10, 20]) { total += n; }
total"
        ),
        36
    );
}

#[test]
fn for_loop_variables_can_have_any_name() {
    assert_eq!(run_i32("let mut total = 0;\nfor X in [1, 2, 3] { total += X; }\ntotal"), 6);
}

#[test]
fn for_loops_with_an_own_option() {
    // Loops use `std`'s `Option`, whatever the program calls `Option`.
    assert_eq!(
        run_i32(
            "enum Option { Some, Nothing }
let mut total = 0;
for n in [1, 2] { total += n; }
if Option::Some is Some { total } else { 0 }"
        ),
        3
    );
}
