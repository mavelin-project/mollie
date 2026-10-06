//! Plain-text output of diagnostics (without `ariadne`).

use mollie_index::Idx;
use mollie_shared::{Span, SpanRange};
use mollie_typing::{DefinitionType, Diagnostic, IntType, LookupType, ModuleId, PrimitiveType, TypeError, TypeErrorValue};

use crate::context;

/// A span starting at the 0-based `line` and `column`.
const fn at(line: u32, column: u32) -> Span {
    Span::new(0, 1, SpanRange::new(line, column, line, column + 1))
}

#[test]
fn unexpected_shows_expected_and_found() {
    let (mut tcx, _) = context();
    let root = tcx.def_registry.register_program_root();
    let found = tcx.types.core_types.f32;
    let diagnostic = Diagnostic::new(TypeError::Unexpected {
        expected: TypeErrorValue::PrimitiveType(PrimitiveType::Int(IntType::I32)),
        found: TypeErrorValue::ExplicitType(found),
    })
    .with_primary_span(root, at(2, 14));

    assert_eq!(
        tcx.display_of_diagnostic(&diagnostic).to_string(),
        "error: expected `i32`\n  --> <root>:3:15: found `f32`"
    );
}

#[test]
fn not_found_names_the_module_it_looked_in() {
    let (mut tcx, _) = context();
    let geometry = tcx.def_registry.register_module("geometry", Span::default()).unwrap();
    let diagnostic = Diagnostic::new(TypeError::NotFound {
        name: String::from("Square"),
        was_looking_for: LookupType::Type { inside: geometry },
    })
    .with_primary_span(geometry, at(0, 0));

    assert_eq!(
        tcx.display_of_diagnostic(&diagnostic).to_string(),
        "error: there's no type called `Square` in `geometry`\n  --> geometry:1:1: tried to access here"
    );
}

#[test]
fn nested_modules_are_shown_as_paths() {
    let (mut tcx, _) = context();
    let geometry = tcx.def_registry.register_module("geometry", Span::default()).unwrap();
    let shapes = tcx.def_registry.register_module_in_module(geometry, "shapes", Span::default()).unwrap();
    let diagnostic = Diagnostic::new(TypeError::TopLevelCode).with_primary_span(shapes, at(4, 0));

    assert_eq!(
        tcx.display_of_diagnostic(&diagnostic).to_string(),
        "error: only the root module can contain top-level code\n  --> geometry::shapes:5:1: move this into a function"
    );
}

#[test]
fn both_spans_are_labelled() {
    let (tcx, _) = context();
    let diagnostic = Diagnostic::new(TypeError::AlreadyExists {
        name: String::from("Point"),
        primary: DefinitionType::Local,
        secondary: DefinitionType::Import,
    })
    .with_primary_span(ModuleId::ZERO, at(0, 7))
    .with_secondary_span(ModuleId::ZERO, at(3, 7));
    // Items of the host are in its module.

    assert_eq!(
        tcx.display_of_diagnostic(&diagnostic).to_string(),
        "error: `Point` is already declared\n  --> <host>:1:8: declared here\n  --> <host>:4:8: and imported here"
    );
}

#[test]
fn diagnostic_without_span_has_only_the_message() {
    let (tcx, _) = context();
    let diagnostic = Diagnostic::new(TypeError::NotAssignable);

    assert_eq!(
        tcx.display_of_diagnostic(&diagnostic).to_string(),
        "error: invalid left-hand side of assignment"
    );
}
