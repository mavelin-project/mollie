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
    /// A field that has no default value wasn't given a value when
    /// constructing an ADT.
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
    /// Unifying would create a type that contains itself.
    InfiniteType {
        ty: TypeRef,
    },
    /// Assignment to a variable declared with `const`.
    AssignToImmutable {
        name: String,
    },
    /// Left side of an assignment isn't a variable, a field or an array
    /// element.
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
    /// Declaration (struct, function, impl, ...) inside a block. Declarations
    /// are only allowed at the top level of a module.
    LocalDeclaration,
    /// Executable statement at the top level of a module other than the root
    /// one.
    TopLevelCode,
    /// Function of a trait impl with its own generic parameters, which trait
    /// objects couldn't call.
    GenericMethod,
    /// Value interpolated into a string that can't be converted to a string.
    NotFormattable {
        ty: TypeRef,
    },
    /// `match` that doesn't handle every possible value.
    NonExhaustive {
        /// Names of variants (or values) that aren't handled.
        missing: Vec<String>,
    },
    /// Children given to a type that doesn't implement `Container`.
    NotContainer {
        ty: TypeRef,
    },
    /// Several children given to a container of a single child.
    TooManyChildren {
        found: usize,
    },
    /// Pattern that can't match values of the matched type.
    PatternMismatch {
        ty: TypeRef,
    },
    /// `?` applied to a value that isn't a `Result` or an `Option`, or in a
    /// function that doesn't return the same kind of value.
    NotTryable {
        ty: TypeRef,
    },
    /// `return` outside of a function, like in a default value of a field.
    ReturnOutsideFunction,
    /// A constant whose value depends on itself.
    ConstCycle {
        name: String,
    },
    /// Generic functions and types instantiated with too large types, or too
    /// many instances (like a generic function calling itself with a bigger
    /// type every time).
    InstantiationLimit,
    /// `extern func` outside of a stub of the host's API.
    ExternOutsideStub,
    /// A module of the host the program wasn't compiled with (a capability,
    /// see `DefRegistry::restrict`).
    Unavailable {
        name: String,
    },
    /// A capitalized name in a pattern that isn't a variant of the matched
    /// value (or whose enum isn't known there).
    UnknownVariant {
        name: String,
    },
    /// `break` or `continue` outside of a loop (or inside a closure in a
    /// loop).
    BreakOutsideLoop,
    /// `break 'label` or `continue 'label` without a loop with that label.
    UnknownLabel {
        name: String,
    },
    /// `break value` out of a `while` or `for` loop, which can't produce a
    /// value.
    BreakValueOutsideLoop,
    /// A type argument that doesn't implement the bounds of its generic
    /// parameter.
    UnsatisfiedBound {
        ty: TypeRef,
        /// The bound, as a trait type with its arguments (`Source<i32>`).
        bound: TypeRef,
    },
    /// `super.name()` of a trait function without a default implementation.
    NoDefault {
        name: String,
    },
    /// `super.name()` outside of a function of a trait impl.
    SuperOutsideTraitImpl,
    /// A named argument that isn't a parameter of the function.
    UnknownArgument {
        name: String,
    },
    /// An argument given twice, by position and by name, or twice by name.
    DuplicateArgument {
        name: String,
    },
    /// A parameter without a value or a default value.
    MissingArgument {
        name: String,
    },
    /// A positional argument after a named one.
    PositionalAfterNamed,
    /// Named arguments for a function value, whose parameters have no names.
    NamedArgumentsNotSupported,
    /// A format specifier that can't be applied to the value.
    InvalidFormat {
        ty: TypeRef,
        spec: String,
    },
    /// An item of the standard library used by the syntax (e.g. `Range` of
    /// `start..end`) isn't available.
    MissingLangItem {
        name: &'static str,
    },
    /// `value.name()` of a function without `self`.
    NotAMethod {
        name: String,
    },
    /// A value type containing itself, which would have an infinite size.
    RecursiveValueType {
        adt: AdtRef,
    },
    /// `mut self` in a function of a trait impl, or of a type that isn't a
    /// value type.
    InvalidMutSelf,
    // InvalidPostfixFunction { reasons: Vec<Positioned<PostfixRequirement>> },
    // Parse(mollie_parser::ParseError),
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
            Self::PatternMismatch { ty } => format!("pattern can't match values of `{}`", tcx.display_of(*ty)),
            Self::NotTryable { ty } => format!("`?` can't be applied to `{}`", tcx.display_of(*ty)),
            Self::ReturnOutsideFunction => String::from("`return` outside of a function"),
            Self::ConstCycle { name } => format!("constant `{name}` depends on itself"),
            Self::InstantiationLimit => String::from("generic code is instantiated with too large types, or too many times"),
            Self::ExternOutsideStub => String::from("only stubs of the host's API can declare `extern` functions"),
            Self::UnknownVariant { name } => format!("`{name}` isn't a variant of the matched value"),
            Self::Unavailable { name } => format!("`{name}` isn't available to this addon"),
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
                tcx.def_registry.adt_types[*adt].name.as_deref().unwrap_or_default()
            ),
            Self::InvalidMutSelf => String::from("`mut self` is only allowed in functions of inherent impls of value types"),
        }
    }
}
