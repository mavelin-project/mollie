//! `return`, `?`, `panic` and methods with their own generics.

use mollie_compiler::sandbox::{Limits, TrapKind};

use crate::{run_bool, run_i32, run_i32_stressed, run_limited};

#[test]
fn early_return() {
    assert_eq!(
        run_i32(
            "func sign(x: i32) -> i32 {
    if x > 0 {
        return 1;
    }

    if x < 0 { return 0 - 1; }

    0
}

func find(values: i32[], wanted: i32) -> i32 {
    let mut i = 0;

    while i < values.len() as i32 {
        if values[i as usize] == wanted {
            return i;
        }

        i += 1;
    }

    0 - 1
}

sign(5) * 1000 + sign(0 - 5) * 100 + find([4, 5, 6], 6) * 10 + find([1], 9)"
        ),
        1000 - 100 + 20 - 1
    );
}

#[test]
fn return_from_closure() {
    assert_eq!(
        run_i32(
            "func apply(f: func(i32) -> i32, x: i32) -> i32 { f(x) }

let f = |x| {
    if x > 10 { return 10; }

    x * 2
};

apply(f, 3) + apply(f, 50)"
        ),
        16
    );
}

#[test]
fn question_mark_on_results() {
    assert_eq!(
        run_i32(
            r#"func digit(text: string) -> Result<i32, string> {
    match text {
        "0" => Ok { value: 0 },
        "1" => Ok { value: 1 },
        "2" => Ok { value: 2 },
        other => Err { error: "not a digit: ${other}" },
    }
}

func sum(a: string, b: string) -> Result<i32, string> {
    Ok { value: digit(a)? + digit(b)? }
}

let good = sum("1", "2").unwrap_or(100);
let bad = match sum("1", "x") {
    Ok => 0,
    Err { error } => error.len() as i32,
};

good * 100 + bad"#
        ),
        // "not a digit: x" is 14 bytes.
        300 + 14
    );
}

#[test]
fn question_mark_on_options() {
    assert_eq!(
        run_i32(
            "func first(values: i32[]) -> Option<i32> {
    if values.len() == 0 { None } else { Some { value: values[0] } }
}

func sum_of_firsts(a: i32[], b: i32[]) -> Option<i32> {
    Some { value: first(a)? + first(b)? }
}

sum_of_firsts([3], [4]).unwrap_or(0) * 10 + sum_of_firsts([3], []).unwrap_or(5)"
        ),
        75
    );
}

#[test]
fn panic_stops_the_program_with_its_message() {
    let trap = run_limited::<i32>(
        r#"func check(x: i32) -> i32 {
    if x > 3 { panic("too large: ${x}"); }

    x
}

check(1) + check(10)"#,
        |types| types.i32,
        Limits::default(),
        false,
    )
    .expect_err("the program must panic");

    assert_eq!(trap.kind, TrapKind::Panic);
    assert_eq!(trap.message.as_deref(), Some("too large: 10"));
}

#[test]
fn unwrap_of_none_panics() {
    let trap =
        run_limited::<i32>("let none: Option<i32> = None;\nnone.unwrap()", |types| types.i32, Limits::default(), false).expect_err("the program must panic");

    assert_eq!(trap.kind, TrapKind::Panic);
    assert_eq!(trap.message.as_deref(), Some("called `unwrap` on `None`"));
}

#[test]
fn unwrap_and_expect_of_values() {
    assert!(run_bool(
        r#"let some: Option<i32> = Some { value: 3 };
let ok: Result<string, i32> = Ok { value: "x" };

some.unwrap() == 3 && some.expect("a value") == 3 && ok.unwrap() == "x""#
    ));
}

#[test]
fn methods_with_their_own_generics() {
    assert_eq!(
        run_i32(
            r#"struct Holder<T> { value: T }

impl<T> Holder<T> {
    func map<U>(self, f: func(T) -> U) -> Holder<U> {
        Holder { value: f(self.value) }
    }
}

let number = Holder { value: 20 };
let text = number.map(|value| { "${value}!" });
let length = text.map(|value| { value.len() as i32 });
let doubled = number.map(|value| { value * 2 });

length.value * 100 + doubled.value"#
        ),
        300 + 40
    );
}

#[test]
fn generic_methods_survive_collections() {
    assert_eq!(
        run_i32_stressed(
            r#"struct Holder<T> { value: T }

impl<T> Holder<T> {
    func map<U>(self, f: func(T) -> U) -> Holder<U> {
        Holder { value: f(self.value) }
    }
}

let mut total = 0;
let mut i = 0;

while i < 20 {
    let text = Holder { value: i }.map(|value| { "${value}" });

    total += text.map(|value| { value.len() as i32 }).value;
    i += 1;
}

total"#
        ),
        // 10 one-digit numbers and 10 two-digit numbers.
        10 + 20
    );
}
