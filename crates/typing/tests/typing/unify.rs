use mollie_typing::{AdtTypeInfo, ArrayKind, ArrayKindInfo, ArrayTypeInfo, FuncTypeInfo, IntType, PrimitiveType, Type, TypeInfo, TypeSolver, UIntType, UnifyArgs, UnifyError};

use crate::{adt, context, info};

const I32: TypeInfo = TypeInfo::Primitive(PrimitiveType::Int(IntType::I32));
const U8: TypeInfo = TypeInfo::Primitive(PrimitiveType::UInt(UIntType::U8));
const F32: TypeInfo = TypeInfo::Primitive(PrimitiveType::F32);
const BOOL: TypeInfo = TypeInfo::Primitive(PrimitiveType::Bool);

#[test]
fn same_primitives_unify() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let expected = info(&mut solver, I32);
    let found = info(&mut solver, I32);

    assert!(solver.unify(UnifyArgs { expected, found }).is_ok());
}

#[test]
fn different_primitives_do_not_unify() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let expected = info(&mut solver, I32);
    let found = info(&mut solver, F32);

    assert!(matches!(solver.unify(UnifyArgs { expected, found }), Err(UnifyError::Unexpected { .. })));
}

#[test]
fn integer_literal_takes_the_integer_type_it_meets() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let literal = info(&mut solver, TypeInfo::Integer);
    let found = info(&mut solver, U8);

    assert!(solver.unify(UnifyArgs { expected: literal, found }).is_ok());
    assert_eq!(solver.solve(literal), solver.context.types.core_types.u8);
}

#[test]
fn integer_literal_defaults_to_i32() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let literal = info(&mut solver, TypeInfo::Integer);

    assert_eq!(solver.solve(literal), solver.context.types.core_types.i32);
}

#[test]
fn integer_literal_does_not_unify_with_float() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let literal = info(&mut solver, TypeInfo::Integer);
    let float = info(&mut solver, F32);

    assert!(
        solver
            .unify(UnifyArgs {
                expected: float,
                found: literal
            })
            .is_err()
    );
}

#[test]
fn unknown_takes_the_type_it_meets_in_both_directions() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let first = solver.add_unknown(None, None);
    let second = solver.add_unknown(None, None);
    let boolean = info(&mut solver, BOOL);

    assert!(
        solver
            .unify(UnifyArgs {
                expected: first,
                found: boolean
            })
            .is_ok()
    );
    assert!(
        solver
            .unify(UnifyArgs {
                expected: boolean,
                found: second
            })
            .is_ok()
    );
    assert_eq!(solver.solve(first), solver.context.types.core_types.bool);
    assert_eq!(solver.solve(second), solver.context.types.core_types.bool);
}

#[test]
fn unresolved_unknown_solves_to_error() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let unknown = solver.add_unknown(None, None);
    let solved_type = solver.solve(unknown);

    assert_eq!(solver.context.types[solved_type], Type::Error);
}

#[test]
fn any_accepts_primitives() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let any = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Any));
    let found = info(&mut solver, I32);

    assert!(solver.unify(UnifyArgs { expected: any, found }).is_ok());
}

#[test]
fn occurs_check_rejects_infinite_types() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let element = solver.add_unknown(None, None);
    let array = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Growable }));

    assert!(matches!(
        solver.unify(UnifyArgs {
            expected: element,
            found: array
        }),
        Err(UnifyError::Infinite { .. })
    ));
    assert!(matches!(
        solver.unify(UnifyArgs {
            expected: array,
            found: element
        }),
        Err(UnifyError::Infinite { .. })
    ));

    // The variable stays unbound, so solving it terminates.
    let solved_type = solver.solve(element);

    assert_eq!(solver.context.types[solved_type], Type::Error);
}

#[test]
fn arrays_of_different_sizes_do_not_unify() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let element = info(&mut solver, I32);
    let three = info(&mut solver, TypeInfo::Size(3));
    let four = info(&mut solver, TypeInfo::Size(4));
    let expected = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Fixed(three) }));
    let found = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Fixed(four) }));

    assert!(matches!(
        solver.unify(UnifyArgs { expected, found }),
        Err(UnifyError::ArityMismatch {
            expected: 3,
            found: 4,
            func: _
        })
    ));
}

#[test]
fn growable_and_fixed_arrays_do_not_unify() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let element = info(&mut solver, I32);
    let three = info(&mut solver, TypeInfo::Size(3));
    let expected = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Growable }));
    let found = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Fixed(three) }));

    assert!(solver.unify(UnifyArgs { expected, found }).is_err());
}

#[test]
fn literals_become_the_expected_array() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let element = info(&mut solver, I32);
    let growable = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Growable }));
    let to_growable = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Literal(3) }));

    assert!(solver.unify(UnifyArgs { expected: growable, found: to_growable }).is_ok());

    let three = info(&mut solver, TypeInfo::Size(3));
    let fixed = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Fixed(three) }));
    let to_fixed = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Literal(3) }));

    assert!(solver.unify(UnifyArgs { expected: fixed, found: to_fixed }).is_ok());

    // A literal nothing decides is growable.
    let undecided = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Literal(2) }));

    let solved = [to_growable, to_fixed, undecided].map(|array| solver.solve(array));

    assert!(matches!(solver.context.types[solved[0]], Type::Array(_, ArrayKind::Growable)));
    assert!(matches!(solver.context.types[solved[1]], Type::Array(_, ArrayKind::Fixed(size)) if solver.context.types[size] == Type::Size(3)));
    assert!(matches!(solver.context.types[solved[2]], Type::Array(_, ArrayKind::Growable)));
}

#[test]
fn literals_of_other_lengths_are_not_fixed_arrays() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let element = info(&mut solver, I32);
    let three = info(&mut solver, TypeInfo::Size(3));
    let expected = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Fixed(three) }));
    let found = info(&mut solver, TypeInfo::Array(ArrayTypeInfo { element, kind: ArrayKindInfo::Literal(2) }));

    assert!(matches!(
        solver.unify(UnifyArgs { expected, found }),
        Err(UnifyError::ArityMismatch { expected: 3, found: 2, func: _ })
    ));
}

#[test]
fn function_return_types_must_unify() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let arg = info(&mut solver, I32);
    let int_returns = info(&mut solver, I32);
    let bool_returns = info(&mut solver, BOOL);
    let expected = info(
        &mut solver,
        TypeInfo::Func(FuncTypeInfo {
            args: Box::new([arg]),
            returns: int_returns,
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Func(FuncTypeInfo {
            args: Box::new([arg]),
            returns: bool_returns,
        }),
    );

    assert!(matches!(solver.unify(UnifyArgs { expected, found }), Err(UnifyError::Unexpected { .. })));
}

#[test]
fn adt_type_args_must_unify() {
    let (mut tcx, mut diagnostics) = context();
    let holder = adt(&mut tcx, "Holder", 1);
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let int = info(&mut solver, I32);
    let float = info(&mut solver, F32);
    let expected = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: holder,
            type_args: Box::new([int]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: holder,
            type_args: Box::new([float]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected, found }).is_err());
}

#[test]
fn adt_type_args_are_inferred() {
    let (mut tcx, mut diagnostics) = context();
    let holder = adt(&mut tcx, "Holder", 1);
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let unknown = solver.add_unknown(None, None);
    let int = info(&mut solver, I32);
    let expected = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: holder,
            type_args: Box::new([unknown]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: holder,
            type_args: Box::new([int]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected, found }).is_ok());
    assert_eq!(solver.solve(unknown), solver.context.types.core_types.i32);
}

#[test]
fn different_adts_do_not_unify() {
    let (mut tcx, mut diagnostics) = context();
    let point = adt(&mut tcx, "Point", 0);
    let vector = adt(&mut tcx, "Vector", 0);
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let expected = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: point,
            type_args: Box::new([]),
        }),
    );
    let found = info(
        &mut solver,
        TypeInfo::Adt(AdtTypeInfo {
            id: vector,
            type_args: Box::new([]),
        }),
    );

    assert!(solver.unify(UnifyArgs { expected, found }).is_err());
}
