//! Named arguments and default values of parameters.

use crate::{run_i32, run_i32_stressed};

#[test]
fn defaults_and_named_arguments() {
    assert_eq!(
        run_i32(
            "func area(width: i32, height: i32 = width, scale: i32 = 1) -> i32 { width * height * scale }

area(3) + area(2, 5) * 10 + area(height: 1, width: 4, scale: 100)"
        ),
        9 + 100 + 400
    );
}

#[test]
fn arguments_are_evaluated_once_in_order() {
    assert_eq!(
        run_i32(
            "struct Log { value: i32 }

func record(log: Log, digit: i32) -> i32 {
    log.value = log.value * 10 + digit;

    digit
}

func pair(a: i32, b: i32, sum: i32 = a + b) -> i32 { sum }

let log = Log { value: 0 };
let sum = pair(b: record(log, 1), a: record(log, 2));

log.value * 100 + sum"
        ),
        12 * 100 + 3
    );
}

#[test]
fn defaults_of_methods() {
    assert_eq!(
        run_i32_stressed(
            "struct Pen { color: i32, marks: i32[] }

impl Pen {
    func new(color: i32 = 7) -> Pen { Pen { color, marks: [] } }

    func draw(self, color: i32 = self.color, times: i32 = 1) -> i32 {
        for i in 0..times { self.marks.push(color); }

        self.marks.len() as i32
    }
}

let pen = Pen::new();

pen.draw();
pen.draw(times: 2);
pen.draw(1, times: 3);

let mut total = 0;

for mark in pen.marks { total += mark; }

total * 10 + Pen::new(color: 2).color"
        ),
        (7 + 14 + 3) * 10 + 2
    );
}

#[test]
fn defaults_of_generic_functions() {
    assert_eq!(
        run_i32(
            "func repeat<T>(value: T, count: i32 = 2) -> T[] {
    let mut values: T[] = [];

    for i in 0..count { values.push(value); }

    values
}

repeat(5).len() as i32 * 10 + repeat(value: \"x\", count: 3).len() as i32"
        ),
        23
    );
}
