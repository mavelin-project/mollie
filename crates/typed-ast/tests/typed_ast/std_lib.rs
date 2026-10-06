//! The standard library and its prelude.

use mollie_typed_ast::TypedASTContext;
use mollie_typing::{LookupType, TypeError, TypeRef};

use crate::{assert_no_errors, check, check_with_modules, only_errors};

#[test]
fn prelude_is_visible_without_imports() {
    assert_no_errors(check(
        "let some: Option<i32> = Option::Some { value: 1 };
let failed: Result<i32, string> = Result::Err { error: \"failed\" };",
    ));
}

#[test]
fn prelude_is_visible_in_submodules() {
    assert_no_errors(check_with_modules("module shapes;", &[(
        "shapes",
        "func first(values: i32[]) -> Option<i32> { Option::None }",
    )]));
}

#[test]
fn std_modules_can_be_imported() {
    assert_no_errors(check(
        "import { Option } from std::option;

let none: Option<i32> = Option::None;",
    ));
}

#[test]
fn methods_of_option_and_result() {
    assert_no_errors(check(
        "let some: Option<i32> = Option::Some { value: 1 };
let a: i32 = some.unwrap_or(0);
let b: bool = some.is_some() && !some.is_none();

let result: Result<i32, string> = Result::Ok { value: 2 };
let c: Option<i32> = result.ok();
let d: Option<string> = result.err();
let e: i32 = result.unwrap_or(0);
let f: bool = result.is_ok() || result.is_err();",
    ));
}

#[test]
fn methods_of_option_are_typed() {
    let errors = check(
        "let some: Option<i32> = Option::Some { value: 1 };
let value: bool = some.unwrap_or(0);",
    );

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn declarations_shadow_the_prelude() {
    assert_no_errors(check(
        "enum Option { Yes, No }

let answer: Option = Option::Yes;",
    ));
}

#[test]
fn programs_can_opt_out_of_std() {
    let mut context = TypedASTContext::default();
    let void = context.tcx.types.core_types.void;

    context.use_std = false;
    context.process((), "let none: Option<i32> = Option::None;", Vec::<(String, TypeRef)>::new(), void);

    let errors = context.diagnostics.errors.into_values().map(|diagnostic| diagnostic.error).collect::<Vec<_>>();

    assert!(
        errors
            .iter()
            .any(|error| matches!(&**error, TypeError::NotFound { name, was_looking_for: LookupType::Type { .. } } if name == "Option")),
        "{errors:?}"
    );
}

#[test]
fn std_is_checked_once() {
    let mut context = TypedASTContext::default();
    let void = context.tcx.types.core_types.void;

    for _ in 0..2 {
        context.process((), "let none: Option<i32> = Option::None;", Vec::<(String, TypeRef)>::new(), void);
    }

    assert!(context.diagnostics.is_empty(), "{:?}", context.diagnostics.errors);
    assert_eq!(context.tcx.def_registry.extern_roots.len(), 1);
}
