//! Format specifiers of interpolated values: `"${value:spec}"`.
#![allow(clippy::literal_string_with_formatting_args)]

use crate::{run_bool, run_bool_stressed};

#[test]
fn integers() {
    assert!(run_bool(
        "let n = 42;
let negative = -7;
let byte: u8 = 255;

\"${n:5}\" == \"   42\"
    && \"${n:<5}|\" == \"42   |\"
    && \"${n:^6}\" == \"  42  \"
    && \"${n:05}\" == \"00042\"
    && \"${negative:04}\" == \"-007\"
    && \"${byte:x}\" == \"ff\"
    && \"${byte:X}\" == \"FF\"
    && \"${n:b}\" == \"101010\"
    && \"${byte:08b}\" == \"11111111\"
    && \"${negative:x}\" == \"-7\""
    ));
}

#[test]
fn floats() {
    assert!(run_bool(
        "let value = 3.14159;

\"${value:.2}\" == \"3.14\" && \"${value:8.3}\" == \"   3.142\" && \"${value:.0}\" == \"3\" && \"${value:<6.1}|\" == \"3.1   |\""
    ));
}

#[test]
fn strings_and_booleans() {
    assert!(run_bool(
        "let name = \"ab\";

\"[${name:5}]\" == \"[ab   ]\" && \"[${name:>5}]\" == \"[   ab]\" && \"[${true:^8}]\" == \"[  true  ]\" && \"${name:1}\" == \"ab\""
    ));
}

#[test]
fn formatting_under_gc_pressure() {
    assert!(run_bool_stressed(
        "let mut text = \"\";

for i in 0..20 { text = text + \"${i:02x}\"; }

text.len() == 40 && text.slice(0, 6) == \"000102\" && text.slice(38, 40) == \"13\""
    ));
}
