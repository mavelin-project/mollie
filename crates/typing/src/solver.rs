use std::collections::HashMap;

use indexmap::IndexMap;
use mollie_index::IndexVec;
use mollie_shared::{MaybePositioned, Span};

use crate::{
    AdtRef, AdtTypeInfo, AdtVariantRef, ArrayKind, ArrayKindInfo, ArrayTypeInfo, DiagnosticContext, FieldRef, FuncTypeInfo, IntType, PrimitiveType, TraitTypeInfo, Type, TypeError,
    TypeErrorRef, TypeErrorValue, TypeInfo, TypeInfoRef,
    ty::TypeRef,
    type_context::{Bound, ModuleSpan, TyCtxt},
};

mollie_index::new_idx_type!(TypeFrameRef);

#[derive(Debug)]
pub struct TypeSolver<'a> {
    pub context: &'a mut TyCtxt,
    pub diagnostics: &'a mut DiagnosticContext,
    pub type_infos: IndexVec<TypeInfoRef, MaybePositioned<TypeInfo>>,
    pub available_generics: IndexMap<String, (TypeInfoRef, TypeRef)>,
    pub bounds: Vec<Bound>,
    frames: IndexVec<TypeFrameRef, TypeFrame>,
}

#[derive(Debug, Default)]
struct TypeFrame(HashMap<String, (TypeInfoRef, bool, Option<Span>)>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnifyArgs {
    pub expected: TypeInfoRef,
    pub found: TypeInfoRef,
}

impl<'a> TypeSolver<'a> {
    pub fn from_context(context: &'a mut TyCtxt, diagnostics: &'a mut DiagnosticContext) -> Self {
        Self {
            context,
            diagnostics,
            type_infos: IndexVec::new(),
            available_generics: IndexMap::new(),
            bounds: Vec::new(),
            frames: IndexVec::from_iter([TypeFrame::default()]),
        }
    }

    pub fn fork(&mut self) -> TypeSolver<'_> {
        let generics = self.available_generics.iter().map(|(name, &(_, ty))| (name.clone(), ty)).collect::<Vec<_>>();
        let bounds = self.bounds.clone();

        let mut solver = TypeSolver::from_context(self.context, self.diagnostics);

        solver.bounds = bounds;

        for (name, ty) in generics {
            let info = Self::type_to_info(&mut solver.type_infos, solver.context, ty, &[]);

            solver.available_generics.insert(name, (info, ty));
        }

        solver
    }

    pub fn error(&mut self, error: TypeError, span: ModuleSpan) -> TypeErrorRef {
        self.diagnostics.error(error, span)
    }

    pub fn finalize(&mut self) {
        for type_info in self.type_infos.values_mut() {
            if let TypeInfo::Unknown(Some(fallback)) = type_info.value {
                type_info.value = TypeInfo::Ref(fallback);
            }
        }
    }

    pub fn push_frame(&mut self) -> TypeFrameRef {
        self.frames.insert(TypeFrame::default())
    }

    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    pub fn set_var<T: Into<String>>(&mut self, name: T, ty: TypeInfoRef) {
        self.set_var_with_mutability(name, ty, true);
    }

    pub fn set_var_with_mutability<T: Into<String>>(&mut self, name: T, ty: TypeInfoRef, mutable: bool) {
        if let Some(frame) = self.frames.last_mut() {
            frame.0.insert(name.into(), (ty, mutable, None));
        }
    }

    pub fn declare_var<T: Into<String>>(&mut self, name: T, ty: TypeInfoRef, mutable: bool, span: Span) {
        if let Some(frame) = self.frames.last_mut() {
            frame.0.insert(name.into(), (ty, mutable, Some(span)));
        }
    }

    pub fn var_span(&self, name: impl AsRef<str>) -> Option<Span> {
        let name = name.as_ref();

        self.frames.values().rev().find_map(|frame| frame.0.get(name)).and_then(|&(_, _, span)| span)
    }

    pub fn is_var_mutable(&self, name: impl AsRef<str>) -> Option<bool> {
        let name = name.as_ref();

        self.frames.values().rev().find_map(|frame| frame.0.get(name).map(|&(_, mutable, _)| mutable))
    }

    pub fn get_var(&self, name: impl AsRef<str>) -> Option<(TypeFrameRef, TypeInfoRef)> {
        let name = name.as_ref();

        for (frame_ref, frame) in self.frames.iter().rev() {
            if let Some(&(var, ..)) = frame.0.get(name) {
                return Some((frame_ref, var));
            }
        }

        None
    }

    pub fn add_info(&mut self, info: TypeInfo, span: Option<Span>) -> TypeInfoRef {
        self.type_infos.insert(MaybePositioned::new(info, span))
    }

    pub fn add_unknown(&mut self, fallback: Option<TypeInfo>, span: Option<Span>) -> TypeInfoRef {
        let fallback = fallback.map(|fallback| self.add_info(fallback, span));

        self.type_infos.insert(MaybePositioned::new(TypeInfo::Unknown(fallback), span))
    }

    pub fn get_info(&self, info_ref: TypeInfoRef) -> TypeInfoRef {
        if let &TypeInfo::Ref(info_ref) = &self.type_infos[info_ref].value {
            self.get_info(info_ref)
        } else {
            info_ref
        }
    }

    #[must_use = "callers must check for unification error"]
    pub fn unify(&mut self, args: UnifyArgs) -> Result<(), UnifyError> {
        let expected = self.get_info(args.expected);
        let found = self.get_info(args.found);

        if expected == found {
            return Ok(());
        }

        match (self.type_infos[expected].value.clone(), self.type_infos[found].value.clone()) {
            (TypeInfo::Ref(expected), _) => self.unify(UnifyArgs { expected, found })?,
            (_, TypeInfo::Ref(found)) => self.unify(UnifyArgs { expected, found })?,
            (TypeInfo::Unknown(None), _)
            | (TypeInfo::Integer, TypeInfo::Primitive(PrimitiveType::Int(_) | PrimitiveType::UInt(_)) | TypeInfo::Integer)
            | (TypeInfo::Float, TypeInfo::Primitive(PrimitiveType::F32 | PrimitiveType::F64) | TypeInfo::Float) => {
                self.bind(expected, found)?;
            }
            (_, TypeInfo::Unknown(None))
            | (TypeInfo::Primitive(PrimitiveType::Int(_) | PrimitiveType::UInt(_)), TypeInfo::Integer)
            | (TypeInfo::Primitive(PrimitiveType::F32 | PrimitiveType::F64), TypeInfo::Float) => {
                self.bind(found, expected)?;
            }
            (TypeInfo::Unknown(Some(_)), _) => self.bind(expected, found)?,
            (_, TypeInfo::Unknown(Some(_))) => self.bind(found, expected)?,
            // Errors are already reported, don't report everything they touch again.
            (TypeInfo::Error | TypeInfo::Primitive(PrimitiveType::Any), _) | (_, TypeInfo::Error) => (),
            // Generic parameters are rigid: inside a generic definition `T` is only equal to
            // itself. Instantiation must replace generics with fresh unknowns beforehand.
            (TypeInfo::Generic(expected_index), TypeInfo::Generic(found_index)) => {
                if expected_index != found_index {
                    return Err(UnifyError::Unexpected { expected, found });
                }
            }
            (TypeInfo::Array(expected_arr), TypeInfo::Array(found_arr)) => {
                self.unify(UnifyArgs {
                    expected: expected_arr.element,
                    found: found_arr.element,
                })?;
                self.unify_array_kinds(expected, expected_arr, found, found_arr)?;
            }
            (TypeInfo::Size(expected_size), TypeInfo::Size(found_size)) => {
                if expected_size != found_size {
                    return Err(UnifyError::ArityMismatch {
                        expected: expected_size,
                        found: found_size,
                        func: None,
                    });
                }
            }
            (TypeInfo::Func(expected_func), TypeInfo::Func(found_func)) => {
                if expected_func.args.len() != found_func.args.len() {
                    return Err(UnifyError::ArityMismatch {
                        expected: expected_func.args.len(),
                        found: found_func.args.len(),
                        func: Some(expected),
                    });
                }

                for (expected, found) in expected_func.args.into_iter().zip(found_func.args) {
                    self.unify(UnifyArgs { expected, found })?;
                }

                self.unify(UnifyArgs {
                    expected: expected_func.returns,
                    found: found_func.returns,
                })?;
            }
            (TypeInfo::Adt(expected_adt), TypeInfo::Adt(found_adt)) => {
                if expected_adt.id != found_adt.id || expected_adt.type_args.len() != found_adt.type_args.len() {
                    return Err(UnifyError::Unexpected { expected, found });
                }

                for (expected, found) in expected_adt.type_args.into_iter().zip(found_adt.type_args) {
                    self.unify(UnifyArgs { expected, found })?;
                }
            }
            (TypeInfo::Trait(expected_trait), TypeInfo::Trait(found_trait)) => {
                if expected_trait.id != found_trait.id || expected_trait.type_args.len() != found_trait.type_args.len() {
                    return Err(UnifyError::Unexpected { expected, found });
                }

                for (expected, found) in expected_trait.type_args.into_iter().zip(found_trait.type_args) {
                    self.unify(UnifyArgs { expected, found })?;
                }
            }
            (TypeInfo::Trait(expected_trait), _) => self.unify_trait_impl(expected, &expected_trait, found)?,
            (TypeInfo::Primitive(expected_primitive), TypeInfo::Primitive(found_primitive)) => {
                if expected_primitive != found_primitive {
                    return Err(UnifyError::Unexpected { expected, found });
                }
            }
            (..) => return Err(UnifyError::Unexpected { expected, found }),
        }

        Ok(())
    }

    fn unify_array_kinds(&mut self, expected: TypeInfoRef, expected_arr: ArrayTypeInfo, found: TypeInfoRef, found_arr: ArrayTypeInfo) -> Result<(), UnifyError> {
        let set = |solver: &mut Self, info: TypeInfoRef, array: ArrayTypeInfo, kind: ArrayKindInfo| {
            solver.type_infos[info].value = TypeInfo::Array(ArrayTypeInfo { kind, ..array });
        };

        match (expected_arr.kind, found_arr.kind) {
            (ArrayKindInfo::Growable, ArrayKindInfo::Growable)
            | (ArrayKindInfo::View { writable: false }, ArrayKindInfo::Growable | ArrayKindInfo::Fixed(_) | ArrayKindInfo::View { .. })
            // A writable view of an array: it's changed in place (fixed arrays
            // only through a mutable place, checked by the typed AST).
            | (ArrayKindInfo::View { writable: true }, ArrayKindInfo::Growable | ArrayKindInfo::Fixed(_) | ArrayKindInfo::View { writable: true }) => (),
            // A literal viewed: it's made fixed, then viewed.
            (ArrayKindInfo::View { .. }, ArrayKindInfo::Literal(length)) => {
                let size = self.add_info(TypeInfo::Size(length), None);

                set(self, found, found_arr, ArrayKindInfo::Fixed(size));
            }
            (ArrayKindInfo::Growable, ArrayKindInfo::Literal(_)) => set(self, found, found_arr, ArrayKindInfo::Growable),
            (ArrayKindInfo::Literal(_), ArrayKindInfo::Growable) => set(self, expected, expected_arr, ArrayKindInfo::Growable),
            (ArrayKindInfo::Fixed(size), ArrayKindInfo::Literal(length)) => {
                let length = self.add_info(TypeInfo::Size(length), None);

                self.unify(UnifyArgs { expected: size, found: length })?;
                set(self, found, found_arr, ArrayKindInfo::Fixed(size));
            }
            (ArrayKindInfo::Literal(length), ArrayKindInfo::Fixed(size)) => {
                let length = self.add_info(TypeInfo::Size(length), None);

                self.unify(UnifyArgs { expected: length, found: size })?;
                set(self, expected, expected_arr, ArrayKindInfo::Fixed(size));
            }
            // Literals of other lengths (branches of an `if`) are growable.
            (ArrayKindInfo::Literal(length), ArrayKindInfo::Literal(other_length)) => {
                if length != other_length {
                    set(self, expected, expected_arr, ArrayKindInfo::Growable);
                    set(self, found, found_arr, ArrayKindInfo::Growable);
                }
            }
            (ArrayKindInfo::Fixed(size), ArrayKindInfo::Fixed(other_size)) => self.unify(UnifyArgs {
                expected: size,
                found: other_size,
            })?,
            _ => return Err(UnifyError::Unexpected { expected, found }),
        }

        Ok(())
    }

    fn bind(&mut self, var: TypeInfoRef, to: TypeInfoRef) -> Result<(), UnifyError> {
        if self.occurs(var, to) {
            return Err(UnifyError::Infinite { ty: to });
        }

        self.type_infos[var].value = TypeInfo::Ref(to);

        Ok(())
    }

    fn occurs(&self, var: TypeInfoRef, info: TypeInfoRef) -> bool {
        let info = self.get_info(info);

        if info == var {
            return true;
        }

        match &self.type_infos[info].value {
            TypeInfo::Array(array) => self.occurs(var, array.element) || matches!(array.kind, ArrayKindInfo::Fixed(size) if self.occurs(var, size)),
            TypeInfo::Func(func) => func.args.iter().any(|&arg| self.occurs(var, arg)) || self.occurs(var, func.returns),
            TypeInfo::Adt(adt) => adt.type_args.iter().any(|&arg| self.occurs(var, arg)),
            TypeInfo::Trait(trait_info) => trait_info.type_args.iter().any(|&arg| self.occurs(var, arg)),
            _ => false,
        }
    }

    fn unify_trait_impl(&mut self, expected: TypeInfoRef, expected_trait: &TraitTypeInfo, found: TypeInfoRef) -> Result<(), UnifyError> {
        let solved_found = self.solve(found);
        // Arguments of the trait object select the impl when they're known
        // (`Source<i32>` and `Source<string>` of one type).
        let trait_args: Box<[_]> = expected_trait.type_args.iter().map(|&arg| self.solve(arg)).collect();

        let Some(vtable) = self.context.find_trait_impl(solved_found, expected_trait.id, &trait_args) else {
            return Err(UnifyError::Unexpected { expected, found });
        };

        let impl_ty = self.context.impl_registry.impls[vtable].ty;
        let trait_args = self.context.impl_registry.impls[vtable].trait_args.clone();
        let generics: Box<[_]> = (0..self.context.impl_registry.impls[vtable].generics.len())
            .map(|_| self.add_unknown(None, None))
            .collect();

        let impl_ty = Self::type_to_info(&mut self.type_infos, self.context, impl_ty, &generics);

        self.unify(UnifyArgs { expected: impl_ty, found })?;

        // Generic 0 of a trait impl is `Self`.
        if let Some(&this) = generics.first() {
            self.unify(UnifyArgs { expected: this, found })?;
        }

        for (&expected, trait_arg) in expected_trait.type_args.iter().zip(trait_args) {
            let found = Self::type_to_info(&mut self.type_infos, self.context, trait_arg, &generics);

            self.unify(UnifyArgs { expected, found })?;
        }

        Ok(())
    }

    pub fn solve(&mut self, info: TypeInfoRef) -> TypeRef {
        match &self.type_infos[info].value {
            &TypeInfo::Primitive(primitive_type) => self.context.types.get_or_add(Type::Primitive(primitive_type)),
            &TypeInfo::Array(array) => {
                let element = self.solve(array.element);
                let kind = match array.kind {
                    // Nothing made it fixed or viewed.
                    ArrayKindInfo::Growable | ArrayKindInfo::Literal(_) => ArrayKind::Growable,
                    ArrayKindInfo::View { writable } => ArrayKind::View { writable },
                    ArrayKindInfo::Fixed(size) => ArrayKind::Fixed(self.solve(size)),
                };

                self.context.types.get_or_add(Type::Array(element, kind))
            }
            &TypeInfo::Size(size) => self.context.types.get_or_add(Type::Size(size)),
            TypeInfo::Func(func) => {
                let args = func.args.clone();
                let returns = self.solve(func.returns);
                let args = args.into_iter().map(|arg| self.solve(arg)).collect();

                self.context.types.get_or_add(Type::Func(args, returns))
            }
            &TypeInfo::Unknown(Some(info)) | &TypeInfo::Ref(info) => self.solve(info),
            &TypeInfo::Unknown(None) | TypeInfo::Error => self.context.types.get_or_add(Type::Error),
            &TypeInfo::Integer => self.context.types.get_or_add(Type::Primitive(PrimitiveType::Int(IntType::I32))),
            &TypeInfo::Float => self.context.types.get_or_add(Type::Primitive(PrimitiveType::F32)),
            TypeInfo::Adt(adt) => {
                let adt_ref = adt.id;
                let type_args = adt.type_args.clone().into_iter().map(|arg| self.solve(arg)).collect();

                self.context.types.get_or_add(Type::Adt(adt_ref, type_args))
            }
            TypeInfo::Trait(trait_info) => {
                let trait_ref = trait_info.id;
                let type_args = trait_info.type_args.clone().into_iter().map(|arg| self.solve(arg)).collect();

                self.context.types.get_or_add(Type::Trait(trait_ref, type_args))
            }
            &TypeInfo::Generic(i) => self.context.types.get_or_add(Type::Generic(i)),
        }
    }

    pub fn type_to_info(infos: &mut IndexVec<TypeInfoRef, MaybePositioned<TypeInfo>>, storage: &TyCtxt, ty: TypeRef, type_args: &[TypeInfoRef]) -> TypeInfoRef {
        match storage.types[ty].clone() {
            Type::Primitive(primitive_type) => infos.insert(MaybePositioned::new(TypeInfo::Primitive(primitive_type), None)),
            Type::Array(element, kind) => {
                let element = Self::type_to_info(infos, storage, element, type_args);
                let kind = match kind {
                    ArrayKind::Growable => ArrayKindInfo::Growable,
                    ArrayKind::View { writable } => ArrayKindInfo::View { writable },
                    ArrayKind::Fixed(size) => ArrayKindInfo::Fixed(Self::type_to_info(infos, storage, size, type_args)),
                };

                infos.insert(MaybePositioned::new(TypeInfo::Array(ArrayTypeInfo { element, kind }), None))
            }
            Type::Size(size) => infos.insert(MaybePositioned::new(TypeInfo::Size(size), None)),
            Type::Adt(id, adt_type_args) => {
                let type_args = adt_type_args
                    .into_iter()
                    .map(|type_arg| Self::type_to_info(infos, storage, type_arg, type_args))
                    .collect();

                infos.insert(MaybePositioned::new(TypeInfo::Adt(AdtTypeInfo { id, type_args }), None))
            }
            Type::Trait(id, trait_type_args) => {
                let type_args = trait_type_args
                    .into_iter()
                    .map(|type_arg| Self::type_to_info(infos, storage, type_arg, type_args))
                    .collect();

                infos.insert(MaybePositioned::new(TypeInfo::Trait(TraitTypeInfo { id, type_args }), None))
            }
            Type::Func(args, returns) => {
                let args = args.into_iter().map(|arg| Self::type_to_info(infos, storage, arg, type_args)).collect();
                let returns = Self::type_to_info(infos, storage, returns, type_args);

                infos.insert(MaybePositioned::new(TypeInfo::Func(FuncTypeInfo { args, returns }), None))
            }
            Type::Generic(i) => type_args
                .get(i)
                .copied()
                .unwrap_or_else(|| infos.insert(MaybePositioned::new(TypeInfo::Generic(i), None))),
            Type::Error => infos.insert(MaybePositioned::new(TypeInfo::Error, None)),
        }
    }

    pub fn instantiate_adt(&mut self, adt: AdtRef, variant: AdtVariantRef, type_args: &[TypeInfoRef]) -> impl Iterator<Item = (FieldRef, TypeInfoRef)> {
        self.context.def_registry.adt_types[adt].variants[variant]
            .fields
            .iter()
            .map(|(field_ref, field)| (field_ref, Self::type_to_info(&mut self.type_infos, self.context, field.ty, type_args)))
    }
}

#[derive(Debug)]
pub enum UnifyError {
    ArityMismatch { expected: usize, found: usize, func: Option<TypeInfoRef> },
    Unexpected { expected: TypeInfoRef, found: TypeInfoRef },
    Infinite { ty: TypeInfoRef },
}

impl UnifyError {
    pub fn into_type_error(self, solver: &mut TypeSolver) -> TypeError {
        match self {
            Self::Unexpected { expected, found } => TypeError::Unexpected {
                expected: TypeErrorValue::ExplicitType(solver.solve(expected)),
                found: TypeErrorValue::ExplicitType(solver.solve(found)),
            },
            Self::ArityMismatch { expected, found, func } => TypeError::ArgumentCountMismatch {
                expected,
                found,
                func: func.map(|ty| solver.solve(ty)),
            },
            Self::Infinite { ty } => TypeError::InfiniteType { ty: solver.solve(ty) },
        }
    }
}

#[cfg(test)]
mod tests {
    use mollie_index::IndexBoxedSlice;
    use mollie_shared::Span;

    use crate::{
        Adt, AdtKind, AdtTypeInfo, DiagnosticContext, FuncTypeInfo, IntType, PrimitiveType, TyCtxt, TypeInfo, TypeSolver, UnifyArgs, solver::UnifyError,
    };

    fn fixture() -> (TyCtxt, DiagnosticContext) {
        (TyCtxt::new(), DiagnosticContext::default())
    }

    #[test]
    fn distinct_adts_do_not_unify() {
        let (mut ctx, mut diagnostics) = fixture();
        let point = ctx
            .def_registry
            .register_adt(
                Adt {
                    name: String::from("Point"),
                    collectable: true,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::default(),
                },
                Span::default(),
            )
            .unwrap();

        let vector = ctx
            .def_registry
            .register_adt(
                Adt {
                    name: String::from("Vector"),
                    collectable: true,
                    kind: AdtKind::Struct,
                    generics: 0,
                    variants: IndexBoxedSlice::default(),
                },
                Span::default(),
            )
            .unwrap();

        let mut solver = TypeSolver::from_context(&mut ctx, &mut diagnostics);

        let expected = solver.add_info(
            TypeInfo::Adt(AdtTypeInfo {
                id: point,
                type_args: Box::new([]),
            }),
            None,
        );

        let found = solver.add_info(
            TypeInfo::Adt(AdtTypeInfo {
                id: vector,
                type_args: Box::new([]),
            }),
            None,
        );

        std::assert_matches!(solver.unify(UnifyArgs { expected, found }), Err(UnifyError::Unexpected { .. }));
    }

    #[test]
    fn func_arity_mismatch_is_rejected() {
        let (mut ctx, mut diagnostics) = fixture();
        let mut solver = TypeSolver::from_context(&mut ctx, &mut diagnostics);
        let i32_ty = solver.add_info(TypeInfo::Primitive(PrimitiveType::Int(IntType::I32)), None);

        let expected = solver.add_info(
            TypeInfo::Func(FuncTypeInfo {
                args: Box::new([i32_ty]),
                returns: i32_ty,
            }),
            None,
        );
        let found = solver.add_info(
            TypeInfo::Func(FuncTypeInfo {
                args: Box::new([i32_ty, i32_ty]),
                returns: i32_ty,
            }),
            None,
        );

        std::assert_matches!(
            solver.unify(UnifyArgs { expected, found }),
            Err(UnifyError::ArityMismatch { expected: 1, found: 2, .. })
        );
    }
}
