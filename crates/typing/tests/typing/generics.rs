//! Generic parameters are rigid: inside a generic definition `T` is equal only
//! to itself.

use mollie_typing::{IntType, PrimitiveType, TypeInfo, TypeSolver, UnifyArgs};

use crate::{context, generic, info};

#[test]
fn generic_does_not_unify_with_concrete_type() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let generic = info(&mut solver, TypeInfo::Generic(0));
    let int = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Int(IntType::I32)));

    assert!(solver.unify(UnifyArgs { expected: generic, found: int }).is_err());
    assert!(solver.unify(UnifyArgs { expected: int, found: generic }).is_err());
}

#[test]
fn integer_literal_does_not_unify_with_generic() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let generic = info(&mut solver, TypeInfo::Generic(0));
    let literal = info(&mut solver, TypeInfo::Integer);

    assert!(
        solver
            .unify(UnifyArgs {
                expected: generic,
                found: literal
            })
            .is_err()
    );
}

#[test]
fn generic_unifies_only_with_the_same_generic() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let first = info(&mut solver, TypeInfo::Generic(0));
    let same = info(&mut solver, TypeInfo::Generic(0));
    let other = info(&mut solver, TypeInfo::Generic(1));

    assert!(solver.unify(UnifyArgs { expected: first, found: same }).is_ok());
    assert!(solver.unify(UnifyArgs { expected: first, found: other }).is_err());
}

#[test]
fn unknown_can_become_generic() {
    let (mut tcx, mut diagnostics) = context();
    let generic_ty = generic(&mut tcx, 0);
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let unknown = solver.add_unknown(None, None);
    let generic = info(&mut solver, TypeInfo::Generic(0));

    assert!(
        solver
            .unify(UnifyArgs {
                expected: unknown,
                found: generic
            })
            .is_ok()
    );
    assert_eq!(solver.solve(unknown), generic_ty);
}

#[test]
fn fork_keeps_available_generics() {
    let (mut tcx, mut diagnostics) = context();
    let generic_ty = generic(&mut tcx, 0);
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let generic_info = info(&mut solver, TypeInfo::Generic(0));

    solver.available_generics.insert(String::from("T"), (generic_info, generic_ty));

    let fork = solver.fork();
    let &(forked_info, forked_ty) = fork.available_generics.get("T").expect("generic `T` must be available in the fork");

    assert_eq!(forked_ty, generic_ty);
    assert!(matches!(fork.type_infos[forked_info].value, TypeInfo::Generic(0)));
}

#[test]
fn fork_has_its_own_variables() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let int = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Int(IntType::I32)));

    solver.set_var("x", int);

    let fork = solver.fork();

    assert!(fork.get_var("x").is_none());
}
