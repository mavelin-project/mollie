use std::{mem, ptr, slice, str};

use cranelift::codegen::ir;
use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_shared::{FormatKind, FormatSpec};
use mollie_typing::AdtVariantRef;

use crate::{
    allocator::{Array, Heap, TypeLayout, TypeLayoutField},
    sandbox::{self, TrapKind},
};

static BYTES: TypeLayout = TypeLayout::of::<u8>();

/// # Safety
///
/// `string` must point to a live string of a program (an array of bytes)
/// that isn't collected or changed while the result lives.
pub const unsafe fn bytes<'a>(string: *const Array) -> &'a [u8] {
    let array = unsafe { &*string };

    if array.length == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(array.ptr.cast::<u8>(), array.length) }
    }
}

/// # Safety
///
/// As for [`bytes`]; the string must be valid UTF-8 (strings of programs
/// are).
pub const unsafe fn as_str<'a>(string: *const Array) -> &'a str {
    // Strings are only created from valid UTF-8, and sliced at char
    // boundaries.
    unsafe { str::from_utf8_unchecked(bytes(string)) }
}

unsafe fn alloc_in(heap: &Heap, keep: &[*const Array], length: usize, may_collect: bool) -> *mut Array {
    for &string in keep {
        heap.root(string.cast());
    }

    let string = unsafe { heap.alloc_array(&BYTES, length, may_collect) }.cast::<Array>();

    for &string in keep {
        heap.unroot(string.cast());
    }

    string
}

unsafe fn alloc(keep: &[*const Array], length: usize) -> *mut Array {
    sandbox::current_heap().map_or_else(
        || {
            sandbox::set_trap(TrapKind::OutOfMemory);

            ptr::null_mut()
        },
        |heap| unsafe { alloc_in(heap, keep, length, true) },
    )
}

unsafe fn fill(string: *mut Array, text: &str) -> *mut Array {
    if !string.is_null() {
        unsafe { ptr::copy_nonoverlapping(text.as_ptr(), (*string).ptr.cast::<u8>(), text.len()) };
    }

    string
}

unsafe fn from_str(text: &str) -> *mut Array {
    unsafe { fill(alloc(&[], text.len()), text) }
}

pub fn new(heap: &Heap, text: &str) -> *mut Array {
    unsafe { fill(alloc_in(heap, &[], text.len(), false), text) }
}

pub(crate) unsafe extern "C" fn eq(a: *const Array, b: *const Array) -> i8 {
    i8::from(unsafe { bytes(a) == bytes(b) })
}

pub(crate) unsafe extern "C" fn concat(a: *const Array, b: *const Array) -> *mut Array {
    let (a_length, b_length) = unsafe { ((*a).length, (*b).length) };
    let string = unsafe { alloc(&[a, b], a_length + b_length) };

    if string.is_null() {
        return string;
    }

    let data = unsafe { (*string).ptr.cast::<u8>() };

    unsafe {
        ptr::copy_nonoverlapping(bytes(a).as_ptr(), data, a_length);
        ptr::copy_nonoverlapping(bytes(b).as_ptr(), data.add(a_length), b_length);
    }

    string
}

pub(crate) unsafe extern "C" fn slice(string: *const Array, start: usize, end: usize) -> *mut Array {
    let text = unsafe { as_str(string) };

    if start > end || end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return ptr::null_mut();
    }

    let result = unsafe { alloc(&[string], end - start) };

    if result.is_null() {
        return result;
    }

    unsafe { ptr::copy_nonoverlapping(bytes(string).as_ptr().add(start), (*result).ptr.cast::<u8>(), end - start) };

    result
}

pub(crate) unsafe extern "C" fn from_int(value: i64) -> *mut Array {
    unsafe { from_str(&value.to_string()) }
}

pub(crate) unsafe extern "C" fn from_uint(value: u64) -> *mut Array {
    unsafe { from_str(&value.to_string()) }
}

pub(crate) unsafe extern "C" fn from_f32(value: f32) -> *mut Array {
    unsafe { from_str(&value.to_string()) }
}

pub(crate) unsafe extern "C" fn from_f64(value: f64) -> *mut Array {
    unsafe { from_str(&value.to_string()) }
}

pub(crate) unsafe extern "C" fn format_int(value: i64, spec: u64) -> *mut Array {
    let spec = FormatSpec::unpack(spec);
    let digits = format_digits(value.unsigned_abs(), spec.kind);
    let text = if value < 0 { format!("-{digits}") } else { digits };

    unsafe { from_str(&spec.pad(&text, true)) }
}

pub(crate) unsafe extern "C" fn format_uint(value: u64, spec: u64) -> *mut Array {
    let spec = FormatSpec::unpack(spec);

    unsafe { from_str(&spec.pad(&format_digits(value, spec.kind), true)) }
}

fn format_digits(value: u64, kind: FormatKind) -> String {
    match kind {
        FormatKind::Display => value.to_string(),
        FormatKind::Hex => format!("{value:x}"),
        FormatKind::UpperHex => format!("{value:X}"),
        FormatKind::Binary => format!("{value:b}"),
    }
}

pub(crate) unsafe extern "C" fn format_f32(value: f32, spec: u64) -> *mut Array {
    let spec = FormatSpec::unpack(spec);
    let text = spec
        .precision
        .map_or_else(|| value.to_string(), |precision| format!("{value:.*}", usize::from(precision)));

    unsafe { from_str(&spec.pad(&text, true)) }
}

pub(crate) unsafe extern "C" fn format_f64(value: f64, spec: u64) -> *mut Array {
    let spec = FormatSpec::unpack(spec);
    let text = spec
        .precision
        .map_or_else(|| value.to_string(), |precision| format!("{value:.*}", usize::from(precision)));

    unsafe { from_str(&spec.pad(&text, true)) }
}

pub(crate) unsafe extern "C" fn format_str(string: *const Array, spec: u64) -> *mut Array {
    let spec = FormatSpec::unpack(spec);
    // Copied before allocating, which may collect the string.
    let text = spec.pad(unsafe { as_str(string) }, false);

    unsafe { from_str(&text) }
}

pub(crate) unsafe extern "C" fn from_bool(value: i8) -> *mut Array {
    unsafe { from_str(if value == 0 { "false" } else { "true" }) }
}

static STRING_ELEMENTS: TypeLayout = TypeLayout {
    fields: &[(AdtVariantRef::ZERO, 0, MollieType::Regular(ir::types::I64), TypeLayoutField::Collectable)],
    adt_ty: None,
    size: mem::size_of::<usize>(),
    align: mem::align_of::<usize>(),
    kind: None,
};

pub(crate) unsafe extern "C" fn contains(string: *const Array, part: *const Array) -> i8 {
    i8::from(unsafe { as_str(string).contains(as_str(part)) })
}

pub(crate) unsafe extern "C" fn starts_with(string: *const Array, part: *const Array) -> i8 {
    i8::from(unsafe { bytes(string).starts_with(bytes(part)) })
}

pub(crate) unsafe extern "C" fn ends_with(string: *const Array, part: *const Array) -> i8 {
    i8::from(unsafe { bytes(string).ends_with(bytes(part)) })
}

pub(crate) unsafe extern "C" fn byte_index_of(string: *const Array, part: *const Array) -> isize {
    unsafe { as_str(string).find(as_str(part)) }.map_or(-1, |index| isize::try_from(index).unwrap_or(-1))
}

pub(crate) unsafe extern "C" fn compare(a: *const Array, b: *const Array) -> i32 {
    use std::cmp::Ordering;

    match unsafe { bytes(a).cmp(bytes(b)) } {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

pub(crate) unsafe extern "C" fn hash(string: *const Array) -> u64 {
    unsafe { bytes(string) }
        .iter()
        .fold(0xCBF2_9CE4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3))
}

pub(crate) unsafe extern "C" fn is_int(string: *const Array) -> i8 {
    i8::from(unsafe { as_str(string) }.parse::<i64>().is_ok())
}

pub(crate) unsafe extern "C" fn to_int(string: *const Array) -> i64 {
    unsafe { as_str(string) }.parse().unwrap_or(0)
}

pub(crate) unsafe extern "C" fn is_float(string: *const Array) -> i8 {
    i8::from(unsafe { as_str(string) }.parse::<f32>().is_ok())
}

pub(crate) unsafe extern "C" fn to_float(string: *const Array) -> f32 {
    unsafe { as_str(string) }.parse().unwrap_or(0.0)
}

pub(crate) unsafe extern "C" fn char_width_at(string: *const Array, index: usize) -> usize {
    let text = unsafe { as_str(string) };

    if text.is_char_boundary(index) {
        text[index..].chars().next().map_or(0, char::len_utf8)
    } else {
        0
    }
}

pub(crate) unsafe extern "C" fn byte_at(string: *const Array, index: usize) -> u8 {
    unsafe { bytes(string) }.get(index).copied().unwrap_or(0)
}

unsafe fn derived(keep: *const Array, text: &str) -> *mut Array {
    unsafe { fill(alloc(&[keep], text.len()), text) }
}

pub(crate) unsafe extern "C" fn trim(string: *const Array) -> *mut Array {
    unsafe { derived(string, as_str(string).trim()) }
}

pub(crate) unsafe extern "C" fn to_lower(string: *const Array) -> *mut Array {
    unsafe { derived(string, &as_str(string).to_ascii_lowercase()) }
}

pub(crate) unsafe extern "C" fn to_upper(string: *const Array) -> *mut Array {
    unsafe { derived(string, &as_str(string).to_ascii_uppercase()) }
}

pub(crate) unsafe extern "C" fn repeat(string: *const Array, count: usize) -> *mut Array {
    let text = unsafe { as_str(string) };

    let Some(length) = text.len().checked_mul(count).filter(|&length| isize::try_from(length).is_ok()) else {
        sandbox::set_trap(TrapKind::OutOfMemory);

        return ptr::null_mut();
    };

    let result = unsafe { alloc(&[string], length) };

    if !result.is_null() {
        let data = unsafe { (*result).ptr.cast::<u8>() };

        for index in 0..count {
            unsafe { ptr::copy_nonoverlapping(text.as_ptr(), data.add(index * text.len()), text.len()) };
        }
    }

    result
}

pub(crate) unsafe extern "C" fn split(string: *const Array, separator: *const Array) -> *mut Array {
    let Some(heap) = sandbox::current_heap() else {
        sandbox::set_trap(TrapKind::OutOfMemory);

        return ptr::null_mut();
    };
    let (text, separator_text) = unsafe { (as_str(string), as_str(separator)) };
    // Offsets, since the parts are copied after allocations (which don't
    // move the string).
    let parts = text
        .split(separator_text)
        .map(|part| {
            let start = part.as_ptr().addr() - text.as_ptr().addr();

            (start, start + part.len())
        })
        .collect::<Vec<_>>();

    for value in [string, separator] {
        heap.root(value.cast());
    }

    let array = unsafe { heap.alloc_array(&STRING_ELEMENTS, parts.len(), true) }.cast::<Array>();

    if !array.is_null() {
        heap.root(array.cast());

        for (index, &(start, end)) in parts.iter().enumerate() {
            let part = unsafe { alloc_in(heap, &[], end - start, true) };

            if part.is_null() {
                break;
            }

            unsafe {
                ptr::copy_nonoverlapping(text.as_ptr().add(start), (*part).ptr.cast::<u8>(), end - start);
                (*array).ptr.cast::<*mut Array>().add(index).write(part);
            }
        }

        heap.unroot(array.cast());
    }

    for value in [string, separator] {
        heap.unroot(value.cast());
    }

    // Null (and the program stopped) if an allocation failed.
    if sandbox::current_state().is_some_and(|state| unsafe { state.as_ref() }.trap != 0) {
        ptr::null_mut()
    } else {
        array
    }
}

pub(crate) unsafe extern "C" fn char_count(string: *const Array) -> usize {
    unsafe { as_str(string) }.chars().count()
}
