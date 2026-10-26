use mollie_typing::TypeError;

use crate::{assert_no_errors, check_with_library, check_with_modules, single_error};

const LIBRARY: &str = "pub struct Point { pub x: i32, y: i32 }
pub struct Sized { pub width: i32, height: i32 = 1 }

impl Point {
    pub func new() -> Point { Point { x: 1, y: 2 } }
    pub func shown(self) -> i32 { self.x }
    func hidden(self) -> i32 { self.y }
    func make_hidden() -> Point { Point { x: 0, y: 0 } }
}

trait Secret { func secret(self) -> i32; }
impl Secret for Point { func secret(self) -> i32 { 3 } }

pub enum Choice { One { value: i32 }, Two }

pub func shown() -> i32 { 1 }
func hidden() -> i32 { 2 }
pub const LIMIT: i32 = 4;
const SECRET: i32 = 5;

pub module shapes;
module inner;
pub import { Square } from shapes;
import { Circle } from shapes;";

const MODULES: &[(&str, &str)] = &[
    ("shapes", "pub struct Square { pub side: i32 }\npub struct Circle { pub radius: i32 }"),
    ("inner", "pub func deep() -> i32 { 6 }"),
];

fn check(source: &str) -> (Vec<TypeError>, mollie_typing::TyCtxt) {
    check_with_library(LIBRARY, MODULES, source, None, true)
}

#[track_caller]
fn private(source: &str, name: &str) {
    let error = single_error(check(source));

    assert!(matches!(&error, TypeError::Private { name: found } if found == name), "{source}: {error:?}");
}

#[test]
fn pub_items_of_a_library_can_be_used() {
    assert_no_errors(check(
        "import { shown, Point, Choice, LIMIT } from lib;
import { Square } from lib::shapes;
let a: i32 = shown() + lib::shown() + LIMIT + lib::LIMIT;
let point = Point::new();
let b: i32 = point.x + point.shown();
let c = lib::Choice::One { value: 2 };
let d = lib::Square { side: 3 };
let f = Square { side: 1 };
let e: i32 = d.side + f.side;",
    ));
}

#[test]
fn private_items_of_a_library_are_errors() {
    private("import { hidden } from lib;", "hidden");
    private("let x: i32 = lib::hidden();", "hidden");
    private("let x: i32 = lib::SECRET;", "SECRET");
    private("import { deep } from lib::inner;", "inner");
}

#[test]
fn private_fields_are_errors() {
    private("let point = lib::Point::new();\nlet y: i32 = point.y;", "y");
    private("let point = lib::Point { x: 1, y: 2 };", "y");
    private("let point = lib::Point::new();\npoint.y = 3;", "y");

    // A private field with a default can be left out, not one without.
    assert_no_errors(check("let size = lib::Sized { width: 2 };"));
    assert!(matches!(single_error(check("let point = lib::Point { x: 1 };")), TypeError::MissingField { .. }));
}

#[test]
fn private_functions_of_impls_are_errors() {
    private("let point = lib::Point::new();\nlet y: i32 = point.hidden();", "hidden");
    private("let point = lib::Point::make_hidden();", "make_hidden");
    private("let point = lib::Point::new();\nlet s: i32 = point.secret();", "secret");
}

#[test]
fn pub_imports_re_export() {
    assert_no_errors(check("import { Square } from lib;\nlet square = Square { side: 1 };"));
    private("import { Circle } from lib;", "Circle");
}

#[test]
fn a_package_uses_its_private_items() {
    assert_no_errors(check_with_library(
        LIBRARY,
        MODULES,
        "import { hidden, Circle } from lib;
import { deep } from lib::inner;
let point = lib::Point { x: 1, y: 2 };
let a: i32 = hidden() + deep() + point.y + point.hidden() + point.secret() + lib::SECRET;",
        Some("game"),
        true,
    ));
}

#[test]
fn libraries_are_hidden_until_granted() {
    let error = single_error(check_with_library(LIBRARY, MODULES, "import { shown } from lib;", None, false));

    assert!(matches!(&error, TypeError::Unavailable { name } if name == "lib"), "{error:?}");
}

#[test]
fn modules_of_a_program_see_each_other() {
    // One package: nothing needs `pub`.
    assert_no_errors(check_with_modules(
        "module shapes;
import { Square, area } from shapes;
let square = Square { side: 2 };
let a: i32 = area(square) + square.side;",
        &[("shapes", "struct Square { side: i32 }\nfunc area(square: Square) -> i32 { square.side * square.side }")],
    ));
}

#[test]
fn libraries_need_no_top_level_code() {
    let (errors, _) = check_with_library("let x = 1;", &[], "", None, true);

    assert!(errors.iter().any(|error| matches!(error, TypeError::TopLevelCode)), "{errors:?}");
}
