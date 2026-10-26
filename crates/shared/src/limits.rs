pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;

pub const MAX_NESTING: usize = 256;

pub const MAX_INTERPOLATION_NESTING: usize = 32;

pub const MAX_INSTANCE_TYPE_SIZE: usize = 256;

pub const MAX_INSTANCES: usize = 20_000;

pub const DEFAULT_CALL_DEPTH: u32 = 1024;

pub fn remaining_stack() -> Option<usize> {
    stacker::remaining_stack()
}

const RED_ZONE: usize = 256 * 1024;
const STACK_GROWTH: usize = 4 * 1024 * 1024;

pub fn grow_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, STACK_GROWTH, f)
}
