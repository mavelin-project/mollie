//! Default implementations of trait functions and `super.name()`.

use crate::{run_i32, run_i32_stressed};

const SHAPE: &str = "trait Shape {
    func area(self) -> i32;

    func doubled(self) -> i32 { self.area() * 2 }

    func score(self) -> i32 { self.doubled() + 1 }
}

struct Square { side: i32 }

impl Shape for Square {
    func area(self) -> i32 { self.side * self.side }
}

struct Rect { w: i32, h: i32 }

impl Shape for Rect {
    func area(self) -> i32 { self.w * self.h }

    func score(self) -> i32 { super.score() * 100 }
}
";

#[test]
fn defaults_call_functions_of_the_impl() {
    assert_eq!(
        run_i32(&format!("{SHAPE}Square {{ side: 3 }}.doubled() + Square {{ side: 1 }}.score()")),
        18 + 3
    );
}

#[test]
fn overrides_call_defaults_with_super() {
    assert_eq!(run_i32(&format!("{SHAPE}Rect {{ w: 2, h: 3 }}.score()")), 1300);
}

#[test]
fn defaults_through_trait_objects() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPE}struct Holder {{ shape: Shape }}

let a = Holder {{ shape: Square {{ side: 2 }} }};
let b = Holder {{ shape: Rect {{ w: 1, h: 1 }} }};

a.shape.score() + b.shape.score()"
        )),
        9 + 300
    );
}

#[test]
fn defaults_of_generic_traits_and_impls() {
    assert_eq!(
        run_i32_stressed(
            "trait Source<T> {
    func get(self) -> T;

    func get_all(self, count: i32) -> T[] {
        let mut values: T[] = [];

        for i in 0..count { values.push(self.get()); }

        values
    }
}

struct Constant<T> { value: T }

impl<T> Source<T> for Constant<T> {
    func get(self) -> T { self.value }
}

let values = Constant { value: 7 }.get_all(4);
let mut total = 0;

for value in values { total += value; }

total"
        ),
        28
    );
}
