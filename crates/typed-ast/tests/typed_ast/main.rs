//! Type-checks small programs end to end and checks the reported errors.

mod arguments;
mod bounds;
mod constants;
mod containers;
mod declarations;
mod defaults;
mod expected;
mod formatting;
mod functions;
mod generics;
mod iteration;
mod literals;
mod loops;
mod modules;
mod operators;
mod patterns;
mod ranges;
mod returns;
mod robustness;
mod statements;
mod std_lib;
mod strings;
mod traits;
mod values;

use std::collections::HashMap;

use mollie_shared::pretty_fmt::FmtIteratorExt;
use mollie_typed_ast::{ModuleLoader, ParsedModule, TypedASTContext};
use mollie_typing::{DefRegistry, ModuleId, TyCtxt, TypeError, TypeRef};

/// Loads submodules from memory, by their path relative to the root module
/// (like `a::b`).
struct MemoryLoader(HashMap<String, String>);

impl ModuleLoader for MemoryLoader {
    type Error = ();

    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error> {
        let source = self.0.get(&module_path(registry, module)).ok_or(())?;

        ParsedModule::parse(source).map_err(|_| ())
    }
}

fn module_path(registry: &DefRegistry, module: ModuleId) -> String {
    let current = &registry.modules[module];

    match current.parent {
        // Paths are relative to the root of the program.
        Some(parent) if registry.modules[parent].parent.is_some() => format!("{}::{}", module_path(registry, parent), current.name),
        _ => current.name.clone(),
    }
}

/// Type-checks `source` as the root module, with `modules` (path and source)
/// as its submodules, and returns all reported errors.
pub fn check_with_modules(source: &str, modules: &[(&str, &str)]) -> (Vec<TypeError>, TyCtxt) {
    let mut context = TypedASTContext::default();
    let void = context.tcx.types.core_types.void;
    let loader = MemoryLoader(modules.iter().map(|&(path, source)| (path.to_owned(), source.to_owned())).collect());

    context.process(loader, source, Vec::<(String, TypeRef)>::new(), void);

    (
        context.diagnostics.errors.into_values().map(|diagnostic| diagnostic.error).collect(),
        context.tcx,
    )
}

/// Type-checks `source` as the root module and returns all reported errors.
pub fn check(source: &str) -> (Vec<TypeError>, TyCtxt) {
    check_with_modules(source, &[])
}

#[track_caller]
pub fn assert_no_errors((errors, tcx): (Vec<TypeError>, TyCtxt)) {
    assert!(
        errors.is_empty(),
        "expected no errors, found: {}",
        errors.iter().map(|err| err.message(&tcx)).join('\n')
    );
}

/// Asserts that exactly one error was reported, and returns it.
#[track_caller]
pub fn single_error((errors, tcx): (Vec<TypeError>, TyCtxt)) -> TypeError {
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error, found: {}",
        errors.iter().map(|err| err.message(&tcx)).join('\n')
    );

    errors.into_iter().next().unwrap()
}

/// Asserts that at least one error was reported, and that all of them match
/// `predicate`.
#[track_caller]
pub fn only_errors((errors, tcx): (Vec<TypeError>, TyCtxt), predicate: impl Fn(&TypeError) -> bool) {
    assert!(!errors.is_empty(), "expected errors, found none");
    assert!(
        errors.iter().all(&predicate),
        "found unexpected errors: {}",
        errors.iter().filter(|&err| !predicate(err)).map(|err| err.message(&tcx)).join('\n')
    );
}
