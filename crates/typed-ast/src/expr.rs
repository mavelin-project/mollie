use std::{iter::once, mem};

use derive_where::derive_where;
use mollie_const::ConstantValue;
use mollie_index::{Idx, IndexBoxedSlice};
use mollie_shared::{FormatKind, FormatSpec, LangItem, Operator, Positioned, Span, UnaryOperator};
use mollie_typing::{
    AdtKind, AdtRef, AdtTypeInfo, AdtVariantRef, Arg, ArgType, ArrayTypeInfo, ConstRef, FieldRef, FuncRef, FuncTypeInfo, ImplRef, IntType, LookupType,
    ModuleId, ModuleItem, ModuleSpan, PrimitiveType, SpecialAdtKind, TraitFuncRef, TraitRef, TraitTypeInfo, TyCtxt, Type, TypeError, TypeErrorRef,
    TypeErrorValue, TypeInfo, TypeInfoRef, TypeSolver, UIntType, UnifyArgs, VFuncRef,
};

use crate::{
    ConstantContext, Descriptor, FirstPass, FromParsed, IntoConstVal, SolvedPass, TypedAST, TypedASTContextRef, UsedItem,
    block::{Block, BlockRef},
    module_map::evaluate_constant,
    stmt::{Stmt, StmtRef},
    ty::TypePathResult,
};

/// Parameters of a called function that is known where it's called, so its
/// arguments can be named and have defaults.
struct KnownParams {
    /// Names of parameters passed in parentheses (without the receiver of a
    /// method).
    names: Box<[String]>,
    /// Functions computing defaults of parameters.
    defaults: Box<[Option<FuncRef>]>,
    /// Whether defaults take the receiver (`self`) first.
    receiver: bool,
    /// Whether the method takes `mut self`: its receiver is a place it gives
    /// the changed value back to, so it isn't moved into a variable.
    mut_self: bool,
    /// Type arguments of the defaults: those of the call for functions,
    /// otherwise inferred from what they're given.
    type_args: Option<Box<[TypeInfoRef]>>,
}

mollie_index::new_idx_type!(ExprRef);
mollie_index::new_idx_type!(LoopId);

#[derive(Debug, Clone)]
pub enum LitExpr {
    Bool(bool),
    F32(f32),
    Int(i64),
    String(String),
}

#[derive_where(Debug, Clone; D::Type)]
pub enum IsPattern<D: Descriptor> {
    Literal(ExprRef),
    /// `_`, matching anything.
    Wildcard,
    /// A name matching anything, bound to the value.
    Binding {
        name: String,
        ty: D::Type,
    },
    /// A variant of an enum, or a struct, with patterns of some of its fields.
    /// Fields without a pattern are bound to variables named like them.
    EnumVariant {
        adt: AdtRef,
        adt_variant: AdtVariantRef,
        adt_type_args: Box<[D::Type]>,
        values: Box<[(FieldRef, String, Option<Self>)]>,
    },
    /// A trait object (or a value) of a specific type, bound to `name`:
    /// `shape is Circle circle`.
    TypeName {
        ty: D::Type,
        name: String,
    },
}

impl IsPattern<FirstPass> {
    /// The ADT of `ty`, with its type arguments, if it's known to be an ADT.
    fn adt_of(context: &TypedASTContextRef<'_>, ty: TypeInfoRef) -> Option<(AdtRef, Box<[TypeInfoRef]>)> {
        match &context.type_solver.type_infos[context.type_solver.get_info(ty)].value {
            TypeInfo::Adt(adt) => Some((adt.id, adt.type_args.clone())),
            _ => None,
        }
    }

    /// The variant called `name` of `adt`, if it's an enum.
    fn variant_named(context: &TypedASTContextRef<'_>, adt: AdtRef, name: &str) -> Option<AdtVariantRef> {
        let adt = &context.type_solver.context.def_registry.adt_types[adt];

        if adt.kind != AdtKind::Enum {
            return None;
        }

        adt.variants
            .iter()
            .find_map(|(variant_ref, variant)| (variant.name.as_deref() == Some(name)).then_some(variant_ref))
    }

    /// Lowers a pattern matching values of type `expected`, declaring its
    /// bindings in the current scope.
    pub fn lower(
        pattern: mollie_parser::IsPattern,
        expected: TypeInfoRef,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> Self {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || {
            match pattern {
                mollie_parser::IsPattern::Literal(literal) => {
                    let literal = Expr::from_parsed(literal, ast, context, span);

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected,
                        found: ast[literal].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, span));
                    }

                    Self::Literal(literal)
                }
                mollie_parser::IsPattern::Wildcard => Self::Wildcard,
                mollie_parser::IsPattern::Type { ty, pattern } => {
                    let single_name = match ty.value.segments.as_slice() {
                        [segment] if segment.value.args.is_none() => Some(segment.value.name.value.0.clone()),
                        _ => None,
                    };

                    if let Some(name) = single_name {
                        // A single name is first looked up among variants of
                        // the matched enum: `Some {
                        // value }`, `None`.
                        if let Some((adt, type_args)) = Self::adt_of(context, expected)
                            && let Some(variant) = Self::variant_named(context, adt, &name)
                        {
                            return Self::variant(adt, type_args, variant, pattern, ast, context, span);
                        }

                        // Like in Rust's prelude, `Some`, `None`, `Ok` and
                        // `Err` are variants of
                        // `Option` and `Result` by their names, whether
                        // the type of the value is known yet or not (it's then
                        // inferred from the pattern).
                        let registry = &context.type_solver.context;
                        let prelude_variant = [LangItem::Option, LangItem::Result]
                            .into_iter()
                            .filter_map(|item| registry.get_adt_item(item))
                            .find_map(|adt| Self::variant_named(context, adt, &name).map(|variant| (adt, variant)));

                        if let Some((adt, variant)) = prelude_variant {
                            let generics = context.type_solver.context.def_registry.adt_types[adt].generics;
                            let type_args: Box<[_]> = (0..generics).map(|_| context.type_solver.add_unknown(None, Some(span))).collect();
                            let found = context.type_solver.add_info(
                                TypeInfo::Adt(AdtTypeInfo {
                                    id: adt,
                                    type_args: type_args.clone(),
                                }),
                                Some(span),
                            );

                            if context.type_solver.unify(UnifyArgs { expected, found }).is_err() {
                                let ty = context.type_solver.solve(expected);

                                context.type_solver.error(TypeError::PatternMismatch { ty }, ModuleSpan(ast.module, span));
                            }

                            return Self::variant(adt, type_args, variant, pattern, ast, context, span);
                        }

                        // Otherwise, a single name without fields binds the
                        // value, if it's written like a
                        // binding. A capitalized name is a
                        // variant that isn't one of the matched value (or whose
                        // enum isn't known): binding the value instead would
                        // match anything.
                        if pattern.is_none() {
                            if name.starts_with(|character: char| character.is_uppercase()) {
                                context.type_solver.error(TypeError::UnknownVariant { name }, ModuleSpan(ast.module, ty.span));

                                return Self::Wildcard;
                            }

                            ast.declare_var(context, name.clone(), expected, true, ty.span);

                            return Self::Binding { name, ty: expected };
                        }
                    }

                    match TypePathResult::from_parsed(ty.value, ast, context, ty.span) {
                        TypePathResult::Adt(adt, type_args, variant) => {
                            let kind = context.type_solver.context.def_registry.adt_types[adt].kind;
                            let variant = match (variant, kind, &pattern) {
                                (Some(variant), ..) => variant,
                                // `Circle circle`: a value of a type, bound to a name.
                                (
                                    None,
                                    _,
                                    Some(Positioned {
                                        value: mollie_parser::TypePattern::Name(name),
                                        span: name_span,
                                    }),
                                ) => {
                                    let ty = context.type_solver.add_info(TypeInfo::Adt(AdtTypeInfo { id: adt, type_args }), Some(span));

                                    ast.declare_var(context, name.0.clone(), ty, true, *name_span);

                                    return Self::TypeName { ty, name: name.0.clone() };
                                }
                                (None, AdtKind::Enum, _) => {
                                    context.type_solver.error(TypeError::VariantRequired(adt), ModuleSpan(ast.module, span));

                                    return Self::Wildcard;
                                }
                                (None, ..) => AdtVariantRef::ZERO,
                            };

                            let found = context.type_solver.add_info(
                                TypeInfo::Adt(AdtTypeInfo {
                                    id: adt,
                                    type_args: type_args.clone(),
                                }),
                                Some(span),
                            );

                            if context.type_solver.unify(UnifyArgs { expected, found }).is_err() {
                                let ty = context.type_solver.solve(expected);

                                context.type_solver.error(TypeError::PatternMismatch { ty }, ModuleSpan(ast.module, span));
                            }

                            Self::variant(adt, type_args, variant, pattern, ast, context, span)
                        }
                        // Already reported.
                        TypePathResult::Error(..) => Self::Wildcard,
                        _ => {
                            let ty = context.type_solver.solve(expected);

                            context.type_solver.error(TypeError::PatternMismatch { ty }, ModuleSpan(ast.module, span));

                            Self::Wildcard
                        }
                    }
                }
            }
        })
    }

    /// Lowers patterns of fields of a variant (or a struct).
    #[allow(clippy::too_many_arguments)]
    fn variant(
        adt: AdtRef,
        adt_type_args: Box<[TypeInfoRef]>,
        adt_variant: AdtVariantRef,
        pattern: Option<Positioned<mollie_parser::TypePattern>>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> Self {
        let fields = context.type_solver.instantiate_adt(adt, adt_variant, &adt_type_args).collect::<Box<[_]>>();
        let mut values = Vec::new();

        match pattern.map(|pattern| pattern.value) {
            None => (),
            Some(mollie_parser::TypePattern::Values(patterns)) => {
                for field_pattern in patterns {
                    let name_span = field_pattern.value.name.span;
                    let name = field_pattern.value.name.value.0;
                    let field = fields
                        .iter()
                        .find(|&&(field, _)| context.type_solver.context.def_registry.adt_types[adt].variants[adt_variant].fields[field].name == name);

                    let Some(&(field, field_ty)) = field else {
                        context.type_solver.error(
                            TypeError::NoField {
                                adt,
                                variant: adt_variant,
                                name,
                            },
                            ModuleSpan(ast.module, field_pattern.span),
                        );

                        continue;
                    };

                    let nested = field_pattern
                        .value
                        .value
                        .map(|nested| Self::lower(nested.value, field_ty, ast, context, nested.span));

                    if nested.is_none() {
                        ast.declare_var(context, name.clone(), field_ty, true, name_span);
                    }

                    values.push((field, name, nested));
                }
            }
            Some(mollie_parser::TypePattern::Name(_)) => {
                let ty = context.type_solver.add_info(
                    TypeInfo::Adt(AdtTypeInfo {
                        id: adt,
                        type_args: adt_type_args.clone(),
                    }),
                    Some(span),
                );
                let ty = context.type_solver.solve(ty);

                context.type_solver.error(TypeError::PatternMismatch { ty }, ModuleSpan(ast.module, span));
            }
        }

        Self::EnumVariant {
            adt,
            adt_variant,
            adt_type_args,
            values: values.into_boxed_slice(),
        }
    }

    /// Whether the pattern matches every value of its type.
    fn is_irrefutable(&self, context: &TypedASTContextRef<'_>) -> bool {
        match self {
            Self::Wildcard | Self::Binding { .. } => true,
            Self::Literal(_) | Self::TypeName { .. } => false,
            Self::EnumVariant { adt, values, .. } => {
                let adt = &context.type_solver.context.def_registry.adt_types[*adt];

                (adt.kind != AdtKind::Enum || adt.variants.len() == 1) && Self::fields_are_irrefutable(values, context)
            }
        }
    }

    fn fields_are_irrefutable(values: &[(FieldRef, String, Option<Self>)], context: &TypedASTContextRef<'_>) -> bool {
        values
            .iter()
            .all(|(.., nested)| nested.as_ref().is_none_or(|nested| nested.is_irrefutable(context)))
    }

    /// Values of type `ty` that no arm handles (by name), if any. Arms are
    /// patterns with whether they have a guard. Values of types that can't
    /// be listed (like numbers) are handled only by a pattern matching
    /// anything.
    fn missing(arms: &[(Self, bool)], ty: TypeInfoRef, ast: &TypedAST<FirstPass>, context: &TypedASTContextRef<'_>) -> Vec<String> {
        // Arms with guards may not match.
        let arms = arms.iter().filter(|(_, guarded)| !guarded).map(|(pattern, _)| pattern).collect::<Vec<_>>();

        if arms.iter().any(|pattern| pattern.is_irrefutable(context)) {
            return Vec::new();
        }

        match &context.type_solver.type_infos[context.type_solver.get_info(ty)].value {
            TypeInfo::Adt(adt) if context.type_solver.context.def_registry.adt_types[adt.id].kind == AdtKind::Enum => {
                let adt_ref = adt.id;

                context.type_solver.context.def_registry.adt_types[adt_ref]
                    .variants
                    .iter()
                    .filter(|&(variant_ref, _)| {
                        !arms.iter().any(|pattern| {
                            matches!(pattern, Self::EnumVariant { adt, adt_variant, values, .. }
                                if *adt == adt_ref && *adt_variant == variant_ref && Self::fields_are_irrefutable(values, context))
                        })
                    })
                    .map(|(_, variant)| variant.name.clone().unwrap_or_default())
                    .collect()
            }
            TypeInfo::Primitive(PrimitiveType::Bool) => [true, false]
                .into_iter()
                .filter(|&value| {
                    !arms
                        .iter()
                        .any(|pattern| matches!(pattern, Self::Literal(literal) if matches!(ast[*literal].value, Expr::Lit(LitExpr::Bool(literal)) if literal == value)))
                })
                .map(|value| value.to_string())
                .collect(),
            // The type isn't known yet, the error would be confusing.
            TypeInfo::Unknown(_) | TypeInfo::Error => Vec::new(),
            _ => vec![String::from("_")],
        }
    }
}

#[derive_where(Debug, Clone; D::Type, D::IndexResult)]
pub enum Expr<D: Descriptor = SolvedPass> {
    Lit(LitExpr),
    Var(String),
    Array {
        element: D::Type,
        elements: Box<[ExprRef]>,
    },
    IfElse {
        condition: ExprRef,
        block: BlockRef,
        otherwise: Option<ExprRef>,
    },
    While {
        condition: ExprRef,
        block: BlockRef,
        id: LoopId,
    },
    /// `loop { ... }`, left with `break`, which can give it a value.
    Loop {
        block: BlockRef,
        id: LoopId,
    },
    /// `break` out of the loop `id`, with its value for `loop`.
    Break {
        id: LoopId,
        value: Option<ExprRef>,
    },
    /// `continue` with the next iteration of the loop `id`.
    Continue {
        id: LoopId,
    },
    Block(BlockRef),
    Unary {
        operator: Positioned<UnaryOperator>,
        expr: ExprRef,
    },
    Binary {
        operator: Positioned<Operator>,
        lhs: ExprRef,
        rhs: ExprRef,
    },
    Closure {
        args: Box<[Arg<D::Type>]>,
        captures: Box<[(String, D::Type)]>,
        body: BlockRef,
    },
    Call {
        func: ExprRef,
        args: Box<[ExprRef]>,
    },
    Construct {
        adt: AdtRef,
        variant: AdtVariantRef,
        fields: Box<[(FieldRef, D::Type, ExprRef)]>,
    },
    AdtIndex {
        target: ExprRef,
        field: D::IndexResult,
    },
    VTableIndex {
        target: Option<ExprRef>,
        target_ty: D::Type,
        vtable: ImplRef,
        func: VFuncRef,
        /// Type arguments of the function's own generics, if it has any.
        type_args: Box<[D::Type]>,
    },
    ArrayIndex {
        target: ExprRef,
        element: ExprRef,
    },
    TraitFunc {
        target: ExprRef,
        trait_ref: TraitRef,
        func: TraitFuncRef,
    },
    /// A function of a trait bounding the generic type of `target`, called
    /// on the impl of the type it's instantiated with.
    BoundFunc {
        target: ExprRef,
        trait_ref: TraitRef,
        func: TraitFuncRef,
    },
    /// A function, with type arguments for its generic parameters.
    Func {
        func: FuncRef,
        type_args: Box<[D::Type]>,
    },
    TypeCast(ExprRef, PrimitiveType),
    /// `${value:spec}`: a number, a boolean or a string formatted into a
    /// string.
    Format {
        value: ExprRef,
        spec: FormatSpec,
    },
    /// `return value`, or `return` without a value.
    Return(Option<ExprRef>),
    /// A constant of a module.
    Const(ConstRef),
    /// `panic(message)`: stops the program with a message.
    Panic(ExprRef),
    /// Code that can't be reached, like the end of an exhaustive `match`.
    /// Reaching it stops the program.
    Unreachable,
    IsPattern {
        target: ExprRef,
        pattern: IsPattern<D>,
    },
    Error(TypeErrorRef),
}

impl Expr<FirstPass> {
    /// Reports fields of a constructed ADT that were given neither a value nor
    /// a default value.
    fn check_missing_fields(
        context: &mut TypedASTContextRef<'_>,
        module: ModuleId,
        adt: AdtRef,
        variant: AdtVariantRef,
        fields: &[(FieldRef, TypeInfoRef, ExprRef)],
        span: Span,
    ) {
        let is_enum = context.type_solver.context.def_registry.adt_types[adt].kind == AdtKind::Enum;

        for &(field_ref, _, value) in fields {
            // Field 0 of an enum variant is the discriminant, which is never
            // given explicitly.
            if value != ExprRef::INVALID || (is_enum && field_ref == FieldRef::ZERO) {
                continue;
            }

            let field = &context.type_solver.context.def_registry.adt_types[adt].variants[variant].fields[field_ref];

            if field.default_value.is_none() {
                let name = field.name.clone();

                context
                    .type_solver
                    .error(TypeError::MissingField { adt, variant, name }, ModuleSpan(module, span));
            }
        }
    }

    fn error_expr(error: TypeError, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        let error = context.type_solver.error(error, ModuleSpan(ast.module, span));
        let ty = context.type_solver.add_info(TypeInfo::Error, Some(span));

        ast.add_expr(Self::Error(error), ty, span)
    }

    /// Creates a number literal of an explicitly given primitive type, like
    /// `5u8` or `1f32`.
    fn number_literal(
        number: Positioned<mollie_parser::Number>,
        primitive: PrimitiveType,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
    ) -> ExprRef {
        #[allow(clippy::cast_precision_loss)]
        let lit = match (number.value, primitive) {
            (mollie_parser::Number::I64(value), PrimitiveType::F32) => LitExpr::F32(value as f32),
            (mollie_parser::Number::I64(value), primitive) if primitive.is_num() => LitExpr::Int(value),
            (mollie_parser::Number::F32(value), PrimitiveType::F32) => LitExpr::F32(value),
            (value, _) => {
                let found = match value {
                    mollie_parser::Number::I64(_) => PrimitiveType::Int(IntType::I64),
                    mollie_parser::Number::F32(_) => PrimitiveType::F32,
                };

                return Self::error_expr(
                    TypeError::Unexpected {
                        expected: TypeErrorValue::PrimitiveType(primitive),
                        found: TypeErrorValue::PrimitiveType(found),
                    },
                    ast,
                    context,
                    number.span,
                );
            }
        };

        let ty = context.type_solver.add_info(TypeInfo::Primitive(primitive), Some(number.span));

        ast.add_expr(Self::Lit(lit), ty, number.span)
    }

    /// Lowers `<number><postfix>` into a call of the postfix function, like
    /// A constant. It's evaluated when it's first used.
    fn const_expr(constant: ConstRef, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        evaluate_constant(context, constant);

        let ty = context.type_solver.context.def_registry.constants[constant].ty;
        let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[]);

        ast.add_expr(Self::Const(constant), ty, span)
    }

    /// A reference to a function. Generic functions are instantiated with
    /// fresh type variables, inferred from how the function is used.
    fn func_expr(func_ref: FuncRef, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        let func = &context.type_solver.context.def_registry.functions[func_ref];
        let (generics, ty) = (func.generics, func.ty);
        let type_args: Box<[TypeInfoRef]> = (0..generics).map(|_| context.type_solver.add_unknown(None, Some(span))).collect();
        let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &type_args);

        ast.add_expr(Self::Func { func: func_ref, type_args }, ty, span)
    }

    /// Lowers `match target { pattern if guard => body, ... }` into
    ///
    /// ```text
    /// {
    ///     const <match> = target;
    ///
    ///     if <match> is pattern && guard { body } else if ... else { <unreachable> }
    /// }
    /// ```
    fn lower_match(
        match_expr: mollie_parser::MatchExpr,
        expected: Option<TypeInfoRef>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        let target = Self::from_parsed(match_expr.target.value, ast, context, match_expr.target.span);

        Self::lower_match_on(target, match_expr.arms.value, expected, ast, context, span)
    }

    /// `panic(message)`, unless the program declares something called
    /// `panic`. Like `return`, it fits where any value is expected.
    fn lower_panic(args: Vec<Positioned<mollie_parser::Expr>>, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        let string = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span));
        let found = args.len();
        let mut args = args.into_iter();

        let (Some(message), None) = (args.next(), args.next()) else {
            return Self::error_expr(
                TypeError::ArgumentCountMismatch {
                    expected: 1,
                    found,
                    func: None,
                },
                ast,
                context,
                span,
            );
        };

        let message_span = message.span;
        let message = Self::from_parsed_expecting(message.value, string, ast, context, message_span);

        if let Err(err) = context.type_solver.unify(UnifyArgs {
            expected: string,
            found: ast[message].ty,
        }) {
            let err = err.into_type_error(&mut context.type_solver);

            context.type_solver.error(err, ModuleSpan(ast.module, message_span));
        }

        let ty = context.type_solver.add_unknown(Some(TypeInfo::Primitive(PrimitiveType::Void)), Some(span));

        ast.add_expr(Self::Panic(message), ty, span)
    }

    /// Lowers `target?` into
    ///
    /// ```text
    /// match target { Ok { value } => value, Err { error } => return Err { error } }
    /// match target { Some { value } => value, None => return None }
    /// ```
    ///
    /// for a `Result` in a function returning a `Result`, or an `Option` in a
    /// function returning an `Option`.
    fn lower_try(
        target: Positioned<mollie_parser::Expr>,
        expected: Option<TypeInfoRef>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        use mollie_parser::{Ident, IsPattern as Pattern, MatchArm, NameValue, NameValuePattern, NodeExpr, TypePathExpr, TypePathSegment, TypePattern};

        let target = Self::from_parsed(target.value, ast, context, target.span);
        let registry = &context.type_solver.context;
        let (option, result) = (registry.get_adt_item(LangItem::Option), registry.get_adt_item(LangItem::Result));
        let target_adt = IsPattern::adt_of(context, ast[target].ty).map(|(adt, _)| adt);
        let returns_adt = context.returns.and_then(|returns| IsPattern::adt_of(context, returns)).map(|(adt, _)| adt);

        let at = |value| span.wrap(value);
        let path = |name: &str| {
            span.wrap(TypePathExpr {
                segments: vec![span.wrap(TypePathSegment {
                    name: span.wrap(Ident::new(name)),
                    args: None,
                })],
            })
        };
        let fields = |name: &str| {
            Some(span.wrap(TypePattern::Values(vec![span.wrap(NameValuePattern {
                name: span.wrap(Ident::new(name)),
                value: None,
            })])))
        };
        let arm = |pattern: Pattern, body: mollie_parser::Expr| {
            span.wrap(MatchArm {
                pattern: span.wrap(pattern),
                guard: None,
                body: span.wrap(body),
            })
        };

        let (success, failure) = match (target_adt, returns_adt) {
            (Some(adt), Some(returns)) if Some(adt) == result && adt == returns => (
                "Ok",
                arm(
                    Pattern::Type {
                        ty: path("Err"),
                        pattern: fields("error"),
                    },
                    mollie_parser::Expr::Return(Some(Box::new(at(mollie_parser::Expr::Node(NodeExpr {
                        name: path("Err"),
                        from: None,
                        properties: vec![span.wrap(NameValue {
                            name: span.wrap(Ident::new("error")),
                            value: None,
                        })],
                        children: span.wrap(Vec::new()),
                    }))))),
                ),
            ),
            (Some(adt), Some(returns)) if Some(adt) == option && adt == returns => (
                "Some",
                arm(
                    Pattern::Type {
                        ty: path("None"),
                        pattern: None,
                    },
                    mollie_parser::Expr::Return(Some(Box::new(at(mollie_parser::Expr::Ident(Ident::new("None")))))),
                ),
            ),
            _ => {
                let ty = context.type_solver.solve(ast[target].ty);

                return Self::error_expr(TypeError::NotTryable { ty }, ast, context, span);
            }
        };

        let success = arm(
            Pattern::Type {
                ty: path(success),
                pattern: fields("value"),
            },
            mollie_parser::Expr::Ident(Ident::new("value")),
        );

        Self::lower_match_on(target, vec![success, failure], expected, ast, context, span)
    }

    /// Lowers a `match` of the already lowered `target`.
    fn lower_match_on(
        target: ExprRef,
        match_arms: Vec<Positioned<mollie_parser::MatchArm>>,
        expected: Option<TypeInfoRef>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        let target_ty = ast[target].ty;
        // The target is evaluated once. The name can't clash with variables.
        let name = format!("<match {}>", span.start);

        context.type_solver.set_var_with_mutability(&name, target_ty, false);

        let declaration = ast.add_stmt(Stmt::NewVar {
            mutable: false,
            name: name.clone(),
            value: target,
        });
        let result = context.type_solver.add_unknown(None, Some(span));
        let mut arms = Vec::with_capacity(match_arms.len());
        let mut patterns = Vec::with_capacity(match_arms.len());

        for arm in match_arms {
            let arm = arm.value;
            let pattern_span = arm.pattern.span;

            // Bindings of the pattern are visible in the guard and the body.
            context.type_solver.push_frame();

            let scrutinee = ast.add_expr(Self::Var(name.clone()), target_ty, pattern_span);
            let pattern = IsPattern::lower(arm.pattern.value, target_ty, ast, context, pattern_span);
            let bool_ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), None);
            let mut condition = ast.add_expr(
                Self::IsPattern {
                    target: scrutinee,
                    pattern: pattern.clone(),
                },
                bool_ty,
                pattern_span,
            );

            patterns.push((pattern, arm.guard.is_some()));

            if let Some(guard) = arm.guard {
                let guard_span = guard.span;
                let guard = Self::from_parsed(guard.value, ast, context, guard_span);
                let bool_ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), None);

                if let Err(err) = context.type_solver.unify(UnifyArgs {
                    expected: bool_ty,
                    found: ast[guard].ty,
                }) {
                    let err = err.into_type_error(&mut context.type_solver);

                    context.type_solver.error(err, ModuleSpan(ast.module, guard_span));
                }

                condition = ast.add_expr(
                    Self::Binary {
                        operator: guard_span.wrap(Operator::And),
                        lhs: condition,
                        rhs: guard,
                    },
                    bool_ty,
                    guard_span,
                );
            }

            let body_span = arm.body.span;
            let body = match expected {
                Some(expected) => Self::from_parsed_expecting(arm.body.value, expected, ast, context, body_span),
                None => Self::from_parsed(arm.body.value, ast, context, body_span),
            };

            if let Err(err) = context.type_solver.unify(UnifyArgs {
                expected: result,
                found: ast[body].ty,
            }) {
                let err = err.into_type_error(&mut context.type_solver);

                context.type_solver.error(err, ModuleSpan(ast.module, body_span));
            }

            context.type_solver.pop_frame();

            let block = ast.add_block(
                Block {
                    stmts: Box::new([]),
                    expr: Some(body),
                },
                ast[body].ty,
                body_span,
            );

            arms.push((condition, block));
        }

        let missing = IsPattern::missing(&patterns, target_ty, ast, context);

        if !missing.is_empty() {
            context.type_solver.error(TypeError::NonExhaustive { missing }, ModuleSpan(ast.module, span));
        }

        let mut chain = ast.add_expr(Self::Unreachable, result, span);

        for (condition, block) in arms.into_iter().rev() {
            chain = ast.add_expr(
                Self::IfElse {
                    condition,
                    block,
                    otherwise: Some(chain),
                },
                result,
                span,
            );
        }

        let block = ast.add_block(
            Block {
                stmts: Box::new([declaration]),
                expr: Some(chain),
            },
            result,
            span,
        );

        ast.add_expr(Self::Block(block), result, span)
    }

    /// Lowers children given in a node expression, as a value of type
    /// `expected`: an array of children if it's an array type (`Drawable[]`),
    /// otherwise the only child (`Drawable`).
    fn children(
        children: Positioned<Vec<Positioned<mollie_parser::NodeExpr>>>,
        expected: TypeInfoRef,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
    ) -> ExprRef {
        let span = children.span;
        let count = children.value.len();
        let element = match context.type_solver.type_infos[context.type_solver.get_info(expected)].value {
            TypeInfo::Array(array) => Some(array.element),
            _ => None,
        };

        let value = if let Some(element) = element {
            let mut elements = Vec::with_capacity(count);

            // Every child is checked against the element type, so children of
            // different types implementing a trait can be given together.
            for child in children.value {
                let child = Self::from_parsed(mollie_parser::Expr::Node(child.value), ast, context, child.span);

                if let Err(err) = context.type_solver.unify(UnifyArgs {
                    expected: element,
                    found: ast[child].ty,
                }) {
                    let err = err.into_type_error(&mut context.type_solver);

                    context.type_solver.error(err, ModuleSpan(ast.module, ast[child].span));
                }

                elements.push(child);
            }

            let ty = context
                .type_solver
                .add_info(TypeInfo::Array(ArrayTypeInfo { element, size: Some(count) }), Some(span));

            ast.add_expr(
                Self::Array {
                    element,
                    elements: elements.into_boxed_slice(),
                },
                ty,
                span,
            )
        } else {
            if count > 1 {
                context
                    .type_solver
                    .error(TypeError::TooManyChildren { found: count }, ModuleSpan(ast.module, span));
            }

            let child = children.value.into_iter().next().expect("children aren't empty");

            Self::from_parsed(mollie_parser::Expr::Node(child.value), ast, context, child.span)
        };

        // The container decides the type, e.g. children of different types
        // implementing the trait of its children.
        if let Err(err) = context.type_solver.unify(UnifyArgs {
            expected,
            found: ast[value].ty,
        }) {
            let err = err.into_type_error(&mut context.type_solver);

            context.type_solver.error(err, ModuleSpan(ast.module, span));
        }

        value
    }

    /// The impl of the `Container` lang item for `ty`, with the type of its
    /// children and type arguments of the impl.
    fn container_impl(ty: TypeInfoRef, context: &mut TypedASTContextRef<'_>, span: ModuleSpan) -> Option<(ImplRef, TypeInfoRef, Box<[TypeInfoRef]>)> {
        let container = context.type_solver.context.get_trait_item(LangItem::Container)?;
        let solved = context.type_solver.solve(ty);
        let vtable = context.type_solver.context.find_vtable(solved, Some(container))?;
        let children = *context.type_solver.context.impl_registry.impls[vtable].trait_args.first()?;
        let type_args = context.use_vtable_impl(solved, vtable, span);
        let children = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, children, &type_args);

        Some((vtable, children, type_args))
    }

    /// Lowers a constructed container with children into
    /// `{ const <container> = construct; <container>.set_children(children);
    /// <container> }`.
    #[allow(clippy::too_many_arguments)]
    fn with_children(
        construct: ExprRef,
        vtable: ImplRef,
        type_args: &[TypeInfoRef],
        children: ExprRef,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        let Some(func) = context
            .type_solver
            .context
            .get_trait_item(LangItem::Container)
            .and_then(|container| context.type_solver.context.get_trait_func_item(container, LangItem::ContainerSetChildren))
            .map(|func| func.as_vfunc())
        else {
            return construct;
        };

        let target_ty = ast[construct].ty;
        let name = format!("<container {}>", span.start);

        context.type_solver.set_var_with_mutability(&name, target_ty, false);

        let declaration = ast.add_stmt(Stmt::NewVar {
            mutable: false,
            name: name.clone(),
            value: construct,
        });
        let func_ty = TypeSolver::type_to_info(
            &mut context.type_solver.type_infos,
            context.type_solver.context,
            context.type_solver.context.impl_registry.impls[vtable].functions[func].ty,
            type_args,
        );
        let solved = context.type_solver.solve(target_ty);
        let item = UsedItem::VTable(solved, vtable, type_args.iter().map(|&ty| context.type_solver.solve(ty)).collect());

        ast.use_item(
            &context.type_solver.context.def_registry.adt_types,
            &context.type_solver.context.impl_registry,
            &context.type_solver.context.def_registry.traits,
            context.vtables,
            context.functions,
            &mut context.type_solver.context.types,
            item,
            None,
        );

        let receiver = ast.add_expr(Self::Var(name.clone()), target_ty, span);
        let func = ast.add_expr(
            Self::VTableIndex {
                target: Some(receiver),
                target_ty,
                vtable,
                func,
                type_args: Box::new([]),
            },
            func_ty,
            span,
        );
        let void = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), None);
        let call = ast.add_expr(
            Self::Call {
                func,
                args: Box::new([children]),
            },
            void,
            span,
        );
        let call = ast.add_stmt(Stmt::Expr(call));
        let result = ast.add_expr(Self::Var(name), target_ty, span);
        let block = ast.add_block(
            Block {
                stmts: Box::new([declaration, call]),
                expr: Some(result),
            },
            target_ty,
            span,
        );

        ast.add_expr(Self::Block(block), target_ty, span)
    }

    /// Lowers `expr`, which is expected to be of type `expected` (see
    /// [`TypedASTContextRef::expected`]). The caller still checks the type.
    pub fn from_parsed_expecting(
        expr: mollie_parser::Expr,
        expected: TypeInfoRef,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        context.expected = Some(expected);

        let expr = Self::from_parsed(expr, ast, context, span);

        context.expected = None;

        expr
    }

    /// The variant called `name` of the enum `expected` is of, with the
    /// enum's type arguments: `Some` and `None` where an `Option` is
    /// expected.
    fn expected_variant(expected: Option<TypeInfoRef>, name: &str, context: &TypedASTContextRef<'_>) -> Option<(AdtRef, Box<[TypeInfoRef]>, AdtVariantRef)> {
        let (adt, type_args) = IsPattern::adt_of(context, expected?)?;
        let variant = IsPattern::variant_named(context, adt, name)?;

        Some((adt, type_args, variant))
    }

    /// Types of parameters (without the receiver) of the method `name` of
    /// values of type `target`, if it's known already. Used as expected types
    /// of arguments.
    fn method_params(target: TypeInfoRef, name: &str, context: &mut TypedASTContextRef<'_>, span: ModuleSpan) -> Option<Box<[TypeInfoRef]>> {
        let func = match &context.type_solver.type_infos[context.type_solver.get_info(target)].value {
            TypeInfo::Adt(_) => Self::method_type(target, name, context, span)?,
            TypeInfo::Trait(trait_info) => {
                let (trait_ref, trait_args) = (trait_info.id, trait_info.type_args.clone());
                let func = context.type_solver.context.def_registry.traits[trait_ref]
                    .functions
                    .values()
                    .find(|func| func.name == name)?;
                // Generic 0 of a trait is `Self`, then come its own arguments.
                let substitution = once(target).chain(trait_args).collect::<Box<[_]>>();
                #[allow(clippy::needless_collect, reason = "the parameters borrow `context`")]
                let params = func
                    .args
                    .iter()
                    .filter(|arg| matches!(arg.kind, ArgType::Regular))
                    .map(|arg| arg.ty)
                    .collect::<Vec<_>>();

                return Some(
                    params
                        .into_iter()
                        .map(|ty| TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &substitution))
                        .collect(),
                );
            }
            TypeInfo::Generic(_) => Self::bound_method(target, name, context)?,
            _ => return None,
        };

        match &context.type_solver.type_infos[func].value {
            TypeInfo::Func(func) => func.args.get(1..).map(Box::from),
            _ => None,
        }
    }

    /// Whether values of the type `ty` are values of a value type.
    fn is_value_type(ty: TypeInfoRef, context: &TypedASTContextRef<'_>) -> bool {
        matches!(
            &context.type_solver.type_infos[context.type_solver.get_info(ty)].value,
            TypeInfo::Adt(adt) if context.type_solver.context.def_registry.value_types.contains(&adt.id)
        )
    }

    /// Why the place `place` can't be changed, if it can't. Fields of objects
    /// and elements of arrays can always be changed, but fields of values of
    /// value types are parts of their place, whose root must be a mutable
    /// variable.
    fn place_error(place: ExprRef, ast: &TypedAST<FirstPass>, context: &TypedASTContextRef<'_>) -> Option<TypeError> {
        match &ast[place].value {
            Self::Var(name) if context.type_solver.is_var_mutable(name) == Some(false) => Some(TypeError::AssignToImmutable { name: name.clone() }),
            &Self::AdtIndex { target, .. } if Self::is_value_type(ast[target].ty, context) => Self::place_error(target, ast, context),
            Self::Var(_) | Self::AdtIndex { .. } | Self::ArrayIndex { .. } | Self::Error(_) => None,
            _ => Some(TypeError::NotAssignable),
        }
    }

    /// Values of arguments of a call that takes them only by position,
    /// reporting named ones.
    fn positional_args(
        args: Vec<Positioned<mollie_parser::CallArg>>,
        ast: &TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
    ) -> Vec<Positioned<mollie_parser::Expr>> {
        args.into_iter()
            .map(|arg| {
                if let Some(name) = &arg.value.name {
                    context
                        .type_solver
                        .error(TypeError::NamedArgumentsNotSupported, ModuleSpan(ast.module, name.span));
                }

                arg.value.value
            })
            .collect()
    }

    /// Parameters of the function called by `func`, if it's a known function
    /// whose arguments can be named and have defaults.
    fn known_params(func: ExprRef, is_field: bool, ast: &TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>) -> Option<KnownParams> {
        let registry = &context.type_solver.context.def_registry;

        match &ast[func].value {
            Self::Func { func, type_args } => {
                let names: Box<[_]> = registry.functions[*func].arg_names.clone().into();
                let defaults = registry.func_defaults.get(func).cloned().unwrap_or_else(|| vec![None; names.len()].into());

                Some(KnownParams {
                    names,
                    defaults,
                    receiver: false,
                    mut_self: false,
                    type_args: Some(type_args.clone()),
                })
            }
            // `value.name(...)`, if the method is known already.
            Self::AdtIndex { target, field } if !is_field => {
                let target_info = context.type_solver.get_info(ast[*target].ty);

                if !matches!(
                    context.type_solver.type_infos[target_info].value,
                    TypeInfo::Adt(_) | TypeInfo::Primitive(_) | TypeInfo::Array(_)
                ) {
                    return None;
                }

                let solved = context.type_solver.solve(ast[*target].ty);
                let (vtable, vfunc) = context.type_solver.context.find_vtable_by_func(solved, field)?;
                let registry = &context.type_solver.context.impl_registry;
                let function = &registry.impls[vtable].functions[vfunc];

                if function.arg_names.first().map(String::as_str) != Some("self") {
                    return None;
                }

                let names: Box<[_]> = function.arg_names[1..].into();
                let defaults = registry
                    .method_defaults
                    .get(&(vtable, vfunc))
                    .cloned()
                    .unwrap_or_else(|| vec![None; names.len()].into());

                Some(KnownParams {
                    names,
                    defaults,
                    receiver: true,
                    mut_self: registry.mut_self.contains(&(vtable, vfunc)),
                    type_args: None,
                })
            }
            // `Type::name(...)`, where `self` (if any) is the first argument.
            &Self::VTableIndex {
                target: None,
                vtable,
                func: vfunc,
                ..
            } => {
                let registry = &context.type_solver.context.impl_registry;
                let function = &registry.impls[vtable].functions[vfunc];
                let names: Box<[_]> = function.arg_names.clone().into();
                let this = function.arg_names.first().is_some_and(|name| name == "self");
                let defaults = registry.method_defaults.get(&(vtable, vfunc)).map_or_else(
                    || vec![None; names.len()].into(),
                    |defaults| this.then_some(None).into_iter().chain(defaults.iter().copied()).collect(),
                );

                Some(KnownParams {
                    names,
                    defaults,
                    receiver: false,
                    // The first argument is the receiver, written back.
                    mut_self: this && registry.mut_self.contains(&(vtable, vfunc)),
                    type_args: None,
                })
            }
            _ => None,
        }
    }

    /// Matches arguments of a call of `func` (named or not) to `known`
    /// parameters, filling the missing ones with their defaults. Values are
    /// put into variables (of the frame pushed by the caller), in the order
    /// they're evaluated, by the returned statements.
    fn match_args(
        known: &KnownParams,
        args: Vec<Positioned<mollie_parser::CallArg>>,
        func: ExprRef,
        params: Option<&[TypeInfoRef]>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> (Box<[ExprRef]>, Vec<StmtRef>) {
        fn bind(
            name: String,
            value: ExprRef,
            ast: &mut TypedAST<FirstPass>,
            context: &mut TypedASTContextRef<'_>,
            stmts: &mut Vec<StmtRef>,
        ) -> (String, TypeInfoRef) {
            let ty = ast[value].ty;

            context.type_solver.set_var_with_mutability(name.clone(), ty, false);
            stmts.push(ast.add_stmt(Stmt::NewVar {
                mutable: false,
                name: name.clone(),
                value,
            }));

            (name, ty)
        }

        let var = |(name, ty): &(String, TypeInfoRef), ast: &mut TypedAST<FirstPass>| ast.add_expr(Self::Var(name.clone()), *ty, span);

        let count = known.names.len();
        let mut stmts = Vec::new();
        let mut slots: Vec<Option<(String, TypeInfoRef)>> = vec![None; count];
        let mut errors: Vec<Option<ExprRef>> = vec![None; count];
        let mut extra = Vec::new();
        let mut seen_named = false;
        // The first argument of `Type::method(value)` for a `mut self` method:
        // the place it changes, passed as it is.
        let mut placed: Option<ExprRef> = None;

        // Defaults may use `self`, so the receiver is evaluated first. The
        // receiver of a `mut self` method is the place it changes, so it stays
        // there and defaults read it again.
        let receiver = if known.receiver
            && let Self::AdtIndex { target, field } = &ast[func].value
        {
            let (target, field) = (*target, field.clone());

            if known.mut_self {
                Some(Err(target))
            } else {
                let receiver = bind(String::from("<self>"), target, ast, context, &mut stmts);
                let target = var(&receiver, ast);

                ast.exprs[func].value = Self::AdtIndex { target, field };

                Some(Ok(receiver))
            }
        } else {
            None
        };

        for (position, arg) in args.into_iter().enumerate() {
            let mollie_parser::CallArg { name, value } = arg.value;
            let slot = match name {
                None if seen_named => {
                    context.type_solver.error(TypeError::PositionalAfterNamed, ModuleSpan(ast.module, value.span));

                    None
                }
                None => Some(position),
                Some(name) => {
                    seen_named = true;

                    match known.names.iter().position(|param| *param == name.value.0) {
                        Some(index) if slots[index].is_some() => {
                            context
                                .type_solver
                                .error(TypeError::DuplicateArgument { name: name.value.0 }, ModuleSpan(ast.module, name.span));

                            None
                        }
                        Some(index) => Some(index),
                        None => {
                            context
                                .type_solver
                                .error(TypeError::UnknownArgument { name: name.value.0 }, ModuleSpan(ast.module, name.span));

                            None
                        }
                    }
                }
            };

            let value = match slot.and_then(|slot| params?.get(slot)) {
                Some(&param) => Self::from_parsed_expecting(value.value, param, ast, context, value.span),
                None => Self::from_parsed(value.value, ast, context, value.span),
            };

            match slot {
                Some(0) if known.mut_self && !known.receiver && placed.is_none() => {
                    // A temporary value just loses the change.
                    if let Some(error @ TypeError::AssignToImmutable { .. }) = Self::place_error(value, ast, context) {
                        context.type_solver.error(error, ModuleSpan(ast.module, ast[value].span));
                    }

                    placed = Some(value);
                }
                Some(slot) if slot < count => slots[slot] = Some(bind(format!("<arg{slot}>"), value, ast, context, &mut stmts)),
                // Too many arguments, reported once the call is checked.
                Some(_) => extra.push(value),
                // Still checked, but not passed.
                None => stmts.push(ast.add_stmt(Stmt::Expr(value))),
            }
        }

        // Defaults, in the order of parameters, since they may use earlier
        // ones.
        for index in 0..count {
            if slots[index].is_some() || (index == 0 && placed.is_some()) {
                continue;
            }

            let earlier: Option<Vec<ExprRef>> = (0..index)
                .map(|earlier| match (&slots[earlier], placed) {
                    (_, Some(place)) if earlier == 0 => Some(place),
                    (Some(temporary), _) => Some(var(temporary, ast)),
                    (None, _) => None,
                })
                .collect();

            match (known.defaults.get(index).copied().flatten(), earlier) {
                (Some(default), Some(earlier)) => {
                    let default_func = match &known.type_args {
                        Some(type_args) => {
                            let ty = context.type_solver.context.def_registry.functions[default].ty;
                            let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, type_args);

                            ast.add_expr(
                                Self::Func {
                                    func: default,
                                    type_args: type_args.clone(),
                                },
                                ty,
                                span,
                            )
                        }
                        None => Self::func_expr(default, ast, context, span),
                    };
                    let receiver = receiver.as_ref().map(|receiver| match receiver {
                        Ok(temporary) => var(temporary, ast),
                        Err(place) => *place,
                    });
                    let default_args: Box<[_]> = receiver.into_iter().chain(earlier).collect();
                    let returns = match &context.type_solver.type_infos[context.type_solver.get_info(ast[default_func].ty)].value {
                        TypeInfo::Func(func_info) => func_info.returns,
                        _ => context.type_solver.add_unknown(None, Some(span)),
                    };
                    let found = context.type_solver.add_info(
                        TypeInfo::Func(FuncTypeInfo {
                            args: default_args.iter().map(|&arg| ast[arg].ty).collect(),
                            returns,
                        }),
                        Some(span),
                    );

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: ast[default_func].ty,
                        found,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, span));
                    }

                    let value = ast.add_expr(
                        Self::Call {
                            func: default_func,
                            args: default_args,
                        },
                        returns,
                        span,
                    );

                    slots[index] = Some(bind(format!("<arg{index}>"), value, ast, context, &mut stmts));
                }
                // An earlier argument is missing (already reported).
                (Some(_), None) => {
                    errors[index] = Some(ast.add_expr(Self::Unreachable, context.type_solver.add_info(TypeInfo::Error, None), span));
                }
                (None, _) => {
                    let error = context.type_solver.error(
                        TypeError::MissingArgument {
                            name: known.names[index].clone(),
                        },
                        ModuleSpan(ast.module, span),
                    );

                    errors[index] = Some(ast.add_expr(Self::Error(error), context.type_solver.add_info(TypeInfo::Error, None), span));
                }
            }
        }

        let args = slots
            .iter()
            .zip(errors)
            .enumerate()
            .map(|(index, (slot, error))| match (slot, placed) {
                (_, Some(place)) if index == 0 => place,
                (Some(temporary), _) => var(temporary, ast),
                (None, _) => error.unwrap_or(ExprRef::INVALID),
            })
            .chain(extra)
            .collect();

        (args, stmts)
    }

    /// `super.name(args)`: a call of the default of the trait function `name`,
    /// with `self` if it takes it, inside a function of a trait impl.
    fn lower_super_call(
        name: Positioned<mollie_parser::Ident>,
        args: Vec<Positioned<mollie_parser::Expr>>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        fn fail(error: TypeError, span: Span, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>) -> ExprRef {
            let error = context.type_solver.error(error, ModuleSpan(ast.module, span));
            let ty = context.type_solver.add_info(TypeInfo::Error, None);

            ast.add_expr(Expr::Error(error), ty, span)
        }

        let Some(impl_ref) = context.trait_impl else {
            return fail(TypeError::SuperOutsideTraitImpl, span, ast, context);
        };

        let generator = &context.type_solver.context.impl_registry.impls[impl_ref];
        let (self_ty, trait_args) = (generator.ty, generator.trait_args.clone());
        let Some(trait_ref) = generator.origin_trait else {
            return fail(TypeError::SuperOutsideTraitImpl, span, ast, context);
        };

        let default = context.type_solver.context.def_registry.traits[trait_ref]
            .functions
            .values()
            .find(|func| func.name == name.value.0)
            .and_then(|func| Some((func.default?, func.args.first().is_some_and(|arg| matches!(arg.kind, ArgType::This)))));

        let Some((default, takes_self)) = default else {
            return fail(TypeError::NoDefault { name: name.value.0 }, name.span, ast, context);
        };

        // Generic 0 of a default is `Self`, then come the trait's type
        // arguments. Generics of the impl are rigid here.
        let type_args: Box<[_]> = once(self_ty)
            .chain(trait_args)
            .map(|ty| TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[]))
            .collect();
        let default_ty = context.type_solver.context.def_registry.functions[default].ty;
        let func_ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, default_ty, &type_args);
        let func = ast.add_expr(Self::Func { func: default, type_args }, func_ty, name.span);
        let (params, returns) = match &context.type_solver.type_infos[context.type_solver.get_info(func_ty)].value {
            TypeInfo::Func(func_info) => (func_info.args.clone(), func_info.returns),
            _ => (Box::default(), context.type_solver.add_unknown(None, Some(span))),
        };

        let mut call_args = Vec::with_capacity(args.len() + usize::from(takes_self));

        if takes_self {
            if context.type_solver.get_var("self").is_none() {
                return fail(TypeError::SuperOutsideTraitImpl, span, ast, context);
            }

            call_args.push(Self::from_parsed(mollie_parser::Expr::This, ast, context, span));
        }

        for arg in args {
            let arg = match params.get(call_args.len()) {
                Some(&param) => Self::from_parsed_expecting(arg.value, param, ast, context, arg.span),
                None => Self::from_parsed(arg.value, ast, context, arg.span),
            };

            call_args.push(arg);
        }

        let found = context.type_solver.add_info(
            TypeInfo::Func(FuncTypeInfo {
                args: call_args.iter().map(|&arg| ast[arg].ty).collect(),
                returns,
            }),
            Some(span),
        );

        if let Err(err) = context.type_solver.unify(UnifyArgs { expected: func_ty, found }) {
            let err = err.into_type_error(&mut context.type_solver);

            context.type_solver.error(err, ModuleSpan(ast.module, span));
        }

        ast.add_expr(
            Self::Call {
                func,
                args: call_args.into_boxed_slice(),
            },
            returns,
            span,
        )
    }

    /// Type (with the receiver) of the method `name` of a value of the ADT
    /// type `target`, if it's known already. Generics of the impl are those of
    /// the receiver (`T` of `Holder<T>`), the function's own generics are
    /// inferred from the call.
    fn method_type(target: TypeInfoRef, name: &str, context: &mut TypedASTContextRef<'_>, span: ModuleSpan) -> Option<TypeInfoRef> {
        // Methods of known types: ADTs, and primitives and arrays (with methods
        // of `std`'s traits, like `chars` of strings).
        if !matches!(
            context.type_solver.type_infos[context.type_solver.get_info(target)].value,
            TypeInfo::Adt(_) | TypeInfo::Primitive(_) | TypeInfo::Array(_)
        ) {
            return None;
        }

        let solved = context.type_solver.solve(target);
        let types = &context.type_solver.context.types;

        // Not known yet (an array of unknown elements): errors match any impl.
        if matches!(types[solved], Type::Error) || matches!(types[solved], Type::Array(element, _) if types[element] == Type::Error) {
            return None;
        }

        let (vtable, func) = context.type_solver.context.find_vtable_by_func(solved, name)?;
        let mut type_args = context.use_vtable_impl(solved, vtable, span).into_vec();
        let implementation = &context.type_solver.context.impl_registry.impls[vtable];
        let (func_ty, method_generics) = (implementation.functions[func].ty, implementation.functions[func].generics);

        type_args.extend((0..method_generics).map(|_| context.type_solver.add_unknown(None, None)));

        Some(TypeSolver::type_to_info(
            &mut context.type_solver.type_infos,
            context.type_solver.context,
            func_ty,
            &type_args,
        ))
    }

    /// Type (with the receiver) of the function `name` of a trait bounding the
    /// generic type `target`, e.g. `area` of `T: Shape`.
    fn bound_method(target: TypeInfoRef, name: &str, context: &mut TypedASTContextRef<'_>) -> Option<TypeInfoRef> {
        let &TypeInfo::Generic(generic) = &context.type_solver.type_infos[context.type_solver.get_info(target)].value else {
            return None;
        };

        let (trait_args, args, returns) = context.type_solver.bounds.iter().filter(|bound| bound.generic == generic).find_map(|bound| {
            let func = context.type_solver.context.def_registry.traits[bound.trait_ref]
                .functions
                .values()
                .find(|func| func.name == name)?;

            Some((bound.trait_args.clone(), func.args.iter().map(|arg| arg.ty).collect::<Box<[_]>>(), func.returns))
        })?;

        let func_ty = context.type_solver.context.types.get_or_add(Type::Func(args, returns));
        // Generic 0 of a trait is `Self`, then come its own arguments.
        let substitution = once(target)
            .chain(
                trait_args
                    .iter()
                    .map(|&ty| TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[])),
            )
            .collect::<Box<[_]>>();

        Some(TypeSolver::type_to_info(
            &mut context.type_solver.type_infos,
            context.type_solver.context,
            func_ty,
            &substitution,
        ))
    }

    /// Type of the field `name` of a struct of type `target`, if the type is
    /// already known.
    fn field_type(target: TypeInfoRef, name: &str, context: &mut TypedASTContextRef<'_>) -> Option<TypeInfoRef> {
        let TypeInfo::Adt(adt_info) = &context.type_solver.type_infos[context.type_solver.get_info(target)].value else {
            return None;
        };

        let adt = &context.type_solver.context.def_registry.adt_types[adt_info.id];

        if adt.kind == AdtKind::Enum {
            return None;
        }

        // The field's type with the target's type arguments substituted.
        let field_ty = adt.variants[AdtVariantRef::ZERO].fields.values().find(|field| field.name == name)?.ty;
        let type_args = adt_info.type_args.clone();

        Some(TypeSolver::type_to_info(
            &mut context.type_solver.type_infos,
            context.type_solver.context,
            field_ty,
            &type_args,
        ))
    }

    /// Whether values of type `target` may have a method called `name`. Only
    /// ADTs can also have fields, so other types are assumed to.
    fn has_method(target: TypeInfoRef, name: &str, context: &TypedASTContextRef<'_>) -> bool {
        let TypeInfo::Adt(adt_info) = &context.type_solver.type_infos[context.type_solver.get_info(target)].value else {
            return true;
        };

        let tcx = &context.type_solver.context;

        tcx.impl_registry.impls.values().any(|implementation| {
            matches!(tcx.types[implementation.ty], Type::Adt(adt, _) if adt == adt_info.id) && implementation.functions.values().any(|func| func.name == name)
        })
    }

    /// Lowers `"a ${b} c"` into `"a " + (b as string) + " c"`.
    fn template(template: mollie_parser::TemplateExpr, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        let mut result = None;

        for part in template.0 {
            let part = match part {
                mollie_parser::TemplatePart::Text(text) => {
                    let ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span));

                    ast.add_expr(Self::Lit(LitExpr::String(text)), ty, span)
                }
                mollie_parser::TemplatePart::Expr(expr, spec) => {
                    let expr_span = expr.span;
                    let expr = Self::from_parsed(expr.value, ast, context, expr_span);

                    Self::into_string(expr, spec, ast, context, expr_span)
                }
            };

            result = Some(match result {
                Some(lhs) => {
                    let ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span));

                    ast.add_expr(
                        Self::Binary {
                            operator: span.wrap(Operator::Add),
                            lhs,
                            rhs: part,
                        },
                        ty,
                        span,
                    )
                }
                None => part,
            });
        }

        result.unwrap_or_else(|| {
            let ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span));

            ast.add_expr(Self::Lit(LitExpr::String(String::new())), ty, span)
        })
    }

    /// Converts an interpolated value to a string: strings are kept, numbers
    /// and booleans are cast, or formatted with `spec`.
    fn into_string(expr: ExprRef, spec: Option<FormatSpec>, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        let ty = context.type_solver.solve(ast[expr].ty);

        if let Some(spec) = spec {
            let primitive = match context.type_solver.context.types[ty] {
                Type::Error => return expr,
                Type::Primitive(primitive) => Some(primitive),
                _ => None,
            };
            let valid = primitive.is_some_and(|primitive| {
                let integer = primitive.is_num();
                let number = integer || primitive.is_f32();
                let formattable = number || matches!(primitive, PrimitiveType::String | PrimitiveType::Bool);

                // Precision is for floats, other kinds for integers, and zeros
                // pad numbers.
                formattable && (spec.precision.is_none() || primitive.is_f32()) && (spec.kind == FormatKind::Display || integer) && (!spec.zero || number)
            });

            if !valid {
                return Self::error_expr(TypeError::InvalidFormat { ty, spec: spec.to_string() }, ast, context, span);
            }

            let string = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span));

            return ast.add_expr(Self::Format { value: expr, spec }, string, span);
        }

        match context.type_solver.context.types[ty].clone() {
            Type::Primitive(PrimitiveType::String) | Type::Error => expr,
            Type::Primitive(primitive) if primitive.is_num() || primitive.is_f32() || primitive == PrimitiveType::Bool => {
                let string = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span));

                ast.add_expr(Self::TypeCast(expr, PrimitiveType::String), string, span)
            }
            _ => Self::error_expr(TypeError::NotFormattable { ty }, ast, context, span),
        }
    }

    /// `10px` into `px(10.0)`.
    fn postfix_call(
        number: Positioned<mollie_parser::Number>,
        postfix: Positioned<String>,
        ast: &mut TypedAST<FirstPass>,
        context: &mut TypedASTContextRef<'_>,
        span: Span,
    ) -> ExprRef {
        let postfix_span = postfix.span;

        let Some(func_ref) = context
            .type_solver
            .context
            .def_registry
            .lookup(ast.module, postfix.value.as_str())
            .and_then(|item| if let ModuleItem::Func(func) = item { Some(func) } else { None })
        else {
            return Self::error_expr(
                TypeError::NoFunction {
                    name: postfix.value,
                    postfix: true,
                },
                ast,
                context,
                postfix_span,
            );
        };

        let func = &context.type_solver.context.def_registry.functions[func_ref];

        // Postfix functions take a number, so they can't be generic.
        if !func.postfix || func.generics > 0 {
            return Self::error_expr(TypeError::NotPostfix { name: postfix.value }, ast, context, postfix_span);
        }

        let func_ty = func.ty;

        let Type::Func(args, returns) = &context.type_solver.context.types[func_ty] else {
            return Self::error_expr(
                TypeError::Unexpected {
                    expected: TypeErrorValue::Function,
                    found: TypeErrorValue::ExplicitType(func_ty),
                },
                ast,
                context,
                postfix_span,
            );
        };

        let &[arg] = args.as_ref() else {
            let expected = args.len();

            return Self::error_expr(
                TypeError::ArgumentCountMismatch {
                    expected,
                    found: 1,
                    func: None,
                },
                ast,
                context,
                postfix_span,
            );
        };

        let returns = *returns;

        let Type::Primitive(primitive) = context.type_solver.context.types[arg] else {
            return Self::error_expr(
                TypeError::Unexpected {
                    expected: TypeErrorValue::ExplicitType(arg),
                    found: TypeErrorValue::PrimitiveType(match number.value {
                        mollie_parser::Number::I64(_) => PrimitiveType::Int(IntType::I64),
                        mollie_parser::Number::F32(_) => PrimitiveType::F32,
                    }),
                },
                ast,
                context,
                number.span,
            );
        };

        let expr = Self::number_literal(number, primitive, ast, context);

        ast.use_item(
            &context.type_solver.context.def_registry.adt_types,
            &context.type_solver.context.impl_registry,
            &context.type_solver.context.def_registry.traits,
            context.vtables,
            context.functions,
            &mut context.type_solver.context.types,
            UsedItem::Func(func_ref, Box::new([])),
            None,
        );

        let func_ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, func_ty, &[]);
        let returns = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, returns, &[]);
        let func = ast.add_expr(
            Self::Func {
                func: func_ref,
                type_args: Box::new([]),
            },
            func_ty,
            postfix_span,
        );

        ast.add_expr(Self::Call { func, args: Box::new([expr]) }, returns, span)
    }
}

impl FromParsed<mollie_parser::LiteralExpr, ExprRef> for Expr<FirstPass> {
    fn from_parsed(lit: mollie_parser::LiteralExpr, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        match lit {
            mollie_parser::LiteralExpr::Number(number, None) => match number.value {
                mollie_parser::Number::I64(value) => ast.add_expr(
                    Self::Lit(LitExpr::Int(value)),
                    context.type_solver.add_info(TypeInfo::Integer, Some(number.span)),
                    number.span,
                ),
                mollie_parser::Number::F32(value) => ast.add_expr(
                    Self::Lit(LitExpr::F32(value)),
                    context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::F32), Some(number.span)),
                    number.span,
                ),
            },
            mollie_parser::LiteralExpr::Number(number, Some(postfix)) => {
                let primitive = match postfix.value.as_str() {
                    "usize" => Some(PrimitiveType::UInt(UIntType::USize)),
                    "u64" => Some(PrimitiveType::UInt(UIntType::U64)),
                    "u32" => Some(PrimitiveType::UInt(UIntType::U32)),
                    "u16" => Some(PrimitiveType::UInt(UIntType::U16)),
                    "u8" => Some(PrimitiveType::UInt(UIntType::U8)),
                    "isize" => Some(PrimitiveType::Int(IntType::ISize)),
                    "i64" => Some(PrimitiveType::Int(IntType::I64)),
                    "i32" => Some(PrimitiveType::Int(IntType::I32)),
                    "i16" => Some(PrimitiveType::Int(IntType::I16)),
                    "i8" => Some(PrimitiveType::Int(IntType::I8)),
                    "f32" => Some(PrimitiveType::F32),
                    _ => None,
                };

                match primitive {
                    Some(primitive) => Self::number_literal(number, primitive, ast, context),
                    None => Self::postfix_call(number, postfix, ast, context, span),
                }
            }
            mollie_parser::LiteralExpr::Bool(value) => ast.add_expr(
                Self::Lit(LitExpr::Bool(value)),
                context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), Some(span)),
                span,
            ),
            mollie_parser::LiteralExpr::String(value) => ast.add_expr(
                Self::Lit(LitExpr::String(value)),
                context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::String), Some(span)),
                span,
            ),
        }
    }
}

impl FromParsed<mollie_parser::Expr, ExprRef> for Expr<FirstPass> {
    fn from_parsed(expr: mollie_parser::Expr, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> ExprRef {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || {
            // Taken right away, so expressions inside don't see it.
            let expected = context.expected.take();

            match expr {
                mollie_parser::Expr::Literal(literal_expr) => Self::from_parsed(literal_expr, ast, context, span),
                mollie_parser::Expr::Unary(unary_expr) => {
                    let expr = Self::from_parsed(unary_expr.expr.value, ast, context, unary_expr.expr.span);

                    ast.add_expr(
                        Self::Unary {
                            operator: unary_expr.operator,
                            expr,
                        },
                        ast[expr].ty,
                        span,
                    )
                }
                mollie_parser::Expr::Template(template) => Self::template(template, ast, context, span),
                mollie_parser::Expr::FunctionCall(func_call_expr)
                    if matches!(&func_call_expr.function.value, mollie_parser::Expr::Ident(name) if name.0 == "panic")
                        && context.type_solver.get_var("panic").is_none()
                        && context.type_solver.context.def_registry.lookup(ast.module, "panic").is_none() =>
                {
                    let args = Self::positional_args(func_call_expr.args.value, ast, context);

                    Self::lower_panic(args, ast, context, span)
                }
                mollie_parser::Expr::FunctionCall(func_call_expr)
                    if matches!(
                        &func_call_expr.function.value,
                        mollie_parser::Expr::Index(mollie_parser::IndexExpr { target, index: Positioned { value: mollie_parser::IndexTarget::Named(_), .. } })
                            if matches!(&target.value, mollie_parser::Expr::Ident(name) if name.0 == "super")
                    ) =>
                {
                    let mollie_parser::Expr::Index(mollie_parser::IndexExpr {
                        index:
                            Positioned {
                                value: mollie_parser::IndexTarget::Named(name),
                                span: name_span,
                            },
                        ..
                    }) = func_call_expr.function.value
                    else {
                        unreachable!("checked by the guard")
                    };

                    let args = Self::positional_args(func_call_expr.args.value, ast, context);

                    Self::lower_super_call(name_span.wrap(name), args, ast, context, span)
                }
                mollie_parser::Expr::FunctionCall(func_call_expr) => {
                    let func_span = func_call_expr.function.span;
                    // `value.name(...)` calls the method `name`. Only if
                    // there's no such method, a field
                    // `name` holding a function is
                    // called (`button.on_click()`).
                    let (func, is_field) = match func_call_expr.function.value {
                        mollie_parser::Expr::Index(mollie_parser::IndexExpr {
                            target,
                            index:
                                Positioned {
                                    value: mollie_parser::IndexTarget::Named(name),
                                    ..
                                },
                        }) => {
                            let target = Self::from_parsed(target.value, ast, context, target.span);
                            let field_ty = Self::field_type(ast[target].ty, &name.0, context).filter(|_| !Self::has_method(ast[target].ty, &name.0, context));
                            let is_field = field_ty.is_some();
                            // The method's type is known right away when the
                            // type of the value is,
                            // so its result can be
                            // used (e.g.
                            // iterated) before the call is solved.
                            let ty = field_ty
                                .or_else(|| Self::bound_method(ast[target].ty, &name.0, context))
                                .or_else(|| Self::method_type(ast[target].ty, &name.0, context, ModuleSpan(ast.module, func_span)))
                                .unwrap_or_else(|| context.type_solver.add_unknown(None, Some(func_span)));

                            (ast.add_expr(Self::AdtIndex { target, field: name.0 }, ty, func_span), is_field)
                        }
                        function => (Self::from_parsed(function, ast, context, func_span), false),
                    };

                    // A `mut self` method changes its receiver, which must be a
                    // mutable place (a temporary value just loses the change).
                    if !is_field
                        && let Self::AdtIndex { target, field } = &ast[func].value
                        && Self::is_value_type(ast[*target].ty, context)
                    {
                        let (target, field) = (*target, field.clone());
                        let solved = context.type_solver.solve(ast[target].ty);

                        if let Some(method) = context.type_solver.context.find_vtable_by_func(solved, &field)
                            && context.type_solver.context.impl_registry.mut_self.contains(&method)
                            && let Some(error @ TypeError::AssignToImmutable { .. }) = Self::place_error(target, ast, context)
                        {
                            context.type_solver.error(error, ModuleSpan(ast.module, ast[target].span));
                        }
                    }

                    // Types of parameters, expected of arguments, if they're
                    // known.
                    let params: Option<Box<[TypeInfoRef]>> = match &ast[func].value {
                        Self::AdtIndex { target, field } if !is_field => {
                            let (target, field) = (*target, field.clone());

                            let known = match &context.type_solver.type_infos[context.type_solver.get_info(ast[func].ty)].value {
                                TypeInfo::Func(func_info) => Some(func_info.args.get(1..).map(Box::from)),
                                _ => None,
                            };

                            known.unwrap_or_else(|| Self::method_params(ast[target].ty, &field, context, ModuleSpan(ast.module, func_span)))
                        }
                        func_expr => {
                            let has_receiver = matches!(
                                func_expr,
                                Self::VTableIndex { target: Some(_), .. } | Self::TraitFunc { .. } | Self::BoundFunc { .. }
                            );

                            match &context.type_solver.type_infos[context.type_solver.get_info(ast[func].ty)].value {
                                TypeInfo::Func(func_info) => func_info.args.get(usize::from(has_receiver)..).map(Box::from),
                                _ => None,
                            }
                        }
                    };

                    let ty = if let TypeInfo::Func(func) = &context.type_solver.type_infos[ast[func].ty].value {
                        func.returns
                    } else {
                        context.type_solver.add_unknown(None, Some(func_span))
                    };

                    let call_args = func_call_expr.args.value;
                    let known = Self::known_params(func, is_field, ast, context);
                    let named = call_args.iter().any(|arg| arg.value.name.is_some());
                    let uses_defaults = known.as_ref().is_some_and(|known| {
                        known
                            .defaults
                            .get(call_args.len()..)
                            .is_some_and(|defaults| defaults.iter().any(Option::is_some))
                    });

                    // Named arguments and defaults are put into variables
                    // first, in the order they're
                    // evaluated: `f(b: x)` is `{ let <arg1>
                    // = x; let <arg0> = <default of a>(); f(<arg0>,
                    // <arg1>) }`.
                    let (args, temporaries) = match &known {
                        Some(known) if named || uses_defaults => {
                            context.type_solver.push_frame();

                            let (args, stmts) = Self::match_args(known, call_args, func, params.as_deref(), ast, context, span);

                            (args, Some(stmts))
                        }
                        _ => {
                            if named {
                                context
                                    .type_solver
                                    .error(TypeError::NamedArgumentsNotSupported, ModuleSpan(ast.module, func_span));
                            }

                            let args = call_args
                                .into_iter()
                                .enumerate()
                                .map(|(index, arg)| {
                                    let arg = arg.value.value;

                                    match params.as_ref().and_then(|params| params.get(index)) {
                                        Some(&param) => Self::from_parsed_expecting(arg.value, param, ast, context, arg.span),
                                        None => Self::from_parsed(arg.value, ast, context, arg.span),
                                    }
                                })
                                .collect::<Box<[_]>>();

                            // `Type::method(value)` of a `mut self` method
                            // changes
                            // `value`, which must be a mutable place (like in
                            // `match_args`).
                            if let Some(known) = &known
                                && known.mut_self
                                && !known.receiver
                                && let Some(&first) = args.first()
                                && let Some(error @ TypeError::AssignToImmutable { .. }) = Self::place_error(first, ast, context)
                            {
                                context.type_solver.error(error, ModuleSpan(ast.module, ast[first].span));
                            }

                            (args, None)
                        }
                    };

                    let have_self = !is_field
                        && matches!(
                            ast[func].value,
                            Self::AdtIndex { .. } | Self::VTableIndex { target: Some(_), .. } | Self::TraitFunc { .. } | Self::BoundFunc { .. }
                        );

                    let info = TypeInfo::Func(FuncTypeInfo {
                        args: if have_self {
                            once(
                                if let TypeInfo::Func(func_info) = &context.type_solver.type_infos[ast[func].ty].value
                                    && let Some(&this) = func_info.args.first()
                                {
                                    this
                                } else if let Self::AdtIndex { target, .. } = &ast[func].value {
                                    ast[*target].ty
                                } else {
                                    // The method is already erroneous
                                    // (reported).
                                    context.type_solver.add_info(TypeInfo::Error, None)
                                },
                            )
                            .chain(args.iter().map(|&arg| ast[arg].ty))
                            .collect()
                        } else {
                            args.iter().map(|&arg| ast[arg].ty).collect()
                        },
                        returns: ty,
                    });
                    let func_ty = context.type_solver.add_info(info, Some(span));

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: ast[func].ty,
                        found: func_ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[func].span));
                    }

                    let call = ast.add_expr(Self::Call { func, args }, ty, span);

                    match temporaries {
                        Some(stmts) => {
                            context.type_solver.pop_frame();

                            let block = ast.add_block(
                                Block {
                                    stmts: stmts.into_boxed_slice(),
                                    expr: Some(call),
                                },
                                ty,
                                span,
                            );

                            ast.add_expr(Self::Block(block), ty, span)
                        }
                        None => call,
                    }
                }
                mollie_parser::Expr::Node(node_expr) => {
                    let name_span = node_expr.name.span;
                    // `Some { value }` where an `Option` is expected.
                    let shorthand = match node_expr.name.value.segments.as_slice() {
                        [segment] if segment.value.args.is_none() => Self::expected_variant(expected, &segment.value.name.value.0, context),
                        _ => None,
                    };
                    let ty = match shorthand {
                        Some((adt, type_args, variant)) => TypePathResult::Adt(adt, type_args, Some(variant)),
                        None => TypePathResult::from_parsed(node_expr.name.value, ast, context, node_expr.name.span),
                    };

                    let (adt, type_args, variant) = match ty {
                        TypePathResult::Adt(adt, type_args, variant) => (adt, type_args, variant),
                        result => {
                            let (ty, found) = match result {
                                TypePathResult::VFunc(.., vtable, vfunc) => {
                                    let ty = TypeSolver::type_to_info(
                                        &mut context.type_solver.type_infos,
                                        context.type_solver.context,
                                        context.type_solver.context.impl_registry.impls[vtable].functions[vfunc].ty,
                                        &[],
                                    );

                                    (ty, TypeErrorValue::Function)
                                }
                                TypePathResult::Trait(trait_ref, type_args) => {
                                    let ty = context.type_solver.add_info(TypeInfo::Trait(TraitTypeInfo { id: trait_ref, type_args }), None);

                                    (ty, TypeErrorValue::Trait)
                                }
                                TypePathResult::Func(func) => {
                                    let ty = TypeSolver::type_to_info(
                                        &mut context.type_solver.type_infos,
                                        context.type_solver.context,
                                        context.type_solver.context.def_registry.functions[func].ty,
                                        &[],
                                    );

                                    (ty, TypeErrorValue::Function)
                                }
                                TypePathResult::Intrinsic => (context.type_solver.add_info(TypeInfo::Error, None), TypeErrorValue::Function),
                                TypePathResult::Const(_) => (context.type_solver.add_info(TypeInfo::Error, None), TypeErrorValue::Value),
                                TypePathResult::Generic(ty) => (ty, TypeErrorValue::Generic),
                                TypePathResult::Module(_) => (context.type_solver.add_info(TypeInfo::Error, None), TypeErrorValue::Module),
                                TypePathResult::Error(..) => (context.type_solver.add_info(TypeInfo::Error, None), TypeErrorValue::Nothing),
                                TypePathResult::Adt(..) => unreachable!(),
                            };

                            return ast.add_expr(
                                Self::Error(context.type_solver.error(
                                    TypeError::Unexpected {
                                        expected: TypeErrorValue::Adt(SpecialAdtKind::AnyOf),
                                        found,
                                    },
                                    ModuleSpan(ast.module, name_span),
                                )),
                                ty,
                                name_span,
                            );
                        }
                    };

                    let variant = match (context.type_solver.context.def_registry.adt_types[adt].kind, variant) {
                        // Structs have a single variant.
                        (AdtKind::Struct | AdtKind::View, _) => AdtVariantRef::ZERO,
                        (AdtKind::Enum, None) => {
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            return ast.add_expr(
                                Self::Error(context.type_solver.error(TypeError::VariantRequired(adt), ModuleSpan(ast.module, name_span))),
                                ty,
                                name_span,
                            );
                        }
                        (AdtKind::Enum, Some(variant)) => variant,
                    };

                    let mut fields = context
                        .type_solver
                        .instantiate_adt(adt, variant, &type_args)
                        .map(|(field_ref, field_type)| (field_ref, field_type, ExprRef::INVALID))
                        .collect::<IndexBoxedSlice<FieldRef, _>>();

                    let ty = context.type_solver.add_info(TypeInfo::Adt(AdtTypeInfo { id: adt, type_args }), None);

                    for prop in node_expr.properties {
                        let name = prop.value.name.value.0;

                        let field = context.type_solver.context.def_registry.adt_types[adt].variants[variant]
                            .fields
                            .iter()
                            .find_map(|(field_ref, field)| if field.name == name { Some(field_ref) } else { None });

                        if let Some(field) = field {
                            let value = match prop.value.value {
                                Some(value) => Self::from_parsed_expecting(value.value, fields[field].1, ast, context, value.span),
                                None => {
                                    if let Some((frame, ty)) = context.type_solver.get_var(&name) {
                                        if let Some(current_frame) = context.current_frame
                                            && frame < current_frame
                                        {
                                            context.captures.push((name.clone(), ty));
                                        }

                                        ast.add_expr(Self::Var(name), ty, prop.value.name.span)
                                    } else {
                                        ast.add_expr(
                                            Self::Error(context.type_solver.error(
                                                TypeError::NotFound {
                                                    name,
                                                    was_looking_for: LookupType::Variable,
                                                },
                                                ModuleSpan(ast.module, prop.value.name.span),
                                            )),
                                            fields[field].1,
                                            prop.value.name.span,
                                        )
                                    }
                                }
                            };

                            // The field decides the type: e.g. a trait object
                            // field accepts any
                            // implementor,
                            // and an unsized array field accepts an array
                            // literal of any size.
                            if let Err(err) = context.type_solver.unify(UnifyArgs {
                                expected: fields[field].1,
                                found: ast[value].ty,
                            }) {
                                let err = err.into_type_error(&mut context.type_solver);

                                context.type_solver.error(err, ModuleSpan(ast.module, prop.value.name.span));
                            }

                            fields[field].2 = value;
                        } else {
                            let ty = context.type_solver.add_info(TypeInfo::Unknown(None), None);

                            ast.add_expr(
                                Self::Error(
                                    context
                                        .type_solver
                                        .error(TypeError::NoField { adt, variant, name }, ModuleSpan(ast.module, prop.span)),
                                ),
                                ty,
                                prop.span,
                            );
                        }
                    }

                    let children = node_expr.children;
                    // Children of containers other than views are given to them
                    // with `set_children` once they're constructed.
                    let mut set_children = None;

                    if !children.value.is_empty() {
                        let view_children = if context.type_solver.context.def_registry.adt_types[adt].kind == AdtKind::View {
                            context.type_solver.context.def_registry.adt_types[adt].variants[variant]
                                .fields
                                .iter()
                                .find_map(|(field_ref, field)| (field.name == "children").then_some(field_ref))
                        } else {
                            None
                        };

                        if let Some(field) = view_children {
                            fields[field].2 = Self::children(children, fields[field].1, ast, context);
                        } else if let Some((vtable, children_ty, impl_type_args)) = Self::container_impl(ty, context, ModuleSpan(ast.module, children.span)) {
                            let value = Self::children(children, children_ty, ast, context);

                            set_children = Some((vtable, impl_type_args, value));
                        } else {
                            let found = context.type_solver.solve(ty);

                            context
                                .type_solver
                                .error(TypeError::NotContainer { ty: found }, ModuleSpan(ast.module, children.span));
                        }
                    }

                    let fields = fields.raw;

                    Self::check_missing_fields(context, ast.module, adt, variant, &fields, span);

                    let construct = ast.add_expr(Self::Construct { adt, variant, fields }, ty, span);

                    match set_children {
                        Some((vtable, impl_type_args, children)) => Self::with_children(construct, vtable, &impl_type_args, children, ast, context, span),
                        None => construct,
                    }
                }
                mollie_parser::Expr::Index(index_expr) => {
                    let target = Self::from_parsed(index_expr.target.value, ast, context, index_expr.target.span);

                    match index_expr.index.value {
                        // Outside of calls, `value.name` is always a field.
                        mollie_parser::IndexTarget::Named(ident) => {
                            let ty = Self::field_type(ast[target].ty, &ident.0, context)
                                .unwrap_or_else(|| context.type_solver.add_unknown(None, Some(index_expr.index.span)));

                            ast.add_expr(Self::AdtIndex { target, field: ident.0 }, ty, span)
                        }
                        mollie_parser::IndexTarget::Expression(expr) => {
                            let element = Self::from_parsed(*expr, ast, context, index_expr.index.span);
                            let usize = context
                                .type_solver
                                .add_info(TypeInfo::Primitive(PrimitiveType::UInt(UIntType::USize)), Some(index_expr.index.span));

                            if let Err(err) = context.type_solver.unify(UnifyArgs {
                                expected: usize,
                                found: ast[element].ty,
                            }) {
                                let err = err.into_type_error(&mut context.type_solver);

                                context.type_solver.error(err, ModuleSpan(ast.module, ast[element].span));
                            }

                            ast.add_expr(
                                Self::ArrayIndex { target, element },
                                context.type_solver.add_unknown(None, Some(index_expr.index.span)),
                                span,
                            )
                        }
                    }
                }
                mollie_parser::Expr::Binary(binary_expr) => {
                    let lhs = Self::from_parsed(binary_expr.lhs.value, ast, context, binary_expr.lhs.span);
                    // An assigned value is expected to be of the variable's
                    // type.
                    let rhs = if binary_expr.operator.value == Operator::Assign {
                        Self::from_parsed_expecting(binary_expr.rhs.value, ast[lhs].ty, ast, context, binary_expr.rhs.span)
                    } else {
                        Self::from_parsed(binary_expr.rhs.value, ast, context, binary_expr.rhs.span)
                    };

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: ast[lhs].ty,
                        found: ast[rhs].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[lhs].span.between(ast[rhs].span)));
                    }

                    if (binary_expr.operator.value == Operator::Assign || binary_expr.operator.value.lower().is_some())
                        && let Some(error) = Self::place_error(lhs, ast, context)
                    {
                        context.type_solver.error(error, ModuleSpan(ast.module, ast[lhs].span));
                    }

                    let ty = match binary_expr.operator.value {
                        Operator::Equal
                        | Operator::NotEqual
                        | Operator::LessThan
                        | Operator::LessThanEqual
                        | Operator::GreaterThan
                        | Operator::GreaterThanEqual => context
                            .type_solver
                            .add_info(TypeInfo::Primitive(PrimitiveType::Bool), Some(binary_expr.operator.span)),
                        _ => ast[lhs].ty,
                    };

                    ast.add_expr(
                        Self::Binary {
                            operator: binary_expr.operator,
                            lhs,
                            rhs,
                        },
                        ty,
                        span,
                    )
                }
                // `start..end` constructs `Range` of the standard library.
                mollie_parser::Expr::Range(range) => {
                    let Some(adt) = context.type_solver.context.get_adt_item(LangItem::Range) else {
                        let error = context
                            .type_solver
                            .error(TypeError::MissingLangItem { name: "Range" }, ModuleSpan(ast.module, span));
                        let ty = context.type_solver.add_info(TypeInfo::Error, None);

                        return ast.add_expr(Self::Error(error), ty, span);
                    };

                    // Values of a range are expected to be of the same type as
                    // its start.
                    let start = Self::from_parsed(range.start.value, ast, context, range.start.span);
                    let element = ast[start].ty;
                    let end = Self::from_parsed_expecting(range.end.value, element, ast, context, range.end.span);

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: element,
                        found: ast[end].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[end].span));
                    }

                    let bool_ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), Some(span));
                    let inclusive = ast.add_expr(Self::Lit(LitExpr::Bool(range.inclusive)), bool_ty, span);
                    let type_args: Box<[_]> = Box::new([element]);
                    let fields = context
                        .type_solver
                        .instantiate_adt(adt, AdtVariantRef::ZERO, &type_args)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .map(|(field, field_ty)| {
                            let value = match context.type_solver.context.def_registry.adt_types[adt].variants[AdtVariantRef::ZERO].fields[field]
                                .name
                                .as_str()
                            {
                                "start" => start,
                                "end" => end,
                                "inclusive" => inclusive,
                                _ => ExprRef::INVALID,
                            };

                            (field, field_ty, value)
                        })
                        .collect::<Box<[_]>>();
                    let ty = context.type_solver.add_info(TypeInfo::Adt(AdtTypeInfo { id: adt, type_args }), Some(span));

                    ast.add_expr(
                        Self::Construct {
                            adt,
                            variant: AdtVariantRef::ZERO,
                            fields,
                        },
                        ty,
                        span,
                    )
                }
                mollie_parser::Expr::TypeIndex(type_path_expr) => {
                    let result = TypePathResult::from_parsed(type_path_expr, ast, context, span);

                    match result {
                        TypePathResult::VFunc(adt_ref, type_info_refs, vtable, func) => {
                            let target_ty = context.type_solver.add_info(
                                TypeInfo::Adt(AdtTypeInfo {
                                    id: adt_ref,
                                    type_args: type_info_refs,
                                }),
                                None,
                            );
                            // Instantiate the impl's generics with fresh type
                            // variables, and tie them to the target type.
                            let generics: Box<[_]> = (0..context.type_solver.context.impl_registry.impls[vtable].generics.len())
                                .map(|_| context.type_solver.add_unknown(None, None))
                                .collect();
                            let impl_ty = TypeSolver::type_to_info(
                                &mut context.type_solver.type_infos,
                                context.type_solver.context,
                                context.type_solver.context.impl_registry.impls[vtable].ty,
                                &generics,
                            );

                            if let Err(err) = context.type_solver.unify(UnifyArgs {
                                expected: impl_ty,
                                found: target_ty,
                            }) {
                                let err = err.into_type_error(&mut context.type_solver);

                                context.type_solver.error(err, ModuleSpan(ast.module, span));
                            }

                            let ty = context.type_solver.context.impl_registry.impls[vtable].functions[func].ty;
                            let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &generics);

                            ast.add_expr(
                                Self::VTableIndex {
                                    target: None,
                                    target_ty,
                                    vtable,
                                    func,
                                    type_args: Box::new([]),
                                },
                                ty,
                                span,
                            )
                        }
                        TypePathResult::Adt(adt, type_info_refs, variant) => {
                            let ty = context.type_solver.add_info(
                                TypeInfo::Adt(AdtTypeInfo {
                                    id: adt,
                                    type_args: type_info_refs.clone(),
                                }),
                                None,
                            );
                            let variant = match (context.type_solver.context.def_registry.adt_types[adt].kind, variant) {
                                // Structs have a single variant.
                                (AdtKind::Struct | AdtKind::View, _) => AdtVariantRef::ZERO,
                                (AdtKind::Enum, None) => {
                                    let ty = context.type_solver.add_info(TypeInfo::Error, None);

                                    return ast.add_expr(
                                        Self::Error(context.type_solver.error(TypeError::VariantRequired(adt), ModuleSpan(ast.module, span))),
                                        ty,
                                        span,
                                    );
                                }
                                (AdtKind::Enum, Some(variant)) => variant,
                            };

                            // No values are given, so every field must have a
                            // default value.
                            let fields = context
                                .type_solver
                                .instantiate_adt(adt, variant, &type_info_refs)
                                .map(|(field_ref, field_type)| (field_ref, field_type, ExprRef::INVALID))
                                .collect::<Box<[_]>>();

                            Self::check_missing_fields(context, ast.module, adt, variant, &fields, span);

                            ast.add_expr(Self::Construct { adt, variant, fields }, ty, span)
                        }
                        TypePathResult::Trait(trait_ref, type_args) => {
                            let ty = context.type_solver.add_info(TypeInfo::Trait(TraitTypeInfo { id: trait_ref, type_args }), None);

                            ast.add_expr(
                                Self::Error(context.type_solver.error(
                                    TypeError::Unexpected {
                                        expected: TypeErrorValue::Value,
                                        found: TypeErrorValue::Trait,
                                    },
                                    ModuleSpan(ast.module, span),
                                )),
                                ty,
                                span,
                            )
                        }
                        TypePathResult::Func(func_ref) => Self::func_expr(func_ref, ast, context, span),
                        TypePathResult::Const(constant) => Self::const_expr(constant, ast, context, span),
                        // Intrinsics can only be called.
                        TypePathResult::Intrinsic => {
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            ast.add_expr(
                                Self::Error(context.type_solver.error(
                                    TypeError::Unexpected {
                                        expected: TypeErrorValue::Value,
                                        found: TypeErrorValue::Function,
                                    },
                                    ModuleSpan(ast.module, span),
                                )),
                                ty,
                                span,
                            )
                        }
                        TypePathResult::Generic(ty) => ast.add_expr(
                            Self::Error(context.type_solver.error(
                                TypeError::Unexpected {
                                    expected: TypeErrorValue::Value,
                                    found: TypeErrorValue::Generic,
                                },
                                ModuleSpan(ast.module, span),
                            )),
                            ty,
                            span,
                        ),
                        TypePathResult::Module(_) => {
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            ast.add_expr(
                                Self::Error(context.type_solver.error(
                                    TypeError::Unexpected {
                                        expected: TypeErrorValue::Value,
                                        found: TypeErrorValue::Module,
                                    },
                                    ModuleSpan(ast.module, span),
                                )),
                                ty,
                                span,
                            )
                        }
                        TypePathResult::Error(error, span) => {
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            ast.add_expr(Self::Error(error), ty, span)
                        }
                    }
                }
                mollie_parser::Expr::Array(array_expr) => {
                    // Elements of an array of an expected type (like `Shape[]`)
                    // have its element type, so they can be of different types
                    // implementing a trait.
                    let element = expected
                        .and_then(|expected| match context.type_solver.type_infos[context.type_solver.get_info(expected)].value {
                            TypeInfo::Array(array)
                                if !matches!(
                                    context.type_solver.type_infos[context.type_solver.get_info(array.element)].value,
                                    TypeInfo::Unknown(_)
                                ) =>
                            {
                                Some(array.element)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| context.type_solver.add_unknown(None, Some(span)));
                    let mut elements = Vec::with_capacity(array_expr.elements.capacity());

                    for arr_element in array_expr.elements {
                        let arr_element = Self::from_parsed_expecting(arr_element.value, element, ast, context, arr_element.span);

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: element,
                            found: ast[arr_element].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, ast[arr_element].span));
                        }

                        elements.push(arr_element);
                    }

                    let elements = elements.into_boxed_slice();
                    let size = elements.len();
                    let array_ty = context
                        .type_solver
                        .add_info(TypeInfo::Array(ArrayTypeInfo { element, size: Some(size) }), Some(span));

                    ast.add_expr(Self::Array { element, elements }, array_ty, span)
                }
                mollie_parser::Expr::IfElse(if_else_expr) => {
                    let condition = Self::from_parsed(if_else_expr.condition.value, ast, context, if_else_expr.condition.span);
                    let bool_ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), None);

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: bool_ty,
                        found: ast[condition].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[condition].span));
                    }

                    // Both branches are expected to be of the type of the
                    // whole.
                    let block = match expected {
                        Some(expected) => Block::from_parsed_expecting(if_else_expr.block.value, expected, ast, context, if_else_expr.block.span),
                        None => Block::from_parsed(if_else_expr.block.value, ast, context, if_else_expr.block.span),
                    };
                    let otherwise = if let Some(otherwise) = if_else_expr.else_block {
                        let expr = match expected {
                            Some(expected) => Self::from_parsed_expecting(otherwise.value, expected, ast, context, otherwise.span),
                            None => Self::from_parsed(otherwise.value, ast, context, otherwise.span),
                        };

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: ast[expr].ty,
                            found: ast[block].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, ast[block].span));
                        }

                        Some(expr)
                    } else {
                        let expected = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), None);

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected,
                            found: ast[block].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, ast[block].span));
                        }

                        None
                    };

                    let ty = ast[block].ty;

                    ast.add_expr(Self::IfElse { condition, block, otherwise }, ty, span)
                }
                mollie_parser::Expr::While(while_expr) => {
                    let id = context.enter_loop(while_expr.label.map(|label| label.value.0), None);
                    let condition = Self::from_parsed(while_expr.condition.value, ast, context, while_expr.condition.span);
                    let expected = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), None);

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected,
                        found: ast[condition].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[condition].span));
                    }

                    let block = Block::from_parsed(while_expr.block.value, ast, context, while_expr.block.span);
                    let ty = ast[block].ty;
                    let expected = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), None);

                    if let Err(err) = context.type_solver.unify(UnifyArgs { expected, found: ty }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[block].span));
                    }

                    context.loops.pop();

                    ast.add_expr(Self::While { condition, block, id }, ty, span)
                }
                mollie_parser::Expr::Loop(loop_expr) => {
                    // The value of the loop is given by `break`. A loop without
                    // `break` has no value.
                    let result = context.type_solver.add_unknown(Some(TypeInfo::Primitive(PrimitiveType::Void)), Some(span));
                    let id = context.enter_loop(loop_expr.label.map(|label| label.value.0), Some(result));
                    let block = Block::from_parsed(loop_expr.block.value, ast, context, loop_expr.block.span);

                    context.loops.pop();

                    ast.add_expr(Self::Loop { block, id }, result, span)
                }
                mollie_parser::Expr::Break(break_expr) => {
                    let scope = match context.find_loop(break_expr.label.as_ref(), ModuleSpan(ast.module, span)) {
                        Ok(scope) => scope,
                        Err(error) => {
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            return ast.add_expr(Self::Error(error), ty, span);
                        }
                    };

                    let value = match (break_expr.value, scope.result) {
                        (Some(value), Some(result)) => {
                            let value_span = value.span;
                            let value = Self::from_parsed_expecting(value.value, result, ast, context, value_span);

                            if let Err(err) = context.type_solver.unify(UnifyArgs {
                                expected: result,
                                found: ast[value].ty,
                            }) {
                                let err = err.into_type_error(&mut context.type_solver);

                                context.type_solver.error(err, ModuleSpan(ast.module, value_span));
                            }

                            Some(value)
                        }
                        (Some(value), None) => {
                            context.type_solver.error(TypeError::BreakValueOutsideLoop, ModuleSpan(ast.module, value.span));

                            None
                        }
                        (None, Some(result)) => {
                            let void = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), Some(span));

                            if let Err(err) = context.type_solver.unify(UnifyArgs { expected: result, found: void }) {
                                let err = err.into_type_error(&mut context.type_solver);

                                context.type_solver.error(err, ModuleSpan(ast.module, span));
                            }

                            None
                        }
                        (None, None) => None,
                    };

                    // Like `return`, it fits where any value is expected.
                    let ty = context.type_solver.add_unknown(Some(TypeInfo::Primitive(PrimitiveType::Void)), Some(span));

                    ast.add_expr(Self::Break { id: scope.id, value }, ty, span)
                }
                mollie_parser::Expr::Continue(continue_expr) => {
                    let scope = match context.find_loop(continue_expr.label.as_ref(), ModuleSpan(ast.module, span)) {
                        Ok(scope) => scope,
                        Err(error) => {
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            return ast.add_expr(Self::Error(error), ty, span);
                        }
                    };

                    let ty = context.type_solver.add_unknown(Some(TypeInfo::Primitive(PrimitiveType::Void)), Some(span));

                    ast.add_expr(Self::Continue { id: scope.id }, ty, span)
                }
                mollie_parser::Expr::Block(block_expr) => {
                    let block = match expected {
                        Some(expected) => Block::from_parsed_expecting(block_expr, expected, ast, context, span),
                        None => Block::from_parsed(block_expr, ast, context, span),
                    };
                    let ty = ast[block].ty;

                    ast.add_expr(Self::Block(block), ty, span)
                }
                // `for name in target { body }` is, like in Rust, ordinary code
                // calling the iteration traits (`Iterable::iter` and
                // `Iterator::next`) by name, so their impls are found once the
                // types are known, wherever that is in the function:
                //
                // {
                //     let mut <for N> = target.iter();
                //     while <for N>.next() is std::option::Option::Some { value: <for value N> } {
                //         let mut name = <for value N>;
                //         body
                //     }
                // }
                //
                // The path to `Some` is complete, so programs can declare their
                // own `Option`, and the loop variable is bound by `let`, so it
                // can have any name.
                mollie_parser::Expr::ForIn(for_in) => {
                    use mollie_parser::{
                        BlockExpr, CallArg, FuncCallExpr, Ident, IndexExpr, IndexTarget, IsExpr, IsPattern as Pattern, NameValuePattern, Stmt as ParsedStmt,
                        TypePathExpr, TypePathSegment, TypePattern, VariableDecl, WhileExpr,
                    };

                    let mollie_parser::ForInExpr {
                        label,
                        name,
                        target,
                        mut block,
                    } = for_in;
                    let target_span = target.span;
                    let iterator = format!("<for {}>", span.start);
                    let item = format!("<for value {}>", span.start);
                    let target_name = format!("<for target {}>", span.start);
                    let target = Self::from_parsed(target.value, ast, context, target_span);
                    let target_ty = ast[target].ty;

                    // A value whose type is already known must implement
                    // `Iterable` (generics and trait objects are checked by the
                    // calls).
                    if !matches!(
                        context.type_solver.type_infos[context.type_solver.get_info(target_ty)].value,
                        TypeInfo::Unknown(_) | TypeInfo::Generic(_) | TypeInfo::Trait(_) | TypeInfo::Error
                    ) {
                        let solved = context.type_solver.solve(target_ty);
                        let iterable = context.type_solver.context.get_trait_item(LangItem::IntoIterator);

                        if let Some(iterable) = iterable
                            && context.type_solver.context.types[solved] != Type::Error
                            && context.type_solver.context.find_vtable(solved, Some(iterable)).is_none()
                        {
                            let error = context
                                .type_solver
                                .error(TypeError::NotIterable { ty: solved }, ModuleSpan(ast.module, target_span));
                            let ty = context.type_solver.add_info(TypeInfo::Error, None);

                            return ast.add_expr(Self::Error(error), ty, span);
                        }
                    }

                    context.type_solver.set_var_with_mutability(&target_name, target_ty, false);

                    let target_stmt = ast.add_stmt(Stmt::NewVar {
                        mutable: false,
                        name: target_name.clone(),
                        value: target,
                    });
                    let call = |target: Positioned<mollie_parser::Expr>, method: &str, at: Span| {
                        at.wrap(mollie_parser::Expr::FunctionCall(FuncCallExpr {
                            function: Box::new(at.wrap(mollie_parser::Expr::Index(IndexExpr {
                                target: Box::new(target),
                                index: at.wrap(IndexTarget::Named(Ident::new(method))),
                            }))),
                            args: at.wrap(Vec::<Positioned<CallArg>>::new()),
                        }))
                    };
                    let some = span.wrap(TypePathExpr {
                        segments: ["std", "option", "Option", "Some"]
                            .into_iter()
                            .map(|segment| {
                                span.wrap(TypePathSegment {
                                    name: span.wrap(Ident::new(segment)),
                                    args: None,
                                })
                            })
                            .collect(),
                    });
                    let binding = name.span.wrap(Pattern::Type {
                        ty: name.span.wrap(TypePathExpr {
                            segments: vec![name.span.wrap(TypePathSegment {
                                name: name.span.wrap(Ident::new(item.as_str())),
                                args: None,
                            })],
                        }),
                        pattern: None,
                    });

                    block.value.stmts.insert(
                        0,
                        name.span.wrap(ParsedStmt::VariableDecl(VariableDecl {
                            mutable: Some(name.span.wrap(())),
                            name: name.clone(),
                            ty: None,
                            value: name.span.wrap(mollie_parser::Expr::Ident(Ident::new(item.as_str()))),
                        })),
                    );
                    let declaration = VariableDecl {
                        mutable: Some(target_span.wrap(())),
                        name: target_span.wrap(Ident::new(iterator.as_str())),
                        ty: None,
                        value: call(
                            target_span.wrap(mollie_parser::Expr::Ident(Ident::new(target_name.as_str()))),
                            "iter",
                            target_span,
                        ),
                    };
                    let condition = span.wrap(mollie_parser::Expr::Is(IsExpr {
                        target: Box::new(call(
                            target_span.wrap(mollie_parser::Expr::Ident(Ident::new(iterator.as_str()))),
                            "next",
                            target_span,
                        )),
                        pattern: span.wrap(Pattern::Type {
                            ty: some,
                            pattern: Some(span.wrap(TypePattern::Values(vec![span.wrap(NameValuePattern {
                                name: span.wrap(Ident::new("value")),
                                value: Some(binding),
                            })]))),
                        }),
                    }));
                    let while_loop = mollie_parser::Expr::While(WhileExpr {
                        label,
                        condition: Box::new(condition),
                        block,
                    });
                    let desugared = mollie_parser::Expr::Block(BlockExpr {
                        stmts: vec![
                            target_span.wrap(ParsedStmt::VariableDecl(declaration)),
                            span.wrap(ParsedStmt::Expression(while_loop)),
                        ],
                        final_stmt: None,
                    });

                    let lowered = Self::from_parsed(desugared, ast, context, span);

                    // The target is evaluated first, in the block.
                    if let Self::Block(block) = ast[lowered].value {
                        let stmts = std::iter::once(target_stmt).chain(ast.blocks[block].value.stmts.iter().copied()).collect();

                        ast.blocks[block].value.stmts = stmts;
                    }

                    lowered
                }
                mollie_parser::Expr::Is(is_expr) => {
                    let target = Self::from_parsed(is_expr.target.value, ast, context, is_expr.target.span);
                    let pattern = IsPattern::lower(is_expr.pattern.value, ast[target].ty, ast, context, is_expr.pattern.span);

                    ast.add_expr(
                        Self::IsPattern { target, pattern },
                        context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Bool), None),
                        span,
                    )
                }
                mollie_parser::Expr::Match(match_expr) => Self::lower_match(match_expr, expected, ast, context, span),
                mollie_parser::Expr::Return(value) => {
                    let Some(returns) = context.returns else {
                        return Self::error_expr(TypeError::ReturnOutsideFunction, ast, context, span);
                    };

                    let value = value.map(|value| Self::from_parsed_expecting(value.value, returns, ast, context, value.span));
                    let found = match value {
                        Some(value) => ast[value].ty,
                        None => context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), Some(span)),
                    };

                    if let Err(err) = context.type_solver.unify(UnifyArgs { expected: returns, found }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context
                            .type_solver
                            .error(err, ModuleSpan(ast.module, value.map_or(span, |value| ast[value].span)));
                    }

                    // `return` produces no value, so it fits where any value is
                    // expected: `None => return 0`.
                    let ty = context.type_solver.add_unknown(Some(TypeInfo::Primitive(PrimitiveType::Void)), Some(span));

                    ast.add_expr(Self::Return(value), ty, span)
                }
                mollie_parser::Expr::Try(target) => Self::lower_try(*target, expected, ast, context, span),
                mollie_parser::Expr::Cast(expr, new_type) => {
                    let expr = Self::from_parsed(expr.value, ast, context, expr.span);

                    ast.add_expr(
                        Self::TypeCast(expr, new_type.value),
                        context.type_solver.add_info(TypeInfo::Primitive(new_type.value), Some(new_type.span)),
                        span,
                    )
                }
                mollie_parser::Expr::Closure(closure_expr) => {
                    let current_frame = context.type_solver.push_frame();
                    let prev_captures = mem::take(&mut context.captures);
                    let prev_current_frame = context.current_frame.replace(current_frame);

                    // A closure passed where a function is expected takes its
                    // parameter types: `numbers.map(|value| { "${value}" })`.
                    let expected_args = expected.and_then(|expected| match &context.type_solver.type_infos[context.type_solver.get_info(expected)].value {
                        TypeInfo::Func(func) if func.args.len() == closure_expr.args.value.len() => Some(func.args.clone()),
                        _ => None,
                    });
                    let args: Box<[_]> = closure_expr
                        .args
                        .value
                        .into_iter()
                        .enumerate()
                        .map(|(index, arg)| {
                            let ty = expected_args
                                .as_ref()
                                .map_or_else(|| context.type_solver.add_unknown(None, Some(arg.span)), |args| args[index]);

                            ast.declare_var(context, arg.value.0.clone(), ty, true, arg.span);

                            Arg {
                                name: arg.value.0,
                                kind: ArgType::Regular,
                                ty,
                            }
                        })
                        .collect();

                    // A closure passed where a function is expected returns its
                    // type. `return` in its body returns from it.
                    let returns = expected
                        .and_then(|expected| match &context.type_solver.type_infos[context.type_solver.get_info(expected)].value {
                            TypeInfo::Func(func) if func.args.len() == args.len() => Some(func.returns),
                            _ => None,
                        })
                        .unwrap_or_else(|| context.type_solver.add_unknown(None, Some(span)));
                    let outer_returns = context.returns.replace(returns);
                    // `break` and `continue` can't leave a closure.
                    let outer_loops = mem::take(&mut context.loops);
                    let body = Block::from_parsed_expecting(closure_expr.body.value, returns, ast, context, closure_expr.body.span);

                    context.returns = outer_returns;
                    context.loops = outer_loops;

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: returns,
                        found: ast[body].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, ast[body].span));
                    }
                    let ty = context.type_solver.add_info(
                        TypeInfo::Func(FuncTypeInfo {
                            args: args.iter().map(|arg| arg.ty).collect(),
                            returns,
                        }),
                        Some(span),
                    );

                    context.type_solver.pop_frame();
                    context.current_frame = prev_current_frame;

                    let captures = mem::replace(&mut context.captures, prev_captures).into_boxed_slice();

                    ast.add_expr(Self::Closure { args, captures, body }, ty, span)
                }
                mollie_parser::Expr::Ident(ident) => {
                    if let Some((frame, ty)) = context.type_solver.get_var(&ident) {
                        if let Some(current_frame) = context.current_frame
                            && frame < current_frame
                        {
                            context.captures.push((ident.0.clone(), ty));
                        }

                        ast.use_var(context, &ident.0, span);

                        ast.add_expr(Self::Var(ident.0), ty, span)
                    } else if let Some((adt, type_args, variant)) = Self::expected_variant(expected, &ident.0, context) {
                        // `None` where an `Option` is expected.
                        let fields = context
                            .type_solver
                            .instantiate_adt(adt, variant, &type_args)
                            .map(|(field, field_ty)| (field, field_ty, ExprRef::INVALID))
                            .collect::<Box<[_]>>();
                        let ty = context.type_solver.add_info(TypeInfo::Adt(AdtTypeInfo { id: adt, type_args }), Some(span));

                        Self::check_missing_fields(context, ast.module, adt, variant, &fields, span);

                        ast.add_expr(Self::Construct { adt, variant, fields }, ty, span)
                    } else if let Some(item) = context.type_solver.context.def_registry.lookup(ast.module, &ident.0) {
                        match item {
                            ModuleItem::SubModule(_) => {
                                let ty = context.type_solver.add_info(TypeInfo::Error, None);

                                ast.add_expr(
                                    Self::Error(context.type_solver.error(
                                        TypeError::Unexpected {
                                            expected: TypeErrorValue::Value,
                                            found: TypeErrorValue::Module,
                                        },
                                        ModuleSpan(ast.module, span),
                                    )),
                                    ty,
                                    span,
                                )
                            }
                            ModuleItem::Adt(adt) => {
                                let found = TypeErrorValue::Adt(SpecialAdtKind::Specific(context.type_solver.context.def_registry.adt_types[adt].kind));
                                let ty = context.type_solver.add_info(TypeInfo::Error, None);

                                ast.add_expr(
                                    Self::Error(context.type_solver.error(
                                        TypeError::Unexpected {
                                            expected: TypeErrorValue::Value,
                                            found,
                                        },
                                        ModuleSpan(ast.module, span),
                                    )),
                                    ty,
                                    span,
                                )
                            }
                            ModuleItem::Trait(_) => {
                                let ty = context.type_solver.add_info(TypeInfo::Error, None);

                                ast.add_expr(
                                    Self::Error(context.type_solver.error(
                                        TypeError::Unexpected {
                                            expected: TypeErrorValue::Value,
                                            found: TypeErrorValue::Trait,
                                        },
                                        ModuleSpan(ast.module, span),
                                    )),
                                    ty,
                                    span,
                                )
                            }
                            ModuleItem::Func(func_ref) => Self::func_expr(func_ref, ast, context, span),
                            ModuleItem::Const(constant) => Self::const_expr(constant, ast, context, span),
                            // Intrinsics can only be called.
                            ModuleItem::Intrinsic(..) => {
                                let ty = context.type_solver.add_info(TypeInfo::Error, None);

                                ast.add_expr(
                                    Self::Error(context.type_solver.error(
                                        TypeError::Unexpected {
                                            expected: TypeErrorValue::Value,
                                            found: TypeErrorValue::Function,
                                        },
                                        ModuleSpan(ast.module, span),
                                    )),
                                    ty,
                                    span,
                                )
                            }
                        }
                    } else {
                        let ty = context.type_solver.add_info(TypeInfo::Error, None);

                        ast.add_expr(
                            Self::Error(context.type_solver.error(
                                TypeError::NotFound {
                                    name: ident.0,
                                    was_looking_for: LookupType::Variable,
                                },
                                ModuleSpan(ast.module, span),
                            )),
                            ty,
                            span,
                        )
                    }
                }
                mollie_parser::Expr::This => {
                    if let Some((frame, ty)) = context.type_solver.get_var("self") {
                        if let Some(current_frame) = context.current_frame
                            && frame < current_frame
                        {
                            context.captures.push(("self".into(), ty));
                        }

                        ast.use_var(context, "self", span);

                        ast.add_expr(Self::Var("self".to_string()), ty, span)
                    } else {
                        let ty = context.type_solver.add_info(TypeInfo::Error, None);

                        ast.add_expr(
                            Self::Error(context.type_solver.error(
                                TypeError::NotFound {
                                    name: String::from("self"),
                                    was_looking_for: LookupType::Variable,
                                },
                                ModuleSpan(ast.module, span),
                            )),
                            ty,
                            span,
                        )
                    }
                }
                // `()`: nothing, like an empty block.
                mollie_parser::Expr::Nothing => {
                    let ty = context.type_solver.add_info(TypeInfo::Primitive(PrimitiveType::Void), Some(span));
                    let block = ast.add_block(
                        Block {
                            stmts: Box::new([]),
                            expr: None,
                        },
                        ty,
                        span,
                    );

                    ast.add_expr(Self::Block(block), ty, span)
                }
            }
        })
    }
}

impl IntoConstVal for ExprRef {
    fn into_const_val(self, ast: &TypedAST<SolvedPass>, type_context: &TyCtxt, const_context: &mut ConstantContext) -> Result<ConstantValue, ()> {
        Ok(match &ast[self].value {
            Expr::Lit(literal_expr) => match (literal_expr, &type_context.types[ast[self].ty]) {
                (&LitExpr::Int(value), Type::Primitive(primitive)) => match primitive {
                    PrimitiveType::Int(IntType::ISize) => ConstantValue::ISize(value.try_into().map_err(|_| ())?),
                    PrimitiveType::Int(IntType::I64) => ConstantValue::I64(value),
                    PrimitiveType::Int(IntType::I32) => ConstantValue::I32(value.try_into().map_err(|_| ())?),
                    PrimitiveType::Int(IntType::I16) => ConstantValue::I16(value.try_into().map_err(|_| ())?),
                    PrimitiveType::Int(IntType::I8) => ConstantValue::I8(value.try_into().map_err(|_| ())?),
                    PrimitiveType::UInt(UIntType::USize) => ConstantValue::USize(value.try_into().map_err(|_| ())?),
                    PrimitiveType::UInt(UIntType::U64) => ConstantValue::U64(value.cast_unsigned()),
                    PrimitiveType::UInt(UIntType::U32) => ConstantValue::U32(value.try_into().map_err(|_| ())?),
                    PrimitiveType::UInt(UIntType::U16) => ConstantValue::U16(value.try_into().map_err(|_| ())?),
                    PrimitiveType::UInt(UIntType::U8) => ConstantValue::U8(value.try_into().map_err(|_| ())?),
                    // Mistyped literals are reported by the type checker.
                    _ => return Err(()),
                },
                (&LitExpr::F32(value), Type::Primitive(PrimitiveType::F32)) => ConstantValue::F32(value),
                (&LitExpr::Bool(value), Type::Primitive(PrimitiveType::Bool)) => ConstantValue::Bool(value),
                (LitExpr::String(value), Type::Primitive(PrimitiveType::String)) => ConstantValue::String(value.clone()),
                // Mistyped literals are reported by the type checker.
                _ => return Err(()),
            },
            Expr::Unary { operator, expr } => {
                let expr = expr.into_const_val(ast, type_context, const_context)?;

                match (expr, operator.value) {
                    // Negating the minimum value overflows.
                    (ConstantValue::I8(v), UnaryOperator::Neg) => ConstantValue::I8(v.checked_neg().ok_or(())?),
                    (ConstantValue::I16(v), UnaryOperator::Neg) => ConstantValue::I16(v.checked_neg().ok_or(())?),
                    (ConstantValue::I32(v), UnaryOperator::Neg) => ConstantValue::I32(v.checked_neg().ok_or(())?),
                    (ConstantValue::I64(v), UnaryOperator::Neg) => ConstantValue::I64(v.checked_neg().ok_or(())?),
                    (ConstantValue::ISize(v), UnaryOperator::Neg) => ConstantValue::ISize(v.checked_neg().ok_or(())?),
                    (ConstantValue::F32(v), UnaryOperator::Neg) => ConstantValue::F32(-v),
                    (ConstantValue::Bool(v), UnaryOperator::Not) => ConstantValue::Bool(!v),
                    _ => return Err(()),
                }
            }
            Expr::IfElse { condition, block, otherwise } => {
                if condition.into_const_val(ast, type_context, const_context)? == ConstantValue::Bool(true) {
                    block.into_const_val(ast, type_context, const_context)?
                } else if let Some(block) = otherwise {
                    block.into_const_val(ast, type_context, const_context)?
                } else {
                    ConstantValue::Nothing
                }
            }
            Expr::Block(block) => {
                const_context.push_frame();

                let value = block.into_const_val(ast, type_context, const_context)?;

                const_context.pop_frame();

                value
            }
            // Variables outside of the evaluated expression aren't constant.
            Expr::Var(name) => const_context.search_var(name).ok_or(())?.clone(),
            Expr::While { condition, block, .. } => {
                while condition.into_const_val(ast, type_context, const_context)? == ConstantValue::Bool(true) {
                    const_context.step().ok_or(())?;

                    block.into_const_val(ast, type_context, const_context)?;
                }

                ConstantValue::Nothing
            }
            Expr::Array { elements, .. } => ConstantValue::Array(
                elements
                    .iter()
                    .map(|expr| expr.into_const_val(ast, type_context, const_context))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::Binary { operator, lhs, rhs } => match (
                lhs.into_const_val(ast, type_context, const_context)?,
                operator.value,
                rhs.into_const_val(ast, type_context, const_context)?,
            ) {
                (ConstantValue::I8(a), Operator::Add, ConstantValue::I8(b)) => ConstantValue::I8(a.checked_add(b).ok_or(())?),
                (ConstantValue::U8(a), Operator::Add, ConstantValue::U8(b)) => ConstantValue::U8(a.checked_add(b).ok_or(())?),
                (ConstantValue::I16(a), Operator::Add, ConstantValue::I16(b)) => ConstantValue::I16(a.checked_add(b).ok_or(())?),
                (ConstantValue::U16(a), Operator::Add, ConstantValue::U16(b)) => ConstantValue::U16(a.checked_add(b).ok_or(())?),
                (ConstantValue::I32(a), Operator::Add, ConstantValue::I32(b)) => ConstantValue::I32(a.checked_add(b).ok_or(())?),
                (ConstantValue::U32(a), Operator::Add, ConstantValue::U32(b)) => ConstantValue::U32(a.checked_add(b).ok_or(())?),
                (ConstantValue::I64(a), Operator::Add, ConstantValue::I64(b)) => ConstantValue::I64(a.checked_add(b).ok_or(())?),
                (ConstantValue::U64(a), Operator::Add, ConstantValue::U64(b)) => ConstantValue::U64(a.checked_add(b).ok_or(())?),
                (ConstantValue::ISize(a), Operator::Add, ConstantValue::ISize(b)) => ConstantValue::ISize(a.checked_add(b).ok_or(())?),
                (ConstantValue::USize(a), Operator::Add, ConstantValue::USize(b)) => ConstantValue::USize(a.checked_add(b).ok_or(())?),
                (ConstantValue::F32(a), Operator::Add, ConstantValue::F32(b)) => ConstantValue::F32(a + b),
                (ConstantValue::I8(a), Operator::Sub, ConstantValue::I8(b)) => ConstantValue::I8(a.checked_sub(b).ok_or(())?),
                (ConstantValue::U8(a), Operator::Sub, ConstantValue::U8(b)) => ConstantValue::U8(a.checked_sub(b).ok_or(())?),
                (ConstantValue::I16(a), Operator::Sub, ConstantValue::I16(b)) => ConstantValue::I16(a.checked_sub(b).ok_or(())?),
                (ConstantValue::U16(a), Operator::Sub, ConstantValue::U16(b)) => ConstantValue::U16(a.checked_sub(b).ok_or(())?),
                (ConstantValue::I32(a), Operator::Sub, ConstantValue::I32(b)) => ConstantValue::I32(a.checked_sub(b).ok_or(())?),
                (ConstantValue::U32(a), Operator::Sub, ConstantValue::U32(b)) => ConstantValue::U32(a.checked_sub(b).ok_or(())?),
                (ConstantValue::I64(a), Operator::Sub, ConstantValue::I64(b)) => ConstantValue::I64(a.checked_sub(b).ok_or(())?),
                (ConstantValue::U64(a), Operator::Sub, ConstantValue::U64(b)) => ConstantValue::U64(a.checked_sub(b).ok_or(())?),
                (ConstantValue::ISize(a), Operator::Sub, ConstantValue::ISize(b)) => ConstantValue::ISize(a.checked_sub(b).ok_or(())?),
                (ConstantValue::USize(a), Operator::Sub, ConstantValue::USize(b)) => ConstantValue::USize(a.checked_sub(b).ok_or(())?),
                (ConstantValue::F32(a), Operator::Sub, ConstantValue::F32(b)) => ConstantValue::F32(a - b),
                (ConstantValue::I8(a), Operator::Mul, ConstantValue::I8(b)) => ConstantValue::I8(a.checked_mul(b).ok_or(())?),
                (ConstantValue::U8(a), Operator::Mul, ConstantValue::U8(b)) => ConstantValue::U8(a.checked_mul(b).ok_or(())?),
                (ConstantValue::I16(a), Operator::Mul, ConstantValue::I16(b)) => ConstantValue::I16(a.checked_mul(b).ok_or(())?),
                (ConstantValue::U16(a), Operator::Mul, ConstantValue::U16(b)) => ConstantValue::U16(a.checked_mul(b).ok_or(())?),
                (ConstantValue::I32(a), Operator::Mul, ConstantValue::I32(b)) => ConstantValue::I32(a.checked_mul(b).ok_or(())?),
                (ConstantValue::U32(a), Operator::Mul, ConstantValue::U32(b)) => ConstantValue::U32(a.checked_mul(b).ok_or(())?),
                (ConstantValue::I64(a), Operator::Mul, ConstantValue::I64(b)) => ConstantValue::I64(a.checked_mul(b).ok_or(())?),
                (ConstantValue::U64(a), Operator::Mul, ConstantValue::U64(b)) => ConstantValue::U64(a.checked_mul(b).ok_or(())?),
                (ConstantValue::ISize(a), Operator::Mul, ConstantValue::ISize(b)) => ConstantValue::ISize(a.checked_mul(b).ok_or(())?),
                (ConstantValue::USize(a), Operator::Mul, ConstantValue::USize(b)) => ConstantValue::USize(a.checked_mul(b).ok_or(())?),
                (ConstantValue::F32(a), Operator::Mul, ConstantValue::F32(b)) => ConstantValue::F32(a * b),
                (ConstantValue::I8(a), Operator::Div, ConstantValue::I8(b)) => ConstantValue::I8(a.checked_div(b).ok_or(())?),
                (ConstantValue::I8(a), Operator::Rem, ConstantValue::I8(b)) => ConstantValue::I8(a.checked_rem(b).ok_or(())?),
                (ConstantValue::U8(a), Operator::Div, ConstantValue::U8(b)) => ConstantValue::U8(a.checked_div(b).ok_or(())?),
                (ConstantValue::U8(a), Operator::Rem, ConstantValue::U8(b)) => ConstantValue::U8(a.checked_rem(b).ok_or(())?),
                (ConstantValue::I16(a), Operator::Div, ConstantValue::I16(b)) => ConstantValue::I16(a.checked_div(b).ok_or(())?),
                (ConstantValue::I16(a), Operator::Rem, ConstantValue::I16(b)) => ConstantValue::I16(a.checked_rem(b).ok_or(())?),
                (ConstantValue::U16(a), Operator::Div, ConstantValue::U16(b)) => ConstantValue::U16(a.checked_div(b).ok_or(())?),
                (ConstantValue::U16(a), Operator::Rem, ConstantValue::U16(b)) => ConstantValue::U16(a.checked_rem(b).ok_or(())?),
                (ConstantValue::I32(a), Operator::Div, ConstantValue::I32(b)) => ConstantValue::I32(a.checked_div(b).ok_or(())?),
                (ConstantValue::I32(a), Operator::Rem, ConstantValue::I32(b)) => ConstantValue::I32(a.checked_rem(b).ok_or(())?),
                (ConstantValue::U32(a), Operator::Div, ConstantValue::U32(b)) => ConstantValue::U32(a.checked_div(b).ok_or(())?),
                (ConstantValue::U32(a), Operator::Rem, ConstantValue::U32(b)) => ConstantValue::U32(a.checked_rem(b).ok_or(())?),
                (ConstantValue::I64(a), Operator::Div, ConstantValue::I64(b)) => ConstantValue::I64(a.checked_div(b).ok_or(())?),
                (ConstantValue::I64(a), Operator::Rem, ConstantValue::I64(b)) => ConstantValue::I64(a.checked_rem(b).ok_or(())?),
                (ConstantValue::U64(a), Operator::Div, ConstantValue::U64(b)) => ConstantValue::U64(a.checked_div(b).ok_or(())?),
                (ConstantValue::U64(a), Operator::Rem, ConstantValue::U64(b)) => ConstantValue::U64(a.checked_rem(b).ok_or(())?),
                (ConstantValue::ISize(a), Operator::Div, ConstantValue::ISize(b)) => ConstantValue::ISize(a.checked_div(b).ok_or(())?),
                (ConstantValue::ISize(a), Operator::Rem, ConstantValue::ISize(b)) => ConstantValue::ISize(a.checked_rem(b).ok_or(())?),
                (ConstantValue::USize(a), Operator::Div, ConstantValue::USize(b)) => ConstantValue::USize(a.checked_div(b).ok_or(())?),
                (ConstantValue::USize(a), Operator::Rem, ConstantValue::USize(b)) => ConstantValue::USize(a.checked_rem(b).ok_or(())?),
                (ConstantValue::F32(a), Operator::Div, ConstantValue::F32(b)) => ConstantValue::F32(a / b),
                (ConstantValue::Bool(a), Operator::And, ConstantValue::Bool(b)) => ConstantValue::Bool(a && b),
                (ConstantValue::Bool(a), Operator::Or, ConstantValue::Bool(b)) => ConstantValue::Bool(a || b),
                (a, Operator::Equal, b) => ConstantValue::Bool(a == b),
                (a, Operator::NotEqual, b) => ConstantValue::Bool(a != b),
                (a, Operator::GreaterThan, b) => ConstantValue::Bool(a > b),
                (a, Operator::LessThan, b) => ConstantValue::Bool(a < b),
                (a, Operator::GreaterThanEqual, b) => ConstantValue::Bool(a >= b),
                (a, Operator::LessThanEqual, b) => ConstantValue::Bool(a <= b),
                // Not evaluable, e.g. assignments or mistyped operands (already
                // reported).
                _ => return Err(()),
            },
            // Values only known at run time.
            Expr::Call { .. }
            | Expr::Closure { .. }
            | Expr::IsPattern { .. }
            | Expr::AdtIndex { .. }
            | Expr::VTableIndex { .. }
            | Expr::ArrayIndex { .. }
            | Expr::Func { .. }
            | Expr::TypeCast(..)
            | Expr::Format { .. }
            | Expr::TraitFunc { .. }
            | Expr::BoundFunc { .. }
            | Expr::Return(_)
            | Expr::Loop { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. }
            | Expr::Panic(_)
            | Expr::Unreachable
            // Already reported.
            | Expr::Error(_) => return Err(()),
            Expr::Construct { variant, fields, .. } => ConstantValue::Construct {
                ty: ast[self].ty.index(),
                variant: variant.index(),
                fields: fields
                    .iter()
                    .map(|v| {
                        (
                            v.0.index(),
                            if v.2 == Self::INVALID {
                                None
                            } else {
                                v.2.into_const_val(ast, type_context, const_context).ok()
                            },
                        )
                    })
                    .collect(),
            },
            Expr::Const(constant) => type_context.def_registry.constants[*constant].value.clone().ok_or(())?,
        })
    }
}
