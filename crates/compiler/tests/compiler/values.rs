//! Value types: `value struct` and `value enum`, copied instead of referenced.

use crate::{compile_error_text, run_bool, run_i32, run_i32_stressed};

const POINT: &str = "value struct Point { x: i32, y: i32 }

impl Point {
    func sum(self) -> i32 { self.x + self.y }

    func move_by(mut self, dx: i32) -> i32 {
        self.x += dx;

        self.x
    }
}
";

#[test]
fn values_are_copied() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}let a = Point {{ x: 1, y: 2 }};
let mut b = a;

b.x = 10;

a.x * 100 + b.x"
        )),
        110
    );
}

#[test]
fn mut_self_writes_back() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}struct Holder {{ point: Point }}

let mut p = Point {{ x: 1, y: 2 }};
let moved = p.move_by(4);

let holder = Holder {{ point: Point {{ x: 0, y: 0 }} }};
holder.point.move_by(3);

let points = [Point {{ x: 1, y: 1 }}, Point {{ x: 2, y: 2 }}];
points[1].move_by(5);

// A temporary just loses the change.
Point {{ x: 0, y: 0 }}.move_by(9);

p.x * 1000 + moved * 100 + holder.point.x * 10 + points[1].x"
        )),
        5000 + 500 + 30 + 7
    );
}

#[test]
fn nested_and_generic_values() {
    assert_eq!(
        run_i32(
            "value struct Pair<T> { a: T, b: T }
value struct Small { flag: bool, count: u8 }
value struct Line { start: Pair<i32>, small: Small, end: Pair<i32> }

let mut line = Line {
    start: Pair { a: 1, b: 2 },
    small: Small { flag: true, count: 7 },
    end: Pair { a: 3, b: 4 },
};

line.end.b = 40;
line.small.count = 9;

let copy = line;

line.start.a = 100;

copy.start.a + copy.start.b * 10 + copy.end.a * 100 + copy.end.b * 1000 + copy.small.count as i32 * 100000"
        ),
        1 + 20 + 300 + 40000 + 900_000
    );
}

#[test]
fn floats_in_values() {
    assert_eq!(
        run_i32(
            "value struct Size { width: f32, height: f32 }

func area(size: Size) -> f32 { size.width * size.height }

let mut size = Size { width: 1.5, height: 4.0 };

size.height = 6.0;

area(size) as i32"
        ),
        9
    );
}

#[test]
fn arrays_of_values() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}let mut points: Point[] = [];

for i in 0..10 {{ points.push(Point {{ x: i, y: i * 2 }}); }}

let mut total = 0;

for point in points {{ total += point.sum(); }}

total"
        )),
        135
    );
}

#[test]
fn references_in_values_survive_collections() {
    assert_eq!(
        run_i32_stressed(
            "value struct Named { name: string, tags: string[], id: i32 }

let mut all: Named[] = [];

for i in 0..20 {
    let named = Named { name: \"item ${i}\", tags: [\"a\", \"b${i}\"], id: i };

    all.push(named);
}

let mut total = 0;

for named in all { total += named.name.len() as i32 + named.tags[1].len() as i32 + named.id; }

total"
        ),
        // `item i` is 6 or 7 bytes, `bi` is 2 or 3 bytes.
        (10 * 6 + 10 * 7) + (10 * 2 + 10 * 3) + 190
    );
}

#[test]
fn value_enums() {
    assert_eq!(
        run_i32(
            "value enum Shape {
    Circle { radius: i32 },
    Rect { w: i32, h: i32 },
    Empty,
}

func area(shape: Shape) -> i32 {
    match shape {
        Circle { radius } => radius * radius * 3,
        Rect { w, h } => w * h,
        Empty => 0,
    }
}

let shapes = [Shape::Circle { radius: 2 }, Shape::Rect { w: 3, h: 4 }, Shape::Empty];
let mut total = 0;

for shape in shapes { total += area(shape); }

total"
        ),
        12 + 12
    );
}

#[test]
fn values_as_trait_objects() {
    assert_eq!(
        run_i32_stressed(
            "trait Shape { func area(self) -> i32; }

value struct Square { side: i32 }

impl Shape for Square {
    func area(self) -> i32 { self.side * self.side }
}

struct Scene { shapes: Shape[] }

let scene = Scene { shapes: [] };

scene.shapes.push(Square { side: 2 });
scene.shapes.push(Square { side: 3 });

let mut total = 0;

for shape in scene.shapes {
    total += shape.area();

    if shape is Square square { total += square.side * 100; }
}

total"
        ),
        13 + 500
    );
}

#[test]
fn values_are_compared_by_fields() {
    assert!(run_bool(&format!(
        "{POINT}let a = Point {{ x: 1, y: 2 }};
let b = Point {{ x: 1, y: 2 }};
let c = Point {{ x: 2, y: 2 }};

a == b && a != c"
    )));
}

#[test]
fn value_constants() {
    assert_eq!(run_i32(&format!("{POINT}const ORIGIN: Point = Point {{ x: 3, y: 4 }};\n\nORIGIN.sum()")), 7);
}

#[test]
fn static_calls_of_mut_self_methods_write_back() {
    assert_eq!(
        run_i32(
            "value struct Counter { total: i32 }
impl Counter { func bump(mut self, by: i32) { self.total += by; } }
let mut counter = Counter { total: 1 };
Counter::bump(counter, 5);
counter.total"
        ),
        6
    );
}

#[test]
fn static_mut_self_calls_need_mutable_places() {
    let text = compile_error_text(
        "value struct Counter { total: i32 }
impl Counter { func bump(mut self, by: i32) { self.total += by; } }
let counter = Counter { total: 1 };
Counter::bump(counter, 5);",
    );

    assert!(text.contains("immutable"), "{text}");
}

#[test]
fn mut_self_in_trait_impls() {
    assert_eq!(
        run_i32(
            "trait Bump { func bump(self) -> i32; }
value struct Counter { total: i32 }
impl Bump for Counter { func bump(mut self) -> i32 { self.total += 1; self.total } }

// Through a bound, the function changes a copy.
func twice<T: Bump>(value: T) -> i32 { value.bump() + value.bump() }

let mut counter = Counter { total: 0 };
counter.bump();
counter.bump();

// The trait object holds a boxed copy, which changes.
let boxed: Bump = counter;
boxed.bump();
boxed.bump();

counter.total * 100 + boxed.bump() * 10 + twice(counter)"
        ),
        2 * 100 + 5 * 10 + (3 + 3)
    );
}
