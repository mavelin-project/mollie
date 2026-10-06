mod ptr;
mod ty;

use std::{fmt, mem::ManuallyDrop};

use cranelift::{
    codegen::ir,
    jit::{JITBuilder, JITModule},
    module::{DataDescription, DataId, Module, ModuleResult, default_libcall_names},
    native,
    prelude::settings,
};

pub use self::{
    ptr::VTablePtr,
    ty::{Field, Struct},
};

pub type Symbol = (&'static str, *const u8);

pub struct CodeGenerator<M: Module> {
    /// The module, released by [`CodeGenerator::release`] when the generator
    /// is dropped.
    pub module: ManuallyDrop<M>,
    pub data_desc: DataDescription,
    /// Frees the memory of the module's code and data (for JIT modules, whose
    /// memory isn't freed when they're dropped).
    release: Option<unsafe fn(M)>,
}

impl<M: Module> Drop for CodeGenerator<M> {
    fn drop(&mut self) {
        // SAFETY: the module isn't used after this.
        let module = unsafe { ManuallyDrop::take(&mut self.module) };

        match self.release {
            // SAFETY: the owner of the generator (the compiler) is dropped, so
            // nothing can call its code anymore.
            Some(release) => unsafe { release(module) },
            None => drop(module),
        }
    }
}

/// Frees the code and data of a JIT module.
///
/// # Safety
///
/// Nothing may use its code or data afterwards.
unsafe fn release_jit(module: JITModule) {
    unsafe { module.free_memory() };
}

impl<M: Module> fmt::Debug for CodeGenerator<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeGenerator").field("data_desc", &self.data_desc).finish_non_exhaustive()
    }
}

impl CodeGenerator<JITModule> {
    /// # Panics
    ///
    /// Panics if the host machine isn't supported by Cranelift.
    pub fn new<I: IntoIterator<Item = Symbol>>(symbols: I, flags: settings::Flags) -> Self {
        let isa = native::builder()
            .expect("host machine is not supported")
            .finish(flags)
            .expect("invalid ISA flags");
        let mut builder = JITBuilder::with_isa(isa, default_libcall_names());

        for (name, ptr) in symbols {
            builder.symbol(name, ptr);
        }

        Self {
            module: ManuallyDrop::new(JITModule::new(builder)),
            data_desc: DataDescription::new(),
            release: Some(release_jit),
        }
    }
}

impl<M: Module> CodeGenerator<M> {
    /// Defines read-only data with the given contents.
    ///
    /// # Errors
    ///
    /// Returns an error if the data can't be declared or defined.
    pub fn static_data<T: Into<Box<[u8]>>>(&mut self, data: T) -> ModuleResult<DataId> {
        self.data_desc.define(data.into());

        let id = self.module.declare_anonymous_data(false, false)?;
        let result = self.module.define_data(id, &self.data_desc);

        self.data_desc.clear();

        result.map(|()| id)
    }

    /// Defines writable zeroed data of the given size.
    ///
    /// # Errors
    ///
    /// Returns an error if the data can't be declared or defined.
    pub fn static_zeroed(&mut self, size: usize) -> ModuleResult<DataId> {
        self.data_desc.define_zeroinit(size);

        let id = self.module.declare_anonymous_data(true, false)?;
        let result = self.module.define_data(id, &self.data_desc);

        self.data_desc.clear();

        result.map(|()| id)
    }
}

/// How a Mollie value is represented in Cranelift IR: a single value, a pair
/// of values (a pointer and its metadata), or the bytes of an inline value
/// (a value type) in integer chunks (see [`chunks`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MollieType {
    Regular(ir::Type),
    Fat(ir::Type, ir::Type),
    /// A value of a value type, with its size and alignment in memory.
    Inline {
        size: u32,
        align: u32,
    },
}

/// Chunks of the bytes of an inline value of `size` bytes.
///
/// They're integer types and their offsets, `I64`s first, then smaller chunks
/// for the rest. Every chunk
/// is aligned to its size (relative to the value), so pointers (8 bytes,
/// 8-aligned) are always whole chunks.
pub fn chunks(size: u32) -> Vec<(ir::Type, u32)> {
    let mut chunks = Vec::new();
    let mut offset = 0;

    while offset < size {
        let chunk = [8, 4, 2, 1]
            .into_iter()
            .find(|&chunk| chunk <= size - offset && offset % chunk == 0)
            .unwrap_or(1);
        let ty = match chunk {
            8 => ir::types::I64,
            4 => ir::types::I32,
            2 => ir::types::I16,
            _ => ir::types::I8,
        };

        chunks.push((ty, offset));
        offset += chunk;
    }

    chunks
}

impl MollieType {
    pub fn bytes(&self) -> u32 {
        match self {
            Self::Regular(ty) => ty.bytes(),
            Self::Fat(ty, metadata_ty) => ty.bytes() + metadata_ty.bytes(),
            Self::Inline { size, .. } => *size,
        }
    }

    /// Alignment of the value in memory, which is the alignment of its largest
    /// component.
    pub fn align(&self) -> u32 {
        match self {
            Self::Regular(ty) => ty.bytes(),
            Self::Fat(ty, metadata_ty) => {
                if ty.bytes() > metadata_ty.bytes() {
                    ty.bytes()
                } else {
                    metadata_ty.bytes()
                }
            }
            Self::Inline { align, .. } => *align,
        }
    }

    /// Types of the IR values of the value, in order.
    pub fn components(&self) -> Vec<ir::Type> {
        match *self {
            Self::Regular(ty) => vec![ty],
            Self::Fat(ty, metadata_ty) => vec![ty, metadata_ty],
            Self::Inline { size, .. } => chunks(size).into_iter().map(|(ty, _)| ty).collect(),
        }
    }

    pub fn add_to_params(self, params: &mut Vec<ir::AbiParam>) {
        params.extend(self.components().into_iter().map(ir::AbiParam::new));
    }

    /// Returns `true` if the mollie type is [`Fat`].
    ///
    /// [`Fat`]: MollieType::Fat
    #[must_use]
    pub const fn is_fat(&self) -> bool {
        matches!(self, Self::Fat(..))
    }
}

impl Default for MollieType {
    fn default() -> Self {
        Self::Regular(ir::Type::default())
    }
}
