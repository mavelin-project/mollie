use cranelift::module::Module;
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{ExprRef, TypedAST};
use mollie_typing::{AdtVariantRef, FieldRef, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles `target.field`.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_field_access(&mut self, ast: &TypedAST, target: ExprRef, field: FieldRef) -> CompileResult<MolValue> {
        let target_value = target.compile(ast, self)?;
        let (field_type, offset, _) = self.field_layout(ast[target].ty, AdtVariantRef::ZERO, field)?;

        self.read_field(&target_value, ast[target].ty, field_type, offset)
    }

    /// The field of representation `field_type` at `offset` of `value` (of
    /// the ADT type `ty`): loaded from the object, or taken from the inline
    /// value of a value type.
    ///
    /// # Errors
    ///
    /// Returns an error if `value` isn't a value of an ADT.
    pub fn read_field(&mut self, value: &MolValue, ty: TypeRef, field_type: MollieType, offset: i32) -> CompileResult<MolValue> {
        match value {
            MolValue::Inline(values) => {
                let Some(MollieType::Inline { size, .. }) = self.ir_type(ty)? else {
                    return Err(CompileError::unsupported("inline value of a type that isn't a value type"));
                };

                Ok(self.extract(values, size, offset.cast_unsigned(), field_type))
            }
            value => {
                let ptr = value.value()?;

                Ok(self.load_value(field_type, ptr, offset))
            }
        }
    }
}
