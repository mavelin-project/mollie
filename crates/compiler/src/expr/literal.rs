use cranelift::{codegen::ir, module::Module, prelude::InstBuilder};
use mollie_const::ConstantValue;
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{ExprRef, LitExpr, TypedAST};
use mollie_typing::{AdtKind, AdtVariantRef, FieldRef, Type, TypeRef};

use crate::{
    MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles a literal.
    ///
    /// # Errors
    ///
    /// Returns an error if a string literal can't be stored with the code.
    pub fn compile_literal(&mut self, ast: &TypedAST, expr: ExprRef, literal: &LitExpr) -> CompileResult<MolValue> {
        Ok(match literal {
            &LitExpr::Int(value) => match self.value_type(ast[expr].ty)? {
                MollieType::Regular(ty) if ty.is_int() => MolValue::Value(self.iconst(ty, value)),
                ty => return Err(CompileError::unsupported(format!("integer literal of {ty:?}"))),
            },
            &LitExpr::F32(value) => MolValue::Value(self.fn_builder.ins().f32const(value)),
            &LitExpr::Bool(value) => MolValue::Value(self.iconst(ir::types::I8, i64::from(value))),
            LitExpr::String(value) => self.string(value)?,
        })
    }

    /// A string constant: a pointer to a static string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string can't be stored with the code.
    pub fn string(&mut self, value: &str) -> CompileResult<MolValue> {
        let data_id = self.compiler.string_object(value)?;

        Ok(MolValue::Value(self.data_addr(data_id)))
    }

    /// Compiles a constant (a default value of a field) of type `ty`.
    ///
    /// # Errors
    ///
    /// Returns an error if the constant isn't a value of `ty`.
    pub fn compile_constant(&mut self, ty: TypeRef, constant: &ConstantValue) -> CompileResult<MolValue> {
        let ptr_type = self.ptr_type();

        Ok(match constant {
            &ConstantValue::I8(value) => MolValue::Value(self.iconst(ir::types::I8, i64::from(value))),
            &ConstantValue::U8(value) => MolValue::Value(self.iconst(ir::types::I8, i64::from(value))),
            &ConstantValue::I16(value) => MolValue::Value(self.iconst(ir::types::I16, i64::from(value))),
            &ConstantValue::U16(value) => MolValue::Value(self.iconst(ir::types::I16, i64::from(value))),
            &ConstantValue::I32(value) => MolValue::Value(self.iconst(ir::types::I32, i64::from(value))),
            &ConstantValue::U32(value) => MolValue::Value(self.iconst(ir::types::I32, i64::from(value))),
            &ConstantValue::I64(value) => MolValue::Value(self.iconst(ir::types::I64, value)),
            // The bits are the same, whatever the signedness.
            &ConstantValue::U64(value) => MolValue::Value(self.iconst(ir::types::I64, value.cast_signed())),
            &ConstantValue::ISize(value) => MolValue::Value(self.iconst(ptr_type, value as i64)),
            &ConstantValue::USize(value) => MolValue::Value(self.iconst(ptr_type, value.cast_signed() as i64)),
            &ConstantValue::F32(value) => MolValue::Value(self.fn_builder.ins().f32const(value)),
            &ConstantValue::Bool(value) => MolValue::Value(self.iconst(ir::types::I8, i64::from(value))),
            ConstantValue::String(value) => self.string(value)?,
            ConstantValue::Array(values) => {
                let Type::Array(element, _) = self.types()[self.resolve(ty)] else {
                    return Err(CompileError::unsupported(format!("array constant of `{}`", self.display(ty))));
                };

                let values = values
                    .iter()
                    .map(|value| self.compile_constant(element, value))
                    .collect::<CompileResult<Vec<_>>>()?;

                let array = MolValue::Value(self.array_of(element, &values)?);

                self.track(ty, &array);

                array
            }
            ConstantValue::Construct { variant, fields, .. } => {
                let variant = AdtVariantRef::new(*variant);
                let compiled = self.compiled_adt(ty)?;
                let type_layout = compiled.type_layout;
                let is_enum = matches!(type_layout.kind, Some(AdtKind::Enum));
                let layouts = compiled
                    .variants
                    .get(variant)
                    .ok_or_else(|| CompileError::unsupported("constant of an unknown variant"))?
                    .fields
                    .iter()
                    .map(|(field_ref, (field, field_ty))| (field_ref, field.ty, field.offset, field.default_value.clone(), *field_ty))
                    .collect::<Vec<_>>();

                // Constants of value types are built in their chunks.
                if let Some(MollieType::Inline { size, .. }) = self.ir_type(ty)? {
                    let mut chunks = self.zero_inline(size);

                    for (field_ref, field_type, offset, default_value, field_ty) in layouts {
                        let value = if is_enum && field_ref == FieldRef::ZERO {
                            MolValue::Value(self.ptr_const(variant.index()))
                        } else {
                            let value = fields
                                .iter()
                                .find_map(|(index, value)| if *index == field_ref.index() { value.as_ref() } else { None })
                                .or(default_value.as_ref())
                                .ok_or_else(|| CompileError::missing_field(format!("#{}", field_ref.index())))?;

                            self.compile_constant(field_ty, value)?
                        };

                        self.insert(&mut chunks, size, offset.cast_unsigned(), field_type, &value)?;
                    }

                    return Ok(MolValue::Inline(chunks));
                }

                let ptr = self.alloc(type_layout);

                // Fields may allocate, while the object is only referenced
                // here.
                self.track(ty, &MolValue::Value(ptr));

                for (field_ref, field_type, offset, default_value, field_ty) in layouts {
                    let value = if is_enum && field_ref == FieldRef::ZERO {
                        MolValue::Value(self.ptr_const(variant.index()))
                    } else {
                        let value = fields
                            .iter()
                            .find_map(|(index, value)| if *index == field_ref.index() { value.as_ref() } else { None })
                            .or(default_value.as_ref())
                            .ok_or_else(|| CompileError::missing_field(format!("#{}", field_ref.index())))?;

                        self.compile_constant(field_ty, value)?
                    };

                    self.store_value(field_type, &value, ptr, offset)?;
                }

                MolValue::Value(ptr)
            }
            ConstantValue::Nothing => MolValue::Nothing,
        })
    }
}
