use crate::run_i32;

const POINT: &str = "struct Point { x: i32, y: i32 }\n";

#[test]
fn construction_and_field_access() {
    assert_eq!(run_i32(&format!("{POINT}let p = Point {{ x: 3, y: 4 }};\np.x * p.y")), 12);
}

#[test]
fn fields_of_different_sizes_are_laid_out_correctly() {
    assert_eq!(
        run_i32(
            "struct Mixed { flag: bool, value: i64, small: u8, ratio: f32 }

let m = Mixed { flag: true, value: 1000000000000, small: 200, ratio: 0.5 };

if m.flag && m.small == 200 && m.ratio == 0.5 { (m.value / 1000) as i32 } else { 0 }"
        ),
        1_000_000_000
    );
}

#[test]
fn field_assignment() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}let p = Point {{ x: 1, y: 2 }};
p.x = 10;
p.y += 5;
p.x + p.y"
        )),
        17
    );
}

#[test]
fn default_field_values() {
    assert_eq!(
        run_i32(
            "struct Config { size: i32 = 10, scale: i32 }

let config = Config { scale: 3 };
config.size * config.scale"
        ),
        30
    );
}

#[test]
fn default_value_of_another_struct() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}struct Line {{ start: Point = Point {{ x: 1, y: 2 }}, end: Point }}

let line = Line {{ end: Point {{ x: 5, y: 7 }} }};
(line.end.x - line.start.x) * 10 + line.end.y - line.start.y"
        )),
        45
    );
}

#[test]
fn methods_and_static_functions() {
    assert_eq!(
        run_i32(&format!(
            "{POINT}impl Point {{
    func new(x: i32, y: i32) -> Point {{
        Point {{ x, y }}
    }}

    func sum(self) -> i32 {{
        self.x + self.y
    }}
}}

Point::new(2, 3).sum()"
        )),
        5
    );
}

#[test]
fn methods_mutate_their_receiver() {
    assert_eq!(
        run_i32(
            "struct Counter { value: i32 }

impl Counter {
    func increment(self) {
        self.value += 1;
    }
}

let counter = Counter { value: 40 };
counter.increment();
counter.increment();
counter.value"
        ),
        42
    );
}

#[test]
fn field_and_method_with_the_same_name() {
    assert_eq!(
        run_i32(
            "struct Counter { count: i32 }

impl Counter {
    func count(self) -> i32 { self.count * 10 }
}

let counter = Counter { count: 4 };
counter.count + counter.count()"
        ),
        44
    );
}

#[test]
fn field_holding_a_function_is_called() {
    assert_eq!(
        run_i32(
            "struct Button { on_click: func(i32) -> i32 }

let offset = 1;
let button = Button { on_click: |x| { x * 2 + offset } };
button.on_click(20)"
        ),
        41
    );
}
