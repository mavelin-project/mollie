//! The standard library: `Option`, `Result`, numbers, arrays, strings and
//! collections.

use crate::{run_bool, run_f32, run_i32, run_i32_stressed};

#[test]
fn option_methods() {
    assert_eq!(
        run_i32(
            "let some: Option<i32> = Option::Some { value: 3 };
let none: Option<i32> = Option::None;

some.unwrap_or(0) + none.unwrap_or(10)"
        ),
        13
    );
    assert!(run_bool(
        "let some: Option<i32> = Option::Some { value: 3 };
let none: Option<i32> = Option::None;

some.is_some() && some.is_none() == false && none.is_none()"
    ));
}

#[test]
fn result_methods() {
    assert_eq!(
        run_i32(
            r#"let ok: Result<i32, string> = Result::Ok { value: 4 };
let failed: Result<i32, string> = Result::Err { error: "bad" };

ok.unwrap_or(0) * 100 + failed.unwrap_or(5) * 10 + failed.err().unwrap_or("").len() as i32 + ok.ok().unwrap_or(0) * 1000"#
        ),
        4000 + 400 + 50 + 3
    );
}

#[test]
fn functions_returning_results() {
    assert_eq!(
        run_i32(
            r#"func parse_digit(text: string) -> Result<i32, string> {
    match text {
        "0" => Result::Ok { value: 0 },
        "1" => Result::Ok { value: 1 },
        other => Result::Err { error: "not a digit: ${other}" },
    }
}

let errors = match parse_digit("x") {
    Ok => 0,
    Err { error } => error.len() as i32,
};

parse_digit("1").unwrap_or(9) * 100 + errors"#
        ),
        // "not a digit: x" is 14 bytes.
        100 + 14
    );
}

#[test]
fn variants_without_their_enum() {
    assert_eq!(
        run_i32(
            "func first(values: i32[]) -> Option<i32> {
    if values.len() == 0 { None } else { Some { value: values[0] } }
}

let mut total = first([7, 8]).unwrap_or(0) + first([]).unwrap_or(100);
let mut maybe: Option<i32> = None;

maybe = Some { value: 1000 };
total + maybe.unwrap_or(0)"
        ),
        1107
    );
}

#[test]
fn numbers() {
    assert_eq!(
        run_i32(
            "let a = -5;
let b = 3;
let twelve = 12;
a.abs() + b.max(7) * 10 + twelve.clamp(0, 4) * 100 + a.min(b) * 1000"
        ),
        5 + 70 + 400 - 5000
    );
}

#[test]
fn floats() {
    let value = run_f32(
        "let two = 2.0;
let x = -2.5;
two.sqrt() * 1000.0 + x.round() * 100.0 + x.floor() * 10.0 + x.abs() + two.pow(3.0)",
    );

    assert!((value - (1414.2136 - 300.0 - 30.0 + 2.5 + 8.0)).abs() < 1e-2, "{value}");

    let trigonometry = run_f32(
        "let angle = PI / 6.0;
angle.sin() * 100.0 + angle.cos().pow(2.0) * 10.0 + (1.0).atan2(1.0) * 4.0 / PI + (1.0).exp().ln()",
    );

    assert!((trigonometry - (50.0 + 7.5 + 1.0 + 1.0)).abs() < 1e-3, "{trigonometry}");
}

#[test]
fn array_methods() {
    assert_eq!(
        run_i32(
            "let values = [5, 3, 9, 1];
values.push(7);
values.insert(0, 10);
let removed = values.remove(2);
let last = values.pop().unwrap();
values.sort_by(|a, b| { a - b });
let copy = values.copy();
copy.reverse();
let position = values.index_of(9).unwrap_or(99) as i32;
let found = if values.contains(4) { 1 } else { 0 };
removed * 10000 + last * 1000 + values[0] * 100 + copy[0] * 10 + position + found"
        ),
        3 * 10000 + 7 * 1000 + 100 + 10 * 10 + 2
    );
}

#[test]
fn stable_sorting() {
    assert_eq!(
        run_i32(
            "struct Item { key: i32, order: i32 }
let items = [Item { key: 2, order: 0 }, Item { key: 1, order: 1 }, Item { key: 2, order: 2 }, Item { key: 1, order: 3 }];
items.sort_by(|a, b| { a.key - b.key });
items[0].order * 1000 + items[1].order * 100 + items[2].order * 10 + items[3].order"
        ),
        1302
    );
}

#[test]
fn string_methods() {
    assert_eq!(
        run_i32(
            "let text = \"  Hello, wörld  \".trim();
let parts = \"a,b,,c\".split(\",\");
let shout = text.to_upper();
let number = \"-42\".parse_int().unwrap_or(0) as i32;
let bad = if \"4x\".parse_int() is None { 1 } else { 0 };
let mut chars = 0;
for c in text.chars() { chars += 1; }
let position = text.find(\"wö\").unwrap_or(99) as i32;
parts.len() as i32 * 10000 + chars * 100 + position + number * 1000000 + bad * 10"
        ),
        4 * 10000 + 12 * 100 + 7 - 42 * 1_000_000 + 10
    );
    assert!(run_bool(
        "\"apple\" < \"banana\" && \"b\" > \"a\" && \"same\" <= \"same\" && \"ab\".repeat(3) == \"ababab\""
    ));
    assert!(run_bool(
        "\"Hello\".to_lower() == \"hello\" && \"abc\".starts_with(\"ab\") && \"abc\".ends_with(\"bc\") && !\"abc\".contains(\"x\")"
    ));
}

const MAP_PROGRAM: &str = "let scores: Map<string, i32> = Map::new();
let mut i = 0;
while i < 100 {
    scores.insert(\"player ${i}\", i);
    i += 1;
}
scores.insert(\"player 7\", 700);
scores.remove(\"player 8\");
let mut order = 0;
for entry in scores {
    if entry.key == \"player 9\" { order = entry.value; }
}
let names: Set<string> = Set::new();
names.insert(\"a\");
names.insert(\"b\");
names.insert(\"a\");
scores.len() as i32 * 100000
    + scores.get(\"player 7\").unwrap_or(0) * 10
    + (if scores.contains(\"player 8\") { 1 } else { 0 })
    + order * 1000
    + names.len() as i32";

#[test]
fn maps_and_sets() {
    assert_eq!(run_i32(MAP_PROGRAM), 99 * 100_000 + 7000 + 9000 + 2);
}

#[test]
fn maps_survive_collections() {
    assert_eq!(run_i32_stressed(MAP_PROGRAM), 99 * 100_000 + 7000 + 9000 + 2);
}
