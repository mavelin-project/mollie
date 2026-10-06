//! Stubs of the host's API: Mollie declarations of everything the host
//! registered (functions, types, traits and impls), for tools like language
//! servers, which check programs without the host.
//!
//! A stub is a root module (`lib.mol`) and a file per module of the host
//! (`graphics.mol`). Functions are declared without bodies:
//!
//! ```mollie
//! module graphics;
//!
//! extern func log(message: string);
//!
//! impl graphics::Size {
//!     extern func area(self) -> f32;
//! }
//! ```
//!
//! Language servers load it with `TypedASTContext::load_host_stub`.

use std::{
    collections::HashMap,
    fmt::{self, Write},
    fs, io,
    path::{Path, PathBuf},
};

use mollie_index::Idx;

use crate::{
    compiler::Compiler,
    typed_ast::{FunctionBody, ModuleLoader},
    typing::{AdtKind, AdtRef, ArgType, ModuleId, ModuleItem, PrimitiveType, TraitRef, TyCtxt, Type, TypeRef},
};

/// Declarations of the host's API, by module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostStub {
    /// Items of the host's own module, and declarations of its submodules.
    pub root: String,
    /// Sources of the host's modules, by path (`graphics`).
    pub modules: Vec<(String, String)>,
}

impl HostStub {
    /// Writes the stub to `dir`: `lib.mol` and a file per module.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be written.
    pub fn write_to(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join("lib.mol"), &self.root)?;

        for (path, source) in &self.modules {
            let file = dir.join(path.replace("::", "/")).with_extension("mol");

            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::write(file, source)?;
        }

        Ok(())
    }

    /// Writes the stub to `dir` like [`HostStub::write_to`], unless it's
    /// already there, and removes files of modules the host doesn't have
    /// anymore. A game can call it on every start (see
    /// `CompilerExt::write_host_stub`): editors only see a change when the
    /// host's API changes. Returns whether files were written.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be read, written or removed.
    pub fn update(&self, dir: &Path) -> io::Result<bool> {
        let existing = Self::read_from(dir).ok();
        // Files are read back in path order.
        let mut sorted = self.modules.clone();

        sorted.sort();

        if existing.as_ref().is_some_and(|existing| existing.root == self.root && existing.modules == sorted) {
            return Ok(false);
        }

        for (path, _) in existing.iter().flat_map(|existing| &existing.modules) {
            if !self.modules.iter().any(|(module, _)| module == path) {
                fs::remove_file(dir.join(path.replace("::", "/")).with_extension("mol"))?;
            }
        }

        self.write_to(dir)?;

        Ok(true)
    }

    /// Reads a stub written by [`HostStub::write_to`]: `lib.mol`, and files of
    /// the modules it declares.
    ///
    /// # Errors
    ///
    /// Returns an error if `lib.mol` can't be read.
    pub fn read_from(dir: &Path) -> io::Result<Self> {
        let root = fs::read_to_string(dir.join("lib.mol"))?;
        let mut modules = Vec::new();

        for entry in walk(dir)? {
            let Ok(relative) = entry.strip_prefix(dir) else {
                continue;
            };

            if relative == Path::new("lib.mol") || entry.extension().is_none_or(|extension| extension != "mol") {
                continue;
            }

            let path = relative
                .with_extension("")
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("::");

            modules.push((path, fs::read_to_string(&entry)?));
        }

        modules.sort();

        Ok(Self { root, modules })
    }
}

/// Files under `dir`, recursively.
fn walk(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            files.extend(walk(&path)?);
        } else {
            files.push(path);
        }
    }

    Ok(files)
}

impl fmt::Display for HostStub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.root)?;

        for (path, source) in &self.modules {
            write!(f, "\n// module `{path}`\n{source}")?;
        }

        Ok(())
    }
}

/// Names of types in a stub: paths from the host's module.
struct Names<'a> {
    tcx: &'a TyCtxt,
    adts: HashMap<AdtRef, String>,
    traits: HashMap<TraitRef, String>,
}

impl Names<'_> {
    fn of(&self, ty: TypeRef) -> String {
        let args = |args: &[TypeRef]| {
            if args.is_empty() {
                String::new()
            } else {
                format!("<{}>", args.iter().map(|&arg| self.of(arg)).collect::<Vec<_>>().join(", "))
            }
        };

        match &self.tcx.types[ty] {
            Type::Primitive(primitive) => primitive.to_string(),
            &Type::Array(element, size) => format!("{}[{}]", self.of(element), size.map_or_default(|size| size.to_string())),
            Type::Adt(adt, type_args) => {
                let name = self
                    .adts
                    .get(adt)
                    .cloned()
                    .or_else(|| self.tcx.def_registry.adt_types[*adt].name.clone())
                    .unwrap_or_default();

                format!("{name}{}", args(type_args))
            }
            Type::Trait(trait_ref, type_args) => {
                let name = self
                    .traits
                    .get(trait_ref)
                    .cloned()
                    .unwrap_or_else(|| self.tcx.def_registry.traits[*trait_ref].name.clone());

                format!("{name}{}", args(type_args))
            }
            Type::Func(params, returns) => format!(
                "func({}){}",
                params.iter().map(|&param| self.of(param)).collect::<Vec<_>>().join(", "),
                self.returns(*returns)
            ),
            Type::Generic(index) => format!("T{index}"),
            Type::Error => String::from("any"),
        }
    }

    /// ` -> R`, or nothing for `void`.
    fn returns(&self, ty: TypeRef) -> String {
        if self.tcx.types[ty] == Type::Primitive(PrimitiveType::Void) {
            String::new()
        } else {
            format!(" -> {}", self.of(ty))
        }
    }

    /// Parameters `name: T`, with `self` for a receiver.
    fn params(&self, names: &[String], types: &[TypeRef], this: bool) -> String {
        types
            .iter()
            .enumerate()
            .map(|(index, &ty)| match names.get(index) {
                _ if this && index == 0 => String::from("self"),
                Some(name) if name != "self" => format!("{name}: {}", self.of(ty)),
                _ => format!("a{index}: {}", self.of(ty)),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Whether a function is the host's (not compiled from Mollie code).
const fn is_host(body: Option<&FunctionBody>) -> bool {
    matches!(body, Some(FunctionBody::Host { .. } | FunctionBody::Import(_)))
}

/// The stub of the API registered in `compiler` by the host.
pub fn host_stub<ML: ModuleLoader>(compiler: &Compiler<ML>) -> HostStub {
    let tcx = &compiler.type_context.tcx;
    let registry = &tcx.def_registry;
    // Modules of the host, with their paths: its own, and its submodules
    // (capabilities are hidden from programs, but not from tools).
    let mut modules = vec![(ModuleId::ZERO, String::new())];
    let mut index = 0;

    while let Some((module, path)) = modules.get(index).cloned() {
        let mut children = registry.modules[module]
            .items
            .iter()
            .filter_map(|(name, &(item, ..))| match item {
                ModuleItem::SubModule(child) => Some((child, name.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();

        if module == ModuleId::ZERO {
            let mut capabilities = compiler.capabilities.iter().map(|(name, &module)| (module, name.clone())).collect::<Vec<_>>();

            capabilities.sort();
            children.extend(capabilities);
        }

        for (child, name) in children {
            let child_path = if path.is_empty() { name } else { format!("{path}::{name}") };

            modules.push((child, child_path));
        }

        index += 1;
    }

    let qualified = |path: &str, name: &str| if path.is_empty() { name.to_owned() } else { format!("{path}::{name}") };
    let mut names = Names {
        tcx,
        adts: HashMap::new(),
        traits: HashMap::new(),
    };

    for (module, path) in &modules {
        for (name, &(item, ..)) in &registry.modules[*module].items {
            match item {
                ModuleItem::Adt(adt) => {
                    names.adts.insert(adt, qualified(path, name));
                }
                ModuleItem::Trait(trait_ref) => {
                    names.traits.insert(trait_ref, qualified(path, name));
                }
                _ => (),
            }
        }
    }

    let mut sources = Vec::new();

    for (module, path) in &modules {
        let mut source = String::new();

        for (name, &(item, ..)) in &registry.modules[*module].items {
            match item {
                ModuleItem::SubModule(_) => {
                    let _ = writeln!(source, "module {name};");
                }
                ModuleItem::Func(func_ref) if is_host(compiler.type_context.functions.get(&func_ref)) => {
                    let func = &registry.functions[func_ref];

                    if let Type::Func(params, returns) = &tcx.types[func.ty] {
                        let _ = writeln!(
                            source,
                            "extern func {name}({}){};",
                            names.params(&func.arg_names, params, false),
                            names.returns(*returns)
                        );
                    }
                }
                ModuleItem::Adt(adt_ref) => {
                    let adt = &registry.adt_types[adt_ref];
                    let fields = |variant: &mollie_typing::AdtVariant| {
                        variant
                            .fields
                            .values()
                            // The discriminant of enums is hidden.
                            .filter(|field| !field.name.starts_with('<'))
                            .map(|field| format!("{}: {}", field.name, names.of(field.ty)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };

                    match adt.kind {
                        AdtKind::Enum => {
                            let variants = adt
                                .variants
                                .values()
                                .map(|variant| {
                                    let fields = fields(variant);
                                    let name = variant.name.clone().unwrap_or_default();

                                    if fields.is_empty() { name } else { format!("{name} {{ {fields} }}") }
                                })
                                .collect::<Vec<_>>()
                                .join(", ");
                            let keyword = if registry.value_types.contains(&adt_ref) {
                                "extern value enum"
                            } else {
                                "extern enum"
                            };

                            let _ = writeln!(source, "{keyword} {name} {{ {variants} }}");
                        }
                        AdtKind::Struct | AdtKind::View => {
                            let keyword = if registry.value_types.contains(&adt_ref) {
                                "extern value struct"
                            } else {
                                "extern struct"
                            };
                            let fields = adt.variants.values().next().map_or_default(fields);

                            let _ = if fields.is_empty() {
                                writeln!(source, "{keyword} {name} {{}}")
                            } else {
                                writeln!(source, "{keyword} {name} {{ {fields} }}")
                            };
                        }
                    }
                }
                ModuleItem::Trait(trait_ref) => {
                    let _ = writeln!(source, "extern trait {name} {{");

                    for function in registry.traits[trait_ref].functions.values() {
                        let params = function
                            .args
                            .iter()
                            .map(|arg| match arg.kind {
                                ArgType::This => String::from("self"),
                                ArgType::Regular => format!("{}: {}", arg.name, names.of(arg.ty)),
                            })
                            .collect::<Vec<_>>()
                            .join(", ");

                        let _ = writeln!(source, "    func {}({params}){};", function.name, names.returns(function.returns));
                    }

                    source.push_str("}\n");
                }
                _ => (),
            }
        }

        // Impls of the host go into its own module, with paths of their types.
        if *module == ModuleId::ZERO {
            // Capabilities are hidden from the module (see
            // `DefRegistry::restrict`), not from tools.
            let mut capabilities = compiler.capabilities.keys().collect::<Vec<_>>();

            capabilities.sort();

            for name in capabilities {
                let _ = writeln!(source, "module {name};");
            }

            for (impl_ref, generator) in tcx.impl_registry.impls.iter() {
                let Some(bodies) = compiler.type_context.vtables.get(&impl_ref) else {
                    continue;
                };

                if !bodies.values().any(|body| is_host(Some(body))) {
                    continue;
                }

                let header = match generator.origin_trait {
                    Some(trait_ref) => {
                        let trait_name = names.traits.get(&trait_ref).cloned().unwrap_or_else(|| registry.traits[trait_ref].name.clone());

                        format!("impl {trait_name} for {}", names.of(generator.ty))
                    }
                    None => format!("impl {}", names.of(generator.ty)),
                };

                let _ = writeln!(source, "{header} {{");

                for (vfunc, function) in generator.functions.iter() {
                    if !is_host(bodies.get(&vfunc)) {
                        continue;
                    }

                    if let Type::Func(params, returns) = &tcx.types[function.ty] {
                        // Functions of the host take the value first.
                        let this = function.arg_names.first().is_none_or(|name| name == "self") && !params.is_empty();

                        let _ = writeln!(
                            source,
                            "    extern func {}({}){};",
                            function.name,
                            names.params(&function.arg_names, params, this),
                            names.returns(*returns)
                        );
                    }
                }

                source.push_str("}\n");
            }
        }

        sources.push((path.clone(), source));
    }

    let mut sources = sources.into_iter();
    let root = sources.next().map_or_default(|(_, source)| source);

    HostStub {
        root,
        modules: sources.collect(),
    }
}
