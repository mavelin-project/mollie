//! Limits on source code, so that untrusted programs can't crash or stall the
//! compiler: their size, how deeply they nest, and how deeply generic
//! functions instantiate each other.

/// Bytes of source of a module.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

/// How deeply expressions, blocks, types and patterns can be nested.
pub const MAX_NESTING: usize = 256;

/// How deeply string interpolations can be nested (`"${"${...}"}"`).
pub const MAX_INTERPOLATION_NESTING: usize = 32;

/// Size (in nodes of the type, like `Pair` and its arguments) of type
/// arguments of a generic instance. It bounds instantiations of generic
/// functions calling themselves with bigger types (`f<T>` calling `f<T[]>`,
/// or `f<Pair<T, T>>`, which grows exponentially).
pub const MAX_INSTANCE_TYPE_SIZE: usize = 256;

/// Instances of generic functions, types and impls used by a program.
pub const MAX_INSTANCES: usize = 20_000;

/// Calls compiled code may nest by default (see `Limits::call_depth`).
pub const DEFAULT_CALL_DEPTH: u32 = 1024;

/// Bytes of stack left on the current thread, if it can be known.
pub fn remaining_stack() -> Option<usize> {
    stacker::remaining_stack()
}

/// Stack kept free before entering a recursive step of the compiler.
const RED_ZONE: usize = 256 * 1024;
/// Stack added when it runs low.
const STACK_GROWTH: usize = 4 * 1024 * 1024;

/// Runs `f`, a recursive step over the code (parsing, type checking or
/// compiling a nested expression), on a bigger stack if the current one runs
/// low. Together with [`MAX_NESTING`], the compiler can't overflow the stack,
/// whatever thread it runs on.
pub fn grow_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_GROWTH, f)
}
