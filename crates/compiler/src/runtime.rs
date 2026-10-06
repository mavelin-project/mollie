//! Host functions called by compiled code. They use the C calling
//! convention, which is what Cranelift generates calls for.

use mollie_ir::Symbol;

use crate::{allocator, math, sandbox, strings};

/// A value of a built-in function of the runtime.
#[derive(Debug, Clone, Copy)]
pub enum Value {
    Ptr,
    I8,
    I32,
    I64,
    F32,
}

/// A built-in function (see `FunctionBody::BuiltIn`) implemented by a
/// function of the runtime, which compiled code calls with its arguments.
pub struct Builtin {
    /// Name of the built-in (see `mollie_typed_ast::builtins`).
    pub name: &'static str,
    pub symbol: &'static str,
    pub address: *const u8,
    pub params: &'static [Value],
    pub returns: Option<Value>,
    /// Whether it allocates (it's called through a wrapper recording the
    /// frame of its caller, since it may collect garbage, and may stop the
    /// program).
    pub allocates: bool,
}

/// Built-ins implemented by the runtime.
pub fn builtins() -> [Builtin; 25] {
    use Value::{F32, I8, I32, I64, Ptr};

    let builtin = |name: &'static str, symbol: &'static str, address: *const u8, params: &'static [Value], returns: Option<Value>, allocates: bool| Builtin {
        name,
        symbol,
        address,
        params,
        returns,
        allocates,
    };

    [
        builtin("f32_sin", "molmath_sin", math::sin_f32 as *const u8, &[F32], Some(F32), false),
        builtin("f32_cos", "molmath_cos", math::cos_f32 as *const u8, &[F32], Some(F32), false),
        builtin("f32_tan", "molmath_tan", math::tan_f32 as *const u8, &[F32], Some(F32), false),
        builtin("f32_atan2", "molmath_atan2", math::atan2_f32 as *const u8, &[F32, F32], Some(F32), false),
        builtin("f32_exp", "molmath_exp", math::exp_f32 as *const u8, &[F32], Some(F32), false),
        builtin("f32_ln", "molmath_ln", math::ln_f32 as *const u8, &[F32], Some(F32), false),
        builtin("f32_pow", "molmath_pow", math::pow_f32 as *const u8, &[F32, F32], Some(F32), false),
        builtin(
            "string_contains",
            "molstr_contains",
            strings::contains as *const u8,
            &[Ptr, Ptr],
            Some(I8),
            false,
        ),
        builtin(
            "string_starts_with",
            "molstr_starts_with",
            strings::starts_with as *const u8,
            &[Ptr, Ptr],
            Some(I8),
            false,
        ),
        builtin(
            "string_ends_with",
            "molstr_ends_with",
            strings::ends_with as *const u8,
            &[Ptr, Ptr],
            Some(I8),
            false,
        ),
        builtin(
            "string_byte_index_of",
            "molstr_byte_index_of",
            strings::byte_index_of as *const u8,
            &[Ptr, Ptr],
            Some(Ptr),
            false,
        ),
        builtin("string_compare", "molstr_compare", strings::compare as *const u8, &[Ptr, Ptr], Some(I32), false),
        builtin("string_hash", "molstr_hash", strings::hash as *const u8, &[Ptr], Some(I64), false),
        builtin("string_is_int", "molstr_is_int", strings::is_int as *const u8, &[Ptr], Some(I8), false),
        builtin("string_to_int", "molstr_to_int", strings::to_int as *const u8, &[Ptr], Some(I64), false),
        builtin("string_is_float", "molstr_is_float", strings::is_float as *const u8, &[Ptr], Some(I8), false),
        builtin("string_to_float", "molstr_to_float", strings::to_float as *const u8, &[Ptr], Some(F32), false),
        builtin(
            "string_char_width_at",
            "molstr_char_width_at",
            strings::char_width_at as *const u8,
            &[Ptr, Ptr],
            Some(Ptr),
            false,
        ),
        builtin("string_byte_at", "molstr_byte_at", strings::byte_at as *const u8, &[Ptr, Ptr], Some(I8), false),
        builtin("string_trim", "molstr_trim", strings::trim as *const u8, &[Ptr], Some(Ptr), true),
        builtin("string_to_lower", "molstr_to_lower", strings::to_lower as *const u8, &[Ptr], Some(Ptr), true),
        builtin("string_to_upper", "molstr_to_upper", strings::to_upper as *const u8, &[Ptr], Some(Ptr), true),
        builtin("string_repeat", "molstr_repeat", strings::repeat as *const u8, &[Ptr, Ptr], Some(Ptr), true),
        builtin("string_split", "molstr_split", strings::split as *const u8, &[Ptr, Ptr], Some(Ptr), true),
        builtin(
            "string_char_count",
            "molstr_char_count",
            strings::char_count as *const u8,
            &[Ptr],
            Some(Ptr),
            false,
        ),
    ]
}

/// Symbols of the runtime, to be registered in the JIT module.
pub fn symbols() -> impl Iterator<Item = Symbol> {
    [
        ("molstr_eq", strings::eq as *const u8),
        ("molstr_concat", strings::concat as *const u8),
        ("molstr_slice", strings::slice as *const u8),
        ("molstr_from_int", strings::from_int as *const u8),
        ("molstr_from_uint", strings::from_uint as *const u8),
        ("molstr_from_f32", strings::from_f32 as *const u8),
        ("molstr_from_bool", strings::from_bool as *const u8),
        ("molstr_format_int", strings::format_int as *const u8),
        ("molstr_format_uint", strings::format_uint as *const u8),
        ("molstr_format_f32", strings::format_f32 as *const u8),
        ("molstr_format_str", strings::format_str as *const u8),
        ("molalloc", allocator::compiled_alloc as *const u8),
        ("molalloc_arr", allocator::compiled_alloc_array as *const u8),
        ("molrealloc_arr", allocator::realloc_array as *const u8),
        ("molexit_push", allocator::exit_push as *const u8),
        ("molvm_out_of_fuel", sandbox::out_of_fuel as *const u8),
        ("molvm_panic", sandbox::panic as *const u8),
        ("molexit_pop", allocator::exit_pop as *const u8),
    ]
    .into_iter()
    .chain(builtins().into_iter().map(|builtin| (builtin.symbol, builtin.address)))
}
