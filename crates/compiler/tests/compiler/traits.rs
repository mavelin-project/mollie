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
fn assigning_implementors_as_trait_objects() {
    // Assignment converts to the place's type: the vtables are compiled even
    // when nothing else converts these types.
    assert_eq!(
        run_i32(
            "trait Shape { func area(self) -> i32; }
struct Square { side: i32 }
impl Shape for Square { func area(self) -> i32 { self.side * self.side } }
struct Circle { radius: i32 }
impl Shape for Circle { func area(self) -> i32 { self.radius * 3 } }
struct Scene { shapes: Shape[], main: Shape }

let scene = Scene { shapes: [], main: Square { side: 1 } };

scene.shapes = [Circle { radius: 2 }];
scene.main = Circle { radius: 5 };

let mut shape: Shape = Square { side: 2 };

shape = Circle { radius: 1 };

scene.shapes[0].area() + scene.main.area() * 10 + shape.area() * 100"
        ),
        6 + 15 * 10 + 3 * 100
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

#[test]
fn returning_implementors_as_trait_objects() {
    // `return` converts to the function's result type: the vtables are
    // compiled even when nothing else converts these types.
    assert_eq!(
        run_i32(
            "trait Shape { func area(self) -> i32; }
struct Square { side: i32 }
impl Shape for Square { func area(self) -> i32 { self.side * self.side } }
struct Circle { radius: i32 }
impl Shape for Circle { func area(self) -> i32 { self.radius * 3 } }

func pick(round: bool) -> Shape {
    if round {
        return Circle { radius: 2 };
    }

    Square { side: 3 }
}

let make: func(bool) -> Shape = |big| {
    if big {
        return Square { side: 10 };
    }

    Circle { radius: 1 }
};

pick(true).area() + pick(false).area() * 10 + make(true).area() * 100 + make(false).area() * 10000"
        ),
        6 + 9 * 10 + 100 * 100 + 3 * 10000
    );
}
