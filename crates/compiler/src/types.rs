use std::hash::Hasher;

use cranelift::{
    codegen::ir,
    prelude::{isa::TargetIsa, types},
};
use indexmap::IndexMap;
use mollie_ir::MollieType;
use mollie_typing::{ArrayKind, IntType, PrimitiveType, TyCtxt, Type, TypeRef, UIntType};

use crate::{
    CompiledAdt,
    allocator::{TypeLayout, TypeLayoutField},
    error::{CompileError, CompileResult},
};

pub fn resolve(tcx: &TyCtxt, ty: TypeRef, generics: &[TypeRef]) -> TypeRef {
    match tcx.types[ty] {
        Type::Generic(index) => generics.get(index).copied().unwrap_or(ty),
        _ => ty,
    }
}

pub fn ir_type(tcx: &TyCtxt, adts: &AdtLayouts, ty: TypeRef, generics: &[TypeRef], isa: &dyn TargetIsa) -> CompileResult<Option<MollieType>> {
    let ptr = isa.pointer_type();

    if let Some(layout) = value_layout(tcx, adts, ty, generics)? {
        return Ok(Some(MollieType::Inline {
            size: u32::try_from(layout.size).map_err(|_| CompileError::unsupported("value type is too large"))?,
            align: u32::try_from(layout.align).map_err(|_| CompileError::unsupported("value type is too large"))?,
        }));
    }

    Ok(Some(match &tcx.types[resolve(tcx, ty, generics)] {
        Type::Primitive(primitive) => match primitive {
            PrimitiveType::Void => return Ok(None),
            PrimitiveType::Any => MollieType::Fat(ptr, ptr),
            PrimitiveType::String | PrimitiveType::Int(IntType::ISize) | PrimitiveType::UInt(UIntType::USize) => MollieType::Regular(ptr),
            PrimitiveType::Int(IntType::I64) | PrimitiveType::UInt(UIntType::U64) => MollieType::Regular(types::I64),
            PrimitiveType::Int(IntType::I32) | PrimitiveType::UInt(UIntType::U32) => MollieType::Regular(types::I32),
            PrimitiveType::Int(IntType::I16) | PrimitiveType::UInt(UIntType::U16) => MollieType::Regular(types::I16),
            PrimitiveType::Int(IntType::I8) | PrimitiveType::UInt(UIntType::U8) | PrimitiveType::Bool => MollieType::Regular(types::I8),
            PrimitiveType::F32 => MollieType::Regular(types::F32),
            PrimitiveType::F64 => MollieType::Regular(types::F64),
        },
        Type::Array(..) | Type::Adt(..) => MollieType::Regular(ptr),
        Type::Trait(..) | Type::Func(..) => MollieType::Fat(ptr, ptr),
        &Type::Generic(index) => return Err(CompileError::unsupported(format!("unresolved generic parameter #{index}"))),
        // Sizes of fixed arrays are only parts of their types.
        &Type::Size(size) => return Err(CompileError::unsupported(format!("size `{size}` used as a type of values"))),
        Type::Error => return Err(CompileError::unsupported("erroneous type")),
    }))
}

pub type AdtLayouts = IndexMap<u64, CompiledAdt>;

pub fn value_layout(tcx: &TyCtxt, adts: &AdtLayouts, ty: TypeRef, generics: &[TypeRef]) -> CompileResult<Option<&'static TypeLayout>> {
    let resolved = resolve(tcx, ty, generics);

    if !tcx.is_value_type(resolved) {
        return Ok(None);
    }

    adts.get(&tcx.types.hash_of_instance(ty, generics))
        .map(|compiled| Some(compiled.type_layout))
        .ok_or_else(|| CompileError::unsupported(format!("value type `{}` wasn't compiled", tcx.display_of(resolved))))
}

pub fn layout_fields(tcx: &TyCtxt, adts: &AdtLayouts, ty: TypeRef, generics: &[TypeRef]) -> CompileResult<Vec<(u32, MollieType, TypeLayoutField)>> {
    if let Some(layout) = value_layout(tcx, adts, ty, generics)? {
        let mut fields = layout.fields.iter().map(|&(_, offset, ty, kind)| (offset, ty, kind)).collect::<Vec<_>>();

        fields.sort_unstable_by_key(|&(offset, ..)| offset);
        fields.dedup();

        return Ok(fields);
    }

    let pointer = MollieType::Regular(types::I64);
    let fat = MollieType::Fat(types::I64, types::I64);

    Ok(match &tcx.types[resolve(tcx, ty, generics)] {
        &Type::Adt(adt_ref, _) if tcx.def_registry.adt_types[adt_ref].collectable => vec![(0, pointer, TypeLayoutField::Collectable)],
        Type::Array(..) | Type::Primitive(PrimitiveType::String) => vec![(0, pointer, TypeLayoutField::Collectable)],
        // The first word of a trait object is the pointer to the value.
        Type::Trait(..) => vec![(0, fat, TypeLayoutField::Collectable)],
        Type::Func(..) => vec![(0, fat, TypeLayoutField::FuncEnv)],
        _ => Vec::new(),
    })
}

pub fn inline_pointers(tcx: &TyCtxt, adts: &AdtLayouts, ty: TypeRef, generics: &[TypeRef]) -> CompileResult<Vec<u32>> {
    let mut pointers = layout_fields(tcx, adts, ty, generics)?
        .into_iter()
        .map(|(offset, _, kind)| if kind == TypeLayoutField::FuncEnv { offset + 8 } else { offset })
        .collect::<Vec<_>>();

    pointers.sort_unstable();
    pointers.dedup();

    Ok(pointers)
}

pub fn instance_hash(tcx: &TyCtxt, type_args: &[TypeRef], generics: &[TypeRef]) -> u64 {
    let mut state = tcx.types.hasher();

    for &arg in type_args {
        tcx.types.hash_instance_into(&mut state, arg, generics);
    }

    state.finish()
}

pub fn is_concrete(tcx: &TyCtxt, ty: TypeRef) -> bool {
    match &tcx.types[ty] {
        Type::Primitive(_) | Type::Size(_) => true,
        &Type::Array(element, kind) => is_concrete(tcx, element) && !matches!(kind, ArrayKind::Fixed(size) if !is_concrete(tcx, size)),
        Type::Adt(_, args) | Type::Trait(_, args) => args.iter().all(|&arg| is_concrete(tcx, arg)),
        Type::Func(args, returns) => args.iter().all(|&arg| is_concrete(tcx, arg)) && is_concrete(tcx, *returns),
        Type::Generic(_) | Type::Error => false,
    }
}

pub fn signature(
    tcx: &TyCtxt,
    adts: &AdtLayouts,
    func_ty: TypeRef,
    generics: &[TypeRef],
    isa: &dyn TargetIsa,
    mut signature: ir::Signature,
    with_env: bool,
) -> CompileResult<ir::Signature> {
    let Type::Func(args, returns) = &tcx.types[resolve(tcx, func_ty, generics)] else {
        return Err(CompileError::unsupported(format!("`{}` is not a function type", tcx.display_of(func_ty))));
    };

    for &arg in args {
        if let Some(ty) = ir_type(tcx, adts, arg, generics, isa)? {
            ty.add_to_params(&mut signature.params);
        }
    }

    if with_env {
        signature.params.push(ir::AbiParam::new(isa.pointer_type()));
    }

    if let Some(ty) = ir_type(tcx, adts, *returns, generics, isa)? {
        ty.add_to_params(&mut signature.returns);
    }

    Ok(signature)
}
