//! Unification of trait objects with the types implementing them.

use mollie_typing::{AdtTypeInfo, IntType, PrimitiveType, TraitTypeInfo, Type, TypeInfo, TypeSolver, UnifyArgs};

use crate::{adt, context, generic, impl_block, info, primitive, trait_decl};

#[test]
fn implementor_unifies_with_trait_object() {
    let (mut tcx, mut diagnostics) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let circle = adt(&mut tcx, "Circle", 0);
    let circle_ty = tcx.inst_adt(circle, &[]);

    impl_block(&mut tcx, Some(shape), circle_ty, &[], 1, &[]);

    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);
    let expected = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: shape,
            type_args: Box::new([]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: circle,
            type_args: Box::new([]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected, found }).is_ok());
}

#[test]
fn non_implementor_does_not_unify_with_trait_object() {
    let (mut tcx, mut diagnostics) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let circle = adt(&mut tcx, "Circle", 0);
    let square = adt(&mut tcx, "Square", 0);
    let circle_ty = tcx.inst_adt(circle, &[]);

    impl_block(&mut tcx, Some(shape), circle_ty, &[], 1, &[]);

    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);
    let expected = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: shape,
            type_args: Box::new([]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: square,
            type_args: Box::new([]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected, found }).is_err());
}

#[test]
fn different_traits_do_not_unify() {
    let (mut tcx, mut diagnostics) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let named = trait_decl(&mut tcx, "Named", 0);
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let shape_object = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: shape,
            type_args: Box::new([]),
        }),
    );
    let other_shape_object = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: shape,
            type_args: Box::new([]),
        }),
    );
    let named_object = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: named,
            type_args: Box::new([]),
        }),
    );

    assert!(
        solver
            .unify(UnifyArgs {
                expected: shape_object,
                found: other_shape_object
            })
            .is_ok()
    );
    assert!(
        solver
            .unify(UnifyArgs {
                expected: shape_object,
                found: named_object
            })
            .is_err()
    );
}

#[test]
fn trait_type_args_are_checked() {
    let (mut tcx, mut diagnostics) = context();
    // trait Source<T>; impl Source<i32> for Num
    let source = trait_decl(&mut tcx, "Source", 1);
    let num = adt(&mut tcx, "Num", 0);
    let num_ty = tcx.inst_adt(num, &[]);
    let i32_ty = primitive(&mut tcx, PrimitiveType::Int(IntType::I32));

    impl_block(&mut tcx, Some(source), num_ty, &[i32_ty], 1, &[]);

    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);
    let int = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Int(IntType::I32)));
    let boolean = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Bool));
    let int_source = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: source,
            type_args: Box::new([int]),
        }),
    );
    let bool_source = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: source,
            type_args: Box::new([boolean]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: num,
            type_args: Box::new([]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected: int_source, found }).is_ok());
    assert!(solver.unify(UnifyArgs { expected: bool_source, found }).is_err());
}

#[test]
fn trait_type_args_are_inferred_through_generic_impl() {
    let (mut tcx, mut diagnostics) = context();
    // trait Source<T>; impl<T> Source<T> for Holder<T> (generic 0 is `Self`,
    // `T` is generic 1)
    let source = trait_decl(&mut tcx, "Source", 1);
    let holder = adt(&mut tcx, "Holder", 1);
    let t = generic(&mut tcx, 1);
    let holder_ty = tcx.inst_adt(holder, &[t]);

    impl_block(&mut tcx, Some(source), holder_ty, &[t], 2, &[]);

    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);
    let unknown = solver.add_unknown(None, None);
    let boolean = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Bool));
    let expected = info(
        &mut solver,
        TypeInfo::Trait(TraitTypeInfo {
            id: source,
            type_args: Box::new([unknown]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: holder,
            type_args: Box::new([boolean]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected, found }).is_ok());
    assert_eq!(solver.solve(unknown), solver.context.types.core_types.bool);
}

#[test]
fn type_context_treats_implementor_as_same_as_trait() {
    let (mut tcx, _) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let circle = adt(&mut tcx, "Circle", 0);
    let square = adt(&mut tcx, "Square", 0);
    let circle_ty = tcx.inst_adt(circle, &[]);
    let square_ty = tcx.inst_adt(square, &[]);
    let shape_ty = tcx.types.get_or_add(Type::Trait(shape, Box::new([])));

    impl_block(&mut tcx, Some(shape), circle_ty, &[], 1, &[]);

    let shapes = tcx.types.get_or_add(Type::Array(shape_ty, None));
    let circles = tcx.types.get_or_add(Type::Array(circle_ty, None));

    assert!(tcx.is_same(shape_ty, circle_ty));
    assert!(tcx.is_same(circle_ty, shape_ty));
    assert!(!tcx.is_same(shape_ty, square_ty));
    assert!(tcx.is_same(shapes, circles));
}
