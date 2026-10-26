use cranelift::{
    codegen::{
        cursor::{Cursor, FuncCursor},
        ir::{
            self,
            immediates::{Ieee32, Ieee64},
        },
    },
    module::FuncId,
    prelude::{FloatCC, InstBuilder},
};

pub fn canonicalize(func: &mut ir::Function, is_import: impl Fn(FuncId) -> bool) {
    let mut pos = FuncCursor::new(func);

    while pos.next_block().is_some() {
        while let Some(inst) = pos.next_inst() {
            let count = pos.func.dfg.inst_args(inst).len();
            let args = match pos.func.dfg.insts[inst] {
                // The value stored, not the address.
                ir::InstructionData::Store { .. } => 0..1,
                ir::InstructionData::Call { func_ref, .. } if calls_import(pos.func, func_ref, &is_import) => 0..count,
                ref data if data.opcode() == ir::Opcode::Bitcast => 0..1,
                ref data if data.opcode() == ir::Opcode::Return => 0..count,
                _ => continue,
            };

            for index in args {
                let value = pos.func.dfg.inst_args(inst)[index];
                let canonical = match pos.func.dfg.value_type(value) {
                    ir::types::F32 => pos.ins().f32const(Ieee32::with_bits(0x7FC0_0000)),
                    ir::types::F64 => pos.ins().f64const(Ieee64::with_bits(0x7FF8_0000_0000_0000)),
                    _ => continue,
                };
                let is_nan = pos.ins().fcmp(FloatCC::Unordered, value, value);
                let value = pos.ins().select(is_nan, canonical, value);

                pos.func.dfg.inst_args_mut(inst)[index] = value;
            }
        }
    }
}

fn calls_import(func: &ir::Function, func_ref: ir::FuncRef, is_import: impl Fn(FuncId) -> bool) -> bool {
    let ir::ExternalName::User(name) = func.dfg.ext_funcs[func_ref].name else {
        return false;
    };
    let name = &func.params.user_named_funcs()[name];

    name.namespace == 0 && is_import(FuncId::from_u32(name.index))
}
