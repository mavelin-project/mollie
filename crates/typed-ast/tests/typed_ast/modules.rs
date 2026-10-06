use mollie_typing::{LookupType, TypeError};

use crate::{assert_no_errors, check, check_with_modules, single_error};

const MATH: (&str, &str) = ("math", "func double(x: i32) -> i32 { x * 2 }");

#[test]
fn imported_function_can_be_called() {
    let errors = check_with_modules(
        "module math;
import { double } from math;
let result: i32 = double(2);",
        &[MATH],
    );

    assert_no_errors(errors);
}

#[test]
fn named_import_of_module() {
    let errors = check_with_modules(
        "module geometry;
import geometry::shapes;",
        &[("geometry", "module shapes;"), ("geometry::shapes", "struct Square { side: f32 }")],
    );

    assert_no_errors(errors);
}

#[test]
fn imports_are_resolved_regardless_of_module_order() {
    // `a` imports from `b`, which imports from `c`, while modules are processed
    // in the order `a`, `b`, `c`.
    let errors = check_with_modules(
        "module a;
module b;
module c;
import { greet } from a;
greet();",
        &[
            ("a", "import { greet } from super::b;"),
            ("b", "import { greet } from super::c;"),
            ("c", "func greet() {}"),
        ],
    );

    assert_no_errors(errors);
}

#[test]
fn submodule_can_use_items_of_parent() {
    let errors = check_with_modules(
        "module shapes;
struct Point { x: f32, y: f32 }",
        &[("shapes", "import { Point } from super;\nstruct Line { start: Point, end: Point }")],
    );

    assert_no_errors(errors);
}

#[test]
fn missing_module_is_reported() {
    let error = single_error(check("module nowhere;"));

    assert!(matches!(error, TypeError::NotFound { ref name, was_looking_for: LookupType::Module { .. } } if name == "nowhere"));
}

#[test]
fn missing_imported_item_is_reported() {
    let error = single_error(check_with_modules(
        "module math;
import { triple } from math;",
        &[MATH],
    ));

    assert!(matches!(error, TypeError::NotFound { ref name, .. } if name == "triple"));
}

#[test]
fn import_conflicting_with_declaration_is_reported() {
    let error = single_error(check_with_modules(
        "func double() {}
module math;
import { double } from math;",
        &[MATH],
    ));

    assert!(matches!(error, TypeError::AlreadyExists { ref name, .. } if name == "double"));
}

#[test]
fn importing_items_from_non_module_is_reported() {
    let error = single_error(check(
        "struct Point {}
import { x } from Point;",
    ));

    assert!(matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn top_level_code_in_submodule_is_reported() {
    let error = single_error(check_with_modules("module math;", &[("math", "let answer = 42;")]));

    assert!(matches!(error, TypeError::TopLevelCode));
}

#[test]
fn syntax_error_in_submodule_is_reported_as_missing_module() {
    let error = single_error(check_with_modules("module math;", &[("math", "func (")]));

    assert!(matches!(error, TypeError::NotFound { .. }));
}
