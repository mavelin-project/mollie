use mollie_index::Idx;
use mollie_parser::Stmt;
use mollie_typing::{DefRegistry, Diagnostic, ModuleId, ModuleItem, ModuleSpan, TypeError};

use crate::{HostVariable, ModuleLoader, ModuleMap, ParsedModule, TypedASTContext, ty::binding_type};

const SOURCES: &[(&str, &str)] = &[
    ("option", include_str!("../../../std/option.mol")),
    ("result", include_str!("../../../std/result.mol")),
    ("iter", include_str!("../../../std/iter.mol")),
    ("container", include_str!("../../../std/container.mol")),
    ("range", include_str!("../../../std/range.mol")),
    ("math", include_str!("../../../std/math.mol")),
    ("hash", include_str!("../../../std/hash.mol")),
    ("array", include_str!("../../../std/array.mol")),
    ("string", include_str!("../../../std/string.mol")),
    ("collections", include_str!("../../../std/collections.mol")),
    ("prelude", include_str!("../../../std/prelude.mol")),
];

const ROOT_SOURCE: &str = include_str!("../../../std/lib.mol");

pub fn std_sources() -> impl Iterator<Item = (&'static str, &'static str)> {
    std::iter::once(("", ROOT_SOURCE)).chain(SOURCES.iter().copied())
}

struct StdLoader {
    root: ModuleId,
}

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

impl TypedASTContext {
    pub fn load_library<L: ModuleLoader>(&mut self, name: &str, source: &str, loader: L, package: Option<&str>, dependencies: &[&str]) -> ModuleId {
        if self.use_std {
            self.load_std();
        }

        let root = self.tcx.def_registry.register_extern_root(name);

        if let Some(package) = package {
            self.tcx.def_registry.packages.insert(root, package.to_owned());
        }

        // Its dependencies are visible while it's checked (and so is the
        // library itself, by its name), then hidden again.
        let granted = dependencies
            .iter()
            .filter(|&&dependency| self.tcx.def_registry.grant_root(dependency))
            .copied()
            .collect::<Vec<_>>();

        let mut map = ModuleMap::new(loader);
        let parsed = ParsedModule::parse(source).unwrap_or_else(|error| {
            self.diagnostics
                .report(Diagnostic::new(TypeError::Parse { message: error.0 }).with_primary_span(root, error.1));

            ParsedModule {
                stmts: Vec::new(),
                final_stmt: None,
                stub: false,
            }
        });

        // Registered without a root of the map: top-level code is an error.
        map.register(&mut self.tcx.def_registry, &mut self.diagnostics, parsed, root);
        map.process_all_imports(&mut self.tcx.def_registry, &mut self.diagnostics);

        let mut context = self.take_ref();

        map.process_all_declarations(&mut context);
        map.process_all_signatures(&mut context);
        map.register_constants(&mut context);
        map.process_all_defaults(&mut context);
        map.process_all_constants(&mut context);
        map.process_all_bodies(&mut context);

        for dependency in granted {
            self.tcx.def_registry.restrict_root(dependency);
        }

        self.tcx.def_registry.restrict_root(name);

        root
    }
}

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
    pub fn load_host_stub(&mut self, root: &str, modules: &[(String, String)]) {
        if self.use_std {
            self.load_std();
        }

        let mut map = ModuleMap::new(StubLoader { modules: modules.to_vec() });
        let parsed = ParsedModule::parse_stub(root).unwrap_or_else(|error| {
            self.diagnostics
                .report(Diagnostic::new(TypeError::Parse { message: error.0 }).with_primary_span(ModuleId::ZERO, error.1));

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

        // `extern let name: Type;`: variables the host gives to programs,
        // declared in every program (see `TypedASTContext::process`).
        let module = map.root();
        let mut variables = Vec::new();

        for stmt in &map.root_module().stmts {
            let Stmt::VariableDecl(decl) = &stmt.value else {
                continue;
            };

            if !decl.external {
                continue;
            }

            let name = decl.name.value.0.clone();

            if let Some(value) = &decl.value {
                context.type_solver.error(TypeError::ExternVariableValue, ModuleSpan(module, value.span));
            }

            match &decl.ty {
                Some(ty) => variables.push(HostVariable {
                    ty: binding_type(ty.clone(), module, &mut context, decl.mutable.is_some()),
                    name,
                    mutable: decl.mutable.is_some(),
                }),
                None => {
                    context.type_solver.error(TypeError::TypeExpected { name }, ModuleSpan(module, decl.name.span));
                }
            }
        }

        self.host_variables.extend(variables);
    }
}
