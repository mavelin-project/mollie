//! Bounds of generic parameters: functions of bounds are called on the impl
//! of the type a generic is instantiated with.

use crate::{run_i32, run_i32_stressed};

const SHAPES: &str = "trait Shape { func area(self) -> i32; }

struct Square { side: i32 }
struct Rect { w: i32, h: i32 }

impl Shape for Square {
    func area(self) -> i32 { self.side * self.side }
}

impl Shape for Rect {
    func area(self) -> i32 { self.w * self.h }
}
";

#[test]
fn generic_functions_call_functions_of_bounds() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}func double_area<T: Shape>(shape: T) -> i32 {{ shape.area() * 2 }}

double_area(Square {{ side: 3 }}) + double_area(Rect {{ w: 2, h: 5 }})"
        )),
        18 + 20
    );
}

#[test]
fn impls_with_bounds() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}struct Twice<T> {{ inner: T }}

impl<T: Shape> Shape for Twice<T> {{
    func area(self) -> i32 {{ self.inner.area() * 2 }}
}}

func total<T: Shape>(shapes: T[]) -> i32 {{
    let mut sum = 0;

    for shape in shapes {{ sum += shape.area(); }}

    sum
}}

let twice = Twice {{ inner: Twice {{ inner: Square {{ side: 2 }} }} }};

twice.area() + total([Rect {{ w: 1, h: 2 }}, Rect {{ w: 3, h: 4 }}])"
        )),
        16 + 14
    );
}

#[test]
fn generic_methods_with_bounds() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}struct Scale {{ factor: i32 }}

impl Scale {{
    func apply<T: Shape>(self, shape: T) -> i32 {{ shape.area() * self.factor }}
}}

Scale {{ factor: 3 }}.apply(Square {{ side: 2 }})"
        )),
        12
    );
}

#[test]
fn bound_calls_under_gc_pressure() {
    assert_eq!(
        run_i32_stressed(&format!(
            "{SHAPES}func largest<T: Shape>(shapes: T[]) -> i32 {{
    let mut best = 0;

    for shape in shapes {{
        let area = shape.area();

        if area > best {{ best = area; }}
    }}

    best
}}

let squares = [Square {{ side: 1 }}, Square {{ side: 5 }}, Square {{ side: 3 }}];

largest(squares)"
        )),
        25
    );
}

#[test]
fn bounds_select_impls_by_arguments() {
    assert_eq!(
        run_i32(
            "trait Source<T> { func get(self) -> T; }

struct Both { number: i32, text: string }

impl Source<i32> for Both {
    func get(self) -> i32 { self.number }
}

impl Source<string> for Both {
    func get(self) -> string { self.text }
}

struct Holder { numbers: Source<i32>, texts: Source<string> }

let both = Both { number: 4, text: \"abc\" };
let holder = Holder { numbers: both, texts: both };

holder.numbers.get() * 10 + holder.texts.get().len() as i32"
        ),
        43
    );
}

#[test]
fn bound_calls_on_trait_objects() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}func double_area<T: Shape>(shape: T) -> i32 {{ shape.area() * 2 }}

struct Holder {{ shape: Shape }}

let holder = Holder {{ shape: Rect {{ w: 2, h: 3 }} }};

double_area(holder.shape)"
        )),
        12
    );
}
