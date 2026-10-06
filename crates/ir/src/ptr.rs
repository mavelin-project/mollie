use cranelift::{
    codegen::ir,
    prelude::{FunctionBuilder, InstBuilder, isa::TargetIsa},
};

/// A vtable is static data: the hash of the implementing type, followed by
/// pointers to the implemented functions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VTablePtr;

impl VTablePtr {
    pub fn get_type_idx(isa: &dyn TargetIsa, fn_builder: &mut FunctionBuilder, vtable_ptr: ir::Value) -> ir::Value {
        let ptr_type = isa.pointer_type();

        fn_builder.ins().load(ptr_type, ir::MemFlagsData::trusted(), vtable_ptr, 0)
    }

    pub fn get_func_ptr(isa: &dyn TargetIsa, fn_builder: &mut FunctionBuilder, vtable_ptr: ir::Value, func_idx: u32) -> ir::Value {
        let ptr_type = isa.pointer_type();

        fn_builder.ins().load(
            ptr_type,
            ir::MemFlagsData::trusted(),
            vtable_ptr,
            (ptr_type.bytes() * (func_idx + 1)).cast_signed(),
        )
    }
}
