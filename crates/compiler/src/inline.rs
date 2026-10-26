use std::cmp::Ordering;

use cranelift::{codegen::ir, module::Module, prelude::InstBuilder};
use mollie_ir::{MollieType, chunks};

use crate::{
    MolValue,
    error::{CompileError, CompileResult},
    func::FunctionCompiler,
};

const fn int_type(bytes: u32) -> ir::Type {
    match bytes {
        1 => ir::types::I8,
        2 => ir::types::I16,
        4 => ir::types::I32,
        _ => ir::types::I64,
    }
}

const fn low_bits(bits: u32) -> i64 {
    if bits >= 64 { -1 } else { ((1u64 << bits) - 1).cast_signed() }
}

const fn chunk_flags() -> ir::MemFlagsData {
    ir::MemFlagsData::new().with_notrap()
}

impl<M: Module> FunctionCompiler<'_, M> {
    fn resize(&mut self, value: ir::Value, from: ir::Type, to: ir::Type) -> ir::Value {
        match from.bits().cmp(&to.bits()) {
            Ordering::Greater => self.fn_builder.ins().ireduce(to, value),
            Ordering::Less => self.fn_builder.ins().uextend(to, value),
            Ordering::Equal => value,
        }
    }

    fn read_bits(&mut self, values: &[ir::Value], size: u32, offset: u32, bytes: u32) -> ir::Value {
        let ty = int_type(bytes);
        let mut result = None;

        for ((chunk_ty, chunk_offset), &chunk) in chunks(size).into_iter().zip(values) {
            let start = offset.max(chunk_offset);
            let end = (offset + bytes).min(chunk_offset + chunk_ty.bytes());

            if start >= end {
                continue;
            }

            // Bits of the chunk from `start`, moved to their place in the
            // result.
            let mut piece = chunk;
            let shift = (start - chunk_offset) * 8;

            if shift > 0 {
                piece = self.fn_builder.ins().ushr_imm_u(piece, i64::from(shift));
            }

            piece = self.resize(piece, chunk_ty, ty);

            let width = (end - start) * 8;

            if width < ty.bits() {
                piece = self.fn_builder.ins().band_imm_u(piece, low_bits(width));
            }

            let place = (start - offset) * 8;

            if place > 0 {
                piece = self.fn_builder.ins().ishl_imm_u(piece, i64::from(place));
            }

            result = Some(match result {
                Some(result) => self.fn_builder.ins().bor(result, piece),
                None => piece,
            });
        }

        result.unwrap_or_else(|| self.fn_builder.ins().iconst(ty, 0))
    }

    fn write_bits(&mut self, values: &mut [ir::Value], size: u32, offset: u32, bytes: u32, value: ir::Value) {
        let ty = int_type(bytes);

        for ((chunk_ty, chunk_offset), chunk) in chunks(size).into_iter().zip(values.iter_mut()) {
            let start = offset.max(chunk_offset);
            let end = (offset + bytes).min(chunk_offset + chunk_ty.bytes());

            if start >= end {
                continue;
            }

            // The part of `value` going into this chunk, at its place in the
            // chunk.
            let mut piece = value;
            let from = (start - offset) * 8;

            if from > 0 {
                piece = self.fn_builder.ins().ushr_imm_u(piece, i64::from(from));
            }

            piece = self.resize(piece, ty, chunk_ty);

            let width = (end - start) * 8;

            if width >= chunk_ty.bits() {
                *chunk = piece;

                continue;
            }

            piece = self.fn_builder.ins().band_imm_u(piece, low_bits(width));

            let place = (start - chunk_offset) * 8;

            if place > 0 {
                piece = self.fn_builder.ins().ishl_imm_u(piece, i64::from(place));
            }

            let kept = !(low_bits(width) << place);
            let cleared = self.fn_builder.ins().band_imm_u(*chunk, kept);

            *chunk = self.fn_builder.ins().bor(cleared, piece);
        }
    }

    fn read_scalar(&mut self, values: &[ir::Value], size: u32, offset: u32, ty: ir::Type) -> ir::Value {
        let bits = self.read_bits(values, size, offset, ty.bytes());

        if ty.is_float() {
            self.fn_builder.ins().bitcast(ty, ir::MemFlagsData::new(), bits)
        } else {
            bits
        }
    }

    fn write_scalar(&mut self, values: &mut [ir::Value], size: u32, offset: u32, ty: ir::Type, value: ir::Value) {
        let bits = if ty.is_float() {
            self.fn_builder.ins().bitcast(int_type(ty.bytes()), ir::MemFlagsData::new(), value)
        } else {
            value
        };

        self.write_bits(values, size, offset, ty.bytes(), bits);
    }

    pub fn extract(&mut self, values: &[ir::Value], size: u32, offset: u32, ty: MollieType) -> MolValue {
        match ty {
            MollieType::Regular(ty) => MolValue::Value(self.read_scalar(values, size, offset, ty)),
            MollieType::Fat(ty, metadata_ty) => MolValue::FatPtr(
                self.read_scalar(values, size, offset, ty),
                self.read_scalar(values, size, offset + ty.bytes(), metadata_ty),
            ),
            MollieType::Inline { size: inner, .. } => MolValue::Inline(
                chunks(inner)
                    .into_iter()
                    .map(|(chunk_ty, chunk_offset)| self.read_bits(values, size, offset + chunk_offset, chunk_ty.bytes()))
                    .collect(),
            ),
        }
    }

    pub fn insert(&mut self, values: &mut [ir::Value], size: u32, offset: u32, ty: MollieType, value: &MolValue) -> CompileResult<()> {
        match (ty, value) {
            (MollieType::Regular(ty), &MolValue::Value(value)) => self.write_scalar(values, size, offset, ty, value),
            (MollieType::Fat(ty, metadata_ty), &MolValue::FatPtr(value, metadata)) => {
                self.write_scalar(values, size, offset, ty, value);
                self.write_scalar(values, size, offset + ty.bytes(), metadata_ty, metadata);
            }
            (MollieType::Inline { size: inner, .. }, MolValue::Inline(inner_values)) => {
                for ((chunk_ty, chunk_offset), &chunk) in chunks(inner).into_iter().zip(inner_values.iter()) {
                    self.write_bits(values, size, offset + chunk_offset, chunk_ty.bytes(), chunk);
                }
            }
            (ty, value) => return Err(CompileError::unsupported(format!("can't put {value:?} into an inline value as {ty:?}"))),
        }

        Ok(())
    }

    pub fn zero_inline(&mut self, size: u32) -> Vec<ir::Value> {
        chunks(size).into_iter().map(|(ty, _)| self.fn_builder.ins().iconst(ty, 0)).collect()
    }

    pub fn load_inline(&mut self, size: u32, ptr: ir::Value, offset: i32) -> Vec<ir::Value> {
        chunks(size)
            .into_iter()
            .map(|(ty, chunk_offset)| self.fn_builder.ins().load(ty, chunk_flags(), ptr, offset + chunk_offset.cast_signed()))
            .collect()
    }

    pub fn store_inline(&mut self, size: u32, values: &[ir::Value], ptr: ir::Value, offset: i32) {
        for ((_, chunk_offset), &value) in chunks(size).into_iter().zip(values) {
            self.fn_builder.ins().store(chunk_flags(), value, ptr, offset + chunk_offset.cast_signed());
        }
    }
}
