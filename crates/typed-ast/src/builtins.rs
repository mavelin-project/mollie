//! Functions of the language implemented by the compiler: methods of arrays
//! and strings. Their bodies are [`FunctionBody::BuiltIn`], named after the
//! function.

use indexmap::IndexMap;
use mollie_index::{Idx, IndexVec};
use mollie_typing::{Type, TypeRef, VFuncRef, VTableFunc, VTableGenerator};

use crate::{FunctionBody, TypedASTContext};

/// Registers an inherent impl of built-in functions for `ty`. Every function
/// is a name, the built-in it's implemented by, its argument names and type.
fn register_impl(context: &mut TypedASTContext, ty: TypeRef, generics: &[TypeRef], functions: &[(&str, &'static str, &[&str], TypeRef)]) {
    let impl_ref = context.tcx.register_impl(VTableGenerator {
        ty,
        origin_trait: None,
        trait_args: Box::new([]),
        generics: generics.into(),
        bounds: Box::new([]),
        functions: functions
            .iter()
            .map(|&(name, _, arg_names, ty)| VTableFunc {
                trait_func: None,
                name: name.to_owned(),
                arg_names: arg_names.iter().map(|&name| name.to_owned()).collect(),
                generics: 0,
                ty,
            })
            .collect::<IndexVec<_, _>>(),
    });

    context.vtables.insert(
        impl_ref,
        functions
            .iter()
            .enumerate()
            .map(|(index, &(_, built_in, ..))| (VFuncRef::new(index), FunctionBody::BuiltIn(built_in)))
            .collect::<IndexMap<_, _>>(),
    );
}

/// Registers built-in functions:
///
/// - `array.push(item)`, `array.len() -> usize` and `array.truncate(length)`;
/// - methods of strings: `len` (in bytes), `slice` (by byte offsets),
///   searching, comparing, hashing, parsing and transforming them;
/// - methods of `f32`: roots, rounding and transcendental functions, the same
///   on every platform.
///
/// The standard library builds on them (`std::array`, `std::string`,
/// `std::math`).
pub fn register(context: &mut TypedASTContext) {
    let types = &mut context.tcx.types;
    let core = types.core_types;
    let (void, bool, usize, isize, i32, i64, u8, u64, f32, string) = (
        core.void,
        core.bool,
        core.usize,
        core.isize,
        core.i32,
        core.i64,
        core.u8,
        core.u64,
        core.f32,
        core.string,
    );
    let element = types.get_or_add(Type::Generic(0));
    let array = types.get_or_add(Type::Array(element, None));
    let strings = types.get_or_add(Type::Array(string, None));
    let mut func = |params: &[TypeRef], returns: TypeRef| types.get_or_add(Type::Func(params.into(), returns));

    let push_ty = func(&[array, element], void);
    let array_len_ty = func(&[array], usize);
    let truncate_ty = func(&[array, usize], void);
    let string_to_usize = func(&[string], usize);
    let slice_ty = func(&[string, usize, usize], string);
    let string_test = func(&[string, string], bool);
    let string_find = func(&[string, string], isize);
    let string_compare = func(&[string, string], i32);
    let string_hash = func(&[string], u64);
    let string_check = func(&[string], bool);
    let string_to_int = func(&[string], i64);
    let string_to_float = func(&[string], f32);
    let string_at = func(&[string, usize], usize);
    let string_byte = func(&[string, usize], u8);
    let string_map = func(&[string], string);
    let string_repeat = func(&[string, usize], string);
    let string_split = func(&[string, string], strings);
    let float_unary = func(&[f32], f32);
    let float_binary = func(&[f32, f32], f32);

    register_impl(context, array, &[element], &[
        ("push", "push", &["self", "item"], push_ty),
        ("len", "array_len", &["self"], array_len_ty),
        ("truncate", "array_truncate", &["self", "length"], truncate_ty),
    ]);
    register_impl(context, string, &[], &[
        ("len", "string_len", &["self"], string_to_usize),
        ("slice", "string_slice", &["self", "start", "end"], slice_ty),
        ("contains", "string_contains", &["self", "part"], string_test),
        ("starts_with", "string_starts_with", &["self", "part"], string_test),
        ("ends_with", "string_ends_with", &["self", "part"], string_test),
        ("byte_index_of", "string_byte_index_of", &["self", "part"], string_find),
        ("compare", "string_compare", &["self", "other"], string_compare),
        ("hash", "string_hash", &["self"], string_hash),
        ("is_int", "string_is_int", &["self"], string_check),
        ("to_int", "string_to_int", &["self"], string_to_int),
        ("is_float", "string_is_float", &["self"], string_check),
        ("to_float", "string_to_float", &["self"], string_to_float),
        ("char_width_at", "string_char_width_at", &["self", "index"], string_at),
        ("byte_at", "string_byte_at", &["self", "index"], string_byte),
        ("char_count", "string_char_count", &["self"], string_to_usize),
        ("trim", "string_trim", &["self"], string_map),
        ("to_lower", "string_to_lower", &["self"], string_map),
        ("to_upper", "string_to_upper", &["self"], string_map),
        ("repeat", "string_repeat", &["self", "count"], string_repeat),
        ("split", "string_split", &["self", "separator"], string_split),
    ]);
    register_impl(context, f32, &[], &[
        ("sqrt", "f32_sqrt", &["self"], float_unary),
        ("floor", "f32_floor", &["self"], float_unary),
        ("ceil", "f32_ceil", &["self"], float_unary),
        ("trunc", "f32_trunc", &["self"], float_unary),
        ("abs", "f32_abs", &["self"], float_unary),
        ("sin", "f32_sin", &["self"], float_unary),
        ("cos", "f32_cos", &["self"], float_unary),
        ("tan", "f32_tan", &["self"], float_unary),
        ("atan2", "f32_atan2", &["self", "x"], float_binary),
        ("exp", "f32_exp", &["self"], float_unary),
        ("ln", "f32_ln", &["self"], float_unary),
        ("pow", "f32_pow", &["self", "exponent"], float_binary),
    ]);
}
