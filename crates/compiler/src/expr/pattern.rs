use cranelift::{
    codegen::ir,
    module::Module,
    prelude::{InstBuilder, IntCC},
};
use mollie_index::Idx;
use mollie_ir::{MollieType, VTablePtr};
use mollie_shared::Operator;
use mollie_typed_ast::{ExprRef, IsPattern, SolvedPass, TypedAST};
use mollie_typing::{AdtKind, Type, TypeRef};

use crate::{
    CompileTypedAST, MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

impl<M: Module> FunctionCompiler<'_, M> {
    /// Compiles `target is pattern`, which binds variables of the pattern in
    /// the current scope.
    ///
    /// # Errors
    ///
    /// Returns an error if the code uses something the compiler doesn't
    /// support, or a type wasn't compiled.
    pub fn compile_is_pattern(&mut self, ast: &TypedAST, target: ExprRef, pattern: &IsPattern<SolvedPass>) -> CompileResult<MolValue> {
        let value = target.compile(ast, self)?;

        self.compile_pattern(ast, &value, ast[target].ty, pattern).map(MolValue::Value)
    }

    /// Checks whether `value` of type `ty` matches `pattern`, binding its
    /// variables. Returns the result of the check.
    fn compile_pattern(&mut self, ast: &TypedAST, value: &MolValue, ty: TypeRef, pattern: &IsPattern<SolvedPass>) -> CompileResult<ir::Value> {
        // Nested code is handled recursively: the stack grows if needed.
        mollie_shared::limits::grow_stack(move || {
            match pattern {
                &IsPattern::Literal(literal) => {
                    let literal_value = literal.compile(ast, self)?;

                    self.bin_op(value, ty, Operator::Equal, &literal_value)
                }
                IsPattern::Wildcard => Ok(self.iconst(ir::types::I8, 1)),
                IsPattern::Binding { name, ty: binding_ty } => {
                    self.declare_binding(name.clone(), *binding_ty, value.clone())?;

                    Ok(self.iconst(ir::types::I8, 1))
                }
                IsPattern::EnumVariant { adt, adt_variant, values, .. } => {
                    let ptr_type = self.ptr_type();
                    // Structs always match, variants of enums are checked by
                    // their discriminant, the first field
                    // of every variant.
                    let is_variant = if self.type_context.tcx.def_registry.adt_types[*adt].kind == AdtKind::Enum {
                        let discriminant = self.read_field(value, ty, MollieType::Regular(ptr_type), 0)?.value()?;
                        let expected = i64::try_from(adt_variant.index()).map_err(|_| CompileError::unsupported("too many variants"))?;

                        self.fn_builder.ins().icmp_imm_u(IntCC::Equal, discriminant, expected)
                    } else {
                        self.iconst(ir::types::I8, 1)
                    };

                    if values.is_empty() {
                        return Ok(is_variant);
                    }

                    let fields_block = self.fn_builder.create_block();
                    let after_block = self.fn_builder.create_block();
                    let result = self.fn_builder.append_block_param(after_block, ir::types::I8);

                    self.fn_builder
                        .ins()
                        .brif(is_variant, fields_block, &[], after_block, &[ir::BlockArg::Value(is_variant)]);
                    self.fn_builder.switch_to_block(fields_block);
                    self.fn_builder.seal_block(fields_block);

                    // The value matches if every nested pattern matches.
                    let mut matches = is_variant;

                    for (field, name, nested) in values {
                        let (field_type, offset, field_ty) = self.field_layout(ty, *adt_variant, *field)?;
                        let field_value = self.read_field(value, ty, field_type, offset)?;

                        match nested {
                            Some(nested) => {
                                let nested_matches = self.compile_pattern(ast, &field_value, field_ty, nested)?;

                                matches = self.fn_builder.ins().band(matches, nested_matches);
                            }
                            None => self.declare_binding(name.clone(), field_ty, field_value)?,
                        }
                    }

                    self.fn_builder.ins().jump(after_block, &[ir::BlockArg::Value(matches)]);
                    self.fn_builder.switch_to_block(after_block);
                    self.fn_builder.seal_block(after_block);

                    Ok(result)
                }
                &IsPattern::TypeName { ty: expected, ref name } => {
                    let expected_hash = self.hash(expected);
                    let is_trait_object = matches!(self.types()[self.resolve(ty)], Type::Trait(..));

                    match value {
                        // Trait objects carry the hash of the type of the value in their vtable.
                        &MolValue::FatPtr(ptr, vtable) if is_trait_object => {
                            let ptr_type = self.ptr_type();
                            let type_hash = VTablePtr::get_type_idx(self.compiler.codegen.module.isa(), &mut self.fn_builder, vtable);
                            let expected_hash = self.iconst(ptr_type, expected_hash.cast_signed());
                            let is_type = self.fn_builder.ins().icmp(IntCC::Equal, type_hash, expected_hash);

                            // The binding is only used where the check
                            // succeeded.
                            // Values of value types are copied out of their
                            // box, only then: the
                            // box may be another, smaller value.
                            let binding = match self.value_type(expected)? {
                                MollieType::Inline { size, .. } => {
                                    let load_block = self.fn_builder.create_block();
                                    let after_block = self.fn_builder.create_block();
                                    let zeroes = self.zero_inline(size);
                                    let results = mollie_ir::chunks(size)
                                        .into_iter()
                                        .map(|(chunk_ty, _)| self.fn_builder.append_block_param(after_block, chunk_ty))
                                        .collect::<Vec<_>>();

                                    self.fn_builder.ins().brif(
                                        is_type,
                                        load_block,
                                        &[],
                                        after_block,
                                        &zeroes.into_iter().map(ir::BlockArg::Value).collect::<Vec<_>>(),
                                    );
                                    self.fn_builder.switch_to_block(load_block);
                                    self.fn_builder.seal_block(load_block);

                                    let loaded = self.load_inline(size, ptr, 0);

                                    self.fn_builder
                                        .ins()
                                        .jump(after_block, &loaded.into_iter().map(ir::BlockArg::Value).collect::<Vec<_>>());
                                    self.fn_builder.switch_to_block(after_block);
                                    self.fn_builder.seal_block(after_block);

                                    MolValue::Inline(results)
                                }
                                _ => MolValue::Value(ptr),
                            };

                            self.declare_binding(name.clone(), expected, binding)?;

                            Ok(is_type)
                        }
                        // The type of other values is known statically.
                        _ => {
                            let is_type = self.hash(ty) == expected_hash;

                            if is_type {
                                self.declare_binding(name.clone(), expected, value.clone())?;
                            }

                            Ok(self.iconst(ir::types::I8, i64::from(is_type)))
                        }
                    }
                }
            }
        })
    }
}
