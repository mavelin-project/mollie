//! Named arguments and default values of parameters.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

const BUTTON: &str = "func button(text: string, enabled: bool = true, width: i32 = text.len() as i32 * 8) -> i32 {
    if enabled { width } else { 0 }
}
";

#[test]
fn named_arguments_and_defaults() {
    assert_no_errors(check(&format!(
        "{BUTTON}let a: i32 = button(\"OK\");
let b: i32 = button(\"OK\", false);
let c: i32 = button(text: \"Cancel\", width: 10);
let d: i32 = button(width: 4, text: \"x\", enabled: false);
let e: i32 = button(\"OK\", width: 2);"
    )));
}

#[test]
fn defaults_of_methods_use_self() {
    assert_no_errors(check(
        "struct Pen { color: i32 }

impl Pen {
    func new(color: i32 = 7) -> Pen { Pen { color } }

    func draw(self, color: i32 = self.color, times: i32 = 1) -> i32 { color * times }
}

let pen = Pen::new();
let a: i32 = pen.draw();
let b: i32 = pen.draw(times: 3);
let c: i32 = Pen::new(color: 2).draw(1, times: 2);",
    ));
}

#[test]
fn defaults_of_generic_functions() {
    assert_no_errors(check(
        "func repeat<T>(value: T, count: i32 = 2) -> T[] {
    let mut values: T[] = [];

    for i in 0..count { values.push(value); }

    values
}

let a: i32[] = repeat(1);
let b: string[] = repeat(value: \"x\", count: 3);",
    ));
}

#[test]
fn unknown_arguments() {
    only_errors(check(&format!("{BUTTON}let a = button(\"OK\", height: 3);")), |error| {
        matches!(error, TypeError::UnknownArgument { .. })
    });
}

#[test]
fn duplicate_arguments() {
    only_errors(check(&format!("{BUTTON}let a = button(\"OK\", text: \"again\");")), |error| {
        matches!(error, TypeError::DuplicateArgument { .. })
    });
}

#[test]
fn missing_arguments() {
    only_errors(check(&format!("{BUTTON}let a = button(enabled: false);")), |error| {
        matches!(error, TypeError::MissingArgument { .. })
    });
}

#[test]
fn positional_after_named() {
    only_errors(check(&format!("{BUTTON}let a = button(text: \"OK\", false);")), |error| {
        matches!(error, TypeError::PositionalAfterNamed)
    });
}

#[test]
fn named_arguments_of_function_values() {
    only_errors(check("let f = |x| { x };\nlet a: i32 = f(x: 1);"), |error| {
        matches!(error, TypeError::NamedArgumentsNotSupported)
    });
}

#[test]
fn defaults_are_checked() {
    only_errors(check("func f(x: i32 = \"no\") -> i32 { x }"), |error| {
        matches!(error, TypeError::Unexpected { .. })
    });
}
