//! Language features computed from an [`Analysis`].

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
};

use lsp_types::{
    CompletionItem, CompletionItemKind, Contents, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Hover, Location, MarkupContent, MarkupKind,
    Message, Uri,
};
use mollie_index::Idx;
use mollie_typed_ast::{Expr, TypedAST};
use mollie_typing::{
    AdtKind, AdtRef, AdtVariantRef, DefinitionType, Diagnostic as TypeDiagnostic, FieldRef, ModuleId, ModuleItem, ModuleSpan, TraitRef, TyCtxt, Type, TypeRef,
};

use crate::analysis::{Analysis, Documents, Encoding, Project};

/// Diagnostics of every file of the program, by file.
pub fn diagnostics(analysis: &Analysis) -> HashMap<Uri, Vec<Diagnostic>> {
    let tcx = &analysis.context.tcx;
    let mut result: HashMap<Uri, Vec<Diagnostic>> = analysis.files.values().map(|file| (file.uri.clone(), Vec::new())).collect();
    let location = |ModuleSpan(module, span): ModuleSpan| analysis.files.get(&module).map(|file| Location::new(file.uri.clone(), file.index.range(span)));

    let mut push = |location: Location, message: String, related: Vec<DiagnosticRelatedInformation>| {
        result.entry(location.uri.clone()).or_default().push(Diagnostic {
            range: location.range,
            severity: Some(DiagnosticSeverity::Error),
            source: Some(String::from("mollie")),
            message: Message::String(message),
            related_information: (!related.is_empty()).then_some(related),
            ..Diagnostic::default()
        });
    };

    for diagnostic in analysis.context.diagnostics.errors.values() {
        let Some(primary) = diagnostic.primary_span.and_then(location) else {
            continue;
        };

        push(primary, diagnostic.message(tcx), related(diagnostic, tcx, &location));
    }

    for (module, message, span) in &analysis.parse_errors {
        if let Some(location) = location(ModuleSpan(*module, span.unwrap_or_default())) {
            push(location, message.clone(), Vec::new());
        }
    }

    result
}

/// Labels of a diagnostic, as related information.
fn related(diagnostic: &TypeDiagnostic, tcx: &TyCtxt, location: &impl Fn(ModuleSpan) -> Option<Location>) -> Vec<DiagnosticRelatedInformation> {
    diagnostic
        .labels(tcx)
        .into_iter()
        .filter_map(|(span, message)| location(span).map(|location| DiagnosticRelatedInformation::new(location, message)))
        .collect()
}

/// Hover of the code at `offset` in `module`.
pub fn hover(analysis: &mut Analysis, module: ModuleId, offset: usize) -> Option<Hover> {
    let (text, span) = if let Some((ast, variable)) = analysis.variable_at(module, offset) {
        let variable = &ast.variables[variable];
        let text = format!("let {}: {}", variable.name, analysis.context.tcx.display_of(variable.ty));

        (text, variable.span)
    } else if let Some((text, span)) = hover_expr(analysis, module, offset) {
        (text, span)
    } else {
        let ident = analysis.ident_at(module, offset)?;
        let item = analysis.context.tcx.def_registry.lookup(module, &ident.value)?;

        (describe_item(&mut analysis.context.tcx, &ident.value, item), ident.span)
    };

    let range = analysis.files.get(&module)?.index.range(span);

    Some(Hover::new(
        Contents::MarkupContent(MarkupContent::new(MarkupKind::Markdown, format!("```mollie\n{text}\n```"))),
        Some(range),
    ))
}

/// Description of the expression at `offset`, and its span.
fn hover_expr(analysis: &mut Analysis, module: ModuleId, offset: usize) -> Option<(String, mollie_shared::Span)> {
    let (ast, expr) = analysis.expr_at(module, offset)?;
    let typed = &ast[expr];
    let (value, ty, span) = (typed.value.clone(), typed.ty, typed.span);

    let text = match value {
        Expr::Func { func, ref type_args } => {
            let type_args = type_args.clone();
            let tcx = &mut analysis.context.tcx;
            let func = &tcx.def_registry.functions[func];
            let (name, arg_names, func_ty) = (func.name.clone(), func.arg_names.clone(), func.ty);
            let func_ty = tcx.types.apply_type_args(func_ty, &type_args);

            signature(tcx, &name, &arg_names, func_ty)
        }
        Expr::AdtIndex { target, field } => {
            let target_ty = ast[target].ty;
            let tcx = &analysis.context.tcx;

            match &tcx.types[target_ty] {
                Type::Adt(adt, _) => {
                    let adt = &tcx.def_registry.adt_types[*adt];

                    format!(
                        "{}.{}: {}",
                        adt.name.as_deref().unwrap_or_default(),
                        adt.variants[AdtVariantRef::default()].fields[field].name,
                        tcx.display_of(ty)
                    )
                }
                _ => tcx.display_of(ty).to_string(),
            }
        }
        Expr::VTableIndex { vtable, func, target_ty, .. } => {
            let tcx = &analysis.context.tcx;
            let function = &tcx.impl_registry.impls[vtable].functions[func];
            let (name, arg_names) = (function.name.clone(), function.arg_names.clone());
            let mut signature = signature(tcx, &name, &arg_names, ty);

            // Functions changing their receiver (of a value type).
            if tcx.impl_registry.mut_self.contains(&(vtable, func)) {
                signature = signature.replacen("(self", "(mut self", 1);
            }

            format!("impl {}\n{signature}", tcx.display_of(target_ty))
        }
        Expr::TraitFunc { trait_ref, func, .. } | Expr::BoundFunc { trait_ref, func, .. } => {
            let tcx = &analysis.context.tcx;
            let r#trait = &tcx.def_registry.traits[trait_ref];
            let function = &r#trait.functions[func];
            let arg_names = function.args.iter().map(|arg| arg.name.clone()).collect::<Vec<_>>();

            format!("trait {}\n{}", r#trait.name, signature(tcx, &function.name, &arg_names, ty))
        }
        Expr::Const(constant) => {
            let tcx = &analysis.context.tcx;
            let constant = &tcx.def_registry.constants[constant];

            format!("const {}: {}", constant.name, tcx.display_of(constant.ty))
        }
        Expr::Construct { adt, .. } => {
            let name = analysis.context.tcx.def_registry.adt_types[adt].name.clone().unwrap_or_default();

            describe_item(&mut analysis.context.tcx, &name, ModuleItem::Adt(adt))
        }
        _ => analysis.context.tcx.display_of(ty).to_string(),
    };

    Some((text, span))
}

/// `func name(a: A, self) -> R` of a function of type `func_ty`.
fn signature(tcx: &TyCtxt, name: &str, arg_names: &[String], func_ty: TypeRef) -> String {
    let Type::Func(args, returns) = &tcx.types[func_ty] else {
        return format!("func {name}: {}", tcx.display_of(func_ty));
    };

    let mut text = format!("func {name}(");

    for (index, (arg_name, &arg)) in arg_names.iter().zip(args).enumerate() {
        if index > 0 {
            text.push_str(", ");
        }

        if arg_name == "self" {
            text.push_str("self");
        } else {
            let _ = write!(text, "{arg_name}: {}", tcx.display_of(arg));
        }
    }

    text.push(')');

    if !matches!(tcx.types[*returns], Type::Primitive(mollie_typing::PrimitiveType::Void)) {
        let _ = write!(text, " -> {}", tcx.display_of(*returns));
    }

    text
}

/// Declaration-like description of `item`, called `name`.
fn describe_item(tcx: &mut TyCtxt, name: &str, item: ModuleItem) -> String {
    match item {
        ModuleItem::SubModule(_) => format!("module {name}"),
        ModuleItem::Func(func) => {
            let func = &tcx.def_registry.functions[func];

            signature(tcx, &func.name, &func.arg_names, func.ty)
        }
        ModuleItem::Adt(adt) => describe_adt(tcx, adt),
        ModuleItem::Trait(trait_ref) => describe_trait(tcx, trait_ref),
        ModuleItem::Const(constant) => {
            let constant = &tcx.def_registry.constants[constant];

            format!("const {}: {}", constant.name, tcx.display_of(constant.ty))
        }
        ModuleItem::Intrinsic(_, ty) => format!("{name}: {}", tcx.display_of(ty)),
    }
}

fn describe_adt(tcx: &TyCtxt, adt_ref: AdtRef) -> String {
    let adt = &tcx.def_registry.adt_types[adt_ref];
    let keyword = match adt.kind {
        AdtKind::Struct => "struct",
        AdtKind::View => "view",
        AdtKind::Enum => "enum",
    };
    // Values of value types are copied.
    let value = if tcx.def_registry.value_types.contains(&adt_ref) { "value " } else { "" };
    let mut text = format!("{value}{keyword} {} {{", adt.name.as_deref().unwrap_or_default());
    let fields = |text: &mut String, variant: &mollie_typing::AdtVariant, indent: &str| {
        for field in variant.fields.values().filter(|field| !field.name.starts_with('<')) {
            let _ = write!(text, "\n{indent}{}: {},", field.name, tcx.display_of(field.ty));
        }
    };

    match adt.kind {
        AdtKind::Enum => {
            for variant in adt.variants.values() {
                let _ = write!(text, "\n    {}", variant.name.as_deref().unwrap_or_default());

                if variant.fields.values().any(|field| !field.name.starts_with('<')) {
                    text.push_str(" {");
                    fields(&mut text, variant, "        ");
                    text.push_str("\n    }");
                }

                text.push(',');
            }
        }
        _ => {
            if let Some(variant) = adt.variants.values().next() {
                fields(&mut text, variant, "    ");
            }
        }
    }

    text.push_str("\n}");
    text
}

fn describe_trait(tcx: &mut TyCtxt, trait_ref: TraitRef) -> String {
    let functions = tcx.def_registry.traits[trait_ref]
        .functions
        .values()
        .map(|function| {
            let args = function.args.iter().map(|arg| arg.ty).collect::<Box<[_]>>();
            let arg_names = function.args.iter().map(|arg| arg.name.clone()).collect::<Vec<_>>();

            (function.name.clone(), arg_names, args, function.returns)
        })
        .collect::<Vec<_>>();
    let mut text = format!("trait {} {{", tcx.def_registry.traits[trait_ref].name);

    for (name, arg_names, args, returns) in functions {
        let func_ty = tcx.types.get_or_add(Type::Func(args, returns));

        let _ = write!(text, "\n    {};", signature(tcx, &name, &arg_names, func_ty));
    }

    text.push_str("\n}");
    text
}

/// Where the item at `offset` in `module` is declared.
pub fn definition(analysis: &Analysis, module: ModuleId, offset: usize) -> Option<Location> {
    let span = if let Some((ast, variable)) = analysis.variable_at(module, offset) {
        Some(ModuleSpan(ast.module, ast.variables[variable].span))
    } else if let Some((ast, expr)) = analysis.expr_at(module, offset) {
        definition_of_expr(analysis, ast, &ast[expr].value)
    } else {
        None
    };

    let span = span.or_else(|| {
        let ident = analysis.ident_at(module, offset)?;
        let item = analysis.context.tcx.def_registry.lookup(module, &ident.value)?;

        analysis.item_span(item)
    })?;

    let file = analysis.files.get(&span.0)?;

    Some(Location::new(file.uri.clone(), file.index.range(span.1)))
}

fn definition_of_expr(analysis: &Analysis, ast: &TypedAST, expr: &Expr) -> Option<ModuleSpan> {
    let tcx = &analysis.context.tcx;

    match *expr {
        Expr::Func { func, .. } => analysis.item_span(ModuleItem::Func(func)),
        Expr::Const(constant) => analysis.item_span(ModuleItem::Const(constant)),
        Expr::Construct { adt, .. } => analysis.item_span(ModuleItem::Adt(adt)),
        Expr::AdtIndex { target, field } => {
            let Type::Adt(adt, _) = tcx.types[ast[target].ty] else {
                return None;
            };

            field_span(analysis, adt, AdtVariantRef::default(), field)
        }
        Expr::VTableIndex { vtable, func, .. } => tcx.impl_registry.func_spans.get(&(vtable, func)).copied().or_else(|| {
            // A default of a trait function.
            let generator = &tcx.impl_registry.impls[vtable];
            let trait_ref = generator.origin_trait?;
            let name = &generator.functions[func].name;

            trait_function_span(analysis, trait_ref, name)
        }),
        Expr::TraitFunc { trait_ref, func, .. } | Expr::BoundFunc { trait_ref, func, .. } => {
            trait_function_span(analysis, trait_ref, &tcx.def_registry.traits[trait_ref].functions[func].name)
        }
        _ => None,
    }
}

fn field_span(analysis: &Analysis, adt: AdtRef, variant: AdtVariantRef, field: FieldRef) -> Option<ModuleSpan> {
    let declaration = analysis.item_span(ModuleItem::Adt(adt))?;
    let adt = &analysis.context.tcx.def_registry.adt_types[adt];
    let variant = &adt.variants[variant];
    let field = &variant.fields[field].name;
    // The declaration's span is narrowed to its name, so the field is
    // searched in the rest of the file.
    let file = analysis.files.get(&declaration.0)?;
    let mut rest = declaration;

    rest.1.end = file.index.text().len();

    analysis.name_in(rest, field, variant.name.as_deref())
}

fn trait_function_span(analysis: &Analysis, trait_ref: TraitRef, name: &str) -> Option<ModuleSpan> {
    let mut declaration = analysis.item_span(ModuleItem::Trait(trait_ref))?;
    let file = analysis.files.get(&declaration.0)?;

    declaration.1.end = file.index.text().len();

    analysis.name_in(declaration, name, None)
}

/// Name inserted at the cursor before completing, so that `value.` parses.
const PLACEHOLDER: &str = "__mollie_completion";

const KEYWORDS: &[&str] = &[
    "let", "mut", "const", "func", "struct", "enum", "view", "trait", "impl", "for", "in", "while", "loop", "break", "continue", "return", "if", "else",
    "match", "is", "as", "import", "from", "module", "self", "super", "true", "false",
];

/// Completion at `offset` in the document `uri` (with text `text`), part of
/// the program whose root is `root` (which is `uri` for a root module).
pub fn completion(
    root: &Uri,
    uri: &Uri,
    text: &str,
    offset: usize,
    documents: &Documents,
    encoding: Encoding,
    project: Option<&Project>,
) -> Vec<CompletionItem> {
    let offset = offset.min(text.len());
    // The identifier being typed is replaced with the placeholder.
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|&(_, character)| character.is_alphanumeric() || character == '_')
        .last()
        .map_or(offset, |(index, _)| index);
    // `value.` (but not a range, `0..`).
    let receiver_end = text[..start].trim_end().strip_suffix('.').filter(|before| !before.ends_with('.')).map(str::len);

    // The placeholder may need a semicolon to end a statement.
    let mut analysis = None;

    for suffix in ["", ";"] {
        let edited = format!("{}{PLACEHOLDER}{suffix}{}", &text[..start], &text[offset..]);
        // The edited document replaces the open one, wherever it is in the
        // program.
        let mut edited_documents = documents.clone();

        edited_documents.set(uri.clone(), edited.clone());

        let root_text = if root == uri {
            Some(edited)
        } else {
            root.to_file_path().ok().and_then(|path| edited_documents.read(&path))
        };
        let Some(current) = root_text.and_then(|root_text| Analysis::try_new(root, root_text, &edited_documents, encoding, project)) else {
            continue;
        };
        let parsed = !current
            .context
            .diagnostics
            .errors
            .values()
            .any(|diagnostic| matches!(&*diagnostic.error, mollie_typing::TypeError::Parse { .. }));

        analysis = Some(current);

        if parsed {
            break;
        }
    }

    let Some(mut analysis) = analysis else {
        return Vec::new();
    };
    let Some(module) = analysis.module_of(uri) else {
        return Vec::new();
    };

    match receiver_end {
        Some(end) => members(&mut analysis, module, end),
        None => scope(&analysis, module, start),
    }
}

/// Fields and methods of the expression ending at `end` (before a `.`).
fn members(analysis: &mut Analysis, module: ModuleId, end: usize) -> Vec<CompletionItem> {
    let Some((ty, bounds)) = analysis
        .asts(module)
        .into_iter()
        .flat_map(|ast| ast.exprs.values().map(move |expr| (ast, expr)))
        .filter(|(_, expr)| expr.span.end == end && expr.span.start < end)
        .min_by_key(|(_, expr)| expr.span.start)
        .map(|(ast, expr)| (expr.ty, analysis.bounds_of(ast)))
    else {
        return Vec::new();
    };

    let tcx = &mut analysis.context.tcx;
    let mut items = Vec::new();

    // A generic parameter has the functions of its bounds.
    if let Type::Generic(index) = tcx.types[ty] {
        for bound in bounds.iter().filter(|bound| bound.generic == index) {
            let r#trait = &tcx.def_registry.traits[bound.trait_ref];

            for function in r#trait.functions.values() {
                items.push(item(function.name.clone(), CompletionItemKind::Method, format!("trait {}", r#trait.name)));
            }
        }
    }

    match tcx.types[ty].clone() {
        Type::Adt(adt, type_args) if tcx.def_registry.adt_types[adt].kind != AdtKind::Enum => {
            let fields = tcx.def_registry.adt_types[adt].variants[AdtVariantRef::default()]
                .fields
                .values()
                .filter(|field| !field.name.starts_with('<'))
                .map(|field| (field.name.clone(), field.ty))
                .collect::<Vec<_>>();

            for (name, field_ty) in fields {
                let field_ty = tcx.types.apply_type_args(field_ty, &type_args);

                items.push(item(name, CompletionItemKind::Field, tcx.display_of(field_ty).to_string()));
            }
        }
        Type::Trait(trait_ref, _) => {
            for function in tcx.def_registry.traits[trait_ref].functions.values() {
                items.push(item(
                    function.name.clone(),
                    CompletionItemKind::Method,
                    format!("trait {}", tcx.def_registry.traits[trait_ref].name),
                ));
            }
        }
        _ => (),
    }

    for (impl_ref, generator) in tcx.impl_registry.impls.iter() {
        if tcx.impl_registry.impl_args(&tcx.types, impl_ref, ty).is_none() {
            continue;
        }

        for function in generator.functions.values() {
            // Functions of the host have no names of parameters, and take
            // the receiver first.
            if function.arg_names.first().is_none_or(|name| name == "self") {
                items.push(item(
                    function.name.clone(),
                    CompletionItemKind::Method,
                    signature(tcx, &function.name, &function.arg_names, function.ty),
                ));
            }
        }
    }

    dedup(items)
}

/// Variables, items and keywords visible at `offset`.
fn scope(analysis: &Analysis, current: ModuleId, offset: usize) -> Vec<CompletionItem> {
    let module = current;
    let tcx = &analysis.context.tcx;
    let mut items = Vec::new();

    // Variables declared before the cursor in the code around it.
    for ast in analysis.asts(module) {
        let around = ast.blocks.values().any(|block| block.span.start <= offset && offset <= block.span.end);

        if !around {
            continue;
        }

        for variable in ast.variables.iter().filter(|variable| variable.span.end <= offset) {
            items.push(item(
                variable.name.clone(),
                CompletionItemKind::Variable,
                tcx.display_of(variable.ty).to_string(),
            ));
        }
    }

    // Later declarations shadow earlier ones.
    items.reverse();

    let registry = &tcx.def_registry;
    let modules = [Some(module), Some(ModuleId::ZERO), registry.prelude];

    for module in modules.into_iter().flatten() {
        for (name, &(item_ref, kind, _)) in &registry.modules[module].items {
            // Imports of the prelude and the host module aren't visible.
            if module != current && kind == DefinitionType::Import && registry.prelude != Some(module) {
                continue;
            }

            if name.starts_with('<') || name.contains('#') {
                continue;
            }

            let (kind, detail) = match item_ref {
                ModuleItem::SubModule(_) => (CompletionItemKind::Module, String::from("module")),
                ModuleItem::Adt(adt) => (
                    match registry.adt_types[adt].kind {
                        AdtKind::Enum => CompletionItemKind::Enum,
                        _ => CompletionItemKind::Struct,
                    },
                    String::new(),
                ),
                ModuleItem::Trait(_) => (CompletionItemKind::Interface, String::from("trait")),
                ModuleItem::Func(func) => {
                    let func = &registry.functions[func];

                    (CompletionItemKind::Function, signature(tcx, &func.name, &func.arg_names, func.ty))
                }
                ModuleItem::Const(constant) => (CompletionItemKind::Constant, tcx.display_of(registry.constants[constant].ty).to_string()),
                ModuleItem::Intrinsic(_, ty) => (CompletionItemKind::Function, tcx.display_of(ty).to_string()),
            };

            items.push(item(name.clone(), kind, detail));
        }
    }

    for name in registry.extern_roots.keys() {
        items.push(item(name.clone(), CompletionItemKind::Module, String::from("library")));
    }

    for keyword in KEYWORDS {
        items.push(item((*keyword).to_owned(), CompletionItemKind::Keyword, String::new()));
    }

    dedup(items)
}

fn item(label: String, kind: CompletionItemKind, detail: String) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail: (!detail.is_empty()).then_some(detail),
        ..CompletionItem::default()
    }
}

/// Keeps the first item of every label.
fn dedup(items: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();

    items
        .into_iter()
        .filter(|item| !item.label.contains(PLACEHOLDER) && seen.insert(item.label.clone()))
        .collect()
}
