#![allow(missing_docs)]

mod assignments;
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
    Adt, AdtKind, AdtRef, AdtVariantRef, Arg, ArrayKind, Bound, ConstRef, DiagnosticContext, FieldRef, FuncRef, ImplRef, ImplRegistry, ModuleId, ModuleSpan,
    PrimitiveType, SpecialAdtKind, Trait, TraitRef, TyCtxt, Type, TypeError, TypeErrorRef, TypeErrorValue, TypeFrameRef, TypeInfoRef, TypeRef, TypeSolver,
    TypeStorage, UnifyArgs, VFuncRef,
};

pub use crate::{
    block::{Block, BlockRef},
    expr::{Expr, ExprRef, IsPattern, LitExpr, LoopId},
    module_map::{FileModuleLoader, LoadFailure, ModuleLoader, ModuleMap, ParsedModule},
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
    Default {
        func: FuncRef,
        type_args: Box<[TypeRef]>,
    },
    Host {
        code: usize,
        context: usize,
    },
    Stub,
}

#[derive(Debug, Default)]
pub struct PendingConstants {
    decls: IndexMap<ConstRef, (ModuleId, mollie_parser::ConstDecl, Span)>,
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
    pub use_std: bool,
    pub program_package: Option<String>,
    pub host_variables: Vec<HostVariable>,
}

#[derive(Debug, Clone)]
pub struct HostVariable {
    pub name: String,
    pub ty: TypeRef,
    pub mutable: bool,
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
            program_package: None,
            host_variables: Vec::new(),
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

        if let Some(package) = &self.program_package {
            self.tcx.def_registry.packages.insert(root, package.clone());
        }

        let mut map = ModuleMap::new(loader);

        map.register_from_str(&mut self.tcx.def_registry, &mut self.diagnostics, source, root);
        map.process_all_imports(&mut self.tcx.def_registry, &mut self.diagnostics);

        let host_variables = self.host_variables.clone();
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

        // Parameters hide variables of the host with the same names.
        for variable in host_variables {
            let ty = TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, variable.ty, &[]);

            context.type_solver.set_var_with_mutability(variable.name, ty, variable.mutable);
        }

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
    pub expected: Option<TypeInfoRef>,
    pub returns: Option<TypeInfoRef>,
    pub loops: Vec<LoopScope>,
    loop_count: usize,
    pub trait_impl: Option<ImplRef>,
}

#[derive(Debug, Clone)]
pub struct LoopScope {
    pub id: LoopId,
    pub label: Option<String>,
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

    pub(crate) fn enter_loop(&mut self, label: Option<String>, result: Option<TypeInfoRef>) -> LoopId {
        let id = LoopId::new(self.loop_count);

        self.loop_count += 1;
        self.loops.push(LoopScope { id, label, result });

        id
    }

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
    Func(FuncRef, Box<[TypeRef]>),
    Method(TypeRef, ImplRef, VFuncRef, Box<[TypeRef]>),
    BoundImpl(TypeRef, TraitRef),
    Value(TypeRef),
}

impl UsedItem {
    fn type_args(&self) -> Vec<TypeRef> {
        match self {
            Self::VTable(ty, _, args) | Self::Method(ty, _, _, args) => std::iter::once(*ty).chain(args.iter().copied()).collect(),
            Self::Adt(_, args) | Self::Func(_, args) => args.to_vec(),
            Self::BoundImpl(ty, _) | Self::Value(ty) => vec![*ty],
        }
    }

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
            Self::Value(ty) => Self::Value(types.apply_type_args(*ty, type_args)),
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

#[derive_where(Debug, Clone; D::Type)]
pub struct Variable<D: Descriptor = SolvedPass> {
    pub name: String,
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
    pub variables: Vec<Variable<D>>,
    pub var_uses: Vec<(Span, Span)>,
    in_progress: Vec<UsedItem>,
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
                                self.use_types_in(adt_types, impls, traits, vtables, functions, types, field.ty, type_args, current_vtable);
                            }
                        }
                    }
                    // Elements of a fixed array are stored inline in it.
                    &UsedItem::Value(ty) => {
                        if let Type::Array(element, _) = types[ty] {
                            self.use_types_in(adt_types, impls, traits, vtables, functions, types, element, &[], current_vtable);
                        }
                    }
                }

                self.in_progress.retain(|in_progress| in_progress != &item);
                self.used_items.push(item);
            }
        });
    }

    #[allow(clippy::too_many_arguments, reason = "the parts of the context it reads, borrowed separately")]
    fn use_value_type(
        &mut self,
        adt_types: &IndexVec<AdtRef, Adt>,
        impls: &ImplRegistry,
        traits: &IndexVec<TraitRef, Trait>,
        vtables: &VTableMap,
        functions: &FunctionMap,
        types: &mut TypeStorage,
        ty: TypeRef,
    ) {
        let item = match &types[ty] {
            Type::Adt(adt, type_args) => UsedItem::Adt(*adt, type_args.clone()),
            Type::Array(_, ArrayKind::Fixed(_)) => UsedItem::Value(ty),
            _ => return,
        };

        self.use_item(adt_types, impls, traits, vtables, functions, types, item, None);
    }

    #[allow(clippy::too_many_arguments, reason = "the parts of the context it reads, borrowed separately")]
    fn use_types_in(
        &mut self,
        adt_types: &IndexVec<AdtRef, Adt>,
        impls: &ImplRegistry,
        traits: &IndexVec<TraitRef, Trait>,
        vtables: &VTableMap,
        functions: &FunctionMap,
        types: &mut TypeStorage,
        ty: TypeRef,
        type_args: &[TypeRef],
        current_vtable: Option<ImplRef>,
    ) {
        match types[ty].clone() {
            Type::Adt(adt, args) => {
                let args = args.iter().map(|&arg| types.apply_type_args(arg, type_args)).collect::<Box<[_]>>();

                for &arg in &args {
                    self.use_types_in(adt_types, impls, traits, vtables, functions, types, arg, &[], current_vtable);
                }

                self.use_item(adt_types, impls, traits, vtables, functions, types, UsedItem::Adt(adt, args), current_vtable);
            }
            // Its elements are used by the item.
            Type::Array(_, ArrayKind::Fixed(_)) => {
                let ty = types.apply_type_args(ty, type_args);

                self.use_item(adt_types, impls, traits, vtables, functions, types, UsedItem::Value(ty), current_vtable);
            }
            Type::Array(element, _) => self.use_types_in(adt_types, impls, traits, vtables, functions, types, element, type_args, current_vtable),
            Type::Func(args, returns) => {
                for arg in args {
                    self.use_types_in(adt_types, impls, traits, vtables, functions, types, arg, type_args, current_vtable);
                }

                self.use_types_in(adt_types, impls, traits, vtables, functions, types, returns, type_args, current_vtable);
            }
            _ => (),
        }
    }
}

fn holds_view(types: &TypeStorage, ty: TypeRef) -> bool {
    match &types[ty] {
        Type::Adt(_, args) | Type::Trait(_, args) => args.iter().any(|&arg| is_or_holds_view(types, arg)),
        &Type::Array(element, _) => is_or_holds_view(types, element),
        _ => false,
    }
}

fn is_or_holds_view(types: &TypeStorage, ty: TypeRef) -> bool {
    matches!(types[ty], Type::Array(_, ArrayKind::View { .. })) || holds_view(types, ty)
}

fn is_concrete(types: &TypeStorage, ty: TypeRef) -> bool {
    match &types[ty] {
        Type::Primitive(_) | Type::Size(_) => true,
        &Type::Array(element, kind) => is_concrete(types, element) && !matches!(kind, ArrayKind::Fixed(size) if !is_concrete(types, size)),
        Type::Adt(_, args) | Type::Trait(_, args) => args.iter().all(|&arg| is_concrete(types, arg)),
        Type::Func(args, returns) => args.iter().all(|&arg| is_concrete(types, arg)) && is_concrete(types, *returns),
        Type::Generic(_) | Type::Error => false,
    }
}

fn comparable_value(tcx: &mut TyCtxt, ty: TypeRef) -> bool {
    let (adt, type_args) = match tcx.types[ty].clone() {
        Type::Adt(adt, type_args) => (adt, type_args),
        // Fixed arrays are compared element by element.
        Type::Array(element, ArrayKind::Fixed(_)) => {
            return operator_supports(Operator::Equal, &tcx.types[element]) || comparable_value(tcx, element);
        }
        _ => return false,
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

fn operator_supports(operator: Operator, ty: &Type) -> bool {
    // Compound assignments (`+=`, ...) have the same requirements as their
    // operator.
    let operator = operator.lower().unwrap_or(operator);

    match ty {
        // Errors are reported elsewhere; any value can be assigned and
        // matched.
        _ if matches!(ty, Type::Error) || matches!(operator, Operator::Assign | Operator::Is) => true,
        Type::Primitive(primitive) => match operator {
            // Strings are concatenated.
            Operator::Add => primitive.is_num() || primitive.is_float() || *primitive == PrimitiveType::String,
            // Remainders and shifts of integers only: shifts by as many bits
            // as their width at most (more wraps around, like Rust's
            // `wrapping_shl`).
            Operator::Rem | Operator::Shl | Operator::Shr => primitive.is_num(),
            Operator::Sub | Operator::Mul | Operator::Div => primitive.is_num() || primitive.is_float(),
            // Strings are ordered by their bytes.
            Operator::LessThan | Operator::LessThanEqual | Operator::GreaterThan | Operator::GreaterThanEqual => {
                primitive.is_num() || primitive.is_float() || *primitive == PrimitiveType::String
            }
            Operator::BitAnd | Operator::BitOr | Operator::BitXor => primitive.is_num() || *primitive == PrimitiveType::Bool,
            Operator::And | Operator::Or => *primitive == PrimitiveType::Bool,
            Operator::Equal | Operator::NotEqual => !matches!(primitive, PrimitiveType::Void | PrimitiveType::Any),
            _ => true,
        },
        _ => false,
    }
}

impl TypedASTContextRef<'_> {
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

        // The target is used as the impl's type: e.g. any array as the view of
        // an impl of `T[..]`.
        if let Err(err) = self.type_solver.unify(UnifyArgs {
            expected: origin_ty,
            found: solved_origin_ty,
        }) {
            let err = err.into_type_error(&mut self.type_solver);

            self.type_solver.error(err, span);
        }

        type_args
    }
}

impl TypedAST<FirstPass> {
    fn solve_expr(&self, ast: &mut TypedAST, expr: ExprRef, context: &mut TypedASTContextRef<'_>) -> ExprRef {
        mollie_shared::limits::grow_stack(move || {
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
                                if !primitive.is_int() && !primitive.is_float() {
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

                    // Assigned where a trait object is expected (like
                    // `self.children = [Square {}]` into `Shape[]`).
                    if operator.value == Operator::Assign {
                        register_coercion(ast, ast[lhs].ty, rhs, context);
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

                            (field_ref, field_type, expr)
                        })
                        .collect();

                    Expr::Construct { adt, variant, fields }
                }
                Expr::AdtIndex { target, field } => {
                    let target = self.solve_expr(ast, target, context);

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
                        if !context.type_solver.context.can_use_method(ast.module, vtable, func) {
                            context
                                .type_solver
                                .error(TypeError::Private { name: field }, ModuleSpan(ast.module, self[expr].span));
                        }

                        let ty = context.type_solver.context.impl_registry.impls[vtable].functions[func].ty;
                        // Instantiate the generics of the impl, then the
                        // function's own generics, with fresh type variables.
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

                        // The target is used as the impl's type (see
                        // `use_vtable_impl`).
                        if let Err(err) = context.type_solver.unify(UnifyArgs {
                            expected: origin_ty,
                            found: solved_origin_ty,
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
                        // Functions of views are compiled once for views of
                        // each element type: every array is given to them as
                        // a view (`self` is one in their code too).
                        let tcx = &mut *context.type_solver.context;
                        let viewed = match (&tcx.types[tcx.impl_registry.impls[vtable].ty], &tcx.types[ast[target].ty]) {
                            (Type::Array(_, ArrayKind::View { .. }), &Type::Array(element, _)) => Some(element),
                            _ => None,
                        };
                        let receiver_ty = viewed.map_or(ast[target].ty, |element| {
                            tcx.types.get_or_add(Type::Array(element, ArrayKind::View { writable: false }))
                        });

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
                                UsedItem::VTable(receiver_ty, vtable, type_args.clone())
                            } else {
                                UsedItem::Method(receiver_ty, vtable, func, type_args.clone())
                            },
                            Some(vtable),
                        );

                        return ast.add_expr(
                            Expr::VTableIndex {
                                target: Some(target),
                                target_ty: receiver_ty,
                                vtable,
                                func,
                                type_args: type_args[impl_generics..].into(),
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

                        let Some(field_ref) = adt.variants[AdtVariantRef::ZERO]
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

                        let ty = adt[field_ref].ty;
                        let (adt_ref, args) = (*adt_ref, args.clone());
                        let ty = context.type_solver.context.types.apply_type_args(ty, &args);

                        if !context.type_solver.context.def_registry.can_use_field(ast.module, adt_ref, field_ref) {
                            context.type_solver.error(TypeError::Private { name: field }, ModuleSpan(ast.module, self[expr].span));
                        }

                        let field = field_ref;

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
                    // Arrays and primitives have functions, but not this one
                    // (like `push` of a fixed array).
                    let error = if matches!(context.type_solver.context.types[ast[target].ty], Type::Array(..) | Type::Primitive(_)) {
                        TypeError::NonIndexable {
                            ty: ast[target].ty,
                            name: field,
                        }
                    } else {
                        TypeError::Unexpected {
                            expected: TypeErrorValue::Adt(SpecialAdtKind::WithExpectation(AdtKind::Enum)),
                            found: TypeErrorValue::ExplicitType(ast[target].ty),
                        }
                    };

                    return ast.add_expr(
                        Expr::Error(context.type_solver.error(error, ModuleSpan(ast.module, ast[target].span))),
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
                    let captures: Box<[_]> = captures.into_iter().map(|(name, ty)| (name, context.type_solver.solve(ty))).collect();

                    // A closure may outlive the array a view refers to.
                    for (name, ty) in &captures {
                        if is_or_holds_view(&context.type_solver.context.types, *ty) {
                            let message = format!("closures can't capture `{name}`, a view of an array (`T[..]`): copy it with `to_array()` first");

                            context.type_solver.error(TypeError::InvalidArray { message }, ModuleSpan(ast.module, self[expr].span));
                        }
                    }

                    let args = args
                        .into_iter()
                        .map(|arg| Arg {
                            name: arg.name,
                            kind: arg.kind,
                            ty: context.type_solver.solve(arg.ty),
                        })
                        .collect();

                    // `return` in the body returns from the closure.
                    let closure_returns = context.type_solver.solve(self[expr].ty);
                    let closure_returns = match context.type_solver.context.types[closure_returns] {
                        Type::Func(_, returns) => Some(returns),
                        _ => None,
                    };
                    let returns_info =
                        closure_returns.map(|returns| TypeSolver::type_to_info(&mut context.type_solver.type_infos, context.type_solver.context, returns, &[]));
                    let outer_returns = mem::replace(&mut context.returns, returns_info);
                    let body = self.solve_block(ast, body, context);

                    context.returns = outer_returns;

                    // Its value is converted to its result type.
                    if let Some(returns) = closure_returns
                        && let Some(tail) = ast[body].value.expr
                        && ast[tail].ty != returns
                    {
                        register_coercion(ast, returns, tail, context);
                    }

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
                                // parameters, so their receiver is their first
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

                    // An erroneous function (like a private one) is reported
                    // already.
                    if let Expr::Error(error) = ast[func].value {
                        return ast.add_expr(Expr::Error(error), ty, self[expr].span);
                    }

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
                Expr::Uninit => Expr::Uninit,
                Expr::Return(value) => {
                    let value = value.map(|value| self.solve_expr(ast, value, context));

                    // Converted to the result type of the function or closure
                    // (e.g. `return Square {}` from a function returning a
                    // trait object needs the vtable).
                    if let (Some(value), Some(returns)) = (value, context.returns) {
                        let returns = context.type_solver.solve(returns);

                        register_coercion(ast, returns, value, context);
                    }

                    Expr::Return(value)
                }
                Expr::Const(constant) => Expr::Const(constant),
                Expr::Panic(message) => Expr::Panic(self.solve_expr(ast, message, context)),
            };

            let ty = context.type_solver.solve(self[expr].ty);

            // Views refer to arrays they don't own, so values that are kept
            // (objects, arrays, results of generic functions) can't hold them.
            if matches!(value, Expr::Construct { .. } | Expr::Array { .. } | Expr::Call { .. }) && holds_view(&context.type_solver.context.types, ty) {
                let message = String::from("a view of an array (`T[..]`) can't be stored in a value: copy it into an array with `to_array()`");

                context.type_solver.error(TypeError::InvalidArray { message }, ModuleSpan(ast.module, self[expr].span));
            }

            // Values of ADTs need their layouts wherever they appear: fields
            // are read and patterns matched on objects the program may not
            // construct itself (arguments of programs, results of the host).
            ast.use_value_type(
                &context.type_solver.context.def_registry.adt_types,
                &context.type_solver.context.impl_registry,
                &context.type_solver.context.def_registry.traits,
                context.vtables,
                context.functions,
                &mut context.type_solver.context.types,
                ty,
            );

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
                    let stmt = match self[stmt] {
                        Stmt::Expr(expr) => Stmt::Expr(self.solve_expr(ast, expr, context)),
                        Stmt::NewVar { mutable, ref name, value } => Stmt::NewVar {
                            mutable,
                            name: name.clone(),
                            value: self.solve_expr(ast, value, context),
                        },
                    };

                    ast.add_stmt(stmt)
                })
                .collect(),
            expr: input_block.value.expr.map(|expr| self.solve_expr(ast, expr, context)),
        };

        ast.add_block(output_block, context.type_solver.solve(input_block.ty), input_block.span)
    }

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

    pub(crate) fn declare_var(&mut self, context: &mut TypedASTContextRef<'_>, name: impl Into<String>, ty: TypeInfoRef, mutable: bool, span: Span) {
        let name = name.into();

        context.type_solver.declare_var(name.clone(), ty, mutable, span);
        self.variables.push(Variable { name, span, ty });
    }

    pub(crate) fn use_var(&mut self, context: &TypedASTContextRef<'_>, name: &str, span: Span) {
        if let Some(declaration) = context.type_solver.var_span(name) {
            self.var_uses.push((span, declaration));
        }
    }

    fn solve_expr_final(self, root: ExprRef, context: &mut TypedASTContextRef<'_>) -> (TypedAST, ExprRef) {
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
        self.check_assignments(root, context);

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

        // `return` in the body returns `returns`.
        let outer_returns = context.returns.replace(returns_info);
        let output_block = self.solve_block(&mut ast, root, context);

        context.returns = outer_returns;

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
    steps: usize,
}

impl ConstantContext {
    const MAX_STEPS: usize = 100_000;

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
    #[allow(clippy::result_unit_err)]
    fn into_const_val(self, ast: &TypedAST, type_context: &TyCtxt, const_context: &mut ConstantContext) -> Result<ConstantValue, ()>;
}

#[cfg(test)]
mod tests {
    use mollie_typing::{Diagnostic, PrimitiveType, Type, TypeError};

    use crate::TypedASTContext;

    #[test]
    fn constructing_wrong_adt_variant_is_rejected() {
        let mut context = TypedASTContext::default();
        let void = context.tcx.types.get_or_add(Type::Primitive(PrimitiveType::Void));
        let source = "struct Point { x: f32, y: f32 }
struct Vector { x: f32, y: f32 }

const p: Point = Vector { x: 1.0, y: 2.0 };";

        context.process((), source, Vec::<(String, mollie_typing::TypeRef)>::new(), void);

        std::assert_matches!(context.diagnostics.errors.raw.as_slice(), [Diagnostic {
            error,
            ..
        }] if matches!(&**error, TypeError::Unexpected { .. }));
    }
}
