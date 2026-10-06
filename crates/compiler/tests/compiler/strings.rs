use crate::{run_bool, run_bool_stressed, run_usize, run_usize_stressed};

#[test]
fn string_equality() {
    assert!(run_bool("\"abc\" == \"abc\" && \"abc\" != \"abd\" && \"ab\" != \"abc\""));
}

#[test]
fn strings_in_fields() {
    assert!(run_bool(
        "struct User { name: string }\n\nlet user = User { name: \"ann\" };\nuser.name == \"ann\""
    ));
}

#[test]
fn strings_as_arguments_and_results() {
    assert!(run_bool("func same(name: string) -> string { name }\n\nsame(\"x\") == \"x\""));
}

#[test]
fn string_escapes() {
    assert!(run_bool(r#""a\"b".len() == 3 && "tab\there".len() == 8 && "\${x}" == "$" + "{x}""#));
}

#[test]
fn concatenation() {
    assert!(run_bool(
        r#"let name = "ann";
let greeting = "hi, " + name + "!";
greeting == "hi, ann!""#
    ));
}

#[test]
fn compound_concatenation() {
    assert!(run_bool(
        r#"let mut text = "a";
text += "b";
text += "c";
text == "abc""#
    ));
}

#[test]
fn length_in_bytes() {
    assert_eq!(run_usize(r#""".len() + "abc".len() + "é".len()"#), 5);
}

#[test]
fn slicing() {
    assert!(run_bool(
        r#"let text = "hello, world";
text.slice(0, 5) == "hello" && text.slice(7, 12) == "world" && text.slice(3, 3) == """#
    ));
}

#[test]
fn slicing_at_char_boundaries() {
    assert!(run_bool(r#""héllo".slice(1, 3) == "é""#));
}

#[test]
fn interpolation() {
    assert!(run_bool(
        r#"let name = "ann";
let age = 30;
"${name} is ${age}" == "ann is 30""#
    ));
}

#[test]
fn interpolation_of_numbers_and_booleans() {
    assert!(run_bool(
        r#"let small: u8 = 255;
let negative = -7;
"${small} ${negative} ${1.5} ${true} ${1 + 2}" == "255 -7 1.5 true 3""#
    ));
}

#[test]
fn interpolation_of_expressions_with_strings() {
    assert!(run_bool(
        r#"func greet(name: string) -> string { "hi, ${name}" }

"<${greet("bob")}>" == "<hi, bob>""#
    ));
}

#[test]
fn concatenation_survives_collections() {
    assert_eq!(
        run_usize_stressed(
            r#"struct Holder { text: string }

let mut text = "";
let mut i = 0;
let holder = Holder { text: "start" };

while i < 50 {
    text = text + "${i},";
    i += 1;
}

text.len() + holder.text.len()"#
        ),
        // "0," to "9," are 2 bytes, "10," to "49," are 3 bytes.
        10 * 2 + 40 * 3 + 5
    );
}

#[test]
fn strings_in_fields_survive_collections() {
    assert!(run_bool_stressed(
        r#"struct User { name: string }

func make(i: i32) -> User { User { name: "user ${i}" } }

let first = make(1);
let mut i = 0;

while i < 20 {
    let other = make(i);
    i += 1;
}

first.name == "user 1""#
    ));
}
