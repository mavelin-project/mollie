#![allow(clippy::missing_panics_doc)]

mod arguments;
mod bounds;
mod constants;
mod containers;
mod declarations;
mod defaults;
mod deferred;
mod expected;
mod fixed_arrays;
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
mod views;
mod visibility;

use std::collections::HashMap;

use mollie_shared::pretty_fmt::FmtIteratorExt;
use mollie_typed_ast::{ModuleLoader, ParsedModule, TypedASTContext};
use mollie_typing::{DefRegistry, ModuleId, TyCtxt, TypeError, TypeRef};

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

pub fn check_with_modules(source: &str, modules: &[(&str, &str)]) -> (Vec<TypeError>, TyCtxt) {
    let mut context = TypedASTContext::default();
    let void = context.tcx.types.core_types.void;
    let loader = MemoryLoader(modules.iter().map(|&(path, source)| (path.to_owned(), source.to_owned())).collect());

    context.process(loader, source, Vec::<(String, TypeRef)>::new(), void);

    (
        context.diagnostics.errors.into_values().map(|diagnostic| *diagnostic.error).collect(),
        context.tcx,
    )
}

pub fn check_with_library(library: &str, modules: &[(&str, &str)], source: &str, package: Option<&str>, granted: bool) -> (Vec<TypeError>, TyCtxt) {
    let mut context = TypedASTContext::default();
    let void = context.tcx.types.core_types.void;
    let loader = MemoryLoader(modules.iter().map(|&(path, source)| (path.to_owned(), source.to_owned())).collect());

    context.load_library("lib", library, loader, package, &[]);

    if granted {
        context.tcx.def_registry.grant_root("lib");
    }

    context.program_package = package.map(str::to_owned);
    context.process(MemoryLoader(HashMap::new()), source, Vec::<(String, TypeRef)>::new(), void);

    (
        context.diagnostics.errors.into_values().map(|diagnostic| *diagnostic.error).collect(),
        context.tcx,
    )
}

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

#[track_caller]
pub fn only_errors((errors, tcx): (Vec<TypeError>, TyCtxt), predicate: impl Fn(&TypeError) -> bool) {
    assert!(!errors.is_empty(), "expected errors, found none");
    assert!(
        errors.iter().all(&predicate),
        "found unexpected errors: {}",
        errors.iter().filter(|&err| !predicate(err)).map(|err| err.message(&tcx)).join('\n')
    );
}
