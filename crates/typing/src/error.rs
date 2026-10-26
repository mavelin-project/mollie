use mollie_shared::{Operator, UnaryOperator};

use crate::{AdtKind, AdtRef, AdtVariantRef, ModuleId, PrimitiveType, TraitRef, TyCtxt, TypeRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialAdtKind {
    AnyOf,
    Specific(AdtKind),
    WithExpectation(AdtKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeErrorValue {
    Type,
    Array(Option<usize>),
    Adt(SpecialAdtKind),
    Trait,
    Value,
    Function,
    Module,
    Generic,
    Nothing,
    PrimitiveType(PrimitiveType),
    ExplicitType(TypeRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupType {
    Variable,
    Type { inside: ModuleId },
    Module { inside: ModuleId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionType {
    Local,
    Import,
}

#[derive(Debug, Clone)]
pub enum TypeError {
    Unexpected {
        expected: TypeErrorValue,
        found: TypeErrorValue,
    },
    VariantRequired(AdtRef),
    NoField {
        adt: AdtRef,
        variant: AdtVariantRef,
        name: String,
    },
    MissingField {
        adt: AdtRef,
        variant: AdtVariantRef,
        name: String,
    },
    NoFunction {
        name: String,
        postfix: bool,
    },
    NonIndexable {
        ty: TypeRef,
        name: String,
    },
    NotFound {
        name: String,
        was_looking_for: LookupType,
    },
    NotPostfix {
        name: String,
    },
    ArgumentCountMismatch {
        expected: usize,
        found: usize,
        func: Option<TypeRef>,
    },
    NonConstantEvaluable,
    AlreadyExists {
        name: String,
        primary: DefinitionType,
        secondary: DefinitionType,
    },
    InfiniteType {
        ty: TypeRef,
    },
    AssignToImmutable {
        name: String,
    },
    NotAssignable,
    MissingTraitFunc {
        trait_ref: TraitRef,
        name: String,
    },
    NotTraitMember {
        trait_ref: TraitRef,
        name: String,
    },
    NotIterable {
        ty: TypeRef,
    },
    InvalidOperator {
        operator: Operator,
        ty: TypeRef,
    },
    InvalidUnaryOperator {
        operator: UnaryOperator,
        ty: TypeRef,
    },
    Parse {
        message: String,
    },
    LocalDeclaration,
    TopLevelCode,
    GenericMethod,
    NotFormattable {
        ty: TypeRef,
    },
    NonExhaustive {
        missing: Vec<String>,
    },
    NotContainer {
        ty: TypeRef,
    },
    TooManyChildren {
        found: usize,
    },
    ConditionalChild,
    InvalidProperty {
        message: String,
    },
    RepeatedChild,
    PatternMismatch {
        ty: TypeRef,
    },
    NotTryable {
        ty: TypeRef,
    },
    ReturnOutsideFunction,
    ConstCycle {
        name: String,
    },
    InstantiationLimit,
    ExternOutsideStub,
    Unavailable {
        name: String,
    },
    Private {
        name: String,
    },
    UnknownVariant {
        name: String,
    },
    BreakOutsideLoop,
    UnknownLabel {
        name: String,
    },
    BreakValueOutsideLoop,
    UnsatisfiedBound {
        ty: TypeRef,
        bound: TypeRef,
    },
    NoDefault {
        name: String,
    },
    SuperOutsideTraitImpl,
    UnknownArgument {
        name: String,
    },
    DuplicateArgument {
        name: String,
    },
    MissingArgument {
        name: String,
    },
    PositionalAfterNamed,
    NamedArgumentsNotSupported,
    InvalidFormat {
        ty: TypeRef,
        spec: String,
    },
    MissingLangItem {
        name: &'static str,
    },
    NotAMethod {
        name: String,
    },
    RecursiveValueType {
        adt: AdtRef,
    },
    InvalidMutSelf,
    InvalidArray {
        message: String,
    },
    TypeExpected {
        name: String,
    },
    MissingValue {
        name: String,
    },
    ExternVariableOutsideStub,
    ExternVariableValue,
    UsedBeforeAssigned {
        name: String,
    },
}

impl TypeError {
    pub fn message(&self, tcx: &TyCtxt) -> String {
        match self {
            Self::Unexpected { expected, found } => match (expected, found) {
                (TypeErrorValue::Nothing, TypeErrorValue::Nothing) => String::from("unexpected value"),
                (TypeErrorValue::Nothing, found) => format!("unexpected {}", found.display(tcx)),
                (expected, found) => format!("expected {}, but found {}", expected.display(tcx), found.display(tcx)),
            },
            Self::VariantRequired(_) => String::from("can't construct"),
            Self::NoField { .. } => String::from("no field"),
            Self::MissingField { .. } => String::from("missing field"),
            Self::NoFunction { name, postfix: true } => format!("there's no postfix function called `{name}`"),
            Self::NoFunction { name, postfix: false } => format!("there's no function called `{name}`"),
            Self::NonIndexable { .. } => String::from("non-indexable value"),
            Self::NotFound { name, was_looking_for } => match *was_looking_for {
                LookupType::Variable => format!("there's no variable called `{name}`"),
                LookupType::Type { inside } if tcx.def_registry.is_root(inside) => format!("there's no type called `{name}`"),
                LookupType::Type { inside } => format!("there's no type called `{name}` in `{}`", tcx.def_registry.modules[inside].name),
                LookupType::Module { inside } if tcx.def_registry.is_root(inside) => format!("there's no module called `{name}`"),
                LookupType::Module { inside } => format!("there's no module called `{name}` in `{}`", tcx.def_registry.modules[inside].name),
            },
            Self::NotPostfix { name } => format!("`{name}` can't be used in postfix context"),
            Self::ArgumentCountMismatch { expected, found, func } => {
                let amount = if found > expected { "more" } else { "less" };

                func.map_or_else(
                    || format!("received {amount} arguments than was expected"),
                    |func| format!("received {amount} arguments than was expected by function signature: {}", tcx.display_of(func)),
                )
            }
            Self::NonConstantEvaluable => String::from("expression can't be evaluated at compile-time"),
            Self::AlreadyExists { name, primary, secondary } => {
                if *primary == DefinitionType::Local || *secondary == DefinitionType::Local {
                    format!("`{name}` is already declared")
                } else {
                    format!("`{name}` is already imported")
                }
            }
            Self::InfiniteType { .. } => String::from("infinite type"),
            Self::AssignToImmutable { name } => format!("can't assign twice to immutable variable `{name}`"),
            Self::NotAssignable => String::from("invalid left-hand side of assignment"),
            Self::MissingTraitFunc { trait_ref, name } => {
                format!("missing implementation of `{}::{name}`", tcx.def_registry.traits[*trait_ref].name)
            }
            Self::NotTraitMember { trait_ref, name } => {
                format!("`{name}` is not a member of trait `{}`", tcx.def_registry.traits[*trait_ref].name)
            }
            Self::NotIterable { ty } => format!("`{}` can't be iterated", tcx.display_of(*ty)),
            Self::InvalidOperator { operator, ty } => format!("operator `{operator}` can't be applied to `{}`", tcx.display_of(*ty)),
            Self::InvalidUnaryOperator { operator, ty } => format!("operator `{operator}` can't be applied to `{}`", tcx.display_of(*ty)),
            Self::Parse { .. } => String::from("syntax error"),
            Self::LocalDeclaration => String::from("declarations are only allowed at the top level of a module"),
            Self::TopLevelCode => String::from("only the root module can contain top-level code"),
            Self::GenericMethod => String::from("functions of trait impls can't have their own generic parameters"),
            Self::NotFormattable { ty } => format!("`{}` can't be interpolated into a string", tcx.display_of(*ty)),
            Self::NonExhaustive { missing } => format!(
                "`match` doesn't handle {}",
                missing.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
            ),
            Self::NotContainer { ty } => format!("`{}` can't have children", tcx.display_of(*ty)),
            Self::TooManyChildren { found } => format!("expected a single child, found {found}"),
            Self::ConditionalChild => String::from("the only child can't be given under a condition without `else`"),
            Self::InvalidProperty { message } | Self::InvalidArray { message } => message.clone(),
            Self::RepeatedChild => String::from("`for` can't give the only child of a container"),
            Self::PatternMismatch { ty } => format!("pattern can't match values of `{}`", tcx.display_of(*ty)),
            Self::NotTryable { ty } => format!("`?` can't be applied to `{}`", tcx.display_of(*ty)),
            Self::ReturnOutsideFunction => String::from("`return` outside of a function"),
            Self::ConstCycle { name } => format!("constant `{name}` depends on itself"),
            Self::InstantiationLimit => String::from("generic code is instantiated with too large types, or too many times"),
            Self::ExternOutsideStub => String::from("only stubs of the host's API can declare `extern` functions"),
            Self::UnknownVariant { name } => format!("`{name}` isn't a variant of the matched value"),
            Self::Unavailable { name } => format!("`{name}` isn't available to this addon"),
            Self::Private { name } => format!("`{name}` is private to its package"),
            Self::BreakOutsideLoop => String::from("`break` or `continue` outside of a loop"),
            Self::UnknownLabel { name } => format!("there's no loop labeled `'{name}`"),
            Self::BreakValueOutsideLoop => String::from("only `loop` can produce a value with `break`"),
            Self::UnsatisfiedBound { ty, bound } => format!("`{}` doesn't implement `{}`", tcx.display_of(*ty), tcx.display_of(*bound)),
            Self::NoDefault { name } => format!("trait function `{name}` has no default implementation"),
            Self::SuperOutsideTraitImpl => String::from("`super` outside of a function of a trait impl"),
            Self::UnknownArgument { name } => format!("there's no parameter called `{name}`"),
            Self::DuplicateArgument { name } => format!("argument `{name}` is given twice"),
            Self::MissingArgument { name } => format!("missing argument `{name}`"),
            Self::PositionalAfterNamed => String::from("positional argument after a named one"),
            Self::NamedArgumentsNotSupported => String::from("function values can't take named arguments"),
            Self::InvalidFormat { ty, spec } => format!("format `{spec}` can't be applied to `{}`", tcx.display_of(*ty)),
            Self::MissingLangItem { name } => format!("`{name}` of the standard library isn't available"),
            Self::NotAMethod { name } => format!("`{name}` doesn't take `self`, so it can't be called on a value"),
            Self::RecursiveValueType { adt } => format!(
                "value type `{}` contains itself, so it would never end",
                tcx.def_registry.adt_types[*adt].name
            ),
            Self::InvalidMutSelf => String::from("`mut self` is only allowed in functions of inherent impls of value types"),
            Self::TypeExpected { name } => format!("`extern let {name}` needs a type"),
            Self::MissingValue { name } => format!("`{name}` needs a type or a value"),
            Self::ExternVariableOutsideStub => String::from("only stubs of the host's API can declare `extern` variables"),
            Self::ExternVariableValue => String::from("`extern` variables can't have a value"),
            Self::UsedBeforeAssigned { name } => format!("`{name}` is used before it's assigned"),
        }
    }
}
