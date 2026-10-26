use cranelift::{codegen::ir, module::Module, prelude::InstBuilder};
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typed_ast::{ExprRef, TypedAST};
use mollie_typing::{AdtVariantRef, ArrayKind, FieldRef, Type, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    pub fn compile_array(&mut self, ast: &TypedAST, expr: ExprRef, elements: &[ExprRef]) -> CompileResult<MolValue> {
        // The expected type, e.g. an array of trait objects for a field,
        // decides the representation of elements. A literal given as a view
        // is a fixed array of its own type.
        let expected = ast[expr].expected_ty;
        let array_ty = if self.fixed_length(expected).is_none() && self.fixed_length(ast[expr].ty).is_some() {
            ast[expr].ty
        } else {
            expected
        };
        let Type::Array(element, _) = self.types()[self.resolve(array_ty)] else {
            return Err(CompileError::unsupported(format!("array literal of `{}`", self.display(array_ty))));
        };

        let mut values = Vec::with_capacity(elements.len());

        for &element_expr in elements {
            let value = element_expr.compile(ast, self)?;

            values.push(self.coerce(value, ast[element_expr].ty, element)?);
        }

        if self.fixed_length(array_ty).is_some() {
            return self.fixed_array_of(array_ty, &values);
        }

        Ok(MolValue::Value(self.array_of(element, &values)?))
    }

    pub fn compile_array_index(&mut self, ast: &TypedAST, expr: ExprRef, target: ExprRef, index: ExprRef) -> CompileResult<MolValue> {
        let element_type = self.value_type(ast[expr].ty)?;

        // Elements of fixed arrays are parts of the array's value.
        if self.fixed_length(ast[target].ty).is_some() {
            let place = self
                .place(ast, expr)?
                .ok_or_else(|| CompileError::unsupported("element of a fixed array without a place"))?;

            return self.read_place(&place, element_type);
        }

        let array = target.compile(ast, self)?.value()?;
        let index = index.compile(ast, self)?.value()?;
        let addr = self.element_addr(array, index, element_type.bytes());

        Ok(self.load_value(element_type, addr, 0))
    }

    pub fn fixed_length(&self, ty: TypeRef) -> Option<usize> {
        let Type::Array(_, ArrayKind::Fixed(size)) = self.types()[self.resolve(ty)] else {
            return None;
        };

        match self.types()[self.resolve(size)] {
            Type::Size(length) => Some(length),
            _ => None,
        }
    }

    pub fn fixed_array_of(&mut self, ty: TypeRef, values: &[MolValue]) -> CompileResult<MolValue> {
        let Some(MollieType::Inline { size, align }) = self.ir_type(ty)? else {
            return Err(CompileError::unsupported(format!("`{}` isn't a fixed array", self.display(ty))));
        };

        let slot = self.stack_slot(size, align);

        for (index, value) in values.iter().enumerate() {
            let (element_type, offset, _) = self.field_layout(ty, AdtVariantRef::ZERO, FieldRef::new(index))?;

            self.store_value(element_type, value, slot, offset)?;
        }

        let array = MolValue::Inline(self.load_inline(size, slot, 0));

        self.track(ty, &array);

        Ok(array)
    }

    pub fn stack_slot(&mut self, size: u32, align: u32) -> ir::Value {
        let slot = self.fn_builder.create_sized_stack_slot(ir::StackSlotData::new(
            ir::StackSlotKind::ExplicitSlot,
            size.max(1),
            u8::try_from(align.max(1).trailing_zeros()).unwrap_or(0),
        ));
        let ptr_type = self.ptr_type();

        self.fn_builder.ins().stack_addr(ptr_type, slot, 0)
    }
}
