use mollie_typing::{PrimitiveType, TypeInfo, TypeSolver};

use crate::{context, info};

#[test]
fn variables_remember_mutability() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let ty = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Bool));

    solver.set_var("mutable", ty);
    solver.set_var_with_mutability("constant", ty, false);

    assert_eq!(solver.is_var_mutable("mutable"), Some(true));
    assert_eq!(solver.is_var_mutable("constant"), Some(false));
    assert_eq!(solver.is_var_mutable("missing"), None);
}

#[test]
fn inner_scope_shadows_outer_variable() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let ty = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Bool));

    solver.set_var("x", ty);
    solver.push_frame();
    solver.set_var_with_mutability("x", ty, false);

    assert_eq!(solver.is_var_mutable("x"), Some(false));

    solver.pop_frame();

    assert_eq!(solver.is_var_mutable("x"), Some(true));
}

#[test]
fn variables_of_popped_scope_are_gone() {
    let (mut tcx, mut diagnostics) = context();
    let mut solver = TypeSolver::from_context(&mut tcx, &mut diagnostics);

    let ty = info(&mut solver, TypeInfo::Primitive(PrimitiveType::Bool));

    solver.push_frame();
    solver.set_var("inner", ty);
    solver.pop_frame();

    assert!(solver.get_var("inner").is_none());
}
