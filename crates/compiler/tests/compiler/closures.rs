use crate::run_i32;

const APPLY: &str = "func apply(f: func(i32) -> i32, x: i32) -> i32 {\n    f(x)\n}\n";

#[test]
fn closure_without_captures() {
    assert_eq!(run_i32("let add = |a, b| { a + b };\nadd(2, 3)"), 5);
}

#[test]
fn closure_with_captures() {
    assert_eq!(run_i32("let base = 10;\nlet add = |x| { x + base };\nadd(5)"), 15);
}

#[test]
fn closure_passed_to_a_function() {
    assert_eq!(run_i32(&format!("{APPLY}let k = 3;\napply(|x| {{ x * k }}, 7)")), 21);
}

#[test]
fn function_used_as_a_value() {
    assert_eq!(run_i32(&format!("{APPLY}func double(x: i32) -> i32 {{ x * 2 }}\n\napply(double, 21)")), 42);
}

#[test]
fn closure_returned_from_a_function() {
    assert_eq!(
        run_i32(
            "func adder(n: i32) -> func(i32) -> i32 {
    |x| { x + n }
}

let add_two = adder(2);
add_two(40)"
        ),
        42
    );
}

#[test]
fn closure_stored_in_a_field() {
    assert_eq!(
        run_i32(
            "struct Operation { run: func(i32) -> i32 }

let offset = 5;
let operation = Operation { run: |x| { x + offset } };
operation.run(1)"
        ),
        6
    );
}
