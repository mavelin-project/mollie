use crate::run_i32;

const OPTION: &str = "enum Option<T> { Some { value: T }, None }\n";

#[test]
fn variants_are_matched() {
    assert_eq!(
        run_i32(
            "enum Shape { Circle { radius: i32 }, Square { side: i32 }, Empty }

func area(shape: Shape) -> i32 {
    if shape is Shape::Circle { radius } {
        3 * radius * radius
    } else if shape is Shape::Square { side } {
        side * side
    } else {
        0
    }
}

area(Shape::Circle { radius: 2 }) + area(Shape::Square { side: 3 }) + area(Shape::Empty)"
        ),
        21
    );
}

#[test]
fn bound_value_is_usable() {
    assert_eq!(
        run_i32(&format!(
            "{OPTION}let value = Option::Some {{ value: 7 }};

if value is Option::Some {{ value }} {{ value * 6 }} else {{ 0 }}"
        )),
        42
    );
}

#[test]
fn nested_patterns_must_match_too() {
    assert_eq!(
        run_i32(&format!(
            "{OPTION}func is_five(value: Option<i32>) -> i32 {{
    if value is Option::Some {{ value: 5 }} {{ 1 }} else {{ 0 }}
}}

is_five(Option::Some {{ value: 5 }}) * 10 + is_five(Option::Some {{ value: 3 }}) + is_five(Option::None) * 100"
        )),
        10
    );
}

#[test]
fn generic_methods_are_instantiated_per_type() {
    assert_eq!(
        run_i32(
            "struct Holder<T> { value: T }

impl<T> Holder<T> {
    func get(self) -> T {
        self.value
    }
}

let number = Holder { value: 40 };
let flag = Holder { value: true };

if flag.get() { number.get() + 2 } else { 0 }"
        ),
        42
    );
}
