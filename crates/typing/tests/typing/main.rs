//! Tests of the type solver, the definition and impl registries.
#![allow(clippy::missing_panics_doc)]

mod diagnostics;
mod generics;
mod registry;
mod scopes;
mod traits;
mod unify;

use mollie_index::{IndexBoxedSlice, IndexVec};
use mollie_shared::Span;
use mollie_typing::{
    Adt, AdtKind, AdtRef, DiagnosticContext, ImplRef, PrimitiveType, Trait, TraitRef, TyCtxt, Type, TypeInfo, TypeInfoRef, TypeRef, TypeSolver, VTableFunc,
    VTableGenerator,
};

pub fn context() -> (TyCtxt, DiagnosticContext) {
    (TyCtxt::new(), DiagnosticContext::default())
}

pub fn primitive(tcx: &mut TyCtxt, primitive: PrimitiveType) -> TypeRef {
    tcx.types.get_or_add(Type::Primitive(primitive))
}

pub fn generic(tcx: &mut TyCtxt, index: usize) -> TypeRef {
    tcx.types.get_or_add(Type::Generic(index))
}

/// Registers a struct without fields in the root module.
pub fn adt(tcx: &mut TyCtxt, name: &str, generics: usize) -> AdtRef {
    tcx.def_registry
        .register_adt(
            Adt {
                name: Some(name.to_owned()),
                collectable: true,
                kind: AdtKind::Struct,
                generics,
                variants: IndexBoxedSlice::default(),
            },
            Span::default(),
        )
        .unwrap()
}

/// Registers a trait without functions in the root module.
pub fn trait_decl(tcx: &mut TyCtxt, name: &str, generics: usize) -> TraitRef {
    tcx.def_registry
        .register_trait(
            Trait {
                name: name.to_owned(),
                generics,
                functions: IndexVec::new(),
            },
            Span::default(),
        )
        .unwrap()
}

/// Registers an impl block. `generics` is the total count of the impl's
/// generics, including `Self` (generic 0) for trait impls.
pub fn impl_block(tcx: &mut TyCtxt, origin_trait: Option<TraitRef>, ty: TypeRef, trait_args: &[TypeRef], generics: usize, functions: &[&str]) -> ImplRef {
    let generics = (0..generics).map(|index| generic(tcx, index)).collect();
    let void = tcx.types.core_types.void;
    let func_ty = tcx.types.get_or_add(Type::Func(Box::new([ty]), void));

    tcx.register_impl(VTableGenerator {
        ty,
        origin_trait,
        trait_args: trait_args.into(),
        generics,
        bounds: Box::new([]),
        functions: functions
            .iter()
            .map(|&name| VTableFunc {
                trait_func: None,
                name: name.to_owned(),
                arg_names: vec![String::from("self")],
                generics: 0,
                ty: func_ty,
            })
            .collect(),
    })
}

pub fn info(solver: &mut TypeSolver<'_>, info: TypeInfo) -> TypeInfoRef {
    solver.add_info(info, None)
}
