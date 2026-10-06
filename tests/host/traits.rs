//! Trait impls crossing the boundary between the host and compiled code.

use mollie::GcPtr;

use crate::{VALUE_CALLS, run_i32, run_kept};

#[test]
fn host_impl_is_called_through_trait_object() {
    // The vtable entry of a host function is a wrapper recording the frame of
    // compiled code for the garbage collector.
    VALUE_CALLS.set(0);

    assert_eq!(run_i32("let value: Valued = Coin { cents: 25 };\nvalue.value() + value.value()"), 50);
    assert_eq!(VALUE_CALLS.get(), 2);
}

#[test]
fn host_and_mollie_impls_are_called_through_trait_objects() {
    assert_eq!(
        run_i32(
            "struct Note { amount: i32 }

impl Valued for Note {
    func value(self) -> i32 { self.amount * 100 }
}

let coin: Valued = Coin { cents: 5 };
let note: Valued = Note { amount: 2 };

coin.value() + note.value()"
        ),
        205
    );
}

#[test]
fn host_impl_is_called_directly() {
    assert_eq!(run_i32("let coin = Coin { cents: 7 };\ncoin.value()"), 7);
}

/// A trait object returned by compiled code.
#[derive(Clone, Copy)]
#[repr(C)]
struct TraitObject {
    data: GcPtr<()>,
    vtable: *const ValuedVTable,
}

/// The vtable of `Valued`: the hash of the implementing type, followed by
/// functions in the trait's order.
#[repr(C)]
struct ValuedVTable {
    hash: u64,
    value: extern "C" fn(GcPtr<()>) -> i32,
}

#[test]
fn mollie_impl_of_host_trait_is_called_by_host() {
    // The compiler and the lock are kept while the object is used: compiled
    // code needs its compiler, and another program could collect the
    // object, since nothing roots it.
    let program = run_kept(
        "struct Note { amount: i32 }

impl Valued for Note {
    func value(self) -> i32 { self.amount * 3 }
}

Note { amount: 14 }",
        |items, _| items.valued_ty,
        false,
    );
    let object: TraitObject = program.value;
    let vtable = unsafe { &*object.vtable };

    assert_ne!(vtable.hash, 0);
    assert_eq!(program.run(|| (vtable.value)(object.data)), 42);
}
