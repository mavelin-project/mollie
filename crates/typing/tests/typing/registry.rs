use std::mem::size_of;

use mollie_index::{Idx, IndexBoxedSlice, IndexVec};
use mollie_shared::Span;
use mollie_typing::{
    Adt, AdtKind, Arg, ArgType, DiagnosticContext, IntType, LookupError, ModuleId, ModuleItem, ModuleSpan, PrimitiveType, Trait, TraitFunc, Type, TypeError,
    VFuncRef,
};

use crate::{adt, context, generic, impl_block, primitive, trait_decl};

#[test]
fn impl_types_match_in_one_direction() {
    let (mut tcx, _) = context();
    let t = generic(&mut tcx, 0);
    let i32_ty = primitive(&mut tcx, PrimitiveType::Int(IntType::I32));
    let any_array = tcx.types.get_or_add(Type::Array(t, None));
    let i32_array = tcx.types.get_or_add(Type::Array(i32_ty, None));
    let i32_array_3 = tcx.types.get_or_add(Type::Array(i32_ty, Some(3)));

    // Generics and unsized arrays are wildcards on the impl side...
    assert!(tcx.types.impl_matches(any_array, i32_array_3));
    assert!(tcx.types.impl_matches(i32_array, i32_array_3));
    assert!(tcx.types.impl_matches(t, i32_ty));
    // ...but not on the side of the type that is looked up.
    assert!(!tcx.types.impl_matches(i32_array_3, i32_array));
    assert!(!tcx.types.impl_matches(i32_ty, t));
}

#[test]
fn trait_impl_lookup_finds_impl_of_the_trait() {
    let (mut tcx, _) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let named = trait_decl(&mut tcx, "Named", 0);
    let circle = adt(&mut tcx, "Circle", 0);
    let circle_ty = tcx.inst_adt(circle, &[]);
    let impl_ref = impl_block(&mut tcx, Some(shape), circle_ty, &[], 1, &[]);

    assert_eq!(tcx.trait_impl_lookup(shape, circle_ty), Ok(impl_ref));
    assert_eq!(tcx.trait_impl_lookup(named, circle_ty), Err(LookupError::NotFound));
}

#[test]
fn trait_impl_lookup_reports_ambiguity() {
    let (mut tcx, _) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let holder = adt(&mut tcx, "Holder", 1);
    let t = generic(&mut tcx, 1);
    let i32_ty = primitive(&mut tcx, PrimitiveType::Int(IntType::I32));
    let bool_ty = primitive(&mut tcx, PrimitiveType::Bool);
    let any_holder = tcx.inst_adt(holder, &[t]);
    let i32_holder = tcx.inst_adt(holder, &[i32_ty]);
    let bool_holder = tcx.inst_adt(holder, &[bool_ty]);

    // impl<T> Shape for Holder<T> and impl Shape for Holder<i32>
    let generic_impl = impl_block(&mut tcx, Some(shape), any_holder, &[], 2, &[]);
    let specific_impl = impl_block(&mut tcx, Some(shape), i32_holder, &[], 1, &[]);

    assert_eq!(tcx.trait_impl_lookup(shape, bool_holder), Ok(generic_impl));
    assert!(matches!(
        tcx.trait_impl_lookup(shape, i32_holder),
        Err(LookupError::Ambiguous(impls)) if impls.len() == 2 && impls.contains(&generic_impl) && impls.contains(&specific_impl)
    ));
}

#[test]
fn blanket_impl_applies_to_any_type() {
    let (mut tcx, _) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let t = generic(&mut tcx, 1);
    let bool_ty = primitive(&mut tcx, PrimitiveType::Bool);

    // impl<T> Shape for T
    let impl_ref = impl_block(&mut tcx, Some(shape), t, &[], 2, &[]);

    assert!(tcx.impl_registry.blanket_impls.contains(&impl_ref));
    assert_eq!(tcx.trait_impl_lookup(shape, bool_ty), Ok(impl_ref));
}

#[test]
fn method_lookup_prefers_inherent_methods() {
    let (mut tcx, _) = context();
    let named = trait_decl(&mut tcx, "Named", 0);
    let circle = adt(&mut tcx, "Circle", 0);
    let circle_ty = tcx.inst_adt(circle, &[]);
    let inherent = impl_block(&mut tcx, None, circle_ty, &[], 0, &["name"]);

    impl_block(&mut tcx, Some(named), circle_ty, &[], 1, &["name"]);

    assert_eq!(tcx.method_lookup(None, circle_ty, "name"), Ok((inherent, VFuncRef::ZERO)));
}

#[test]
fn method_lookup_falls_back_to_trait_methods() {
    let (mut tcx, _) = context();
    let shape = trait_decl(&mut tcx, "Shape", 0);
    let circle = adt(&mut tcx, "Circle", 0);
    let circle_ty = tcx.inst_adt(circle, &[]);

    impl_block(&mut tcx, None, circle_ty, &[], 0, &["radius"]);

    let trait_impl = impl_block(&mut tcx, Some(shape), circle_ty, &[], 1, &["area"]);

    assert_eq!(tcx.method_lookup(None, circle_ty, "area"), Ok((trait_impl, VFuncRef::ZERO)));
    assert_eq!(tcx.find_vtable_by_func(circle_ty, "area"), Some((trait_impl, VFuncRef::ZERO)));
    assert_eq!(tcx.method_lookup(None, circle_ty, "perimeter"), Err(LookupError::NotFound));
}

#[test]
fn methods_of_generic_impl_apply_to_instantiations() {
    let (mut tcx, _) = context();
    let holder = adt(&mut tcx, "Holder", 1);
    let t = generic(&mut tcx, 0);
    let i32_ty = primitive(&mut tcx, PrimitiveType::Int(IntType::I32));
    let any_holder = tcx.inst_adt(holder, &[t]);
    let i32_holder = tcx.inst_adt(holder, &[i32_ty]);

    // impl<T> Holder<T> { func get(self) }
    let impl_ref = impl_block(&mut tcx, None, any_holder, &[], 1, &["get"]);

    assert_eq!(tcx.method_lookup(None, i32_holder, "get"), Ok((impl_ref, VFuncRef::ZERO)));
}

#[test]
fn duplicate_names_are_reported() {
    let (mut tcx, _) = context();

    adt(&mut tcx, "Thing", 0);

    let duplicate = tcx.def_registry.register_trait(
        Trait {
            name: String::from("Thing"),
            generics: 0,
            functions: IndexVec::new(),
        },
        Span::default(),
    );

    assert!(matches!(duplicate, Err(diagnostic) if matches!(diagnostic.error, TypeError::AlreadyExists { ref name, .. } if name == "Thing")));
}

#[test]
fn anonymous_adts_are_not_module_items() {
    let (mut tcx, _) = context();

    let anonymous = tcx.def_registry.register_adt(
        Adt {
            name: None,
            collectable: true,
            kind: AdtKind::Struct,
            generics: 0,
            variants: IndexBoxedSlice::default(),
        },
        Span::default(),
    );

    assert!(anonymous.is_ok());
    assert!(tcx.def_registry.modules[ModuleId::ZERO].items.is_empty());
}

#[test]
fn submodules_are_registered_in_their_parent() {
    let (mut tcx, _) = context();

    let module = tcx.def_registry.register_module("math", Span::default()).unwrap();

    assert_eq!(tcx.def_registry.modules[module].parent, Some(ModuleId::ZERO));
    assert_eq!(tcx.def_registry.modules[ModuleId::ZERO].get_item("math"), Some(ModuleItem::SubModule(module)));
}

#[test]
fn trait_function_offsets_skip_the_type_id_slot() {
    let (mut tcx, _) = context();
    let void = tcx.types.core_types.void;
    let this = generic(&mut tcx, 0);
    let function = |name: &str| TraitFunc {
        name: name.to_owned(),
        args: Box::new([Arg {
            name: String::from("self"),
            kind: ArgType::This,
            ty: this,
        }]),
        returns: void,
        default: None,
    };

    let shape = Trait {
        name: String::from("Shape"),
        generics: 0,
        functions: IndexVec::from_iter([function("area"), function("name")]),
    };

    assert_eq!(shape.get_func_offset("area"), Some(size_of::<usize>()));
    assert_eq!(shape.get_func_offset("name"), Some(2 * size_of::<usize>()));
    assert_eq!(shape.get_func_offset("missing"), None);
}

#[test]
fn generics_can_be_hashed() {
    let (mut tcx, _) = context();
    let first = generic(&mut tcx, 0);
    let second = generic(&mut tcx, 1);
    let i32_ty = primitive(&mut tcx, PrimitiveType::Int(IntType::I32));

    assert_ne!(tcx.types.hash_of(first), tcx.types.hash_of(second));

    let substituted = tcx.types.apply_type_args(first, &[i32_ty]);

    assert_eq!(tcx.types.hash_of(substituted), tcx.types.hash_of(i32_ty));
}

#[test]
fn diagnostics_are_collected() {
    let mut diagnostics = DiagnosticContext::default();

    assert!(diagnostics.is_empty());

    let error = diagnostics.error(TypeError::NotAssignable, ModuleSpan(ModuleId::ZERO, Span::default()));

    assert!(!diagnostics.is_empty());
    assert!(matches!(diagnostics.errors[error].error, TypeError::NotAssignable));
}
