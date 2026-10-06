mod block;
mod builtins;
mod expr;
mod module_map;
mod std_lib;
mod stmt;
mod ty;

use std::{iter::once, mem, ops::Index};

use derive_where::derive_where;
use indexmap::IndexMap;
use mollie_const::ConstantValue;
use mollie_index::{Idx, IndexVec};
use mollie_shared::{Operator, Positioned, Span, UnaryOperator, limits};
use mollie_typing::{
    Adt, AdtKind, AdtRef, AdtVariantRef, Arg, Bound, ConstRef, DiagnosticContext, FieldRef, FuncRef, ImplRef, ImplRegistry, ModuleId, ModuleSpan,
    PrimitiveType, SpecialAdtKind, Trait, TraitRef, TyCtxt, Type, TypeError, TypeErrorRef, TypeErrorValue, TypeFrameRef, TypeInfoRef, TypeRef, TypeSolver,
    TypeStorage, UnifyArgs, VFuncRef,
};

pub use crate::{
    block::{Block, BlockRef},
    expr::{Expr, ExprRef, IsPattern, LitExpr, LoopId},
    module_map::{FileModuleLoader, ModuleLoader, ModuleMap, ParsedModule},
    std_lib::std_sources,
    stmt::{Stmt, StmtRef},
};

pub enum FunctionBody {
    Local {
        ast: TypedAST,
        entry: BlockRef,
    },
    Import(&'static str),
    BuiltIn(&'static str),
    /// A function of a trait impl that uses the trait's default: the
    /// function `func` instantiated with `type_args` (`Self` and the trait's
    /// type arguments, in terms of the impl's generics).
    Default {
        func: FuncRef,
        type_args: Box<[TypeRef]>,
    },
    /// A function of the host, called by address: `code` is an
    /// `extern "C" fn(context, result, arguments...)` writing its result to
    /// `result`, and gets `context` first (see `mollie::host`). Values of value
    /// types are passed as pointers.
    Host {
        code: usize,
        context: usize,
    },
    /// A function declared by a stub of the host's API (`extern func`), for
    /// tools: it can't be compiled.
    Stub,
}

/// Declarations of constants that aren't evaluated yet. Constants are
/// evaluated on demand, when they're used, so they can use each other in any
/// order.
#[derive(Debug, Default)]
pub struct PendingConstants {
    decls: IndexMap<ConstRef, (ModuleId, mollie_parser::ConstDecl, Span)>,
    /// Constants being evaluated, to report cycles.
    evaluating: Vec<ConstRef>,
}

impl PendingConstants {
    pub(crate) fn add(&mut self, constant: ConstRef, module: ModuleId, decl: mollie_parser::ConstDecl, span: Span) {
        self.decls.insert(constant, (module, decl, span));
    }

    pub(crate) fn ids(&self) -> Vec<ConstRef> {
        self.decls.keys().copied().collect()
    }
}

pub struct TypedASTContext {
    pub vtables: VTableMap,
    pub functions: FunctionMap,
    pub constants: PendingConstants,
    pub tcx: TyCtxt,
    pub diagnostics: DiagnosticContext,
    /// Whether programs get the standard library (`std` and its prelude).
    /// It's loaded with the first program, so it must be disabled before.
    pub use_std: bool,
}

impl Default for TypedASTContext {
    fn default() -> Self {
        Self::new(TyCtxt::new())
    }
}

impl TypedASTContext {
    pub fn new(tcx: TyCtxt) -> Self {
        let mut context = Self {
            vtables: IndexMap::new(),
            functions: IndexMap::new(),
            constants: PendingConstants::default(),
            tcx,
            diagnostics: DiagnosticContext::default(),
            use_std: true,
        };

        builtins::register(&mut context);

        context
    }

    pub fn take_ref(&mut self) -> TypedASTContextRef<'_> {
        TypedASTContextRef::new(
            &mut self.vtables,
            &mut self.functions,
            &mut self.constants,
            TypeSolver::from_context(&mut self.tcx, &mut self.diagnostics),
        )
    }

    /// Type-checks a program. `source` is its root module: declarations and
    /// top-level code, which must evaluate to `returns` and can use `params`
    /// as variables. Submodules are loaded with `loader`.
    ///
    /// Every program gets a new root module (the `module` of the returned
    /// AST), so programs don't share their items: a program can be checked
    /// again, e.g. when it's reloaded.
    ///
    /// Errors are collected in [`TypedASTContext::diagnostics`].
    pub fn process<L: ModuleLoader, I: IntoIterator<Item = (String, TypeRef)>>(
        &mut self,
        loader: L,
        source: &str,
        params: I,
        returns: TypeRef,
    ) -> (TypedAST, BlockRef) {
        if self.use_std {
            self.load_std();
        }

        // Every program has its own root module (see
        // `DefRegistry::register_program_root`).
        let root = self.tcx.def_registry.register_program_root();
        let mut map = ModuleMap::new(loader);

        map.register_from_str(&mut self.tcx.def_registry, &mut self.diagnostics, source, root);
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

        for (name, ty) in params {
            let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[]);

            context.type_solver.set_var(name, ty);
        }

        map.process_root(&mut context, returns)
    }
}

pub type VTableMap = IndexMap<ImplRef, IndexMap<VFuncRef, FunctionBody>>;
pub type FunctionMap = IndexMap<FuncRef, FunctionBody>;
pub type Captures = Vec<(String, TypeInfoRef)>;

pub struct TypedASTContextRef<'a> {
    pub vtables: &'a mut VTableMap,
    pub functions: &'a mut FunctionMap,
    pub constants: &'a mut PendingConstants,
    pub type_solver: TypeSolver<'a>,
    pub captures: Captures,
    pub current_frame: Option<TypeFrameRef>,
    pub inside_call: bool,
    /// Type expected of the expression being lowered, set by the expression
    /// containing it (see [`Expr::from_parsed_expecting`]). It's only a hint
    /// for expressions whose type depends on it, like array literals and
    /// variants without the name of their enum; the type is still checked by
    /// the container.
    pub expected: Option<TypeInfoRef>,
    /// Return type of the function (or closure, or program) being lowered,
    /// for `return` and `?`.
    pub returns: Option<TypeInfoRef>,
    /// Loops around the code being lowered, the innermost last, for `break`
    /// and `continue`.
    pub loops: Vec<LoopScope>,
    /// Number of loops lowered, for their ids.
    loop_count: usize,
    /// The trait impl whose function is being lowered, for `super.name()`.
    pub trait_impl: Option<ImplRef>,
}

/// A loop around the code being lowered.
#[derive(Debug, Clone)]
pub struct LoopScope {
    pub id: LoopId,
    pub label: Option<String>,
    /// Type of the value given by `break`, only for `loop`.
    pub result: Option<TypeInfoRef>,
}

impl<'a> TypedASTContextRef<'a> {
    pub const fn new(vtables: &'a mut VTableMap, functions: &'a mut FunctionMap, constants: &'a mut PendingConstants, type_solver: TypeSolver<'a>) -> Self {
        Self {
            vtables,
            functions,
            constants,
            type_solver,
            captures: Vec::new(),
            current_frame: None,
            inside_call: false,
            expected: None,
            returns: None,
            loops: Vec::new(),
            loop_count: 0,
            trait_impl: None,
        }
    }

    /// Starts lowering a loop, returning its id. The caller pops it from
    /// [`TypedASTContextRef::loops`] once its body is lowered.
    pub(crate) fn enter_loop(&mut self, label: Option<String>, result: Option<TypeInfoRef>) -> LoopId {
        let id = LoopId::new(self.loop_count);

        self.loop_count += 1;
        self.loops.push(LoopScope { id, label, result });

        id
    }

    /// The loop left by `break` or `continue`: the innermost one, or the one
    /// labeled `label`. Reports an error if there's none.
    pub(crate) fn find_loop(&mut self, label: Option<&Positioned<mollie_parser::Ident>>, span: ModuleSpan) -> Result<LoopScope, TypeErrorRef> {
        let scope = match label {
            Some(label) => self.loops.iter().rev().find(|scope| scope.label.as_deref() == Some(label.value.0.as_str())),
            None => self.loops.last(),
        };

        scope.cloned().ok_or_else(|| {
            let error = label.map_or(TypeError::BreakOutsideLoop, |label| TypeError::UnknownLabel { name: label.value.0.clone() });

            self.type_solver.error(error, span)
        })
    }

    pub const fn inside_call(&mut self) -> &mut Self {
        self.inside_call = true;

        self
    }

    pub fn fork(&mut self) -> TypedASTContextRef<'_> {
        TypedASTContextRef {
            vtables: self.vtables,
            functions: self.functions,
            constants: self.constants,
            type_solver: self.type_solver.fork(),
            captures: Vec::new(),
            current_frame: None,
            inside_call: self.inside_call,
            expected: None,
            returns: None,
            loops: Vec::new(),
            loop_count: self.loop_count,
            trait_impl: None,
        }
    }
}

pub trait FromParsed<T, O = Self> {
    fn from_parsed(value: T, ast: &mut TypedAST<FirstPass>, context: &mut TypedASTContextRef<'_>, span: Span) -> O;
}

pub trait TypeLevelFromParsed<T, O = Self> {
    fn from_parsed(value: T, module: ModuleId, context: &mut TypedASTContextRef<'_>, span: Span) -> O;
}

pub trait Descriptor {
    type Type;
    type IndexResult;
}

pub struct FirstPass;

impl Descriptor for FirstPass {
    type IndexResult = String;
    type Type = TypeInfoRef;
}

pub struct SolvedPass;

impl Descriptor for SolvedPass {
    type IndexResult = FieldRef;
    type Type = TypeRef;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsedItem {
    VTable(TypeRef, ImplRef, Box<[TypeRef]>),
    Adt(AdtRef, Box<[TypeRef]>),
    /// A function with type arguments for its generic parameters.
    Func(FuncRef, Box<[TypeRef]>),
    /// A function of an impl block with its own generics, for values of a
    /// type, with type arguments of the impl followed by the function's.
    Method(TypeRef, ImplRef, VFuncRef, Box<[TypeRef]>),
    /// The impl of a trait for a type that is generic where it's used (a
    /// call of a bound's function); it becomes [`UsedItem::VTable`] once the
    /// type is known.
    BoundImpl(TypeRef, TraitRef),
}

impl UsedItem {
    /// Types the item is instantiated with.
    fn type_args(&self) -> Vec<TypeRef> {
        match self {
            Self::VTable(ty, _, args) | Self::Method(ty, _, _, args) => std::iter::once(*ty).chain(args.iter().copied()).collect(),
            Self::Adt(_, args) | Self::Func(_, args) => args.to_vec(),
            Self::BoundImpl(ty, _) => vec![*ty],
        }
    }

    /// The item with generics replaced by `type_args`, e.g. an item used by a
    /// generic function, for an instance of the function.
    fn substitute(&self, types: &mut TypeStorage, type_args: &[TypeRef]) -> Self {
        let mut apply = |types_of: &[TypeRef]| types_of.iter().map(|&ty| types.apply_type_args(ty, type_args)).collect::<Box<[_]>>();

        match self {
            Self::VTable(ty, vtable, args) => {
                let args = apply(args);

                Self::VTable(types.apply_type_args(*ty, type_args), *vtable, args)
            }
            Self::Adt(adt, args) => Self::Adt(*adt, apply(args)),
            Self::Func(func, args) => Self::Func(*func, apply(args)),
            Self::Method(ty, vtable, func, args) => {
                let args = apply(args);

                Self::Method(types.apply_type_args(*ty, type_args), *vtable, *func, args)
            }
            Self::BoundImpl(ty, trait_ref) => Self::BoundImpl(types.apply_type_args(*ty, type_args), *trait_ref),
        }
    }
}

#[derive_where(Debug, Clone; T, D::Type)]
pub struct Typed<T, D: Descriptor> {
    pub value: T,
    pub span: Span,
    pub ty: D::Type,
    pub expected_ty: D::Type,
}

/// A variable declared in the source, for tools like language servers.
#[derive_where(Debug, Clone; D::Type)]
pub struct Variable<D: Descriptor = SolvedPass> {
    pub name: String,
    /// Span of its name where it's declared.
    pub span: Span,
    pub ty: D::Type,
}

#[derive_where(Debug, Clone; D::Type, D::IndexResult)]
#[derive_where(Default)]
pub struct TypedAST<D: Descriptor = SolvedPass> {
    pub module: ModuleId,
    pub blocks: IndexVec<BlockRef, Typed<Block, D>>,
    pub statements: IndexVec<StmtRef, Stmt>,
    pub exprs: IndexVec<ExprRef, Typed<Expr<D>, D>>,
    pub used_items: Vec<UsedItem>,
    /// Variables declared in the code, in the order of their declarations.
    pub variables: Vec<Variable<D>>,
    /// Uses of variables: spans of uses and of the declarations they refer to.
    pub var_uses: Vec<(Span, Span)>,
    /// Items whose dependencies are being collected by [`TypedAST::use_item`],
    /// to stop on recursive types and mutually recursive functions.
    in_progress: Vec<UsedItem>,
    /// Whether [`TypedAST::use_item`] refused an instance that is too large,
    /// or one too many (see [`mollie_shared::limits::MAX_INSTANCES`]).
    instances_exceeded: bool,
}

impl<D: Descriptor> TypedAST<D>
where
    D::Type: Copy,
{
    pub fn add_block(&mut self, block: Block, ty: D::Type, span: Span) -> BlockRef {
        let result = BlockRef::new(self.blocks.len());

        self.blocks.push(Typed {
            value: block,
            span,
            ty,
            expected_ty: ty,
        });

        result
    }

    pub fn add_stmt(&mut self, stmt: Stmt) -> StmtRef {
        let result = StmtRef::new(self.statements.len());

        self.statements.push(stmt);

        result
    }

    pub fn add_expr(&mut self, expr: Expr<D>, ty: D::Type, span: Span) -> ExprRef {
        let result = ExprRef::new(self.exprs.len());

        self.exprs.push(Typed {
            value: expr,
            span,
            ty,
            expected_ty: ty,
        });

        result
    }

    #[track_caller]
    #[allow(clippy::too_many_arguments, reason = "the parts of the context it reads, borrowed separately")]
    fn use_item(
        &mut self,
        adt_types: &IndexVec<AdtRef, Adt>,
        impls: &ImplRegistry,
        traits: &IndexVec<TraitRef, Trait>,
        vtables: &VTableMap,
        functions: &FunctionMap,
        types: &mut TypeStorage,
        item: UsedItem,
        current_vtable: Option<ImplRef>,
    ) {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || {
            if !self.used_items.contains(&item) && !self.in_progress.contains(&item) {
                // Generic code instantiating itself with growing types would
                // never stop: instances are limited.
                if self.used_items.len() >= limits::MAX_INSTANCES || !item.type_args().iter().all(|&ty| types.size_within(ty, limits::MAX_INSTANCE_TYPE_SIZE)) {
                    self.instances_exceeded = true;

                    return;
                }

                self.in_progress.push(item.clone());

                match &item {
                    UsedItem::VTable(_, vtable, vtable_type_args) => {
                        for func in vtables.get(vtable).into_iter().flat_map(IndexMap::values) {
                            if let FunctionBody::Default { func, type_args } = func {
                                let type_args = type_args.iter().map(|&ty| types.apply_type_args(ty, vtable_type_args)).collect();

                                self.use_item(
                                    adt_types,
                                    impls,
                                    traits,
                                    vtables,
                                    functions,
                                    types,
                                    UsedItem::Func(*func, type_args),
                                    current_vtable,
                                );
                            }

                            if let FunctionBody::Local { ast, .. } = func {
                                for item in &ast.used_items {
                                    if matches!(item, UsedItem::VTable(_, vtable, _) if current_vtable == Some(*vtable)) {
                                        continue;
                                    }

                                    let item = item.substitute(types, vtable_type_args);

                                    self.use_item(adt_types, impls, traits, vtables, functions, types, item, current_vtable);
                                }
                            }
                        }
                    }
                    // Items used by a generic function are used with its type
                    // arguments applied.
                    UsedItem::Func(func, type_args) => {
                        if let Some(FunctionBody::Local { ast, .. }) = functions.get(func) {
                            for item in &ast.used_items {
                                let item = item.substitute(types, type_args);

                                self.use_item(adt_types, impls, traits, vtables, functions, types, item, current_vtable);
                            }
                        }
                    }
                    UsedItem::Method(_, vtable, func, type_args) => {
                        if let Some(FunctionBody::Local { ast, .. }) = vtables.get(vtable).and_then(|functions| functions.get(func)) {
                            for item in &ast.used_items {
                                let item = item.substitute(types, type_args);

                                self.use_item(adt_types, impls, traits, vtables, functions, types, item, current_vtable);
                            }
                        }
                    }
                    UsedItem::BoundImpl(ty, trait_ref) => {
                        if is_concrete(types, *ty)
                            && let Ok(vtable) = impls.trait_impl_lookup(types, *trait_ref, *ty)
                            && let Some(args) = impls.impl_args(types, vtable, *ty)
                            && let Some(args) = args.iter().copied().collect::<Option<Box<[_]>>>()
                        {
                            self.use_item(
                                adt_types,
                                impls,
                                traits,
                                vtables,
                                functions,
                                types,
                                UsedItem::VTable(*ty, vtable, args),
                                current_vtable,
                            );
                        }
                    }
                    UsedItem::Adt(adt, type_args) => {
                        for variant in adt_types[*adt].variants.values() {
                            for field in variant.fields.values() {
                                #[allow(clippy::too_many_arguments)]
                                fn recurse_on_field<D: Descriptor>(
                                    ty: TypeRef,
                                    ast: &mut TypedAST<D>,
                                    adt_types: &IndexVec<AdtRef, Adt>,
                                    impls: &ImplRegistry,
                                    traits: &IndexVec<TraitRef, Trait>,
                                    vtables: &VTableMap,
                                    functions: &FunctionMap,
                                    types: &mut TypeStorage,
                                    current_vtable: Option<ImplRef>,
                                    type_args: &[TypeRef],
                                ) where
                                    D::Type: Copy,
                                {
                                    match &types[ty] {
                                        Type::Adt(adt, field_type_args) => {
                                            let adt = *adt;
                                            let field_type_args = field_type_args.clone();
                                            let type_args: Box<[_]> = field_type_args.into_iter().map(|ty| types.apply_type_args(ty, type_args)).collect();

                                            for &arg in &type_args {
                                                recurse_on_field(
                                                    arg,
                                                    ast,
                                                    adt_types,
                                                    impls,
                                                    traits,
                                                    vtables,
                                                    functions,
                                                    types,
                                                    current_vtable,
                                                    type_args.as_ref(),
                                                );
                                            }

                                            ast.use_item(
                                                adt_types,
                                                impls,
                                                traits,
                                                vtables,
                                                functions,
                                                types,
                                                UsedItem::Adt(adt, type_args),
                                                current_vtable,
                                            );
                                        }
                                        &Type::Array(element, _) => {
                                            recurse_on_field(element, ast, adt_types, impls, traits, vtables, functions, types, current_vtable, type_args);
                                        }
                                        Type::Func(args, returns) => {
                                            let args = args.clone();
                                            let returns = *returns;

                                            for arg in args {
                                                recurse_on_field(arg, ast, adt_types, impls, traits, vtables, functions, types, current_vtable, type_args);
                                            }

                                            recurse_on_field(returns, ast, adt_types, impls, traits, vtables, functions, types, current_vtable, type_args);
                                        }
                                        _ => (),
                                    }
                                }

                                recurse_on_field(field.ty, self, adt_types, impls, traits, vtables, functions, types, current_vtable, type_args);
                            }
                        }
                    }
                }

                self.in_progress.retain(|in_progress| in_progress != &item);
                self.used_items.push(item);
            }
        });
    }
}

/// Whether `ty` has no generic parameters (or errors) left in it.
fn is_concrete(types: &TypeStorage, ty: TypeRef) -> bool {
    match &types[ty] {
        Type::Primitive(_) => true,
        &Type::Array(element, _) => is_concrete(types, element),
        Type::Adt(_, args) | Type::Trait(_, args) => args.iter().all(|&arg| is_concrete(types, arg)),
        Type::Func(args, returns) => args.iter().all(|&arg| is_concrete(types, arg)) && is_concrete(types, *returns),
        Type::Generic(_) | Type::Error => false,
    }
}

/// Whether values of `ty` are value structs whose fields can all be compared
/// with `==` (they're compared field by field).
fn comparable_value(tcx: &mut TyCtxt, ty: TypeRef) -> bool {
    let Type::Adt(adt, type_args) = tcx.types[ty].clone() else {
        return false;
    };

    if !tcx.def_registry.value_types.contains(&adt) || tcx.def_registry.adt_types[adt].kind == AdtKind::Enum {
        return false;
    }

    // Collected, since checking the fields needs `tcx` mutably.
    #[allow(clippy::needless_collect, reason = "the fields borrow `tcx`")]
    let fields = tcx.def_registry.adt_types[adt].variants[AdtVariantRef::ZERO]
        .fields
        .values()
        .map(|field| field.ty)
        .collect::<Vec<_>>();

    fields.into_iter().all(|field| {
        let field = tcx.types.apply_type_args(field, &type_args);

        operator_supports(Operator::Equal, &tcx.types[field]) || comparable_value(tcx, field)
    })
}

/// Checks whether a binary `operator` can be applied to operands of type `ty`
/// (both operands are already required to have the same type).
fn operator_supports(operator: Operator, ty: &Type) -> bool {
    // Compound assignments (`+=`, ...) have the same requirements as their
    // operator.
    let operator = operator.lower().unwrap_or(operator);

    match ty {
        // Already reported elsewhere.
        Type::Error => true,
        _ if matches!(operator, Operator::Assign | Operator::Is) => true,
        Type::Primitive(primitive) => match operator {
            // Strings are concatenated.
            Operator::Add => primitive.is_num() || primitive.is_f32() || *primitive == PrimitiveType::String,
            // Remainders of integers only.
            Operator::Rem => primitive.is_num(),
            Operator::Sub | Operator::Mul | Operator::Div => primitive.is_num() || primitive.is_f32(),
            // Strings are ordered by their bytes.
            Operator::LessThan | Operator::LessThanEqual | Operator::GreaterThan | Operator::GreaterThanEqual => {
                primitive.is_num() || primitive.is_f32() || *primitive == PrimitiveType::String
            }
            Operator::BitAnd | Operator::BitOr => primitive.is_num() || *primitive == PrimitiveType::Bool,
            Operator::And | Operator::Or => *primitive == PrimitiveType::Bool,
            Operator::Equal | Operator::NotEqual => !matches!(primitive, PrimitiveType::Void | PrimitiveType::Any),
            _ => true,
        },
        _ => false,
    }
}

impl TypedASTContextRef<'_> {
    /// Reports type arguments that don't satisfy `bounds` of the generics
    /// they're given for.
    fn check_bounds(&mut self, bounds: &[Bound], type_args: &[TypeRef], span: ModuleSpan) {
        for bound in bounds {
            let Some(&ty) = type_args.get(bound.generic) else {
                continue;
            };

            // `T: Source<U>` with the call's `U`.
            let types = &mut self.type_solver.context.types;
            let trait_args: Box<[_]> = bound.trait_args.iter().map(|&arg| types.apply_type_args(arg, type_args)).collect();
            let satisfied = match types[ty] {
                // A generic parameter of the code around must have the bound
                // itself.
                Type::Generic(generic) => self.type_solver.bounds.iter().any(|available| {
                    available.generic == generic
                        && available.trait_ref == bound.trait_ref
                        && available.trait_args.len() == trait_args.len()
                        && available
                            .trait_args
                            .iter()
                            .zip(&trait_args)
                            .all(|(&found, &expected)| types.is_same(found, expected))
                }),
                _ => self.type_solver.context.impl_registry.satisfies(types, ty, bound.trait_ref, Some(&trait_args)),
            };

            if !satisfied {
                let bound = self.type_solver.context.types.get_or_add(Type::Trait(bound.trait_ref, trait_args));

                self.type_solver.error(TypeError::UnsatisfiedBound { ty, bound }, span);
            }
        }
    }

    fn use_vtable_impl(&mut self, target: TypeRef, vtable: ImplRef, span: ModuleSpan) -> Box<[TypeInfoRef]> {
        // Instantiate the impl's generics with fresh type variables.
        let type_args: Box<[_]> = (0..self.type_solver.context.impl_registry.impls[vtable].generics.len())
            .map(|_| self.type_solver.add_unknown(None, None))
            .collect();

        let solved_origin_ty = TypeSolver::type_to_info(&mut self.type_solver.type_infos, self.type_solver.context, target, &[]);
        let origin_ty = TypeSolver::type_to_info(
            &mut self.type_solver.type_infos,
            self.type_solver.context,
            self.type_solver.context.impl_registry.impls[vtable].ty,
            &type_args,
        );

        if self.type_solver.context.impl_registry.impls[vtable].origin_trait.is_some()
            && let Err(err) = self.type_solver.unify(UnifyArgs {
                expected: origin_ty,
                found: type_args[0],
            })
        {
            let err = err.into_type_error(&mut self.type_solver);

            self.type_solver.error(err, span);
        }

        if let Err(err) = self.type_solver.unify(UnifyArgs {
            expected: solved_origin_ty,
            found: origin_ty,
        }) {
            let err = err.into_type_error(&mut self.type_solver);

            self.type_solver.error(err, span);
        }

        type_args
    }
}

impl TypedAST<FirstPass> {
    fn solve_expr(&self, ast: &mut TypedAST, expr: ExprRef, context: &mut TypedASTContextRef<'_>) -> ExprRef {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || {
            /// Registers the vtables needed to use `expr` where `expected` is
            /// required, looking through the shape of the expected type:
            ///
            /// - a trait object: the vtable of the value's type;
            /// - an array: every element of an array literal, against the
            ///   element type;
            /// - an ADT: every given field of a construction of it, against the
            ///   field's type with the ADT's type arguments applied;
            /// - blocks and `if`s: their resulting expressions.
            ///
            /// Values that aren't literals have nothing to look into, and types
            /// without an impl are skipped (reported by the type check).
            fn register_coercion(ast: &mut TypedAST, expected: TypeRef, expr: ExprRef, context: &mut TypedASTContextRef<'_>) {
                if expr == ExprRef::INVALID {
                    return;
                }

                // The value of these is the value of their tail expressions.
                match ast[expr].value {
                    Expr::Block(block) => {
                        if let Some(tail) = ast[block].value.expr {
                            register_coercion(ast, expected, tail, context);
                        }

                        return;
                    }
                    Expr::IfElse { block, otherwise, .. } => {
                        let tail = ast[block].value.expr;

                        for branch in tail.into_iter().chain(otherwise) {
                            register_coercion(ast, expected, branch, context);
                        }

                        return;
                    }
                    _ => (),
                }

                match context.type_solver.context.types[expected].clone() {
                    Type::Trait(trait_ref, trait_args) => {
                        let (ty, span) = (ast[expr].ty, ast[expr].span);

                        let Some(vtable) = context.type_solver.context.find_trait_impl(ty, trait_ref, &trait_args) else {
                            return;
                        };

                        let type_args = context.use_vtable_impl(ty, vtable, ModuleSpan(ast.module, span));
                        let item = UsedItem::VTable(ty, vtable, type_args.into_iter().map(|ty| context.type_solver.solve(ty)).collect());

                        ast.use_item(
                            &context.type_solver.context.def_registry.adt_types,
                            &context.type_solver.context.impl_registry,
                            &context.type_solver.context.def_registry.traits,
                            context.vtables,
                            context.functions,
                            &mut context.type_solver.context.types,
                            item,
                            Some(vtable),
                        );
                    }
                    Type::Array(element, _) => {
                        if let Expr::Array { elements, .. } = &ast[expr].value {
                            let elements = elements.clone();

                            for value in elements {
                                register_coercion(ast, element, value, context);
                            }
                        }
                    }
                    Type::Adt(adt, type_args) => {
                        if let Expr::Construct { adt: found, variant, fields } = &ast[expr].value
                            && *found == adt
                        {
                            let variant = *variant;
                            let fields = fields.iter().map(|&(field, _, value)| (field, value)).collect::<Vec<_>>();

                            for (field, value) in fields {
                                let field_ty = context.type_solver.context.def_registry.adt_types[adt].variants[variant].fields[field].ty;
                                let field_ty = context.type_solver.context.types.apply_type_args(field_ty, &type_args);

                                register_coercion(ast, field_ty, value, context);
                            }
                        }
                    }
                    _ => (),
                }
            }

            let inside_call = mem::take(&mut context.inside_call);
            let value = match self[expr].value.clone() {
                Expr::Lit(lit_expr) => Expr::Lit(lit_expr),
                Expr::Var(var) => Expr::Var(var),
                Expr::Unary { operator, expr } => {
                    let expr = self.solve_expr(ast, expr, context);
                    let expr_ty = ast[expr].ty;

                    match operator.value {
                        UnaryOperator::Neg => {
                            if let Type::Primitive(primitive) = context.type_solver.context.types[expr_ty] {
                                if !primitive.is_int() && !primitive.is_f32() {
                                    context.type_solver.error(
                                        TypeError::InvalidUnaryOperator {
                                            operator: operator.value,
                                            ty: expr_ty,
                                        },
                                        ModuleSpan(ast.module, operator.span),
                                    );
                                }
                            } else {
                                context.type_solver.error(
                                    TypeError::InvalidUnaryOperator {
                                        operator: operator.value,
                                        ty: expr_ty,
                                    },
                                    ModuleSpan(ast.module, operator.span),
                                );
                            }
                        }
                        UnaryOperator::Not => {
                            if let Type::Primitive(primitive) = context.type_solver.context.types[expr_ty] {
                                if !matches!(primitive, PrimitiveType::Bool) {
                                    context.type_solver.error(
                                        TypeError::InvalidUnaryOperator {
                                            operator: operator.value,
                                            ty: expr_ty,
                                        },
                                        ModuleSpan(ast.module, operator.span),
                                    );
                                }
                            } else {
                                context.type_solver.error(
                                    TypeError::InvalidUnaryOperator {
                                        operator: operator.value,
                                        ty: expr_ty,
                                    },
                                    ModuleSpan(ast.module, operator.span),
                                );
                            }
                        }
                    }

                    Expr::Unary { operator, expr }
                }
                Expr::Binary { operator, lhs, rhs } => {
                    let lhs = self.solve_expr(ast, lhs, context);
                    let rhs = self.solve_expr(ast, rhs, context);

                    if !context.type_solver.context.is_same(ast[lhs].ty, ast[rhs].ty) {
                        context.type_solver.error(
                            TypeError::Unexpected {
                                expected: TypeErrorValue::ExplicitType(ast[lhs].ty),
                                found: TypeErrorValue::ExplicitType(ast[rhs].ty),
                            },
                            ModuleSpan(ast.module, ast[rhs].span),
                        );
                    }

                    // Value types are compared field by field.
                    let supported = operator_supports(operator.value, &context.type_solver.context.types[ast[lhs].ty])
                        || matches!(operator.value, Operator::Equal | Operator::NotEqual) && comparable_value(context.type_solver.context, ast[lhs].ty);

                    if !supported {
                        context.type_solver.error(
                            TypeError::InvalidOperator {
                                operator: operator.value,
                                ty: ast[lhs].ty,
                            },
                            ModuleSpan(ast.module, operator.span),
                        );
                    }

                    Expr::Binary { operator, lhs, rhs }
                }
                Expr::Construct { adt, variant, fields } => {
                    let fields = fields
                        .into_iter()
                        .map(|(field_ref, field_type, field_value)| {
                            let expr = if field_value == ExprRef::INVALID {
                                field_value
                            } else {
                                self.solve_expr(ast, field_value, context)
                            };

                            let field_type = context.type_solver.get_info(field_type);

                            if expr != ExprRef::INVALID {
                                let other = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ast[expr].ty, &[]);

                                if let Err(err) = context.type_solver.unify(UnifyArgs {
                                    expected: field_type,
                                    found: other,
                                }) {
                                    let err = err.into_type_error(&mut context.type_solver);

                                    context.type_solver.error(err, ModuleSpan(ast.module, ast[expr].span));
                                }
                            }

                            let field_type = context.type_solver.solve(field_type);

                            if expr != ExprRef::INVALID && !context.type_solver.context.is_same(ast[expr].ty, field_type) {
                                context.type_solver.error(
                                    TypeError::Unexpected {
                                        expected: TypeErrorValue::ExplicitType(field_type),
                                        found: TypeErrorValue::ExplicitType(ast[expr].ty),
                                    },
                                    ModuleSpan(ast.module, ast[expr].span),
                                );
                            }

                            if expr != ExprRef::INVALID {
                                ast.exprs[expr].expected_ty = field_type;
                            }

                            // Omitted fields take their default value, which
                            // needs no vtables.
                            register_coercion(ast, field_type, expr, context);

                            // match *field_ty {
                            //     Type::Trait(trait_ref, _) => {
                            //         if let Some(vtable) =
                            // context.type_solver.context.
                            // find_vtable(ast[expr].ty,
                            // Some(trait_ref)) {
                            //             let type_args =
                            // context.use_vtable_impl(ast[expr].ty, vtable,
                            // ast[expr].span);
                            // let item = UsedItem::VTable(ast[expr].ty, vtable,
                            // type_args.into_iter().map(|ty|
                            // context.type_solver.solve(ty)).collect());

                            //             ast.use_item(
                            //
                            // &context.type_solver.context.def_registry.
                            // adt_types,
                            //
                            // &context.type_solver.context.def_registry.traits,
                            //                 context.vtables,
                            //                 context.functions,
                            //                 &mut
                            // context.type_solver.context.types,
                            //                 item,
                            //                 Some(vtable),
                            //             );
                            //         }
                            //     }
                            //     Type::Array(element, _) => {
                            //         if let Type::Trait(trait_ref, _) =
                            // context.type_solver.context.types[element]
                            //             && let Expr::Array { elements, .. } =
                            // &ast[expr].value         {
                            //             let elements = elements.clone();

                            //             for element in elements {
                            //                 if let Some(vtable) =
                            // context.type_solver.context.
                            // find_vtable(ast[element].
                            // ty, Some(trait_ref)) {
                            //                     let type_args =
                            // context.use_vtable_impl(ast[element].ty, vtable,
                            // ast[element].span);
                            //                     let item = UsedItem::VTable(
                            //                         ast[element].ty,
                            //                         vtable,
                            //
                            // type_args.into_iter().map(|ty|
                            // context.type_solver.solve(ty)).collect(),
                            //                     );

                            //                     ast.use_item(
                            //
                            // &context.type_solver.context.def_registry.
                            // adt_types,
                            //
                            // &context.type_solver.context.def_registry.traits,
                            //                         context.vtables,
                            //                         context.functions,
                            //                         &mut
                            // context.type_solver.context.types,
                            //                         item,
                            //                         Some(vtable),
                            //                     );
                            //                 }
                            //             }
                            //         }
                            //     }
                            //     _ => (),
                            // }

                            (field_ref, field_type, expr)
                        })
                        .collect();

                    Expr::Construct { adt, variant, fields }
                }
                Expr::AdtIndex { target, field } => {
                    let target = self.solve_expr(ast, target, context);

                    // A function of a bound of a generic parameter.
                    if let Type::Generic(generic) = context.type_solver.context.types[ast[target].ty]
                        && inside_call
                        && let Some((bound, func)) = context.type_solver.bounds.iter().find_map(|bound| {
                            (bound.generic == generic)
                                .then(|| {
                                    context.type_solver.context.def_registry.traits[bound.trait_ref]
                                        .functions
                                        .iter()
                                        .find(|(_, func)| func.name == field)
                                        .map(|(func, _)| (bound.clone(), func))
                                })
                                .flatten()
                        })
                    {
                        let target_ty = ast[target].ty;
                        let func_info = &context.type_solver.context.def_registry.traits[bound.trait_ref].functions[func];
                        // Generic 0 of a trait is `Self`, then come the trait's
                        // own type arguments.
                        let substitution: Box<[_]> = once(target_ty).chain(bound.trait_args.iter().copied()).collect();
                        #[allow(clippy::needless_collect, reason = "the arguments borrow `context`")]
                        let arg_types: Vec<_> = func_info.args.iter().map(|arg| arg.ty).collect();
                        let returns = func_info.returns;
                        let args = arg_types
                            .into_iter()
                            .map(|ty| context.type_solver.context.types.apply_type_args(ty, &substitution))
                            .collect();
                        let returns = context.type_solver.context.types.apply_type_args(returns, &substitution);
                        let ty = context.type_solver.context.types.get_or_add(Type::Func(args, returns));
                        let info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[]);

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: info,
                            found: self[expr].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, self[expr].span));
                        }

                        ast.use_item(
                            &context.type_solver.context.def_registry.adt_types,
                            &context.type_solver.context.impl_registry,
                            &context.type_solver.context.def_registry.traits,
                            context.vtables,
                            context.functions,
                            &mut context.type_solver.context.types,
                            UsedItem::BoundImpl(target_ty, bound.trait_ref),
                            None,
                        );

                        return ast.add_expr(
                            Expr::BoundFunc {
                                target,
                                trait_ref: bound.trait_ref,
                                func,
                            },
                            ty,
                            self[expr].span,
                        );
                    }

                    if let Some((vtable, func)) = context.type_solver.context.find_vtable_by_func(ast[target].ty, &field)
                        && inside_call
                    {
                        let ty = context.type_solver.context.impl_registry.impls[vtable].functions[func].ty;
                        // Instantiate generics of the impl, then the function's
                        // own generics, with fresh type
                        // variables.
                        let impl_generics = context.type_solver.context.impl_registry.impls[vtable].generics.len();
                        let method_generics = context.type_solver.context.impl_registry.impls[vtable].functions[func].generics;
                        let type_args: Box<[_]> = (0..impl_generics + method_generics)
                            .map(|_| context.type_solver.add_unknown(None, None))
                            .collect();

                        let solved_origin_ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ast[target].ty, &[]);
                        let origin_ty = TypeSolver::type_to_info(
                            &mut context.type_solver.type_infos,
                            context.type_solver.context,
                            context.type_solver.context.impl_registry.impls[vtable].ty,
                            &type_args,
                        );

                        if context.type_solver.context.impl_registry.impls[vtable].origin_trait.is_some()
                            && let Err(err) = context.type_solver.unify(UnifyArgs {
                                expected: origin_ty,
                                found: type_args[0],
                            })
                        {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, ast[target].span));
                        }

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: solved_origin_ty,
                            found: origin_ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, ast[target].span));
                        }

                        let info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &type_args);

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: info,
                            found: self[expr].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, self[expr].span));
                        }

                        let type_args: Box<_> = type_args.into_iter().map(|ty| context.type_solver.solve(ty)).collect();

                        if let Some(bounds) = context.type_solver.context.impl_registry.method_bounds.get(&(vtable, func)).cloned() {
                            context.check_bounds(&bounds, &type_args, ModuleSpan(ast.module, self[expr].span));
                        }

                        let ty = context.type_solver.context.types.apply_type_args(ty, &type_args);

                        ast.use_item(
                            &context.type_solver.context.def_registry.adt_types,
                            &context.type_solver.context.impl_registry,
                            &context.type_solver.context.def_registry.traits,
                            context.vtables,
                            context.functions,
                            &mut context.type_solver.context.types,
                            // A generic function is compiled for each instantiation,
                            // apart from the impl's other functions.
                            if method_generics == 0 {
                                UsedItem::VTable(ast[target].ty, vtable, type_args.clone())
                            } else {
                                UsedItem::Method(ast[target].ty, vtable, func, type_args.clone())
                            },
                            Some(vtable),
                        );

                        return ast.add_expr(
                            Expr::VTableIndex {
                                target: Some(target),
                                target_ty: ast[target].ty,
                                vtable,
                                func,
                                type_args: type_args.get(impl_generics..).unwrap_or_default().into(),
                            },
                            ty,
                            self[expr].span,
                        );
                    }

                    if let Type::Trait(trait_ref, _) = context.type_solver.context.types[ast[target].ty]
                        && inside_call
                        && let Some((func, func_info)) = context.type_solver.context.def_registry.traits[trait_ref]
                            .functions
                            .iter()
                            .find(|(_, func)| func.name == field)
                    {
                        // Generic 0 of a trait is `Self`, then come the trait's
                        // own type arguments.
                        let substitution: Box<[_]> = match &context.type_solver.context.types[ast[target].ty] {
                            Type::Trait(_, trait_args) => once(ast[target].ty).chain(trait_args.iter().copied()).collect(),
                            _ => unreachable!(),
                        };

                        let args = func_info
                            .args
                            .iter()
                            .map(|arg| context.type_solver.context.types.apply_type_args(arg.ty, &substitution))
                            .collect();

                        let returns = context.type_solver.context.types.apply_type_args(func_info.returns, &substitution);
                        let ty = context.type_solver.context.types.get_or_add(Type::Func(args, returns));

                        return ast.add_expr(Expr::TraitFunc { target, trait_ref, func }, ty, self[expr].span);
                    }

                    if let Type::Adt(adt_ref, args) = &context.type_solver.context.types[ast[target].ty] {
                        let adt = &context.type_solver.context.def_registry.adt_types[*adt_ref];

                        if matches!(adt.kind, AdtKind::Enum) {
                            let ty = context.type_solver.context.types.get_or_add(Type::Error);

                            return ast.add_expr(
                                Expr::Error(context.type_solver.error(
                                    TypeError::NonIndexable {
                                        ty: ast[target].ty,
                                        name: field,
                                    },
                                    ModuleSpan(ast.module, self[expr].span),
                                )),
                                ty,
                                self[expr].span,
                            );
                        }

                        let Some(field) = adt.variants[AdtVariantRef::ZERO]
                            .fields
                            .iter()
                            .find_map(|(field_ref, variant_field)| if variant_field.name == field { Some(field_ref) } else { None })
                        else {
                            let adt = *adt_ref;
                            let ty = context.type_solver.context.types.get_or_add(Type::Error);

                            return ast.add_expr(
                                Expr::Error(context.type_solver.error(
                                    TypeError::NoField {
                                        adt,
                                        variant: AdtVariantRef::ZERO,
                                        name: field,
                                    },
                                    ModuleSpan(ast.module, self[expr].span),
                                )),
                                ty,
                                self[expr].span,
                            );
                        };

                        let ty = adt[field].ty;
                        let args = args.clone();
                        let ty = context.type_solver.context.types.apply_type_args(ty, &args);

                        let info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, ty, &[]);

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: info,
                            found: self[expr].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, self[expr].span));
                        }

                        return ast.add_expr(Expr::AdtIndex { target, field }, ty, self[expr].span);
                    }

                    let ty = context.type_solver.context.types.get_or_add(Type::Error);

                    return ast.add_expr(
                        Expr::Error(context.type_solver.error(
                            TypeError::Unexpected {
                                expected: TypeErrorValue::Adt(SpecialAdtKind::WithExpectation(AdtKind::Enum)),
                                found: TypeErrorValue::ExplicitType(ast[target].ty),
                            },
                            ModuleSpan(ast.module, ast[target].span),
                        )),
                        ty,
                        self[expr].span,
                    );
                }
                Expr::TraitFunc { .. } | Expr::BoundFunc { .. } => unreachable!(),
                Expr::ArrayIndex { target, element } => {
                    let target = self.solve_expr(ast, target, context);
                    let Type::Array(element_ty, _) = context.type_solver.context.types[ast[target].ty] else {
                        let ty = context.type_solver.context.types.get_or_add(Type::Error);

                        return ast.add_expr(
                            Expr::Error(context.type_solver.error(
                                TypeError::Unexpected {
                                    expected: TypeErrorValue::Array(None),
                                    found: TypeErrorValue::ExplicitType(ast[target].ty),
                                },
                                ModuleSpan(ast.module, ast[target].span),
                            )),
                            ty,
                            self[expr].span,
                        );
                    };

                    let element = self.solve_expr(ast, element, context);
                    let info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, element_ty, &[]);

                    if let Err(err) = context.type_solver.unify(UnifyArgs {
                        expected: info,
                        found: self[expr].ty,
                    }) {
                        let err = err.into_type_error(&mut context.type_solver);

                        context.type_solver.error(err, ModuleSpan(ast.module, self[expr].span));
                    }

                    return ast.add_expr(Expr::ArrayIndex { target, element }, element_ty, self[expr].span);
                }
                Expr::Closure { args, captures, body } => {
                    let captures = captures.into_iter().map(|(name, ty)| (name, context.type_solver.solve(ty))).collect();
                    let args = args
                        .into_iter()
                        .map(|arg| Arg {
                            name: arg.name,
                            kind: arg.kind,
                            ty: context.type_solver.solve(arg.ty),
                        })
                        .collect();

                    let body = self.solve_block(ast, body, context);

                    Expr::Closure { args, captures, body }
                }
                Expr::Call { func, args } => {
                    let func = self.solve_expr(ast, func, context.inside_call());
                    let args: Box<[_]> = args.into_iter().map(|arg| self.solve_expr(ast, arg, context)).collect();
                    let func_ty = ast[func].ty;

                    if let Type::Func(arg_types, returns) = &context.type_solver.context.types[func_ty] {
                        let arg_types = arg_types.clone();
                        let returns = *returns;
                        let skip = usize::from(matches!(
                            ast[func].value,
                            Expr::VTableIndex { target: Some(_), .. } | Expr::TraitFunc { .. } | Expr::BoundFunc { .. }
                        ));
                        // A function called on a value must take it as `self`.
                        let registry = &context.type_solver.context;
                        let without_self = match ast[func].value {
                            Expr::VTableIndex {
                                target: Some(_), vtable, func, ..
                            } => {
                                let function = &registry.impl_registry.impls[vtable].functions[func];
                                // Functions of the host have no names of
                                // parameters, their receiver is their first
                                // one.
                                let takes_self = function.arg_names.first().map_or(!arg_types.is_empty(), |name| name == "self");

                                (!takes_self).then(|| function.name.clone())
                            }
                            Expr::TraitFunc { trait_ref, func, .. } | Expr::BoundFunc { trait_ref, func, .. } => {
                                let function = &registry.def_registry.traits[trait_ref].functions[func];

                                (function.args.first().is_none_or(|arg| arg.name != "self")).then(|| function.name.clone())
                            }
                            _ => None,
                        };

                        if let Some(name) = without_self {
                            let ty = context.type_solver.context.types.get_or_add(Type::Error);

                            return ast.add_expr(
                                Expr::Error(
                                    context
                                        .type_solver
                                        .error(TypeError::NotAMethod { name }, ModuleSpan(ast.module, ast[func].span)),
                                ),
                                ty,
                                self[expr].span,
                            );
                        }

                        let expected = arg_types.len() - skip;
                        let found = args.len();

                        if found != expected {
                            context.type_solver.error(
                                TypeError::ArgumentCountMismatch {
                                    expected,
                                    found,
                                    func: Some(func_ty),
                                },
                                ModuleSpan(ast.module, self[expr].span),
                            );
                        }

                        for (arg, arg_type) in args.iter().copied().zip(arg_types.into_iter().skip(skip)) {
                            if context.type_solver.context.is_same(ast[arg].ty, arg_type) {
                                register_coercion(ast, arg_type, arg, context);
                            } else {
                                context.type_solver.error(
                                    TypeError::Unexpected {
                                        expected: TypeErrorValue::ExplicitType(arg_type),
                                        found: TypeErrorValue::ExplicitType(ast[arg].ty),
                                    },
                                    ModuleSpan(ast.module, ast[arg].span),
                                );
                            }
                        }

                        let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, returns, &[]);

                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: ty,
                            found: self[expr].ty,
                        }) {
                            let err = err.into_type_error(&mut context.type_solver);

                            context.type_solver.error(err, ModuleSpan(ast.module, self[expr].span));
                        }

                        return ast.add_expr(Expr::Call { func, args }, returns, self[expr].span);
                    }

                    let ty = context.type_solver.context.types.get_or_add(Type::Error);

                    return ast.add_expr(
                        Expr::Error(context.type_solver.error(
                            TypeError::Unexpected {
                                expected: TypeErrorValue::Function,
                                found: TypeErrorValue::ExplicitType(ast[func].ty),
                            },
                            ModuleSpan(ast.module, ast[func].span),
                        )),
                        ty,
                        self[expr].span,
                    );
                }
                Expr::Array { element, elements } => {
                    let element = context.type_solver.solve(element);
                    let elements = elements.into_iter().map(|element| self.solve_expr(ast, element, context)).collect();

                    Expr::Array { element, elements }
                }
                Expr::IfElse { condition, block, otherwise } => {
                    let condition = self.solve_expr(ast, condition, context);

                    if context.type_solver.context.types[ast[condition].ty] != Type::Primitive(PrimitiveType::Bool) {
                        let ty = context.type_solver.context.types.get_or_add(Type::Error);

                        return ast.add_expr(
                            Expr::Error(context.type_solver.error(
                                TypeError::Unexpected {
                                    expected: TypeErrorValue::PrimitiveType(PrimitiveType::Bool),
                                    found: TypeErrorValue::ExplicitType(ast[condition].ty),
                                },
                                ModuleSpan(ast.module, ast[condition].span),
                            )),
                            ty,
                            self[expr].span,
                        );
                    }

                    let block = self.solve_block(ast, block, context);
                    let otherwise = otherwise.map(|otherwise| self.solve_expr(ast, otherwise, context));

                    Expr::IfElse { condition, block, otherwise }
                }
                Expr::While { condition, block, id } => {
                    let condition = self.solve_expr(ast, condition, context);

                    if context.type_solver.context.types[ast[condition].ty] != Type::Primitive(PrimitiveType::Bool) {
                        let ty = context.type_solver.context.types.get_or_add(Type::Error);

                        return ast.add_expr(
                            Expr::Error(context.type_solver.error(
                                TypeError::Unexpected {
                                    expected: TypeErrorValue::PrimitiveType(PrimitiveType::Bool),
                                    found: TypeErrorValue::ExplicitType(ast[condition].ty),
                                },
                                ModuleSpan(ast.module, ast[condition].span),
                            )),
                            ty,
                            self[expr].span,
                        );
                    }

                    let block = self.solve_block(ast, block, context);

                    Expr::While { condition, block, id }
                }
                Expr::Loop { block, id } => Expr::Loop {
                    block: self.solve_block(ast, block, context),
                    id,
                },
                Expr::Break { id, value } => Expr::Break {
                    id,
                    value: value.map(|value| self.solve_expr(ast, value, context)),
                },
                Expr::Continue { id } => Expr::Continue { id },
                Expr::Block(block) => {
                    let block = self.solve_block(ast, block, context);
                    let block_ty = context.type_solver.solve(self[expr].ty);

                    // A block typed differently than its value (the value of a
                    // `let` with a type, like a trait object) converts it.
                    if let Some(tail) = ast[block].value.expr
                        && ast[tail].ty != block_ty
                    {
                        register_coercion(ast, block_ty, tail, context);
                    }

                    Expr::Block(block)
                }
                Expr::VTableIndex {
                    target,
                    target_ty,
                    vtable,
                    func,
                    type_args: method_args,
                } => {
                    let target = target.map(|target| self.solve_expr(ast, target, context));
                    let target_ty = context.type_solver.solve(target_ty);
                    let method_args = method_args.iter().map(|&ty| context.type_solver.solve(ty)).collect();

                    let type_args = context.use_vtable_impl(target_ty, vtable, ModuleSpan(ast.module, self[expr].span));
                    let item = UsedItem::VTable(target_ty, vtable, type_args.into_iter().map(|ty| context.type_solver.solve(ty)).collect());

                    ast.use_item(
                        &context.type_solver.context.def_registry.adt_types,
                        &context.type_solver.context.impl_registry,
                        &context.type_solver.context.def_registry.traits,
                        context.vtables,
                        context.functions,
                        &mut context.type_solver.context.types,
                        item,
                        Some(vtable),
                    );

                    Expr::VTableIndex {
                        target,
                        target_ty,
                        vtable,
                        func,
                        type_args: method_args,
                    }
                }
                Expr::TypeCast(expr, ty) => {
                    let expr = self.solve_expr(ast, expr, context);

                    Expr::TypeCast(expr, ty)
                }
                Expr::Format { value, spec } => {
                    let value = self.solve_expr(ast, value, context);

                    Expr::Format { value, spec }
                }
                Expr::IsPattern { target, pattern } => {
                    fn solve_pattern(
                        first_pass: &TypedAST<FirstPass>,
                        ast: &mut TypedAST,
                        pattern: IsPattern<FirstPass>,
                        context: &mut TypedASTContextRef<'_>,
                    ) -> IsPattern<SolvedPass> {
                        match pattern {
                            IsPattern::Literal(expr) => {
                                let expr = first_pass.solve_expr(ast, expr, context);

                                IsPattern::Literal(expr)
                            }
                            IsPattern::Wildcard => IsPattern::Wildcard,
                            IsPattern::Binding { name, ty } => IsPattern::Binding {
                                name,
                                ty: context.type_solver.solve(ty),
                            },
                            IsPattern::EnumVariant {
                                adt,
                                adt_variant,
                                adt_type_args,
                                values,
                            } => {
                                let adt_type_args = adt_type_args.into_iter().map(|type_arg| context.type_solver.solve(type_arg)).collect();
                                let values = values
                                    .into_iter()
                                    .map(|(field, name, pattern)| (field, name, pattern.map(|pattern| solve_pattern(first_pass, ast, pattern, context))))
                                    .collect();

                                IsPattern::EnumVariant {
                                    adt,
                                    adt_variant,
                                    adt_type_args,
                                    values,
                                }
                            }
                            IsPattern::TypeName { ty, name } => {
                                let ty = context.type_solver.solve(ty);

                                IsPattern::TypeName { ty, name }
                            }
                        }
                    }

                    let target = self.solve_expr(ast, target, context);
                    let pattern = solve_pattern(self, ast, pattern, context);

                    Expr::IsPattern { target, pattern }
                }
                Expr::Func { func, type_args } => {
                    let type_args: Box<[TypeRef]> = type_args.iter().map(|&ty| context.type_solver.solve(ty)).collect();

                    if let Some(bounds) = context.type_solver.context.def_registry.func_bounds.get(&func).cloned() {
                        context.check_bounds(&bounds, &type_args, ModuleSpan(ast.module, self[expr].span));
                    }

                    ast.use_item(
                        &context.type_solver.context.def_registry.adt_types,
                        &context.type_solver.context.impl_registry,
                        &context.type_solver.context.def_registry.traits,
                        context.vtables,
                        context.functions,
                        &mut context.type_solver.context.types,
                        UsedItem::Func(func, type_args.clone()),
                        None,
                    );

                    let ty = context.type_solver.solve(self[expr].ty);

                    return ast.add_expr(Expr::Func { func, type_args }, ty, self[expr].span);
                }
                Expr::Error(error) => Expr::Error(error),
                Expr::Unreachable => Expr::Unreachable,
                Expr::Return(value) => Expr::Return(value.map(|value| self.solve_expr(ast, value, context))),
                Expr::Const(constant) => Expr::Const(constant),
                Expr::Panic(message) => Expr::Panic(self.solve_expr(ast, message, context)),
            };

            let ty = context.type_solver.solve(self[expr].ty);

            // Values of ADTs need their layouts wherever they appear: fields
            // are read and patterns matched on objects the program may not
            // construct itself (arguments of programs, results of the host).
            if let Type::Adt(adt, type_args) = &context.type_solver.context.types[ty] {
                let item = UsedItem::Adt(*adt, type_args.clone());

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
            }

            ast.add_expr(value, ty, self[expr].span)
        })
    }

    fn solve_block(&self, ast: &mut TypedAST, block: BlockRef, context: &mut TypedASTContextRef<'_>) -> BlockRef {
        let input_block = &self.blocks[block];
        let output_block = Block {
            stmts: input_block
                .value
                .stmts
                .iter()
                .map(|&stmt| {
                    let stmt = match &self[stmt] {
                        &Stmt::Expr(expr) => Stmt::Expr(self.solve_expr(ast, expr, context)),
                        Stmt::NewVar { mutable, name, value } => Stmt::NewVar {
                            mutable: *mutable,
                            name: name.clone(),
                            value: self.solve_expr(ast, *value, context),
                        },
                    };

                    ast.add_stmt(stmt)
                })
                .collect(),
            expr: input_block.value.expr.map(|expr| self.solve_expr(ast, expr, context)),
        };

        ast.add_block(output_block, context.type_solver.solve(input_block.ty), input_block.span)
    }

    /// Variables declared in the code, with their types solved.
    fn solve_variables(&self, context: &mut TypedASTContextRef<'_>) -> Vec<Variable> {
        self.variables
            .iter()
            .map(|variable| Variable {
                name: variable.name.clone(),
                span: variable.span,
                ty: context.type_solver.solve(variable.ty),
            })
            .collect()
    }

    /// Declares the variable `name`, whose name is at `span`.
    pub(crate) fn declare_var(&mut self, context: &mut TypedASTContextRef<'_>, name: impl Into<String>, ty: TypeInfoRef, mutable: bool, span: Span) {
        let name = name.into();

        context.type_solver.declare_var(name.clone(), ty, mutable, span);
        self.variables.push(Variable { name, span, ty });
    }

    /// Records a use of the variable `name` at `span`, if it's declared in the
    /// source.
    pub(crate) fn use_var(&mut self, context: &TypedASTContextRef<'_>, name: &str, span: Span) {
        if let Some(declaration) = context.type_solver.var_span(name) {
            self.var_uses.push((span, declaration));
        }
    }

    fn solve_expr_final(self, root: ExprRef, context: &mut TypedASTContextRef<'_>) -> (TypedAST, ExprRef) {
        // context.solver.finalize();

        let mut ast = TypedAST {
            module: self.module,
            blocks: IndexVec::new(),
            statements: IndexVec::new(),
            exprs: IndexVec::new(),
            used_items: Vec::new(),
            variables: self.solve_variables(context),
            var_uses: self.var_uses.clone(),
            in_progress: Vec::new(),
            instances_exceeded: false,
        };

        let expr = self.solve_expr(&mut ast, root, context);

        (ast, expr)
    }

    pub fn solve(mut self, root: BlockRef, context: &mut TypedASTContextRef<'_>, returns: TypeRef) -> (TypedAST, BlockRef) {
        // context.solver.finalize();

        let mut ast = TypedAST {
            module: self.module,
            blocks: IndexVec::new(),
            statements: IndexVec::new(),
            exprs: IndexVec::new(),
            used_items: mem::take(&mut self.used_items),
            variables: self.solve_variables(context),
            var_uses: mem::take(&mut self.var_uses),
            in_progress: Vec::new(),
            instances_exceeded: self.instances_exceeded,
        };

        let returns_info = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, returns, &[]);

        if let Err(err) = context.type_solver.unify(UnifyArgs {
            expected: returns_info,
            found: self[root].ty,
        }) {
            let err = err.into_type_error(&mut context.type_solver);

            context.type_solver.error(err, ModuleSpan(ast.module, self[root].span));
        }

        let output_block = self.solve_block(&mut ast, root, context);

        if let Some(expr) = ast[output_block].value.expr
            && !context.type_solver.context.is_same(returns, ast[expr].ty)
        {
            context.type_solver.error(
                TypeError::Unexpected {
                    expected: TypeErrorValue::ExplicitType(returns),
                    found: TypeErrorValue::ExplicitType(ast[expr].ty),
                },
                ModuleSpan(ast.module, ast[expr].span),
            );
        }

        match context.type_solver.context.types[returns].clone() {
            Type::Trait(trait_ref, trait_args) => {
                if let Some(vtable) = context.type_solver.context.find_trait_impl(ast[output_block].ty, trait_ref, &trait_args) {
                    let type_args = context.use_vtable_impl(ast[output_block].ty, vtable, ModuleSpan(ast.module, ast[output_block].span));
                    let item = UsedItem::VTable(
                        ast[output_block].ty,
                        vtable,
                        type_args.into_iter().map(|ty| context.type_solver.solve(ty)).collect(),
                    );

                    ast.use_item(
                        &context.type_solver.context.def_registry.adt_types,
                        &context.type_solver.context.impl_registry,
                        &context.type_solver.context.def_registry.traits,
                        context.vtables,
                        context.functions,
                        &mut context.type_solver.context.types,
                        item,
                        Some(vtable),
                    );
                }
            }
            Type::Array(element, _) => {
                if let Type::Trait(trait_ref, trait_args) = context.type_solver.context.types[element].clone()
                    && let Some(Expr::Array { elements, .. }) = ast[output_block].value.expr.map(|expr| &ast[expr].value)
                {
                    let elements = elements.clone();

                    for element in elements {
                        if let Some(vtable) = context.type_solver.context.find_trait_impl(ast[element].ty, trait_ref, &trait_args) {
                            let type_args = context.use_vtable_impl(ast[element].ty, vtable, ModuleSpan(ast.module, ast[element].span));
                            let item = UsedItem::VTable(ast[element].ty, vtable, type_args.into_iter().map(|ty| context.type_solver.solve(ty)).collect());

                            ast.use_item(
                                &context.type_solver.context.def_registry.adt_types,
                                &context.type_solver.context.impl_registry,
                                &context.type_solver.context.def_registry.traits,
                                context.vtables,
                                context.functions,
                                &mut context.type_solver.context.types,
                                item,
                                Some(vtable),
                            );
                        }
                    }
                }
            }
            _ => (),
        }

        if ast.instances_exceeded {
            context
                .type_solver
                .error(TypeError::InstantiationLimit, ModuleSpan(ast.module, self[root].span));
        }

        (ast, output_block)
    }
}

impl<D: Descriptor> Index<ExprRef> for TypedAST<D> {
    type Output = Typed<Expr<D>, D>;

    fn index(&self, index: ExprRef) -> &Self::Output {
        &self.exprs[index]
    }
}

impl<D: Descriptor> Index<BlockRef> for TypedAST<D> {
    type Output = Typed<Block, D>;

    fn index(&self, index: BlockRef) -> &Self::Output {
        &self.blocks[index]
    }
}

impl<D: Descriptor> Index<StmtRef> for TypedAST<D> {
    type Output = Stmt;

    fn index(&self, index: StmtRef) -> &Self::Output {
        &self.statements[index]
    }
}

#[derive(Debug, Default)]
pub struct ConstantContext {
    pub frames: Vec<IndexMap<String, ConstantValue>>,
    /// Loop iterations done, limited so evaluation always ends.
    steps: usize,
}

impl ConstantContext {
    /// Maximum loop iterations of an evaluation.
    const MAX_STEPS: usize = 100_000;

    /// Counts a loop iteration. Fails when there were too many, so endless
    /// loops in constants are reported instead of hanging the compiler.
    ///
    /// # Errors
    ///
    /// Returns an error if the limit is reached.
    pub const fn step(&mut self) -> Option<()> {
        self.steps += 1;

        if self.steps > Self::MAX_STEPS { None } else { Some(()) }
    }

    pub fn push_frame(&mut self) {
        self.frames.push(IndexMap::new());
    }

    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    pub fn current_frame(&self) -> &IndexMap<String, ConstantValue> {
        &self.frames[self.frames.len() - 1]
    }

    pub fn current_frame_mut(&mut self) -> &mut IndexMap<String, ConstantValue> {
        let frames = self.frames.len();

        &mut self.frames[frames - 1]
    }

    pub fn set_var<T: Into<String>>(&mut self, name: T, value: ConstantValue) {
        if self.frames.is_empty() {
            self.push_frame();
        }

        let frame = self.current_frame_mut();

        frame.insert(name.into(), value);
    }

    pub fn get_var(&self, (frame, var): (usize, usize)) -> &ConstantValue {
        &self.frames[frame][var]
    }

    pub fn search_var<T: AsRef<str>>(&mut self, name: T) -> Option<&ConstantValue> {
        self.search_var_idx(name).map(|idx| self.get_var(idx))
    }

    pub fn search_var_idx<T: AsRef<str>>(&mut self, name: T) -> Option<(usize, usize)> {
        let name = name.as_ref();

        // Innermost frames first, with indices of frames from the outermost.
        for (frame_idx, frame) in self.frames.iter().enumerate().rev() {
            if let Some(var) = frame.get_index_of(name) {
                return Some((frame_idx, var));
            }
        }

        None
    }
}

pub trait IntoConstVal {
    /// # Errors
    ///
    /// TODO
    #[allow(clippy::result_unit_err)]
    fn into_const_val(self, ast: &TypedAST, type_context: &TyCtxt, const_context: &mut ConstantContext) -> Result<ConstantValue, ()>;
}

#[cfg(test)]
mod tests {
    use std::{fmt, path::PathBuf};

    use mollie_index::{Idx, IndexBoxedSlice, IndexVec};
    use mollie_shared::{Span, pretty_fmt::FmtIteratorExt};
    use mollie_typing::{
        Adt, AdtKind, AdtVariant, AdtVariantField, Arg, ArgType, Diagnostic, Func, IntType, ModuleId, PrimitiveType, Trait, TraitFunc, TyCtxt, Type, TypeError,
        UIntType,
    };

    use super::Stmt;
    use crate::{
        FileModuleLoader, FunctionBody, SolvedPass, TypedAST, TypedASTContext, UsedItem,
        block::BlockRef,
        expr::{Expr, ExprRef, IsPattern, LitExpr},
    };

    struct TypeBlockFmt<'a> {
        ast: &'a TypedAST,
        storage: &'a TyCtxt,
        block: BlockRef,
    }

    impl fmt::Display for TypeBlockFmt<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let block = &self.ast[self.block];
            let mut prev_stmt = None;

            for stmt in block.value.stmts.iter().copied() {
                match (prev_stmt.map(|stmt| &self.ast[stmt]), &self.ast[stmt]) {
                    (None, _) | (Some(Stmt::NewVar { .. }), Stmt::NewVar { .. }) => (),
                    (Some(&Stmt::Expr(prev_expr)), &Stmt::Expr(expr)) => match (&self.ast[prev_expr].value, &self.ast[expr].value) {
                        (Expr::Lit(_), Expr::Lit(_))
                        | (Expr::Var(_), Expr::Var(_))
                        | (Expr::Array { .. }, Expr::Array { .. })
                        | (Expr::Binary { .. }, Expr::Binary { .. })
                        | (Expr::Closure { .. }, Expr::Closure { .. })
                        | (Expr::Call { .. }, Expr::Call { .. })
                        | (Expr::Construct { .. }, Expr::Construct { .. })
                        | (Expr::AdtIndex { .. }, Expr::AdtIndex { .. })
                        | (Expr::VTableIndex { .. }, Expr::VTableIndex { .. })
                        | (Expr::ArrayIndex { .. }, Expr::ArrayIndex { .. })
                        | (Expr::Error(_), Expr::Error(_)) => (),
                        _ => writeln!(f)?,
                    },
                    _ => writeln!(f)?,
                }

                match &self.ast[stmt] {
                    &Stmt::Expr(expr) => {
                        writeln!(f, "{};", TypeExprFmt {
                            ast: self.ast,
                            storage: self.storage,
                            expr,
                        })?;
                    }
                    Stmt::NewVar { mutable, name, value } => {
                        writeln!(
                            f,
                            "{} {name}: {} = {};",
                            if *mutable { "let" } else { "const" },
                            self.storage.display_of(self.ast[*value].ty),
                            TypeExprFmt {
                                ast: self.ast,
                                storage: self.storage,
                                expr: *value
                            }
                        )?;
                    }
                }

                prev_stmt = Some(stmt);
            }

            if let Some(expr) = block.value.expr {
                TypeExprFmt {
                    ast: self.ast,
                    storage: self.storage,
                    expr,
                }
                .fmt(f)?;
            }

            Ok(())
        }
    }

    struct TypePatternFmt<'a> {
        ast: &'a TypedAST,
        storage: &'a TyCtxt,
        expr: &'a IsPattern<SolvedPass>,
    }

    impl fmt::Display for TypePatternFmt<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.expr {
                &IsPattern::Literal(expr) => TypeExprFmt {
                    ast: self.ast,
                    storage: self.storage,
                    expr,
                }
                .fmt(f),
                IsPattern::EnumVariant {
                    adt,
                    adt_variant,
                    adt_type_args,
                    values,
                } => write!(
                    f,
                    "{}{}::{}{}",
                    self.storage.def_registry.adt_types[*adt].name.as_deref().unwrap_or_default(),
                    if adt_type_args.is_empty() {
                        String::new()
                    } else {
                        format!("::<{}>", adt_type_args.iter().copied().map(|ty| self.storage.display_of(ty)).join(", "))
                    },
                    self.storage.def_registry.adt_types[*adt].variants[*adt_variant]
                        .name
                        .as_deref()
                        .unwrap_or_default(),
                    if values.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " {{ {} }}",
                            values
                                .iter()
                                .map(|(_, value, pattern)| format!(
                                    "{value}{}",
                                    pattern.as_ref().map_or_default(|pattern| format!(": {}", TypePatternFmt {
                                        ast: self.ast,
                                        storage: self.storage,
                                        expr: pattern
                                    }))
                                ))
                                .join(", ")
                        )
                    }
                ),
                IsPattern::TypeName { ty, name } => write!(f, "{} {name}", self.storage.display_of(*ty)),
                IsPattern::Wildcard => f.write_str("_"),
                IsPattern::Binding { name, .. } => f.write_str(name),
            }
        }
    }

    struct TypeExprFmt<'a> {
        ast: &'a TypedAST,
        storage: &'a TyCtxt,
        expr: ExprRef,
    }

    impl TypeExprFmt<'_> {
        fn fork(&self, expr: ExprRef) -> Self {
            Self {
                ast: self.ast,
                storage: self.storage,
                expr,
            }
        }
    }

    impl fmt::Display for TypeExprFmt<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if self.expr == ExprRef::INVALID {
                return Ok(());
            }

            match &self.ast[self.expr].value {
                Expr::Var(name) => f.write_str(name),
                Expr::Lit(value) => match value {
                    LitExpr::Bool(value) => write!(f, "{value}"),
                    LitExpr::F32(value) => write!(f, "{value}"),
                    LitExpr::Int(value) => write!(f, "{value}{}", self.storage.display_of(self.ast[self.expr].ty)),
                    LitExpr::String(value) => write!(f, "{value:?}"),
                },
                Expr::Array { elements, .. } => {
                    write!(f, "[{}]", elements.iter().map(|&element| self.fork(element)).join(", "))
                }
                &Expr::Unary { operator, expr } => {
                    write!(f, "{}{}", operator.value, self.fork(expr))
                }
                &Expr::Binary { operator, lhs, rhs } => {
                    write!(f, "{} {} {}", self.fork(lhs), operator.value, self.fork(rhs))
                }
                Expr::Closure { args, captures, body } => {
                    write!(
                        f,
                        "|{}|{} {{ {} }}",
                        args.iter().map(|arg| format!("{}: {}", arg.name, self.storage.display_of(arg.ty))).join(", "),
                        if captures.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "({})",
                                captures.iter().map(|(name, ty)| format!("{name}: {}", self.storage.display_of(*ty))).join(", ")
                            )
                        },
                        self.fork(self.ast[*body].value.expr.unwrap())
                    )
                }
                Expr::Construct { adt, variant, fields } => {
                    write!(
                        f,
                        "{}{} {{ {} }}",
                        self.storage.def_registry.adt_types[*adt].name.as_deref().unwrap_or_default(),
                        self.storage.def_registry.adt_types[*adt].variants[*variant]
                            .name
                            .as_ref()
                            .map_or_else(String::new, |name| format!("::{name}")),
                        fields
                            .iter()
                            .skip(usize::from(matches!(self.storage.def_registry.adt_types[*adt].kind, AdtKind::Enum)))
                            .filter(|(.., value)| *value != ExprRef::INVALID)
                            .map(|&(field_ref, _, field_value)| format!(
                                "{}: {}",
                                self.storage.def_registry.adt_types[*adt][(*variant, field_ref)].name,
                                self.fork(field_value)
                            ))
                            .join(", "),
                    )
                }
                &Expr::AdtIndex { target, field } => {
                    write!(
                        f,
                        "{}.{}",
                        self.fork(target),
                        if let Type::Adt(adt, _) = self.storage.types[self.ast[target].ty] {
                            self.storage.def_registry.adt_types[adt][field].name.as_str()
                        } else {
                            "<unknown>"
                        }
                    )
                }
                &Expr::VTableIndex {
                    target,
                    target_ty,
                    vtable,
                    func,
                    ..
                } => match (target, self.storage.impl_registry.impls[vtable].origin_trait) {
                    (Some(target), Some(origin_trait)) => write!(
                        f,
                        "({} as {})::{}",
                        self.fork(target),
                        self.storage.def_registry.traits[origin_trait].name,
                        self.storage.impl_registry.impls[vtable].functions[func].name
                    ),
                    (Some(target), None) => write!(f, "{}.{}", self.fork(target), self.storage.impl_registry.impls[vtable].functions[func].name),
                    (None, Some(origin_trait)) => write!(
                        f,
                        "({} as {}).{}",
                        self.storage.display_of(target_ty),
                        self.storage.def_registry.traits[origin_trait].name,
                        self.storage.impl_registry.impls[vtable].functions[func].name
                    ),
                    (None, None) => write!(
                        f,
                        "{}::{}",
                        self.storage.display_of(target_ty),
                        self.storage.impl_registry.impls[vtable].functions[func].name
                    ),
                },
                &Expr::ArrayIndex { target, element } => {
                    write!(f, "{}[{}]", self.fork(target), self.fork(element))
                }
                &Expr::While { condition, block, .. } => {
                    write!(f, "while {} {{\n{}\n}}", self.fork(condition), TypeBlockFmt {
                        ast: self.ast,
                        storage: self.storage,
                        block
                    })
                }
                &Expr::Block(block) => {
                    write!(f, "{{\n{}\n}}", TypeBlockFmt {
                        ast: self.ast,
                        storage: self.storage,
                        block
                    })
                }
                &Expr::IfElse { condition, block, otherwise } => {
                    write!(
                        f,
                        "if {} {{\n{}\n}}{}",
                        self.fork(condition),
                        TypeBlockFmt {
                            ast: self.ast,
                            storage: self.storage,
                            block
                        },
                        otherwise.map_or_else(String::new, |otherwise| format!(" else {}", self.fork(otherwise)))
                    )
                }
                Expr::Call { func, args } => {
                    write!(f, "{}({})", self.fork(*func), args.iter().map(|&arg| self.fork(arg)).join(", "))
                }
                Expr::IsPattern { target, pattern } => {
                    write!(f, "{} is {}", self.fork(*target), TypePatternFmt {
                        ast: self.ast,
                        storage: self.storage,
                        expr: pattern
                    })
                }
                &Expr::TypeCast(target, ty) => {
                    write!(f, "{} as {ty}", self.fork(target))
                }
                &Expr::Format { value, spec } => {
                    write!(f, "format({}, \"{spec}\")", self.fork(value))
                }
                &Expr::TraitFunc { trait_ref, func, .. } | &Expr::BoundFunc { trait_ref, func, .. } => write!(
                    f,
                    "{}::{}",
                    self.storage.def_registry.traits[trait_ref].name, self.storage.def_registry.traits[trait_ref].functions[func].name
                ),
                &Expr::Func { func, .. } => self.storage.def_registry.functions[func].name.fmt(f),
                Expr::Unreachable => f.write_str("<unreachable>"),
                Expr::Return(Some(value)) => write!(f, "return {}", self.fork(*value)),
                Expr::Return(None) => f.write_str("return"),
                &Expr::Loop { block, .. } => write!(f, "loop {{\n{}\n}}", TypeBlockFmt {
                    ast: self.ast,
                    storage: self.storage,
                    block
                }),
                Expr::Break { value: Some(value), .. } => write!(f, "break {}", self.fork(*value)),
                Expr::Break { value: None, .. } => f.write_str("break"),
                Expr::Continue { .. } => f.write_str("continue"),
                &Expr::Const(constant) => f.write_str(&self.storage.def_registry.constants[constant].name),
                Expr::Panic(message) => write!(f, "panic({})", self.fork(*message)),
                Expr::Error(_) => f.write_str("<error>"),
            }
        }
    }

    #[test]
    fn test_literal() {
        #[allow(unused_variables)]
        let source = "{
            let hello = 12;
            let hello2 = [1, 2, 4i16];
            let volua = |a, b| { a + b };

            enum Option<T> {
                Some { value: T },
                None
            }

            struct A<T> {
                value: T
            }

            struct B<T> {
                value: A<T>
            }

            trait Hello<T> {
                func hello(self) -> A<T>;
            }

            struct C<T> {
                hi: Hello<T>
            }

            impl<T> Hello<T> for A<T> {
                func hello(self) -> A<T> {
                    let hew = A { value: true };

                    A { value: self.value }
                }
            }

            func println(input: i64) -> i64 {
                let dengi = Option::Some { value: A { value: 5i16 } };

                input + 32
            }

            let test_println = println(58);

            let heil = Option::Some { value: 53, double_double };
            let heil2 = Option { value: 53 };
            let world = test::test::Test { };
            let world2 = test::test::Test;

            double_double = 40;

            let a = if 1 == 4 {
                \"peak\"
            } else {
                \"kaep\"
            };

            heil.heil;
            hello.hello;

            if heil is Option::Some { value } {
                println(value);
            }

            hello = 54usize;

            calc_smth(|b| { b * 2 });

            hello = hello + 4 + volua(1, 2);

            let damn = B { value: A { value: 50 } };
            let volua2 = |a| { a.value.value - hello };

            damn = B { value: A { value: 50usize } };
            damn.val;
            damn();
            volua2(damn);
            volua2();
            volua2(hello);

            let mm = C { hi: damn.value.hello() };

            mm = mm;
            mm
        }";
        let source = include_str!("../../../examples/ui.mol");
        let mut context = TypedASTContext::default();

        let mut loader = FileModuleLoader {
            current_dir: PathBuf::from("/home/aiving/Documents/dev-v2/dev/meralus-project/mollie/examples"),
        };

        let usize = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::UInt(UIntType::USize)));
        let string = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::String));
        let bool = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Bool));
        let any = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Any));
        let f32 = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::F32));
        let void = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Void));

        for (name, args, returns) in [
            ("println", Box::new([usize]) as Box<[_]>, void),
            ("println_frame_addr", Box::new([]), void),
            ("println_fat", Box::new([string]), void),
            ("println_str", Box::new([string]), void),
            ("println_bool", Box::new([bool]), void),
            ("println_f32", Box::new([f32]), void),
            ("println_addr", Box::new([any]), void),
            ("get_type_idx", Box::new([any]), usize),
            ("get_size", Box::new([any]), usize),
        ] {
            let ty = context.tcx.types.get_or_add(Type::Func(args, returns));

            let func_ref = context
                .tcx
                .def_registry
                .register_func_in_module(
                    ModuleId::ZERO,
                    Func {
                        postfix: false,
                        generics: 0,
                        name: name.to_string(),
                        arg_names: Vec::new(),
                        ty,
                    },
                    Span::default(),
                )
                .unwrap();

            context.functions.insert(func_ref, FunctionBody::Import(name));
        }

        let func = context.tcx.types.get_or_add(Type::Func(Box::new([]), usize));
        let module = context.tcx.def_registry.register_module("std", Span::default()).unwrap();

        let func_ref = context
            .tcx
            .def_registry
            .register_func_in_module(
                module,
                Func {
                    postfix: false,
                    generics: 0,
                    name: "timestamp".to_string(),
                    arg_names: Vec::new(),
                    ty: func,
                },
                Span::default(),
            )
            .unwrap();

        context.functions.insert(func_ref, FunctionBody::Import("__ext__get_timestamp"));

        // let func_arg =
        // func_compiler.checker.solver.add_info(TypeInfo::Generic(0,
        // None), None);
        // let ty = TypeInfo::Func(Box::new([FuncArg::Regular(func_arg)]),
        // func_compiler.checker.core_types.uint_size);

        // context
        //     .type_context
        //     .register_intrinsic_in_module(module, "size_of_val",
        // IntrinsicKind::SizeOfValue, ty);
        let generic = context.tcx.types.get_or_add(Type::Generic(0));

        context
            .tcx
            .def_registry
            .register_adt_in_module(
                module,
                Adt {
                    name: Some("Option".into()),
                    collectable: true,
                    kind: AdtKind::Enum,
                    generics: 1,
                    variants: IndexBoxedSlice::from_iter([
                        AdtVariant {
                            name: Some("Some".into()),
                            discriminant: 0,
                            fields: IndexBoxedSlice::from_iter([AdtVariantField {
                                name: "value".into(),
                                ty: generic,
                                default_value: None,
                            }]),
                        },
                        AdtVariant {
                            name: Some("None".into()),
                            discriminant: 1,
                            fields: IndexBoxedSlice::default(),
                        },
                    ]),
                },
                Span::default(),
            )
            .unwrap();

        let module = context.tcx.def_registry.register_module("graphics", Span::default()).unwrap();

        context
            .tcx
            .def_registry
            .register_adt_in_module(
                module,
                Adt {
                    name: Some("Image".into()),
                    collectable: true,
                    kind: AdtKind::Enum,
                    generics: 0,
                    variants: IndexBoxedSlice::from_iter([
                        AdtVariant {
                            name: Some("Path".into()),
                            discriminant: 0,
                            fields: IndexBoxedSlice::from_iter([AdtVariantField {
                                name: "value".into(),
                                ty: string,
                                default_value: None,
                            }]),
                        },
                        AdtVariant {
                            name: Some("Url".into()),
                            discriminant: 1,
                            fields: IndexBoxedSlice::from_iter([AdtVariantField {
                                name: "value".into(),
                                ty: string,
                                default_value: None,
                            }]),
                        },
                    ]),
                },
                Span::default(),
            )
            .unwrap();

        let u8 = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::UInt(UIntType::U8)));

        context
            .tcx
            .def_registry
            .register_adt_in_module(
                module,
                Adt {
                    name: Some("Color".into()),
                    collectable: true,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter([
                            AdtVariantField {
                                name: "red".into(),
                                ty: u8,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "blue".into(),
                                ty: u8,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "green".into(),
                                ty: u8,
                                default_value: None,
                            },
                        ]),
                    }]),
                },
                Span::default(),
            )
            .unwrap();

        context
            .tcx
            .def_registry
            .register_adt_in_module(
                module,
                Adt {
                    name: Some("CornerRadius".into()),
                    collectable: true,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter([
                            AdtVariantField {
                                name: "top_left".into(),
                                ty: f32,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "top_right".into(),
                                ty: f32,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "bottom_left".into(),
                                ty: f32,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "bottom_right".into(),
                                ty: f32,
                                default_value: None,
                            },
                        ]),
                    }]),
                },
                Span::default(),
            )
            .unwrap();

        let size_ty = context
            .tcx
            .def_registry
            .register_adt_in_module(
                module,
                Adt {
                    name: Some("Size".into()),
                    collectable: true,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter([
                            AdtVariantField {
                                name: "width".into(),
                                ty: f32,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "height".into(),
                                ty: f32,
                                default_value: None,
                            },
                        ]),
                    }]),
                },
                Span::default(),
            )
            .unwrap();

        let point_ty = context
            .tcx
            .def_registry
            .register_adt_in_module(
                module,
                Adt {
                    name: Some("Point".into()),
                    collectable: true,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter([
                            AdtVariantField {
                                name: "x".into(),
                                ty: f32,
                                default_value: None,
                            },
                            AdtVariantField {
                                name: "y".into(),
                                ty: f32,
                                default_value: None,
                            },
                        ]),
                    }]),
                },
                Span::default(),
            )
            .unwrap();

        let draw_ctx_ty = context
            .tcx
            .def_registry
            .register_adt(
                Adt {
                    name: Some("DrawContext".into()),
                    collectable: false,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::from_iter([AdtVariant {
                        name: None,
                        discriminant: 0,
                        fields: IndexBoxedSlice::from_iter([]),
                    }]),
                },
                Span::default(),
            )
            .unwrap();

        let point_type = context.tcx.types.get_or_add(Type::Adt(point_ty, Box::new([])));
        let size_type = context.tcx.types.get_or_add(Type::Adt(size_ty, Box::new([])));
        let draw_ctx_type = context.tcx.types.get_or_add(Type::Adt(draw_ctx_ty, Box::new([])));

        context
            .tcx
            .def_registry
            .register_trait(
                Trait {
                    name: "Drawable".into(),
                    generics: 1,
                    functions: IndexVec::from_iter([
                        TraitFunc {
                            name: "measure".into(),
                            args: Box::new([
                                Arg {
                                    name: "self".into(),
                                    kind: ArgType::This,
                                    ty: generic,
                                },
                                Arg {
                                    name: "size".into(),
                                    kind: ArgType::Regular,
                                    ty: size_type,
                                },
                                Arg {
                                    name: "ctx".into(),
                                    kind: ArgType::Regular,
                                    ty: draw_ctx_type,
                                },
                            ]),
                            returns: size_type,
                            default: None,
                        },
                        TraitFunc {
                            name: "render".into(),
                            args: Box::new([
                                Arg {
                                    name: "self".into(),
                                    kind: ArgType::This,
                                    ty: generic,
                                },
                                Arg {
                                    name: "origin".into(),
                                    kind: ArgType::Regular,
                                    ty: point_type,
                                },
                                Arg {
                                    name: "size".into(),
                                    kind: ArgType::Regular,
                                    ty: size_type,
                                },
                                Arg {
                                    name: "ctx".into(),
                                    kind: ArgType::Regular,
                                    ty: draw_ctx_type,
                                },
                            ]),
                            returns: void,
                            default: None,
                        },
                    ]),
                },
                Span::default(),
            )
            .unwrap();

        let isize = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Int(IntType::ISize)));
        let input_func = context.tcx.types.get_or_add(Type::Func(Box::new([isize]), isize));
        let func = context.tcx.types.get_or_add(Type::Func(Box::new([input_func]), isize));

        let (solved, block) = context.process(
            &mut loader,
            source,
            [(String::from("context"), draw_ctx_type), (String::from("calc_smth"), func)],
            void,
        );
        let type_context = context.tcx;

        println!("Solved Typed AST dump (fmt):\n{}", TypeBlockFmt {
            ast: &solved,
            storage: &type_context,
            block
        });

        for item in solved.used_items {
            match item {
                UsedItem::VTable(ty, vtable, type_args) => println!(
                    "used vtable item: ({})<{}> for {}",
                    type_context.impl_registry.impls[vtable].origin_trait.map_or_else(
                        || type_context.display_of(ty).to_string(),
                        |origin_trait| type_context.def_registry.traits[origin_trait].name.clone()
                    ),
                    type_args.into_iter().map(|ty| type_context.display_of(ty)).join(", "),
                    type_context.display_of(ty)
                ),
                UsedItem::Adt(adt_ref, type_args) => println!(
                    "used adt item: {}<{}>",
                    type_context.def_registry.adt_types[adt_ref].name.as_deref().unwrap_or_default(),
                    type_args.into_iter().map(|ty| type_context.display_of(ty)).join(", ")
                ),
                UsedItem::Func(func, _) => println!("used func item: {}", type_context.def_registry.functions[func].name),
                UsedItem::Method(_, vtable, func, _) => println!("used method item: {}", type_context.impl_registry.impls[vtable].functions[func].name),
                UsedItem::BoundImpl(ty, trait_ref) => println!(
                    "used bound impl item: {} for {}",
                    type_context.def_registry.traits[trait_ref].name,
                    type_context.display_of(ty)
                ),
            }
        }

        // for error in mem::take(&mut type_context.errors).into_values() {
        //     let mut report =
        // ariadne::Report::build(ariadne::ReportKind::Error, ("ui.mol",
        // error.span.start..error.span.end))
        //         .with_config(ariadne::Config::new().with_compact(true));

        //     error
        //         .value
        //         .add_to_report(("ui.mol", error.span.start..error.span.end),
        // &mut report, &type_context);

        //     report.finish().print(("ui.mol",
        // ariadne::Source::from(source))).unwrap(); }
    }

    #[test]
    fn constructing_wrong_adt_variant_is_rejected() {
        let mut context = TypedASTContext::default();
        let void = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Void));
        let source = "struct Point { x: f32, y: f32 }
struct Vector { x: f32, y: f32 }

const p: Point = Vector { x: 1.0, y: 2.0 };";

        context.process((), source, Vec::<(String, mollie_typing::TypeRef)>::new(), void);

        // for error in context.type_context.errors.values() {
        //     let mut report =
        // ariadne::Report::build(ariadne::ReportKind::Error, ("ui.mol",
        // error.span.start..error.span.end))
        //         .with_config(ariadne::Config::new().with_compact(true));

        //     error
        //         .value
        //         .add_to_report(("ui.mol", error.span.start..error.span.end),
        // &mut report, &context.type_context);

        //     report.finish().print(("ui.mol",
        // ariadne::Source::from(source))).unwrap(); }

        std::assert_matches!(context.diagnostics.errors.raw.as_slice(), [Diagnostic {
            error,
            ..
        }] if matches!(&**error, TypeError::Unexpected { .. }));
    }
}
