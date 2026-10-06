use cranelift::module::Module;
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{ExprRef, TypedAST};
use mollie_typing::{AdtKind, AdtVariantRef, FieldRef, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles `Adt { field: value, ... }`. Omitted fields get their default
    /// values.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_construct(
        &mut self,
        ast: &TypedAST,
        expr: ExprRef,
        variant: AdtVariantRef,
        fields: &[(FieldRef, TypeRef, ExprRef)],
    ) -> CompileResult<MolValue> {
        let ty = ast[expr].ty;
        let compiled = self.compiled_adt(ty)?;
        let type_layout = compiled.type_layout;
        let is_enum = matches!(type_layout.kind, Some(AdtKind::Enum));
        let layouts = compiled
            .variants
            .get(variant)
            .ok_or_else(|| CompileError::unsupported(format!("unknown variant of `{}`", self.display(ty))))?
            .fields
            .iter()
            .map(|(field_ref, (field, field_ty))| (field_ref, field.ty, field.offset, field.default_value.clone(), *field_ty))
            .collect::<Vec<_>>();

        let mollie_typing::Type::Adt(adt_ref, _) = self.types()[self.resolve(ty)] else {
            return Err(CompileError::unsupported(format!("constructing `{}`", self.display(ty))));
        };

        // Values are computed before allocating, so a collection can't happen
        // between the allocation and the stores.
        let mut values = Vec::with_capacity(layouts.len());

        for &(field_ref, _, _, ref default_value, field_ty) in &layouts {
            let value = if is_enum && field_ref == FieldRef::ZERO {
                // The discriminant.
                MolValue::Value(self.ptr_const(variant.index()))
            } else {
                match fields.iter().find(|(field, ..)| *field == field_ref).map(|&(.., value)| value) {
                    Some(value) if value != ExprRef::INVALID => {
                        let compiled = value.compile(ast, self)?;

                        self.coerce(compiled, ast[value].ty, field_ty)?
                    }
                    _ => {
                        if let Some(default_value) = default_value {
                            self.compile_constant(field_ty, default_value)?
                        } else {
                            let name = self.type_context.tcx.def_registry.adt_types[adt_ref].variants[variant].fields[field_ref]
                                .name
                                .clone();

                            return Err(CompileError::missing_field(name));
                        }
                    }
                }
            };

            values.push(value);
        }

        // Values of value types are built in their chunks, without an
        // allocation.
        if let Some(MollieType::Inline { size, .. }) = self.ir_type(ty)? {
            let mut chunks = self.zero_inline(size);

            for ((_, field_type, offset, ..), value) in layouts.iter().zip(&values) {
                self.insert(&mut chunks, size, offset.cast_unsigned(), *field_type, value)?;
            }

            return Ok(MolValue::Inline(chunks));
        }

        let ptr = self.alloc(type_layout);

        for ((_, field_type, offset, ..), value) in layouts.iter().zip(&values) {
            self.store_value(*field_type, value, ptr, *offset)?;
        }

        Ok(MolValue::Value(ptr))
    }
}
