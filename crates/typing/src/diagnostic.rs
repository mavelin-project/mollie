//! Diagnostics: type errors with their locations, and their text.
//!
//! The text of a diagnostic (a message and labels for its spans) is produced
//! in one place, [`Diagnostic::message`] and [`Diagnostic::labels`], and used
//! both by the plain [`fmt::Display`] output ([`DiagnosticDisplay`]) and by
//! `ariadne` reports (with the `ariadne` feature).

use std::fmt;

use mollie_index::IndexVec;
use mollie_shared::Span;

use crate::{
    AdtKind, ModuleId, ModuleSpan, SpecialAdtKind, TyCtxt, TypeErrorRef,
    error::{DefinitionType, LookupType, TypeError, TypeErrorValue},
};

#[derive(Debug)]
pub struct Diagnostic {
    pub error: TypeError,
    pub primary_span: Option<ModuleSpan>,
    pub secondary_span: Option<ModuleSpan>,
}

impl Diagnostic {
    pub const fn new(error: TypeError) -> Self {
        Self {
            error,
            primary_span: None,
            secondary_span: None,
        }
    }

    #[must_use]
    pub const fn with_primary_span(mut self, module: ModuleId, span: Span) -> Self {
        self.primary_span.replace(ModuleSpan(module, span));

        self
    }

    #[must_use]
    pub const fn with_secondary_span(mut self, module: ModuleId, span: Span) -> Self {
        self.secondary_span.replace(ModuleSpan(module, span));

        self
    }

    /// The headline of the diagnostic.
    pub fn message(&self, tcx: &TyCtxt) -> String {
        match &self.error {
            TypeError::Unexpected { expected, found } => match (expected, found) {
                (TypeErrorValue::Nothing, TypeErrorValue::Nothing) => String::from("unexpected value"),
                (TypeErrorValue::Nothing, found) => format!("unexpected {}", found.display(tcx)),
                (expected, _) => format!("expected {}", expected.display(tcx)),
            },
            TypeError::VariantRequired(_) => String::from("can't construct"),
            TypeError::NoField { .. } => String::from("no field"),
            TypeError::MissingField { .. } => String::from("missing field"),
            TypeError::NoFunction { name, postfix: true } => format!("there's no postfix function called `{name}`"),
            TypeError::NoFunction { name, postfix: false } => format!("there's no function called `{name}`"),
            TypeError::NonIndexable { .. } => String::from("non-indexable value"),
            TypeError::NotFound { name, was_looking_for } => match *was_looking_for {
                LookupType::Variable => format!("there's no variable called `{name}`"),
                LookupType::Type { inside } if tcx.def_registry.is_root(inside) => format!("there's no type called `{name}`"),
                LookupType::Type { inside } => format!("there's no type called `{name}` in `{}`", tcx.def_registry.modules[inside].name),
                LookupType::Module { inside } if tcx.def_registry.is_root(inside) => format!("there's no module called `{name}`"),
                LookupType::Module { inside } => format!("there's no module called `{name}` in `{}`", tcx.def_registry.modules[inside].name),
            },
            TypeError::NotPostfix { name } => format!("`{name}` can't be used in postfix context"),
            TypeError::ArgumentCountMismatch { expected, found, .. } => String::from(if found > expected {
                "received more arguments than was expected"
            } else {
                "received less arguments than was expected"
            }),
            TypeError::NonConstantEvaluable => String::from("expression can't be evaluated at compile-time"),
            TypeError::AlreadyExists { name, primary, secondary } => {
                if *primary == DefinitionType::Local || *secondary == DefinitionType::Local {
                    format!("`{name}` is already declared")
                } else {
                    format!("`{name}` is already imported")
                }
            }
            TypeError::InfiniteType { .. } => String::from("infinite type"),
            TypeError::AssignToImmutable { name } => format!("can't assign twice to immutable variable `{name}`"),
            TypeError::NotAssignable => String::from("invalid left-hand side of assignment"),
            TypeError::MissingTraitFunc { trait_ref, name } => {
                format!("missing implementation of `{}::{name}`", tcx.def_registry.traits[*trait_ref].name)
            }
            TypeError::NotTraitMember { trait_ref, name } => {
                format!("`{name}` is not a member of trait `{}`", tcx.def_registry.traits[*trait_ref].name)
            }
            TypeError::NotIterable { ty } => format!("`{}` can't be iterated", tcx.display_of(*ty)),
            TypeError::InvalidOperator { operator, ty } => format!("operator `{operator}` can't be applied to `{}`", tcx.display_of(*ty)),
            TypeError::InvalidUnaryOperator { operator, ty } => format!("operator `{operator}` can't be applied to `{}`", tcx.display_of(*ty)),
            TypeError::Parse { .. } => String::from("syntax error"),
            TypeError::LocalDeclaration => String::from("declarations are only allowed at the top level of a module"),
            TypeError::TopLevelCode => String::from("only the root module can contain top-level code"),
            TypeError::GenericMethod => String::from("functions of trait impls can't have their own generic parameters"),
            TypeError::NotFormattable { ty } => format!("`{}` can't be interpolated into a string", tcx.display_of(*ty)),
            TypeError::NonExhaustive { missing } => format!(
                "`match` doesn't handle {}",
                missing.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
            ),
            TypeError::NotContainer { ty } => format!("`{}` can't have children", tcx.display_of(*ty)),
            TypeError::TooManyChildren { found } => format!("expected a single child, found {found}"),
            TypeError::PatternMismatch { ty } => format!("pattern can't match values of `{}`", tcx.display_of(*ty)),
            TypeError::NotTryable { ty } => format!("`?` can't be applied to `{}`", tcx.display_of(*ty)),
            TypeError::ReturnOutsideFunction => String::from("`return` outside of a function"),
            other => other.message(tcx),
        }
    }

    /// Labels of the primary and secondary spans, for the spans that are
    /// known.
    pub fn labels(&self, tcx: &TyCtxt) -> Vec<(ModuleSpan, String)> {
        let primary = match &self.error {
            TypeError::Unexpected { found, .. } => match found {
                TypeErrorValue::Nothing => String::new(),
                found => format!("found {}", found.display(tcx)),
            },
            TypeError::NoField { adt, variant, name } => {
                let adt_type = &tcx.def_registry.adt_types[*adt];
                let variant_name = if adt_type.kind == AdtKind::Enum {
                    adt_type.variants[*variant].name.as_deref().map_or_default(|name| format!("::{name}"))
                } else {
                    String::new()
                };

                format!(
                    "`{}{variant_name}` doesn't have field called `{name}`",
                    adt_type.name.as_deref().unwrap_or_default()
                )
            }
            TypeError::MissingField { adt, variant, name } => {
                let adt_type = &tcx.def_registry.adt_types[*adt];
                let variant_name = adt_type.variants[*variant].name.as_deref().map_or_default(|name| format!("::{name}"));

                format!(
                    "`{}{variant_name}` requires a value for field `{name}`",
                    adt_type.name.as_deref().unwrap_or_default()
                )
            }
            &TypeError::VariantRequired(adt) => format!(
                "`{}` requires to specify variant explicitly",
                tcx.def_registry.adt_types[adt].name.as_deref().unwrap_or_default()
            ),
            TypeError::NonIndexable { ty, name } => format!("`{}` can't have fields and be indexed by `{name}`", tcx.display_of(*ty)),
            TypeError::NotFound { .. } => String::from("tried to access here"),
            TypeError::NoFunction { postfix: true, .. } | TypeError::NotPostfix { .. } => String::from("tried to use here"),
            TypeError::NoFunction { postfix: false, .. } => String::from("tried to call here"),
            TypeError::AlreadyExists { primary, .. } => String::from(match primary {
                DefinitionType::Local => "declared here",
                DefinitionType::Import => "imported here",
            }),
            TypeError::NonConstantEvaluable => String::from("this expression"),
            TypeError::ArgumentCountMismatch { expected, found, func } => func.map_or_else(
                || format!("expected {expected} arguments, found {found}"),
                |func| {
                    format!(
                        "expected {expected} arguments by function signature `{}` here, found {found}",
                        tcx.display_of(func)
                    )
                },
            ),
            TypeError::InfiniteType { ty } => format!("type of this would have to contain itself: `{}`", tcx.display_of(*ty)),
            TypeError::AssignToImmutable { .. } => String::from("assigned here, declare it with `let mut` to make it mutable"),
            TypeError::NotAssignable => String::from("only variables, fields and array elements can be assigned to"),
            TypeError::MissingTraitFunc { name, .. } => format!("`{name}` is not implemented here"),
            TypeError::NotTraitMember { .. } => String::from("defined here"),
            TypeError::NotIterable { .. } => String::from("this value doesn't implement the iterable trait"),
            TypeError::InvalidUnaryOperator { .. } | TypeError::InvalidOperator { .. } => String::from("used here"),
            TypeError::Parse { message } => message.clone(),
            TypeError::LocalDeclaration => String::from("move this declaration out of the block"),
            TypeError::TopLevelCode => String::from("move this into a function"),
            TypeError::GenericMethod => String::from("trait objects can't call them; add the generic parameter to the impl block instead"),
            TypeError::NotFormattable { .. } => String::from("only strings, numbers and booleans can be interpolated"),
            TypeError::NonExhaustive { .. } => String::from("add the missing patterns, or `_ => ...` to handle the rest"),
            TypeError::NotContainer { .. } => String::from("implement `Container` for this type, or declare it as a `view` with `children`"),
            TypeError::TooManyChildren { .. } => String::from("this type accepts only one child"),
            TypeError::PatternMismatch { .. } => String::from("this pattern"),
            TypeError::NotTryable { .. } => {
                String::from("`?` works on a `Result` in a function returning a `Result`, or on an `Option` in a function returning an `Option`")
            }
            TypeError::ReturnOutsideFunction => String::from("only functions and closures can return"),
            TypeError::BreakOutsideLoop => String::from("only loops can be left with `break` and `continue`"),
            TypeError::ExternOutsideStub => String::from("give the function a body"),
            TypeError::UnknownVariant { name } => format!("write it with its enum, like `Shape::{name}`, or use a lowercase name to bind the value"),
            TypeError::InstantiationLimit => String::from("a generic function here calls itself with ever larger types, or there are too many instances"),
            TypeError::Unavailable { .. } => String::from("the host doesn't allow this addon to use it"),
            TypeError::ConstCycle { .. }
            | TypeError::UnknownLabel { .. }
            | TypeError::NoDefault { .. }
            | TypeError::UnknownArgument { .. }
            | TypeError::DuplicateArgument { .. } => String::from("used here"),
            TypeError::BreakValueOutsideLoop => String::from("`while` and `for` may end without `break`, use `loop` instead"),
            TypeError::UnsatisfiedBound { .. } => String::from("required by a bound of this generic parameter"),
            TypeError::SuperOutsideTraitImpl => String::from("only overrides of trait functions can call their defaults"),
            TypeError::MissingArgument { .. } => String::from("in this call"),
            TypeError::PositionalAfterNamed => String::from("move it before named arguments"),
            TypeError::NamedArgumentsNotSupported => String::from("pass the arguments by position"),
            TypeError::InvalidFormat { .. } => String::from("this format"),
            TypeError::MissingLangItem { .. } => String::from("needed by this expression"),
            TypeError::NotAMethod { name } => format!("call it by the path of its type, like `Type::{name}(...)`"),
            TypeError::RecursiveValueType { .. } => String::from("make the field a regular `struct` or an array to break the cycle"),
            TypeError::InvalidMutSelf => String::from("other functions take `self` by reference already"),
        };

        let secondary = match &self.error {
            TypeError::AlreadyExists { primary, secondary, .. } => String::from(match secondary {
                DefinitionType::Local if *primary == DefinitionType::Local => "and also declared here",
                DefinitionType::Local => "and declared here",
                DefinitionType::Import if *primary == DefinitionType::Import => "and also imported here",
                DefinitionType::Import => "and imported here",
            }),
            _ => String::new(),
        };

        self.primary_span
            .map(|span| (span, primary))
            .into_iter()
            .chain(self.secondary_span.map(|span| (span, secondary)))
            .collect()
    }

    /// Adds the message and labels of the diagnostic to an `ariadne` report.
    #[cfg(feature = "ariadne")]
    pub fn add_to_report(&self, report: &mut ariadne::ReportBuilder<'_, ModuleSpan>, tcx: &TyCtxt) {
        report.set_message(self.message(tcx));

        for (span, label) in self.labels(tcx) {
            let label_builder = ariadne::Label::new(span).with_color(ariadne::Color::Cyan);

            report.add_label(if label.is_empty() { label_builder } else { label_builder.with_message(label) });
        }
    }
}

impl TypeErrorValue {
    /// Displays the value as it's shown in diagnostics, e.g. `` `i32` `` or
    /// `` `struct` ``.
    pub fn display<'a>(&'a self, tcx: &'a TyCtxt) -> impl fmt::Display + 'a {
        fmt::from_fn(move |f| match self {
            Self::Type => f.write_str("`type`"),
            Self::Adt(adt_kind) => f.write_str(match adt_kind {
                SpecialAdtKind::Specific(AdtKind::Struct) => "`struct`",
                SpecialAdtKind::Specific(AdtKind::View) => "`view`",
                SpecialAdtKind::Specific(AdtKind::Enum) => "`enum`",
                SpecialAdtKind::WithExpectation(AdtKind::Struct) => "`enum` or `view`",
                SpecialAdtKind::WithExpectation(AdtKind::View) => "`struct` or `enum`",
                SpecialAdtKind::WithExpectation(AdtKind::Enum) => "`struct` or `view`",
                SpecialAdtKind::AnyOf => "`struct`, `view` or `enum`",
            }),
            Self::Trait => f.write_str("`trait`"),
            Self::Value => f.write_str("`value`"),
            Self::Function => f.write_str("`function`"),
            Self::Array(Some(size)) => write!(f, "array of {size} elements"),
            Self::Array(None) => f.write_str("`array`"),
            Self::Module => f.write_str("`module`"),
            Self::Generic => f.write_str("`generic`"),
            Self::PrimitiveType(ty) => write!(f, "`{ty}`"),
            Self::Nothing => f.write_str("nothing"),
            &Self::ExplicitType(type_ref) => write!(f, "`{}`", tcx.display_of(type_ref)),
        })
    }
}

/// Plain-text display of a diagnostic, without access to the source:
///
/// ```text
/// error: expected `i32`
///   --> geometry::shapes:3:15: found `f32`
/// ```
///
/// Lines and columns start from 1.
pub struct DiagnosticDisplay<'a> {
    diagnostic: &'a Diagnostic,
    tcx: &'a TyCtxt,
}

impl<'a> DiagnosticDisplay<'a> {
    pub const fn new(diagnostic: &'a Diagnostic, tcx: &'a TyCtxt) -> Self {
        Self { diagnostic, tcx }
    }
}

impl fmt::Display for DiagnosticDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.diagnostic.message(self.tcx))?;

        for (ModuleSpan(module, span), label) in self.diagnostic.labels(self.tcx) {
            f.write_str("\n  --> ")?;

            write!(f, "{}", self.tcx.display_of_module(module))?;

            write!(f, ":{}:{}", span.range.start_line + 1, span.range.start_column + 1)?;

            if !label.is_empty() {
                write!(f, ": {label}")?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct DiagnosticContext {
    pub errors: IndexVec<TypeErrorRef, Diagnostic>,
}

impl DiagnosticContext {
    pub fn report(&mut self, error: Diagnostic) -> TypeErrorRef {
        self.errors.insert(error)
    }

    /// Reports `error` located at `span`.
    pub fn error(&mut self, error: TypeError, span: ModuleSpan) -> TypeErrorRef {
        self.report(Diagnostic::new(error).with_primary_span(span.0, span.1))
    }

    pub const fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}
