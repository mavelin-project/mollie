use cranelift::module::Module;
use mollie_typed_ast::{ExprRef, TypedAST};
use mollie_typing::Type;

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles `[a, b, ...]`.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_array(&mut self, ast: &TypedAST, expr: ExprRef, elements: &[ExprRef]) -> CompileResult<MolValue> {
        // The expected type, e.g. an array of trait objects for a field,
        // decides the representation of elements.
        let Type::Array(element, _) = self.types()[self.resolve(ast[expr].expected_ty)] else {
            return Err(CompileError::unsupported(format!("array literal of `{}`", self.display(ast[expr].expected_ty))));
        };

        let mut values = Vec::with_capacity(elements.len());

        for &element_expr in elements {
            let value = element_expr.compile(ast, self)?;

            values.push(self.coerce(value, ast[element_expr].ty, element)?);
        }

        Ok(MolValue::Value(self.array_of(element, &values)?))
    }

    /// Compiles `target[index]`, trapping if the index is out of bounds.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_array_index(&mut self, ast: &TypedAST, expr: ExprRef, target: ExprRef, index: ExprRef) -> CompileResult<MolValue> {
        let array = target.compile(ast, self)?.value()?;
        let index = index.compile(ast, self)?.value()?;
        let element_type = self.value_type(ast[expr].ty)?;
        let addr = self.element_addr(array, index, element_type.bytes());

        Ok(self.load_value(element_type, addr, 0))
    }
}
