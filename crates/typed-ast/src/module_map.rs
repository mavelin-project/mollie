//! Processing of a program's modules in passes, so that items can be used
//! regardless of the order they're declared in:
//!
//! 1. [`ModuleMap::register`]: load submodules and register names of ADTs,
//!    traits and functions.
//! 2. [`ModuleMap::process_all_imports`]: resolve imports.
//! 3. [`ModuleMap::process_all_declarations`]: fields of ADTs and functions of
//!    traits.
//! 4. [`ModuleMap::process_all_defaults`]: default values of fields.
//! 5. [`ModuleMap::process_all_signatures`]: types of functions, impl blocks
//!    and their functions' types.
//! 6. [`ModuleMap::process_all_bodies`]: type-check bodies of functions.
//! 7. [`ModuleMap::process_root`]: type-check top-level code of the root
//!    module.

use std::{
    collections::HashMap,
    fs,
    iter::once,
    path::{Path, PathBuf},
};

use indexmap::IndexMap;
use mollie_const::ConstantValue;
use mollie_index::{Idx, IndexBoxedSlice, IndexVec};
use mollie_parser::FuncModifier;
use mollie_shared::{LangItem, Operator, Positioned, Span};
use mollie_typing::{
    Adt, AdtKind, AdtRef, AdtVariant, AdtVariantField, AdtVariantRef, Arg, ArgType, Bound, Const, ConstRef, DefRegistry, DefinitionType, Diagnostic,
    DiagnosticContext, FieldRef, Func, FuncRef, ImplRef, LangItemValue, LookupType, ModuleId, ModuleItem, ModuleSpan, PrimitiveType, SpecialAdtKind, Trait,
    TraitFunc, TraitFuncRef, Type, TypeError, TypeErrorValue, TypeInfo, TypeRef, TypeSolver, UIntType, UnifyArgs, VFuncRef, VTableFunc, VTableGenerator,
};

use crate::{
    Block, BlockRef, ConstantContext, Expr, FirstPass, FromParsed, FunctionBody, IntoConstVal, Stmt, TypeLevelFromParsed, TypedAST, TypedASTContextRef,
    UsedItem,
};

pub struct ParsedModule {
    pub stmts: Vec<Positioned<mollie_parser::Stmt>>,
    pub final_stmt: Option<Positioned<mollie_parser::Stmt>>,
    /// Whether this is a stub of the host's API (see
    /// [`ParsedModule::parse_stub`]), which may declare `extern` functions.
    pub stub: bool,
}

impl ParsedModule {
    /// # Errors
    ///
    /// Returns an error if `source` has a syntax error, or is too large (see
    /// [`MAX_SOURCE_BYTES`](mollie_shared::limits::MAX_SOURCE_BYTES)).
    pub fn parse(source: &str) -> Result<Self, mollie_parser::ParseError> {
        let limit = mollie_shared::limits::MAX_SOURCE_BYTES;

        if source.len() > limit {
            return Err(mollie_parser::ParseError::new(
                format!("the module is too large (more than {limit} bytes)"),
                None,
            ));
        }

        let (stmts, final_stmt) =
            mollie_parser::parse_statements_until(&mut mollie_parser::Parser::new(&mut mollie_lexer::Lexer::lex(source)), &mollie_lexer::Token::EOF)?;

        Ok(Self {
            stmts,
            final_stmt,
            stub: false,
        })
    }

    /// Parses a stub of the host's API (written by `mollie::host::Host`):
    /// declarations of its functions (`extern func f(a: T) -> R;`), types and
    /// traits, for tools like language servers.
    ///
    /// # Errors
    ///
    /// Returns an error if `source` has a syntax error.
    pub fn parse_stub(source: &str) -> Result<Self, mollie_parser::ParseError> {
        Self::parse(source).map(|module| Self { stub: true, ..module })
    }

    const fn empty() -> Self {
        Self {
            stmts: Vec::new(),
            final_stmt: None,
            stub: false,
        }
    }
}

pub trait ModuleLoader {
    type Error;

    /// # Errors
    ///
    /// Returns an error if the module can't be loaded.
    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error>;
}

impl ModuleLoader for () {
    type Error = ();

    fn load(&mut self, _: &mut DefRegistry, _: ModuleId) -> Result<ParsedModule, Self::Error> {
        Err(())
    }
}

impl<L: ModuleLoader + ?Sized> ModuleLoader for &mut L {
    type Error = L::Error;

    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error> {
        (**self).load(registry, module)
    }
}

pub struct FileModuleLoader {
    pub current_dir: PathBuf,
}

impl ModuleLoader for FileModuleLoader {
    type Error = ();

    fn load(&mut self, registry: &mut DefRegistry, module: ModuleId) -> Result<ParsedModule, Self::Error> {
        /// Paths of modules are relative to the root module (of the program).
        fn module_path(base: &Path, registry: &DefRegistry, module: ModuleId) -> PathBuf {
            let module = &registry.modules[module];

            module
                .parent
                .map_or_else(|| base.to_path_buf(), |parent| module_path(base, registry, parent).join(&module.name))
        }

        let path = module_path(&self.current_dir, registry, module).with_extension("mol");
        // A missing module is reported by the caller.
        let source = fs::read_to_string(path).map_err(drop)?;

        ParsedModule::parse(&source).map_err(|_| ())
    }
}

/// Resolved impl block.
struct ImplInfo {
    impl_ref: ImplRef,
    /// `VFuncRef` of every function, in the order they're written in the
    /// impl block.
    functions: Vec<VFuncRef>,
}

pub struct ModuleMap<T: ModuleLoader> {
    loader: T,
    /// The first registered module: the root of the program (or library),
    /// the only one that can contain top-level code.
    root: Option<ModuleId>,
    modules: IndexMap<ModuleId, ParsedModule>,
    /// Registered constants, evaluated by `process_all_constants`.
    constants: Vec<(ConstRef, ModuleId, mollie_parser::ConstDecl, Span)>,
    /// Impl blocks resolved by the signatures pass, by module and statement
    /// index.
    impls: HashMap<(ModuleId, usize), ImplInfo>,
}

impl<T: ModuleLoader> ModuleMap<T> {
    pub fn new(loader: T) -> Self {
        Self {
            loader,
            root: None,
            modules: IndexMap::new(),
            constants: Vec::new(),
            impls: HashMap::new(),
        }
    }

    pub const fn loader_mut(&mut self) -> &mut T {
        &mut self.loader
    }

    fn module_ids(&self) -> Vec<ModuleId> {
        self.modules.keys().copied().collect()
    }

    /// Parses `data` and registers it as the module `id`, which must already
    /// exist in `registry`.
    pub fn register_from_str(&mut self, registry: &mut DefRegistry, diagnostics: &mut DiagnosticContext, data: &str, id: ModuleId) {
        self.root.get_or_insert(id);

        let module = ParsedModule::parse(data).unwrap_or_else(|error| {
            diagnostics.report(Diagnostic::new(TypeError::Parse { message: error.0 }).with_primary_span(id, error.1.unwrap_or_default()));

            ParsedModule::empty()
        });

        self.register(registry, diagnostics, module, id);
    }

    /// First pass: registers `module` as the module `id` (which must already
    /// exist in `registry`), loads its submodules and registers names of its
    /// ADTs, traits and functions.
    pub fn register(&mut self, registry: &mut DefRegistry, diagnostics: &mut DiagnosticContext, mut module: ParsedModule, id: ModuleId) {
        // Views with children implement `Container`.
        let container_impls = module
            .stmts
            .iter()
            .filter_map(|stmt| match &stmt.value {
                mollie_parser::Stmt::ViewDecl(decl) => view_container_impl(decl, stmt.span),
                _ => None,
            })
            .collect::<Vec<_>>();

        module.stmts.extend(container_impls);

        for stmt in &module.stmts {
            let registered = match &stmt.value {
                mollie_parser::Stmt::Module(module) => {
                    let submodule_id = match registry.register_module_in_module(id, &module.name.value.0, stmt.span) {
                        Ok(id) => id,
                        Err(error) => {
                            diagnostics.report(error);

                            continue;
                        }
                    };

                    let parsed = self.loader.load(registry, submodule_id).unwrap_or_else(|_| {
                        diagnostics.report(
                            Diagnostic::new(TypeError::NotFound {
                                name: module.name.value.0.clone(),
                                was_looking_for: LookupType::Module { inside: id },
                            })
                            .with_primary_span(id, stmt.span),
                        );

                        ParsedModule::empty()
                    });

                    self.register(registry, diagnostics, parsed, submodule_id);

                    continue;
                }
                mollie_parser::Stmt::StructDecl(decl) => registry
                    .register_adt_in_module(id, adt_decl(&decl.name.value, AdtKind::Struct), stmt.span)
                    .map(drop),
                mollie_parser::Stmt::EnumDecl(decl) => registry
                    .register_adt_in_module(id, adt_decl(&decl.name.value, AdtKind::Enum), stmt.span)
                    .map(drop),
                mollie_parser::Stmt::ViewDecl(decl) => registry
                    .register_adt_in_module(id, adt_decl(&decl.name.value, AdtKind::View), stmt.span)
                    .map(drop),
                mollie_parser::Stmt::TraitDecl(decl) => registry
                    .register_trait_in_module(
                        id,
                        Trait {
                            name: decl.name.value.name.value.0.clone(),
                            generics: decl.name.value.generics.len(),
                            functions: IndexVec::new(),
                        },
                        stmt.span,
                    )
                    .map(drop),
                mollie_parser::Stmt::FuncDecl(func) => registry
                    .register_func_in_module(
                        id,
                        Func {
                            name: func.name.value.0.clone(),
                            postfix: func.modifiers.iter().any(|modifier| matches!(modifier.value, FuncModifier::Postfix)),
                            generics: func.generics.len(),
                            arg_names: func.args.value.iter().map(|arg| arg.value.name.value.0.clone()).collect(),
                            // Filled by the signatures pass, once all types are known.
                            ty: TypeRef::ZERO,
                        },
                        stmt.span,
                    )
                    .map(drop),
                mollie_parser::Stmt::ConstDecl(decl) => registry
                    .register_const_in_module(
                        id,
                        Const {
                            name: decl.name.value.0.clone(),
                            // Filled once it's evaluated.
                            ty: TypeRef::ZERO,
                            value: None,
                        },
                        stmt.span,
                    )
                    .map(|constant| self.constants.push((constant, id, decl.clone(), stmt.span))),
                mollie_parser::Stmt::Impl(_) | mollie_parser::Stmt::Import(_) => Ok(()),
                mollie_parser::Stmt::Expression(_) | mollie_parser::Stmt::VariableDecl(_) => {
                    if self.root != Some(id) {
                        diagnostics.report(Diagnostic::new(TypeError::TopLevelCode).with_primary_span(id, stmt.span));
                    }

                    Ok(())
                }
            };

            if let Err(error) = registered {
                diagnostics.report(error);
            }
        }

        if self.root != Some(id)
            && let Some(stmt) = &module.final_stmt
        {
            diagnostics.report(Diagnostic::new(TypeError::TopLevelCode).with_primary_span(id, stmt.span));
        }

        self.modules.insert(id, module);
    }

    /// Second pass: resolves imports of all modules.
    pub fn process_all_imports(&self, registry: &mut DefRegistry, diagnostics: &mut DiagnosticContext) {
        let ids = self.module_ids();

        // An import can refer to an item that another module imports itself, so
        // keep resolving until nothing changes, and only then report
        // what is still unresolved.
        loop {
            let mut progress = false;

            for &id in &ids {
                progress |= self.process_imports_pass(registry, None, id);
            }

            if !progress {
                break;
            }
        }

        for &id in &ids {
            self.process_imports_pass(registry, Some(&mut *diagnostics), id);
        }
    }

    /// Resolves imports of the module `id` and returns `true` if any new item
    /// was imported. Imports that can't be resolved are skipped, and reported
    /// only if `diagnostics` is provided.
    fn process_imports_pass(&self, registry: &mut DefRegistry, mut diagnostics: Option<&mut DiagnosticContext>, id: ModuleId) -> bool {
        /// Inserts an imported item into the module `id`. Returns `Ok(false)`
        /// if this exact import was already done by a previous pass.
        fn import_item(registry: &mut DefRegistry, id: ModuleId, name: &str, item: ModuleItem, span: Span) -> Result<bool, Diagnostic> {
            let span = ModuleSpan(id, span);

            match registry.modules[id].items.get(name) {
                Some(&(existing, DefinitionType::Import, existing_span)) if existing == item && existing_span == span => Ok(false),
                Some(&(_, def_type, one)) => {
                    let [primary, secondary] = if one.0 == id && one.1.range.end_line > span.1.range.end_line {
                        [(span, DefinitionType::Import), (one, def_type)]
                    } else {
                        [(one, def_type), (span, DefinitionType::Import)]
                    };

                    Err(Diagnostic::new(TypeError::AlreadyExists {
                        name: name.to_owned(),
                        primary: primary.1,
                        secondary: secondary.1,
                    })
                    .with_primary_span(primary.0.0, primary.0.1)
                    .with_secondary_span(secondary.0.0, secondary.0.1))
                }
                None => {
                    registry.modules[id].items.insert(name.to_owned(), (item, DefinitionType::Import, span));

                    Ok(true)
                }
            }
        }

        let mut progress = false;
        let mut report = |error: Diagnostic| {
            if let Some(diagnostics) = diagnostics.as_deref_mut() {
                diagnostics.report(error);
            }
        };

        for stmt in &self.modules[&id].stmts {
            let mollie_parser::Stmt::Import(import) = &stmt.value else {
                continue;
            };

            let Some(item) = resolve_path(registry, id, &import.path.value) else {
                let first = import.path.value.segments.first().map(|segment| segment.value.name.value.0.as_str());
                let error = match first {
                    Some(name) if registry.is_restricted(id, name) => TypeError::Unavailable { name: name.to_owned() },
                    _ => TypeError::NotFound {
                        name: import.path.value.to_string(),
                        was_looking_for: LookupType::Module { inside: id },
                    },
                };

                report(Diagnostic::new(error).with_primary_span(id, import.path.span));

                continue;
            };

            match &import.kind {
                mollie_parser::ImportKind::Partial(items) => {
                    let ModuleItem::SubModule(module) = item else {
                        report(
                            Diagnostic::new(TypeError::Unexpected {
                                expected: TypeErrorValue::Module,
                                found: item_kind(registry, item),
                            })
                            .with_primary_span(id, import.path.span),
                        );

                        continue;
                    };

                    for subitem in &items.value {
                        let Some(item) = registry.modules[module].get_item(&subitem.value) else {
                            report(
                                Diagnostic::new(TypeError::NotFound {
                                    name: subitem.value.0.clone(),
                                    was_looking_for: LookupType::Type { inside: module },
                                })
                                .with_primary_span(id, subitem.span),
                            );

                            continue;
                        };

                        match import_item(registry, id, &subitem.value.0, item, subitem.span) {
                            Ok(inserted) => progress |= inserted,
                            Err(error) => report(error),
                        }
                    }
                }
                mollie_parser::ImportKind::Named => {
                    let name = match item {
                        ModuleItem::SubModule(module_id) => registry.modules[module_id].name.clone(),
                        _ => import.path.value.segments.last().map_or_default(|segment| segment.value.name.value.0.clone()),
                    };

                    match import_item(registry, id, &name, item, import.path.span) {
                        Ok(inserted) => progress |= inserted,
                        Err(error) => report(error),
                    }
                }
            }
        }

        progress
    }

    /// Third pass: fields of ADTs and functions of traits.
    pub fn process_all_declarations(&self, context: &mut TypedASTContextRef<'_>) {
        for id in self.module_ids() {
            self.process_declarations(context, id);
        }

        check_value_types(context);
    }

    fn process_declarations(&self, context: &mut TypedASTContextRef<'_>, id: ModuleId) {
        for stmt in &self.modules[&id].stmts {
            match &stmt.value {
                mollie_parser::Stmt::StructDecl(decl) => {
                    let Some(ModuleItem::Adt(adt_ref)) = local_item(&context.type_solver.context.def_registry, id, &decl.name.value.name.value.0, stmt.span)
                    else {
                        continue;
                    };

                    if decl.value {
                        context.type_solver.context.def_registry.value_types.insert(adt_ref);
                    }

                    if let Some(item) = lang_item(&decl.attributes) {
                        context
                            .type_solver
                            .context
                            .def_registry
                            .language_items
                            .insert(item, LangItemValue::Adt(adt_ref));
                    }

                    push_generics(context, &decl.name.value.generics, 0);

                    let fields = lower_fields(context, id, decl.properties.value.iter().map(|prop| (&prop.value.name.value, &prop.value.ty)));

                    pop_generics(context, &decl.name.value.generics);

                    context.type_solver.context.def_registry.adt_types[adt_ref].variants = IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter(fields),
                    }]);
                }
                mollie_parser::Stmt::ViewDecl(decl) => {
                    let Some(ModuleItem::Adt(adt_ref)) = local_item(&context.type_solver.context.def_registry, id, &decl.name.value.name.value.0, stmt.span)
                    else {
                        continue;
                    };

                    push_generics(context, &decl.name.value.generics, 0);

                    let fields = lower_fields(context, id, decl.properties.iter().map(|prop| (&prop.value.name.value, &prop.value.ty)));

                    pop_generics(context, &decl.name.value.generics);

                    context.type_solver.context.def_registry.adt_types[adt_ref].variants = IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter(fields),
                    }]);
                }
                mollie_parser::Stmt::EnumDecl(decl) => {
                    let Some(ModuleItem::Adt(adt_ref)) = local_item(&context.type_solver.context.def_registry, id, &decl.name.value.name.value.0, stmt.span)
                    else {
                        continue;
                    };

                    if decl.value {
                        context.type_solver.context.def_registry.value_types.insert(adt_ref);
                    }

                    if let Some(item) = lang_item(&decl.attributes) {
                        context
                            .type_solver
                            .context
                            .def_registry
                            .language_items
                            .insert(item, LangItemValue::Adt(adt_ref));
                    }

                    push_generics(context, &decl.name.value.generics, 0);

                    let usize = context
                        .type_solver
                        .context
                        .types
                        .get_or_add(Type::Primitive(PrimitiveType::UInt(UIntType::USize)));
                    let mut variants = Vec::with_capacity(decl.variants.value.len());

                    for (discriminant, variant) in decl.variants.value.iter().enumerate() {
                        // Field 0 of every variant is the discriminant.
                        let mut fields = vec![AdtVariantField {
                            name: String::from("<discriminant>"),
                            ty: usize,
                            default_value: None,
                        }];

                        if let Some(properties) = &variant.value.properties {
                            fields.extend(lower_fields(
                                context,
                                id,
                                properties.value.iter().map(|prop| (&prop.value.name.value, &prop.value.ty)),
                            ));
                        }

                        if let Some(item) = lang_item(&variant.value.attributes) {
                            context
                                .type_solver
                                .context
                                .def_registry
                                .language_items
                                .insert(item, LangItemValue::AdtVariant(adt_ref, AdtVariantRef::new(discriminant)));
                        }

                        variants.push(AdtVariant {
                            name: Some(variant.value.name.value.0.clone()),
                            discriminant,
                            fields: IndexBoxedSlice::from_iter(fields),
                        });
                    }

                    pop_generics(context, &decl.name.value.generics);

                    context.type_solver.context.def_registry.adt_types[adt_ref].variants = IndexBoxedSlice::from_iter(variants);
                }
                mollie_parser::Stmt::TraitDecl(decl) => {
                    let Some(ModuleItem::Trait(trait_ref)) =
                        local_item(&context.type_solver.context.def_registry, id, &decl.name.value.name.value.0, stmt.span)
                    else {
                        continue;
                    };

                    if let Some(item) = lang_item(&decl.attributes) {
                        context
                            .type_solver
                            .context
                            .def_registry
                            .language_items
                            .insert(item, LangItemValue::Trait(trait_ref));
                    }

                    // Generic 0 is `Self`, trait's own generics start from 1.
                    let this = push_self_generic(context);

                    push_generics(context, &decl.name.value.generics, 1);

                    let void = context.type_solver.context.types.core_types.void;
                    let mut functions = IndexVec::with_capacity(decl.functions.value.len());

                    for (index, function) in decl.functions.value.iter().enumerate() {
                        if let Some(item) = lang_item(&function.value.attributes) {
                            context
                                .type_solver
                                .context
                                .def_registry
                                .language_items
                                .insert(item, LangItemValue::TraitFunc(trait_ref, TraitFuncRef::new(index)));
                        }

                        let mut args = Vec::with_capacity(function.value.args.len() + usize::from(function.value.this.is_some()));

                        if function.value.this.is_some() {
                            args.push(Arg {
                                name: String::from("self"),
                                kind: ArgType::This,
                                ty: this,
                            });
                        }

                        for arg in &function.value.args {
                            let ty = Type::from_parsed(arg.value.ty.value.clone(), id, context, arg.value.ty.span);

                            args.push(Arg {
                                name: arg.value.name.value.0.clone(),
                                kind: ArgType::Regular,
                                ty,
                            });
                        }

                        let returns = function
                            .value
                            .returns
                            .as_ref()
                            .map_or(void, |returns| Type::from_parsed(returns.value.clone(), id, context, returns.span));

                        // A default implementation is a function whose generic
                        // 0 is `Self`, then come
                        // generics of the trait. Its body
                        // is checked with the other bodies.
                        let default = function.value.body.is_some().then(|| {
                            let ty = context
                                .type_solver
                                .context
                                .types
                                .get_or_add(Type::Func(args.iter().map(|arg| arg.ty).collect(), returns));

                            context.type_solver.context.def_registry.functions.insert(Func {
                                postfix: false,
                                name: format!("{}::{}", decl.name.value.name.value.0, function.value.name.value.0),
                                generics: decl.name.value.generics.len() + 1,
                                arg_names: args.iter().map(|arg| arg.name.clone()).collect(),
                                ty,
                            })
                        });

                        functions.push(TraitFunc {
                            name: function.value.name.value.0.clone(),
                            args: args.into_boxed_slice(),
                            returns,
                            default,
                        });
                    }

                    pop_generics(context, &decl.name.value.generics);
                    pop_self_generic(context);

                    context.type_solver.context.def_registry.traits[trait_ref].functions = functions;
                }
                _ => (),
            }
        }
    }

    /// Makes constants available to code: they're evaluated when they're first
    /// used (see [`evaluate_constant`]). Runs after declarations, so constants
    /// can use any ADT.
    pub fn register_constants(&mut self, context: &mut TypedASTContextRef<'_>) {
        for (constant, module, decl, span) in self.constants.drain(..) {
            context.constants.add(constant, module, decl, span);
        }
    }

    /// Evaluates constants that aren't used by default values or other
    /// constants.
    pub fn process_all_constants(&self, context: &mut TypedASTContextRef<'_>) {
        for constant in context.constants.ids() {
            evaluate_constant(context, constant);
        }
    }

    /// Fourth pass: evaluates default values of ADT fields. Runs after all
    /// declarations, so default values can use any ADT.
    pub fn process_all_defaults(&self, context: &mut TypedASTContextRef<'_>) {
        for id in self.module_ids() {
            self.process_defaults(context, id);
        }
    }

    fn process_defaults(&self, context: &mut TypedASTContextRef<'_>, id: ModuleId) {
        for stmt in &self.modules[&id].stmts {
            // Default values of every variant's properties, with the index of
            // the variant's first property (fields of enum variants
            // start with the discriminant).
            let (name, variants) = match &stmt.value {
                mollie_parser::Stmt::StructDecl(decl) => (&decl.name.value, vec![(
                    AdtVariantRef::ZERO,
                    0,
                    decl.properties.value.iter().map(|prop| prop.value.default_value.as_ref()).collect(),
                )]),
                mollie_parser::Stmt::ViewDecl(decl) => (&decl.name.value, vec![(
                    AdtVariantRef::ZERO,
                    0,
                    decl.properties.iter().map(|prop| prop.value.default_value.as_ref()).collect(),
                )]),
                mollie_parser::Stmt::EnumDecl(decl) => (
                    &decl.name.value,
                    decl.variants
                        .value
                        .iter()
                        .enumerate()
                        .map(|(index, variant)| {
                            let defaults = variant.value.properties.as_ref().map_or_else(Vec::new, |properties| {
                                properties.value.iter().map(|prop| prop.value.default_value.as_ref()).collect()
                            });

                            (AdtVariantRef::new(index), 1, defaults)
                        })
                        .collect(),
                ),
                _ => continue,
            };

            if variants.iter().all(|(.., defaults)| defaults.iter().all(Option::is_none)) {
                continue;
            }

            let Some(ModuleItem::Adt(adt_ref)) = local_item(&context.type_solver.context.def_registry, id, &name.name.value.0, stmt.span) else {
                continue;
            };

            push_generics(context, &name.generics, 0);

            for (variant, first_field, defaults) in variants {
                for (index, default_value) in defaults.into_iter().enumerate() {
                    let Some(default_value) = default_value else {
                        continue;
                    };

                    let field = FieldRef::new(first_field + index);
                    let ty = context.type_solver.context.def_registry.adt_types[adt_ref].variants[variant].fields[field].ty;
                    let value = evaluate_default_value(context, id, ty, default_value);

                    context.type_solver.context.def_registry.adt_types[adt_ref].variants[variant].fields[field].default_value = value;
                }
            }

            pop_generics(context, &name.generics);
        }
    }

    /// Fifth pass: types of functions, impl blocks and their functions'
    /// types.
    pub fn process_all_signatures(&mut self, context: &mut TypedASTContextRef<'_>) {
        for id in self.module_ids() {
            self.process_signatures(context, id);
        }
    }

    fn process_signatures(&mut self, context: &mut TypedASTContextRef<'_>, id: ModuleId) {
        let Self { modules, impls, .. } = self;

        for (stmt_index, stmt) in modules[&id].stmts.iter().enumerate() {
            match &stmt.value {
                mollie_parser::Stmt::FuncDecl(func_decl) => {
                    let Some(ModuleItem::Func(func_ref)) = local_item(&context.type_solver.context.def_registry, id, &func_decl.name.value.0, stmt.span) else {
                        continue;
                    };

                    push_generics(context, &func_decl.generics, 0);

                    let bounds = lower_bounds(context, id, &func_decl.generics, 0);

                    if !bounds.is_empty() {
                        context.type_solver.context.def_registry.func_bounds.insert(func_ref, bounds);
                    }

                    let args: Box<[_]> = func_decl
                        .args
                        .value
                        .iter()
                        .map(|arg| Type::from_parsed(arg.value.ty.value.clone(), id, context, arg.value.ty.span))
                        .collect();

                    let returns = match &func_decl.returns {
                        Some(returns) => Type::from_parsed(returns.value.clone(), id, context, returns.span),
                        None => context.type_solver.context.types.core_types.void,
                    };

                    pop_generics(context, &func_decl.generics);

                    if let Some(defaults) = register_defaults(context, &func_decl.name.value.0, &func_decl.args.value, &args, None, func_decl.generics.len()) {
                        context.type_solver.context.def_registry.func_defaults.insert(func_ref, defaults);
                    }

                    let ty = context.type_solver.context.types.get_or_add(Type::Func(args, returns));

                    context.type_solver.context.def_registry.functions[func_ref].ty = ty;
                }
                mollie_parser::Stmt::Impl(implementation) => {
                    if let Some(info) = process_impl_signature(context, id, stmt.span, implementation) {
                        impls.insert((id, stmt_index), info);
                    }
                }
                _ => (),
            }
        }
    }

    /// Sixth pass: type-checks bodies of functions.
    pub fn process_all_bodies(&self, context: &mut TypedASTContextRef<'_>) {
        for id in self.module_ids() {
            self.process_bodies(context, id);
        }
    }

    fn process_bodies(&self, context: &mut TypedASTContextRef<'_>, id: ModuleId) {
        for (stmt_index, stmt) in self.modules[&id].stmts.iter().enumerate() {
            match &stmt.value {
                mollie_parser::Stmt::FuncDecl(func_decl) => {
                    let Some(ModuleItem::Func(func_ref)) = local_item(&context.type_solver.context.def_registry, id, &func_decl.name.value.0, stmt.span) else {
                        continue;
                    };

                    // Declared by the host: there's no body to check.
                    if func_decl.external {
                        if !self.modules[&id].stub {
                            context.type_solver.error(TypeError::ExternOutsideStub, ModuleSpan(id, func_decl.name.span));
                        }

                        context.functions.insert(func_ref, FunctionBody::Stub);

                        continue;
                    }

                    let func = &context.type_solver.context.def_registry.functions[func_ref];
                    let (arg_names, func_ty) = (func.arg_names.clone(), func.ty);

                    // Generic parameters are rigid inside the body.
                    push_generics(context, &func_decl.generics, 0);

                    let bounds = context
                        .type_solver
                        .context
                        .def_registry
                        .func_bounds
                        .get(&func_ref)
                        .map_or_default(|bounds| bounds.to_vec());
                    let arg_spans = func_decl.args.value.iter().map(|arg| arg.value.name.span).collect::<Vec<_>>();
                    let body = check_function_body(context, id, &arg_names, &arg_spans, func_ty, &func_decl.body, None, bounds.clone());

                    context.functions.insert(func_ref, body);

                    let defaults = context
                        .type_solver
                        .context
                        .def_registry
                        .func_defaults
                        .get(&func_ref)
                        .cloned()
                        .unwrap_or_default();

                    for (index, (arg, default)) in func_decl.args.value.iter().zip(defaults).enumerate() {
                        if let (Some(value), Some(default)) = (&arg.value.default, default) {
                            let func = &context.type_solver.context.def_registry.functions[default];
                            let (arg_names, func_ty) = (func.arg_names.clone(), func.ty);
                            let body = check_function_body(
                                context,
                                id,
                                &arg_names,
                                &arg_spans[..index],
                                func_ty,
                                &default_body(value),
                                None,
                                bounds.clone(),
                            );

                            context.functions.insert(default, body);
                        }
                    }

                    pop_generics(context, &func_decl.generics);
                }
                mollie_parser::Stmt::TraitDecl(decl) => {
                    let Some(ModuleItem::Trait(trait_ref)) =
                        local_item(&context.type_solver.context.def_registry, id, &decl.name.value.name.value.0, stmt.span)
                    else {
                        continue;
                    };

                    // `Self` implements the trait, so its functions can be
                    // called on `self`.
                    let trait_args = (1..=decl.name.value.generics.len())
                        .map(|index| context.type_solver.context.types.get_or_add(Type::Generic(index)))
                        .collect();
                    let bounds = vec![Bound {
                        generic: 0,
                        trait_ref,
                        trait_args,
                    }];

                    push_self_generic(context);
                    push_generics(context, &decl.name.value.generics, 1);

                    for (func, function) in decl.functions.value.iter().enumerate() {
                        let Some(body) = &function.value.body else {
                            continue;
                        };

                        // A trait declared twice has the functions of its last
                        // declaration.
                        let Some(default) = context.type_solver.context.def_registry.traits[trait_ref]
                            .functions
                            .get(TraitFuncRef::new(func))
                            .and_then(|func| func.default)
                        else {
                            continue;
                        };

                        let func = &context.type_solver.context.def_registry.functions[default];
                        let (arg_names, func_ty) = (func.arg_names.clone(), func.ty);
                        let arg_spans = function
                            .value
                            .this
                            .iter()
                            .map(|this| this.span)
                            .chain(function.value.args.iter().map(|arg| arg.value.name.span))
                            .collect::<Vec<_>>();
                        let body = check_function_body(context, id, &arg_names, &arg_spans, func_ty, body, None, bounds.clone());

                        context.functions.insert(default, body);
                    }

                    pop_generics(context, &decl.name.value.generics);
                    pop_self_generic(context);
                }
                mollie_parser::Stmt::Impl(implementation) => {
                    let Some(info) = self.impls.get(&(id, stmt_index)) else {
                        continue;
                    };

                    for (function, &vfunc) in implementation.functions.value.iter().zip(&info.functions) {
                        if function.value.external {
                            if !self.modules[&id].stub {
                                context
                                    .type_solver
                                    .error(TypeError::ExternOutsideStub, ModuleSpan(id, function.value.name.span));
                            }

                            context.vtables.entry(info.impl_ref).or_default().insert(vfunc, FunctionBody::Stub);

                            continue;
                        }

                        let func = &context.type_solver.context.impl_registry.impls[info.impl_ref].functions[vfunc];
                        let (arg_names, func_ty) = (func.arg_names.clone(), func.ty);
                        // Bounds of the impl, then of the function's own
                        // generics.
                        let registry = &context.type_solver.context.impl_registry;
                        let bounds = registry
                            .impls
                            .get(info.impl_ref)
                            .map(|generator| &generator.bounds)
                            .into_iter()
                            .chain(registry.method_bounds.get(&(info.impl_ref, vfunc)))
                            .flat_map(|bounds| bounds.iter().cloned())
                            .collect::<Vec<_>>();
                        let arg_spans = function
                            .value
                            .this
                            .iter()
                            .map(|this| this.span)
                            .chain(function.value.args.iter().map(|arg| arg.value.name.span))
                            .collect::<Vec<_>>();
                        let body = check_function_body(
                            context,
                            id,
                            &arg_names,
                            &arg_spans,
                            func_ty,
                            &function.value.body,
                            Some((info.impl_ref, &implementation.generics, &function.value.generics, function.value.mut_self)),
                            bounds.clone(),
                        );

                        context.vtables.entry(info.impl_ref).or_default().insert(vfunc, body);

                        let defaults = context
                            .type_solver
                            .context
                            .impl_registry
                            .method_defaults
                            .get(&(info.impl_ref, vfunc))
                            .cloned()
                            .unwrap_or_default();

                        for (index, (arg, default)) in function.value.args.iter().zip(defaults).enumerate() {
                            if let (Some(value), Some(default)) = (&arg.value.default, default) {
                                let func = &context.type_solver.context.def_registry.functions[default];
                                let (arg_names, func_ty) = (func.arg_names.clone(), func.ty);
                                // `self` and the parameters before this one.
                                let earlier = index + usize::from(function.value.this.is_some());
                                let body = check_function_body(
                                    context,
                                    id,
                                    &arg_names,
                                    &arg_spans[..earlier],
                                    func_ty,
                                    &default_body(value),
                                    Some((info.impl_ref, &implementation.generics, &function.value.generics, false)),
                                    bounds.clone(),
                                );

                                context.functions.insert(default, body);
                            }
                        }
                    }
                }
                _ => (),
            }
        }
    }

    /// Last pass: type-checks top-level code of the root module, which must
    /// evaluate to `returns`.
    pub fn process_root(&self, context: &mut TypedASTContextRef<'_>, returns: TypeRef) -> (TypedAST, BlockRef) {
        let root = self.root.unwrap_or(ModuleId::ZERO);
        let mut ast = TypedAST::<FirstPass> {
            module: root,
            ..TypedAST::default()
        };

        let mut stmts = Vec::new();
        let mut final_expr = None;

        // Top-level code is the body of the program's function.
        let returns_info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, returns, &[]);

        context.returns = Some(returns_info);

        if let Some(module) = self.modules.get(&root) {
            for stmt in &module.stmts {
                if !is_declaration(&stmt.value)
                    && let Some(stmt) = Stmt::from_parsed(stmt.value.clone(), &mut ast, context, stmt.span)
                {
                    stmts.push(stmt);
                }
            }

            if let Some(Positioned {
                value: mollie_parser::Stmt::Expression(expr),
                span,
            }) = &module.final_stmt
            {
                // The value of the program is expected to be of its type.
                final_expr = Some(Expr::from_parsed_expecting(expr.clone(), returns_info, &mut ast, context, *span));
            }
        }

        let ty = match final_expr {
            Some(expr) => ast[expr].ty,
            None => context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), None),
        };

        let block = ast.add_block(
            Block {
                stmts: stmts.into_boxed_slice(),
                expr: final_expr,
            },
            ty,
            Span::default(),
        );

        ast.solve(block, context, returns)
    }
}

/// `impl<T...> std::container::Container<C> for View<T...>` for a view with a
/// `children` property of type `C`, reading and writing the property.
fn view_container_impl(decl: &mollie_parser::ViewDecl, span: Span) -> Option<Positioned<mollie_parser::Stmt>> {
    use mollie_parser::{
        Argument, BinaryExpr, BlockExpr, Expr as ParsedExpr, Ident, Impl, ImplFunction, IndexExpr, IndexTarget, Stmt as ParsedStmt, Type as ParsedType,
        TypeArgs, TypePathExpr, TypePathSegment,
    };

    let children = decl.properties.iter().find(|property| property.value.name.value.0 == "children")?;
    let children_ty = children.value.ty.clone();
    let segment = |name: Positioned<Ident>, args: Vec<Positioned<ParsedType>>| {
        span.wrap(TypePathSegment {
            name,
            args: (!args.is_empty()).then(|| span.wrap(TypeArgs(args))),
        })
    };
    let ident = |name: &str| span.wrap(Ident::new(name));
    let trait_name = span.wrap(TypePathExpr {
        segments: vec![
            segment(ident("std"), Vec::new()),
            segment(ident("container"), Vec::new()),
            segment(ident("Container"), vec![children_ty.clone()]),
        ],
    });
    let generics = decl
        .name
        .value
        .generics
        .iter()
        .map(|generic| {
            generic.span.wrap(ParsedType::Path(TypePathExpr {
                segments: vec![segment(generic.clone(), Vec::new())],
            }))
        })
        .collect();
    let target = span.wrap(ParsedType::Path(TypePathExpr {
        segments: vec![segment(decl.name.value.name.clone(), generics)],
    }));
    let self_children = || {
        span.wrap(ParsedExpr::Index(IndexExpr {
            target: Box::new(span.wrap(ParsedExpr::This)),
            index: span.wrap(IndexTarget::Named(Ident::new("children"))),
        }))
    };
    let getter = ImplFunction {
        name: ident("children"),
        generics: Vec::new(),
        this: Some(span.wrap(())),
        mut_self: false,
        external: false,
        args: Vec::new(),
        returns: Some(children_ty.clone()),
        body: span.wrap(BlockExpr {
            stmts: Vec::new(),
            final_stmt: Some(Box::new(span.wrap(ParsedStmt::Expression(self_children().value)))),
        }),
    };
    let setter = ImplFunction {
        name: ident("set_children"),
        generics: Vec::new(),
        this: Some(span.wrap(())),
        mut_self: false,
        external: false,
        args: vec![span.wrap(Argument {
            name: ident("children"),
            ty: children_ty,
            default: None,
        })],
        returns: None,
        body: span.wrap(BlockExpr {
            stmts: vec![span.wrap(ParsedStmt::Expression(ParsedExpr::Binary(BinaryExpr {
                lhs: Box::new(self_children()),
                rhs: Box::new(span.wrap(ParsedExpr::Ident(Ident::new("children")))),
                operator: span.wrap(Operator::Assign),
            })))],
            final_stmt: None,
        }),
    };

    Some(
        span.wrap(ParsedStmt::Impl(Impl {
            generics: decl
                .name
                .value
                .generics
                .iter()
                .map(|name| {
                    name.span.wrap(mollie_parser::GenericParam {
                        name: name.clone(),
                        bounds: Vec::new(),
                    })
                })
                .collect(),
            trait_name: Some(trait_name),
            target,
            functions: span.wrap(vec![span.wrap(getter), span.wrap(setter)]),
        })),
    )
}

const fn is_declaration(stmt: &mollie_parser::Stmt) -> bool {
    !matches!(stmt, mollie_parser::Stmt::Expression(_) | mollie_parser::Stmt::VariableDecl(_))
}

fn adt_decl(name: &mollie_parser::NameWithGenerics, kind: AdtKind) -> Adt {
    Adt {
        name: Some(name.name.value.0.clone()),
        collectable: true,
        kind,
        generics: name.generics.len(),
        // Filled by the declarations pass.
        variants: IndexBoxedSlice::default(),
    }
}

/// Returns the item `name` of `module` only if it was declared by the
/// statement at `span`. A duplicate declaration isn't registered (that's
/// reported by the first pass), so it must be skipped by later passes.
fn local_item(registry: &DefRegistry, module: ModuleId, name: &str, span: Span) -> Option<ModuleItem> {
    match registry.modules[module].items.get(name) {
        Some(&(item, DefinitionType::Local, item_span)) if item_span == ModuleSpan(module, span) => Some(item),
        _ => None,
    }
}

/// Resolves a path like `super::module::Item`, relative to the module `id`.
fn resolve_path(registry: &DefRegistry, id: ModuleId, path: &mollie_parser::TypePathExpr) -> Option<ModuleItem> {
    resolve_item_path(registry, id, path).ok()
}

/// Resolves a path like `super::module::Item`, relative to the module `id`.
/// On failure, returns the error along with the span of the segment that
/// caused it.
pub fn resolve_item_path(registry: &DefRegistry, id: ModuleId, path: &mollie_parser::TypePathExpr) -> Result<ModuleItem, (TypeError, Span)> {
    let mut current = ModuleItem::SubModule(id);
    let mut current_span = Span::default();

    for (index, segment) in path.segments.iter().enumerate() {
        let ModuleItem::SubModule(module) = current else {
            return Err((
                TypeError::Unexpected {
                    expected: TypeErrorValue::Module,
                    found: item_kind(registry, current),
                },
                current_span,
            ));
        };

        let name = &segment.value.name.value.0;

        current = if name == "super" {
            let parent = registry.modules[module].parent.ok_or_else(|| {
                (
                    TypeError::NotFound {
                        name: name.clone(),
                        was_looking_for: LookupType::Module { inside: module },
                    },
                    segment.span,
                )
            })?;

            ModuleItem::SubModule(parent)
        } else {
            // The first segment is looked up like a name in code of the
            // module, the next ones are items of the previous module.
            let item = if index == 0 {
                registry.lookup(module, name)
            } else {
                registry.modules[module].get_item(name)
            };

            item.ok_or_else(|| {
                (
                    missing_item(registry, name.clone(), LookupType::Type { inside: module }, index == 0),
                    segment.span,
                )
            })?
        };

        current_span = segment.span;
    }

    Ok(current)
}

/// The error for `name` not found while looking for `was_looking_for`: a
/// module of the host the program can't use (if `first` in its path) isn't
/// missing, it's unavailable.
pub fn missing_item(registry: &DefRegistry, name: String, was_looking_for: LookupType, first: bool) -> TypeError {
    let inside = match was_looking_for {
        LookupType::Type { inside } | LookupType::Module { inside } => Some(inside),
        LookupType::Variable => None,
    };

    if first && inside.is_some_and(|inside| registry.is_restricted(inside, &name)) {
        TypeError::Unavailable { name }
    } else {
        TypeError::NotFound { name, was_looking_for }
    }
}

fn item_kind(registry: &DefRegistry, item: ModuleItem) -> TypeErrorValue {
    match item {
        ModuleItem::SubModule(_) => TypeErrorValue::Module,
        ModuleItem::Adt(adt_ref) => TypeErrorValue::Adt(SpecialAdtKind::Specific(registry.adt_types[adt_ref].kind)),
        ModuleItem::Trait(_) => TypeErrorValue::Trait,
        ModuleItem::Func(_) | ModuleItem::Intrinsic(..) => TypeErrorValue::Function,
        ModuleItem::Const(_) => TypeErrorValue::Value,
    }
}

fn lang_item(attributes: &[Positioned<mollie_parser::Attribute>]) -> Option<LangItem> {
    attributes
        .iter()
        .find_map(|attribute| match attribute.value.value.as_ref().map(|value| &value.value) {
            Some(&mollie_parser::AttributeValue::LangItem(item)) => Some(item),
            _ => None,
        })
}

/// A generic parameter in a declaration: a name (of an ADT or a trait), or a
/// name with bounds (of a function or an impl).
trait GenericName {
    fn generic_name(&self) -> &Positioned<mollie_parser::Ident>;
}

impl GenericName for Positioned<mollie_parser::Ident> {
    fn generic_name(&self) -> &Positioned<mollie_parser::Ident> {
        self
    }
}

impl GenericName for Positioned<mollie_parser::GenericParam> {
    fn generic_name(&self) -> &Positioned<mollie_parser::Ident> {
        &self.value.name
    }
}

fn push_generics<G: GenericName>(context: &mut TypedASTContextRef<'_>, names: &[G], offset: usize) {
    for (index, name) in names.iter().enumerate() {
        let name = name.generic_name();
        let index = index + offset;
        let ty_info = context.type_solver.add_info(TypeInfo::Generic(index), Some(name.span));
        let ty = context.type_solver.context.types.get_or_add(Type::Generic(index));

        context.type_solver.available_generics.insert(name.value.0.clone(), (ty_info, ty));
    }
}

fn pop_generics<G: GenericName>(context: &mut TypedASTContextRef<'_>, names: &[G]) {
    for name in names {
        context.type_solver.available_generics.shift_remove(&name.generic_name().value.0);
    }
}

/// Bounds of generic parameters (`T: Shape + Named`), whose indices start
/// from `offset`. The generics must be available.
fn lower_bounds(context: &mut TypedASTContextRef<'_>, module: ModuleId, params: &[Positioned<mollie_parser::GenericParam>], offset: usize) -> Box<[Bound]> {
    let mut bounds = Vec::new();

    for (index, param) in params.iter().enumerate() {
        for bound in &param.value.bounds {
            let trait_ref = match resolve_item_path(&context.type_solver.context.def_registry, module, &bound.value) {
                Ok(ModuleItem::Trait(trait_ref)) => trait_ref,
                Ok(item) => {
                    let found = item_kind(&context.type_solver.context.def_registry, item);

                    context.type_solver.error(
                        TypeError::Unexpected {
                            expected: TypeErrorValue::Trait,
                            found,
                        },
                        ModuleSpan(module, bound.span),
                    );

                    continue;
                }
                Err((error, span)) => {
                    context.type_solver.error(error, ModuleSpan(module, span));

                    continue;
                }
            };

            let trait_args = bound
                .value
                .segments
                .last()
                .and_then(|segment| segment.value.args.clone())
                .map_or_default(|args| args.value.0)
                .into_iter()
                .map(|arg| Type::from_parsed(arg.value, module, context, arg.span))
                .collect();

            bounds.push(Bound {
                generic: offset + index,
                trait_ref,
                trait_args,
            });
        }
    }

    bounds.into_boxed_slice()
}

/// Makes `Self` available as generic 0 and returns its type.
fn push_self_generic(context: &mut TypedASTContextRef<'_>) -> TypeRef {
    let ty_info = context.type_solver.add_info(TypeInfo::Generic(0), None);
    let ty = context.type_solver.context.types.get_or_add(Type::Generic(0));

    context.type_solver.available_generics.insert(String::from("Self"), (ty_info, ty));

    ty
}

/// Makes `Self` name the target type `ty` of an impl block.
fn push_self_type(context: &mut TypedASTContextRef<'_>, ty: TypeRef) {
    let ty_info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[]);

    context.type_solver.available_generics.insert(String::from("Self"), (ty_info, ty));
}

fn pop_self_generic(context: &mut TypedASTContextRef<'_>) {
    context.type_solver.available_generics.shift_remove("Self");
}

/// Makes generics of an impl block available: for trait impls generic 0 is
/// `Self`, then come the impl's own generics.
fn push_impl_generics<G: GenericName>(context: &mut TypedASTContextRef<'_>, generics: &[G], is_trait_impl: bool) {
    if is_trait_impl {
        push_self_generic(context);
    }

    push_generics(context, generics, usize::from(is_trait_impl));
}

fn pop_impl_generics<G: GenericName>(context: &mut TypedASTContextRef<'_>, generics: &[G]) {
    pop_generics(context, generics);
    pop_self_generic(context);
}

/// Lowers fields of an ADT. Default values are evaluated later, by
/// [`ModuleMap::process_all_defaults`].
fn lower_fields<'p>(
    context: &mut TypedASTContextRef<'_>,
    module: ModuleId,
    properties: impl IntoIterator<Item = (&'p mollie_parser::Ident, &'p Positioned<mollie_parser::Type>)>,
) -> Vec<AdtVariantField> {
    properties
        .into_iter()
        .map(|(name, ty)| AdtVariantField {
            name: name.0.clone(),
            ty: Type::from_parsed(ty.value.clone(), module, context, ty.span),
            default_value: None,
        })
        .collect()
}

/// Type-checks a default value of a field and evaluates it at compile time.
fn evaluate_default_value(
    context: &mut TypedASTContextRef<'_>,
    module: ModuleId,
    ty: TypeRef,
    value: &Positioned<mollie_parser::Expr>,
) -> Option<ConstantValue> {
    evaluate_value(context, module, Some(ty), value).1
}

/// Evaluates a constant if it isn't yet: its type and value are stored in
/// the registry. Constants using each other in a cycle are reported.
pub fn evaluate_constant(context: &mut TypedASTContextRef<'_>, constant: ConstRef) {
    let Some(&(module, _, span)) = context.constants.decls.get(&constant) else {
        // Already evaluated (or failed).
        return;
    };

    if context.constants.evaluating.contains(&constant) {
        let name = context.type_solver.context.def_registry.constants[constant].name.clone();
        let error = context.type_solver.context.types.get_or_add(Type::Error);

        context.type_solver.context.def_registry.constants[constant].ty = error;
        context.type_solver.error(TypeError::ConstCycle { name }, ModuleSpan(module, span));
        // The value stays unknown, every constant of the cycle fails.
        context.constants.decls.shift_remove(&constant);

        return;
    }

    let decl = context.constants.decls[&constant].1.clone();

    context.constants.evaluating.push(constant);

    let ty = decl.ty.map(|ty| Type::from_parsed(ty.value, module, context, ty.span));
    let (ty, value) = evaluate_value(context, module, ty, &decl.value);

    context.constants.evaluating.retain(|&evaluating| evaluating != constant);
    context.constants.decls.shift_remove(&constant);

    let registered = &mut context.type_solver.context.def_registry.constants[constant];

    registered.ty = ty;
    registered.value = value;
}

/// Evaluates `value` at compile time, expecting `ty` if it's given. Returns
/// the type of the value, and the value unless it can't be evaluated (which
/// is reported).
fn evaluate_value(
    context: &mut TypedASTContextRef<'_>,
    module: ModuleId,
    ty: Option<TypeRef>,
    value: &Positioned<mollie_parser::Expr>,
) -> (TypeRef, Option<ConstantValue>) {
    let mut ast = TypedAST::<FirstPass> { module, ..TypedAST::default() };

    let mut default_context = context.fork();
    let expected = match ty {
        Some(ty) => TypeSolver::type_to_info(&mut default_context.type_solver.type_infos, default_context.type_solver.context, ty, &[]),
        None => default_context.type_solver.add_unknown(None, Some(value.span)),
    };
    let value = Expr::from_parsed_expecting(value.value.clone(), expected, &mut ast, &mut default_context, value.span);

    let errors_before = default_context.type_solver.diagnostics.errors.len();

    if let Err(err) = default_context.type_solver.unify(UnifyArgs {
        expected,
        found: ast[value].ty,
    }) {
        let err = err.into_type_error(&mut default_context.type_solver);

        default_context.type_solver.error(err, ModuleSpan(module, ast[value].span));
    }

    let (solved, expr) = ast.solve_expr_final(value, &mut default_context);
    let ty = solved[expr].ty;

    // An ill-typed value is already reported and can't be evaluated.
    if default_context.type_solver.diagnostics.errors.len() != errors_before {
        return (ty, None);
    }

    if let Ok(value) = expr.into_const_val(&solved, default_context.type_solver.context, &mut ConstantContext::default()) {
        (ty, Some(value))
    } else {
        default_context
            .type_solver
            .error(TypeError::NonConstantEvaluable, ModuleSpan(module, solved[expr].span));

        (ty, None)
    }
}

/// Resolves the trait, target type and function signatures of an impl block,
/// and registers it.
fn process_impl_signature(context: &mut TypedASTContextRef<'_>, id: ModuleId, span: Span, implementation: &mollie_parser::Impl) -> Option<ImplInfo> {
    /// A function of the impl: written in it, or the default of a trait
    /// function.
    #[derive(PartialEq, Eq)]
    enum Slot {
        Written(usize),
        Default(TraitFuncRef, FuncRef),
    }

    let trait_span = implementation.trait_name.as_ref().map_or(span, |trait_name| trait_name.span);

    let origin_trait = match &implementation.trait_name {
        Some(trait_name) => match resolve_item_path(&context.type_solver.context.def_registry, id, &trait_name.value) {
            Ok(ModuleItem::Trait(trait_ref)) => Some(trait_ref),
            Ok(item) => {
                let found = item_kind(&context.type_solver.context.def_registry, item);

                context.type_solver.error(
                    TypeError::Unexpected {
                        expected: TypeErrorValue::Trait,
                        found,
                    },
                    ModuleSpan(id, trait_name.span),
                );

                return None;
            }
            Err((error, span)) => {
                context.type_solver.error(error, ModuleSpan(id, span));

                return None;
            }
        },
        None => None,
    };

    // Type arguments of the trait, e.g. `T` in `impl<T> Iterator<T> for ...`.
    let trait_type_args = implementation
        .trait_name
        .as_ref()
        .and_then(|trait_name| trait_name.value.segments.last())
        .and_then(|segment| segment.value.args.clone())
        .map_or_default(|args| args.value.0);

    push_impl_generics(context, &implementation.generics, origin_trait.is_some());

    let ty = Type::from_parsed(implementation.target.value.clone(), id, context, implementation.target.span);

    push_self_type(context, ty);
    let trait_args: Box<[TypeRef]> = trait_type_args
        .into_iter()
        .map(|arg| Type::from_parsed(arg.value, id, context, arg.span))
        .collect();
    let bounds = lower_bounds(context, id, &implementation.generics, usize::from(origin_trait.is_some()));
    let generics = (0..implementation.generics.len() + usize::from(origin_trait.is_some()))
        .map(|index| context.type_solver.context.types.get_or_add(Type::Generic(index)))
        .collect();

    let impl_ref = match context.type_solver.context.get_vtable(ty, origin_trait, &trait_args, &bounds) {
        Some(impl_ref) => impl_ref,
        None => context.type_solver.context.register_impl(VTableGenerator {
            ty,
            origin_trait,
            trait_args,
            generics,
            bounds,
            functions: IndexVec::new(),
        }),
    };

    let functions = &implementation.functions.value;

    // Functions of a trait go first and in the trait's order, since trait
    // objects call them by index.
    let mut order = Vec::with_capacity(functions.len());

    if let Some(trait_ref) = origin_trait {
        let trait_functions = context.type_solver.context.def_registry.traits[trait_ref]
            .functions
            .iter()
            .map(|(trait_func, func)| (trait_func, func.name.clone(), func.default))
            .collect::<Vec<_>>();

        for (trait_func, name, default) in trait_functions {
            match (functions.iter().position(|function| function.value.name.value.0 == name), default) {
                (Some(index), _) => order.push(Slot::Written(index)),
                (None, Some(default)) => order.push(Slot::Default(trait_func, default)),
                (None, None) => {
                    context
                        .type_solver
                        .error(TypeError::MissingTraitFunc { trait_ref, name }, ModuleSpan(id, trait_span));
                }
            }
        }
    }

    for (index, function) in functions.iter().enumerate() {
        if order.contains(&Slot::Written(index)) {
            continue;
        }

        if let Some(trait_ref) = origin_trait {
            context.type_solver.error(
                TypeError::NotTraitMember {
                    trait_ref,
                    name: function.value.name.value.0.clone(),
                },
                ModuleSpan(id, function.value.name.span),
            );
        }

        order.push(Slot::Written(index));
    }

    let void = context.type_solver.context.types.core_types.void;
    let mut vfuncs = vec![VFuncRef::ZERO; functions.len()];

    for slot in order {
        let index = match slot {
            Slot::Written(index) => index,
            // The default is instantiated with `Self` and the trait's type
            // arguments of this impl.
            Slot::Default(trait_func, default) => {
                let type_args: Box<[_]> = once(ty)
                    .chain(context.type_solver.context.impl_registry.impls[impl_ref].trait_args.iter().copied())
                    .collect();
                let registry = &context.type_solver.context.def_registry;
                let name = origin_trait.map_or_default(|trait_ref| registry.traits[trait_ref].functions[trait_func].name.clone());
                let (arg_names, default_ty) = (registry.functions[default].arg_names.clone(), registry.functions[default].ty);
                let func_ty = context.type_solver.context.types.apply_type_args(default_ty, &type_args);
                let vfunc = context.type_solver.context.impl_registry.impls[impl_ref].functions.insert(VTableFunc {
                    trait_func: Some(trait_func),
                    name,
                    arg_names,
                    generics: 0,
                    ty: func_ty,
                });

                context
                    .vtables
                    .entry(impl_ref)
                    .or_default()
                    .insert(vfunc, FunctionBody::Default { func: default, type_args });

                continue;
            }
        };

        let function = &functions[index].value;

        // Trait objects can't call functions with their own generics.
        if origin_trait.is_some()
            && let Some(generic) = function.generics.first()
        {
            context.type_solver.error(TypeError::GenericMethod, ModuleSpan(id, generic.span));
        }

        // The function's own generics come after the impl's.
        let method_generics_offset = implementation.generics.len() + usize::from(origin_trait.is_some());

        push_generics(context, &function.generics, method_generics_offset);

        let method_bounds = lower_bounds(context, id, &function.generics, method_generics_offset);
        let trait_func = origin_trait.and_then(|trait_ref| {
            context.type_solver.context.def_registry.traits[trait_ref]
                .functions
                .iter()
                .find(|(_, func)| func.name == function.name.value.0)
                .map(|(func_ref, _)| func_ref)
        });

        let mut arg_names = Vec::with_capacity(function.args.len() + usize::from(function.this.is_some()));
        let mut args = Vec::with_capacity(function.args.len() + usize::from(function.this.is_some()));

        if function.this.is_some() {
            arg_names.push(String::from("self"));
            args.push(ty);
        }

        for arg in &function.args {
            args.push(Type::from_parsed(arg.value.ty.value.clone(), id, context, arg.value.ty.span));
            arg_names.push(arg.value.name.value.0.clone());
        }

        let returns = function
            .returns
            .as_ref()
            .map_or(void, |returns| Type::from_parsed(returns.value.clone(), id, context, returns.span));

        let func_ty = context.type_solver.context.types.get_or_add(Type::Func(args.into_boxed_slice(), returns));

        pop_generics(context, &function.generics);

        vfuncs[index] = context.type_solver.context.impl_registry.impls[impl_ref].functions.insert(VTableFunc {
            trait_func,
            name: function.name.value.0.clone(),
            arg_names,
            generics: function.generics.len(),
            ty: func_ty,
        });

        if !method_bounds.is_empty() {
            context
                .type_solver
                .context
                .impl_registry
                .method_bounds
                .insert((impl_ref, vfuncs[index]), method_bounds);
        }

        context
            .type_solver
            .context
            .impl_registry
            .func_spans
            .insert((impl_ref, vfuncs[index]), ModuleSpan(id, function.name.span));

        // `mut self` gives the changed receiver back, which only makes sense
        // for values that are copied. Through a trait object, it changes the
        // boxed value.
        if function.mut_self {
            if context.type_solver.context.is_value_type(ty) {
                context.type_solver.context.impl_registry.mut_self.insert((impl_ref, vfuncs[index]));
            } else {
                context.type_solver.error(TypeError::InvalidMutSelf, ModuleSpan(id, function.name.span));
            }
        }

        // Defaults take `self` too, and have the impl's generics followed by
        // the function's.
        let Type::Func(arg_types, _) = context.type_solver.context.types[func_ty].clone() else {
            unreachable!("functions of impls have function types")
        };
        let (this, arg_types) = if function.this.is_some() {
            (Some(arg_types[0]), &arg_types[1..])
        } else {
            (None, &arg_types[..])
        };

        if let Some(defaults) = register_defaults(
            context,
            &function.name.value.0,
            &function.args,
            arg_types,
            this,
            method_generics_offset + function.generics.len(),
        ) {
            context
                .type_solver
                .context
                .impl_registry
                .method_defaults
                .insert((impl_ref, vfuncs[index]), defaults);
        }
    }

    pop_impl_generics(context, &implementation.generics);

    Some(ImplInfo { impl_ref, functions: vfuncs })
}

/// Reports value types containing themselves (directly, or through fields of
/// other value types): they'd have an infinite size. Arrays and other types
/// are references, which break cycles.
fn check_value_types(context: &mut TypedASTContextRef<'_>) {
    fn contains(registry: &DefRegistry, types: &mollie_typing::TypeStorage, target: AdtRef, ty: TypeRef, visited: &mut Vec<AdtRef>) -> bool {
        let Type::Adt(adt, args) = &types[ty] else {
            return false;
        };

        if !registry.value_types.contains(adt) {
            return false;
        }

        if *adt == target {
            return true;
        }

        // Type arguments of value types may be stored inline too.
        if args.iter().any(|&arg| contains(registry, types, target, arg, visited)) {
            return true;
        }

        if visited.contains(adt) {
            return false;
        }

        visited.push(*adt);

        registry.adt_types[*adt]
            .variants
            .values()
            .flat_map(|variant| variant.fields.values())
            .any(|field| contains(registry, types, target, field.ty, visited))
    }

    let registry = &context.type_solver.context.def_registry;
    let types = &context.type_solver.context.types;
    let recursive = registry
        .value_types
        .iter()
        .copied()
        .filter(|&adt| {
            registry.adt_types[adt]
                .variants
                .values()
                .flat_map(|variant| variant.fields.values())
                .any(|field| contains(registry, types, adt, field.ty, &mut Vec::new()))
        })
        .filter_map(|adt| {
            registry
                .modules
                .values()
                .flat_map(|module| module.items.values())
                .find_map(|&(item, kind, span)| (item == ModuleItem::Adt(adt) && kind == DefinitionType::Local).then_some((adt, span)))
        })
        .collect::<Vec<_>>();

    for (adt, span) in recursive {
        context.type_solver.error(TypeError::RecursiveValueType { adt }, span);
    }
}

/// Registers functions computing default values of parameters `args` (of
/// types `arg_types`) of the function `name`, with `generics` generic
/// parameters. The function of a default takes `self` of type `this` (if
/// any) and the parameters before it, and returns its value. Returns them by
/// parameter, or `None` if no parameter has a default.
fn register_defaults(
    context: &mut TypedASTContextRef<'_>,
    name: &str,
    args: &[Positioned<mollie_parser::Argument>],
    arg_types: &[TypeRef],
    this: Option<TypeRef>,
    generics: usize,
) -> Option<Box<[Option<FuncRef>]>> {
    if args.iter().all(|arg| arg.value.default.is_none()) {
        return None;
    }

    let defaults = args
        .iter()
        .zip(arg_types)
        .enumerate()
        .map(|(index, (arg, &ty))| {
            arg.value.default.as_ref()?;

            let params = this.into_iter().chain(arg_types[..index].iter().copied()).collect();
            let arg_names = this
                .map(|_| String::from("self"))
                .into_iter()
                .chain(args[..index].iter().map(|arg| arg.value.name.value.0.clone()))
                .collect();
            let func_ty = context.type_solver.context.types.get_or_add(Type::Func(params, ty));

            Some(context.type_solver.context.def_registry.functions.insert(Func {
                postfix: false,
                name: format!("{name}#default#{index}"),
                generics,
                arg_names,
                ty: func_ty,
            }))
        })
        .collect();

    Some(defaults)
}

/// The default value of a parameter as the body of its function.
fn default_body(value: &Positioned<mollie_parser::Expr>) -> Positioned<mollie_parser::BlockExpr> {
    value.wrap(mollie_parser::BlockExpr {
        stmts: Vec::new(),
        final_stmt: Some(Box::new(value.wrap(mollie_parser::Stmt::Expression(value.value.clone())))),
    })
}

/// The impl block of a function of one, its generics, the function's own
/// generics and whether it takes `mut self`.
type ImplContext<'a> = (
    ImplRef,
    &'a [Positioned<mollie_parser::GenericParam>],
    &'a [Positioned<mollie_parser::GenericParam>],
    bool,
);

/// Type-checks the body of a function against its type `func_ty`. Functions
/// of impl blocks get the impl's generics.
#[allow(clippy::too_many_arguments)]
fn check_function_body(
    context: &mut TypedASTContextRef<'_>,
    module: ModuleId,
    arg_names: &[String],
    // Spans of names of parameters, for tools.
    arg_spans: &[Span],
    func_ty: TypeRef,
    body: &Positioned<mollie_parser::BlockExpr>,
    impl_generics: Option<ImplContext<'_>>,
    bounds: Vec<Bound>,
) -> FunctionBody {
    let Type::Func(arg_types, returns) = context.type_solver.context.types[func_ty].clone() else {
        unreachable!("functions always have a function type after the signatures pass")
    };

    let mut func_context = context.fork();

    // Methods of the bounds can be called on generic parameters.
    func_context.type_solver.bounds = bounds;

    // Generics of the impl, then the function's own generics.
    if let Some((impl_ref, generics, method_generics, _)) = impl_generics {
        let generator = &func_context.type_solver.context.impl_registry.impls[impl_ref];
        let (target, is_trait_impl) = (generator.ty, generator.origin_trait.is_some());

        push_impl_generics(&mut func_context, generics, is_trait_impl);
        push_self_type(&mut func_context, target);

        if is_trait_impl {
            func_context.trait_impl = Some(impl_ref);
        }

        push_generics(&mut func_context, method_generics, generics.len() + usize::from(is_trait_impl));
    }

    let mut ast = TypedAST::<FirstPass> { module, ..TypedAST::default() };

    for (index, (name, &ty)) in arg_names.iter().zip(&arg_types).enumerate() {
        let type_info = TypeSolver::type_to_info(&mut func_context.type_solver.type_infos, func_context.type_solver.context, ty, &[]);

        if let Type::Adt(adt_ref, adt_args) = &func_context.type_solver.context.types[ty] {
            let item = UsedItem::Adt(*adt_ref, adt_args.clone());

            ast.use_item(
                &func_context.type_solver.context.def_registry.adt_types,
                &func_context.type_solver.context.impl_registry,
                &func_context.type_solver.context.def_registry.traits,
                func_context.vtables,
                func_context.functions,
                &mut func_context.type_solver.context.types,
                item,
                None,
            );
        }

        // `self` of a value type can only be changed with `mut self`.
        let mutable = index > 0 || name != "self" || impl_generics.is_none_or(|(.., mut_self)| mut_self) || !func_context.type_solver.context.is_value_type(ty);

        match arg_spans.get(index) {
            Some(&span) => ast.declare_var(&mut func_context, name.clone(), type_info, mutable, span),
            None => func_context.type_solver.set_var_with_mutability(name, type_info, mutable),
        }
    }

    let returns_info = TypeSolver::type_to_info(&mut func_context.type_solver.type_infos, func_context.type_solver.context, returns, &[]);

    func_context.returns = Some(returns_info);

    let body = Block::from_parsed_expecting(body.value.clone(), returns_info, &mut ast, &mut func_context, body.span);

    if let Err(err) = func_context.type_solver.unify(UnifyArgs {
        expected: returns_info,
        found: ast[body].ty,
    }) {
        let err = err.into_type_error(&mut func_context.type_solver);

        func_context.type_solver.error(err, ModuleSpan(module, ast[body].span));
    }

    let (ast, entry) = ast.solve(body, &mut func_context, returns);

    FunctionBody::Local { ast, entry }
}

#[cfg(test)]
mod test_module_map {
    use std::{fmt, mem::transmute};

    use mollie_index::{Idx, IndexVec};
    use mollie_typing::{DefRegistry, DiagnosticContext, ModuleId, TyCtxt};

    use super::{ModuleLoader, ModuleMap, ParsedModule};

    #[test]
    fn test_basic_conflict() {
        struct ModuleCache {
            modules: IndexVec<ModuleId, (String, ariadne::Source<String>)>,
        }

        impl ariadne::Cache<ModuleId> for ModuleCache {
            type Storage = String;

            fn fetch(&mut self, id: &ModuleId) -> Result<&ariadne::Source<String>, impl fmt::Debug> {
                Ok::<_, ()>(&self.modules[*id].1)
            }

            fn display<'a>(&self, id: &'a ModuleId) -> Option<impl fmt::Display + 'a> {
                Some(unsafe { transmute::<&String, &'a String>(&self.modules[*id].0) })
            }
        }

        const LIBRARY_MOL_SOURCE: &str = "module hello;
module hello;

import world;
import { World } from hello;
import { Something } from hello::World;
import { Something } from hello;

struct A {}
        
struct B {}

struct World {}
        
struct B {}";

        const HELLO_MOL_SOURCE: &str = "import { A, B, Seiso } from super;

struct C {
    a: A,
    d: D
}

struct D {
    a: A
}

struct World {}

struct World {}";

        struct TestLoader;

        impl ModuleLoader for TestLoader {
            type Error = ();

            fn load(&mut self, _: &mut DefRegistry, _: ModuleId) -> Result<ParsedModule, Self::Error> {
                let (stmts, final_stmt) = mollie_parser::parse_statements_until(
                    &mut mollie_parser::Parser::new(&mut mollie_lexer::Lexer::lex(HELLO_MOL_SOURCE)),
                    &mollie_lexer::Token::EOF,
                )
                .unwrap();

                Ok(ParsedModule {
                    stmts,
                    final_stmt,
                    stub: false,
                })
            }
        }

        let mut map = ModuleMap::new(TestLoader);
        let mut context = TyCtxt::new();
        let mut diagnostics = DiagnosticContext::default();

        map.register_from_str(&mut context.def_registry, &mut diagnostics, LIBRARY_MOL_SOURCE, ModuleId::ZERO);
        map.process_all_imports(&mut context.def_registry, &mut diagnostics);
        // map.process_all_declarations(&mut ast);

        let mut cache = ModuleCache {
            modules: IndexVec::from_iter([
                (String::from("library.mol"), ariadne::Source::from(LIBRARY_MOL_SOURCE.to_string())),
                (String::from("hello.mol"), ariadne::Source::from(HELLO_MOL_SOURCE.to_string())),
            ]),
        };

        for error in diagnostics.errors.values() {
            if let Some(span) = error.primary_span {
                let mut report = ariadne::Report::build(ariadne::ReportKind::Error, span).with_config(ariadne::Config::new().with_compact(true));

                error.add_to_report(&mut report, &context);

                report.finish().print(&mut cache).unwrap();
            }
        }
    }
}
