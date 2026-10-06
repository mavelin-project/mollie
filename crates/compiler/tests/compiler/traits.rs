use crate::run_i32;

const SHAPES: &str = "trait Shape { func area(self) -> i32; }

struct Square { side: i32 }
struct Rect { w: i32, h: i32 }

impl Shape for Square {
    func area(self) -> i32 { self.side * self.side }
}

impl Shape for Rect {
    func area(self) -> i32 { self.w * self.h }
}

struct Holder { shape: Shape }
";

#[test]
fn trait_method_on_a_concrete_type() {
    assert_eq!(run_i32(&format!("{SHAPES}let square = Square {{ side: 4 }};\nsquare.area()")), 16);
}

#[test]
fn dynamic_dispatch_through_trait_objects() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}let a = Holder {{ shape: Square {{ side: 3 }} }};
let b = Holder {{ shape: Rect {{ w: 2, h: 5 }} }};
a.shape.area() + b.shape.area()"
        )),
        19
    );
}

#[test]
fn arrays_of_trait_objects() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}struct Scene {{ shapes: Shape[] }}

let scene = Scene {{ shapes: [Square {{ side: 1 }}, Square {{ side: 2 }}] }};
scene.shapes[0].area() + scene.shapes[1].area()"
        )),
        5
    );
}

#[test]
fn type_pattern_on_trait_objects() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}func describe(holder: Holder) -> i32 {{
    if holder.shape is Rect rect {{ rect.w * 10 + rect.h }} else {{ 0 }}
}}

describe(Holder {{ shape: Rect {{ w: 2, h: 3 }} }}) + describe(Holder {{ shape: Square {{ side: 9 }} }}) * 100"
        )),
        23
    );
}
