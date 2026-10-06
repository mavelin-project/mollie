//! The standard library, embedded in the compiler and served to programs as
//! the module `std`, like Rust's `std` crate. Items of `std::prelude` are
//! visible in every module (see [`DefRegistry::lookup`]).

use mollie_index::Idx;
use mollie_typing::{DefRegistry, Diagnostic, ModuleId, ModuleItem, TypeError};

use crate::{ModuleLoader, ModuleMap, ParsedModule, TypedASTContext};

/// Sources of modules of `std`, by their path inside it.
const SOURCES: &[(&str, &str)] = &[
    ("option", include_str!("../std/option.mol")),
    ("result", include_str!("../std/result.mol")),
    ("iter", include_str!("../std/iter.mol")),
    ("container", include_str!("../std/container.mol")),
    ("range", include_str!("../std/range.mol")),
    ("math", include_str!("../std/math.mol")),
    ("hash", include_str!("../std/hash.mol")),
    ("array", include_str!("../std/array.mol")),
    ("string", include_str!("../std/string.mol")),
    ("collections", include_str!("../std/collections.mol")),
    ("prelude", include_str!("../std/prelude.mol")),
];

const ROOT_SOURCE: &str = include_str!("../std/lib.mol");

/// Sources of `std`, by path of their module (`""` for its root, `option`,
/// ...), for tools showing them.
pub fn std_sources() -> impl Iterator<Item = (&'static str, &'static str)> {
    std::iter::once(("", ROOT_SOURCE)).chain(SOURCES.iter().copied())
}

/// Loads modules of `std` from the embedded sources.
struct StdLoader {
    root: ModuleId,
}

/// Path of `module` inside the library with the root `root`, like
/// `option`, or `None` if it isn't in the library.
fn path_in(registry: &DefRegistry, root: ModuleId, module: ModuleId) -> Option<String> {
    let mut names = Vec::new();
    let mut current = module;

    while current != root {
        let module = &registry.modules[current];

        names.push(module.name.as_str());
        current = module.parent?;
    }

    names.reverse();

    Some(names.join("::"))
}

impl ModuleLoader for StdLoader {
    type Error = ();

    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error> {
        let path = path_in(registry, self.root, module).ok_or(())?;
        let &(_, source) = SOURCES.iter().find(|&&(name, _)| name == path).ok_or(())?;

        Ok(ParsedModule::parse(source).unwrap_or_else(|error| panic!("syntax error in `std::{path}`: {error:?}")))
    }
}

impl TypedASTContext {
    /// Loads and checks `std`, and makes its prelude visible in every module.
    /// It's done once, before the first program (by
    /// [`TypedASTContext::process`] if [`TypedASTContext::use_std`] is set),
    /// and its errors (a bug of the compiler) are reported in
    /// [`TypedASTContext::diagnostics`].
    pub fn load_std(&mut self) {
        if self.tcx.def_registry.extern_roots.contains_key("std") {
            return;
        }

        let root = self.tcx.def_registry.register_extern_root("std");
        let mut map = ModuleMap::new(StdLoader { root });

        map.register_from_str(&mut self.tcx.def_registry, &mut self.diagnostics, ROOT_SOURCE, root);
        map.process_all_imports(&mut self.tcx.def_registry, &mut self.diagnostics);

        let mut context = self.take_ref();

        map.process_all_declarations(&mut context);
        // Functions get their types before constants and default values of
        // fields are evaluated, which may call them (and report that they
        // can't be evaluated).
        map.process_all_signatures(&mut context);
        map.register_constants(&mut context);
        map.process_all_defaults(&mut context);
        map.process_all_constants(&mut context);
        map.process_all_bodies(&mut context);

        if let Some(ModuleItem::SubModule(prelude)) = self.tcx.def_registry.modules[root].get_item("prelude") {
            self.tcx.def_registry.prelude = Some(prelude);
        }
    }
}

/// Loads modules of a stub of the host's API from its sources, by path
/// (`graphics`, `graphics::shapes`).
struct StubLoader {
    modules: Vec<(String, String)>,
}

impl ModuleLoader for StubLoader {
    type Error = ();

    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error> {
        let path = path_in(registry, ModuleId::ZERO, module).ok_or(())?;
        let (_, source) = self.modules.iter().find(|(name, _)| *name == path).ok_or(())?;

        // Syntax errors are reported by the module map, as an empty module.
        ParsedModule::parse_stub(source).map_err(|_| ())
    }
}

impl TypedASTContext {
    /// Loads a stub of the host's API (written by `mollie::host`) into the
    /// module of the host ([`ModuleId::ZERO`]): `root` declares its items and
    /// submodules (`module graphics;`), whose sources are in `modules` by
    /// path. Tools like language servers check programs against it without
    /// the host. Errors are reported in [`TypedASTContext::diagnostics`].
    pub fn load_host_stub(&mut self, root: &str, modules: &[(String, String)]) {
        if self.use_std {
            self.load_std();
        }

        let mut map = ModuleMap::new(StubLoader { modules: modules.to_vec() });
        let parsed = ParsedModule::parse_stub(root).unwrap_or_else(|error| {
            self.diagnostics
                .report(Diagnostic::new(TypeError::Parse { message: error.0 }).with_primary_span(ModuleId::ZERO, error.1.unwrap_or_default()));

            ParsedModule {
                stmts: Vec::new(),
                final_stmt: None,
                stub: true,
            }
        });

        map.register(&mut self.tcx.def_registry, &mut self.diagnostics, parsed, ModuleId::ZERO);
        map.process_all_imports(&mut self.tcx.def_registry, &mut self.diagnostics);

        let mut context = self.take_ref();

        map.process_all_declarations(&mut context);
        map.process_all_signatures(&mut context);
        map.register_constants(&mut context);
        map.process_all_defaults(&mut context);
        map.process_all_constants(&mut context);
        map.process_all_bodies(&mut context);
    }
}
