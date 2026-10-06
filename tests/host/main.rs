//! Items implemented by the host: host types with host methods, host trait
//! impls used through trait objects, and Mollie impls of host traits called
//! by the host.

mod isolation;
mod methods;
mod safe;
mod traits;

use std::{
    cell::Cell,
    ptr,
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicPtr, Ordering},
    },
};

use mollie::{
    AdtBuilder, GcPtr, MolStr, TraitBuilder, VTableBuilder,
    compiler::{Compiler, allocator::TypeLayout, sandbox::Limits},
    typing::{Type, TypeRef},
};

/// Programs record what the host functions are called with in globals (like
/// the layout of `Coin`), so they run one at a time.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `struct Coin { cents: i32 }`, declared by the host.
#[repr(C)]
pub struct Coin {
    pub cents: i32,
}

/// Layout of `Coin` in the current program, to allocate coins in host
/// functions.
static COIN_LAYOUT: AtomicPtr<TypeLayout> = AtomicPtr::new(ptr::null_mut());
thread_local! {
    /// Number of calls of `Valued::value` implemented by the host, on this
    /// thread (compiled code runs on the thread calling it, so tests running
    /// in parallel don't count each other's calls).
    pub static VALUE_CALLS: Cell<usize> = const { Cell::new(0) };
}

extern "C" fn coin_doubled(coin: GcPtr<Coin>) -> i32 {
    coin.cents * 2
}

/// Returns a new coin, allocated by the host.
extern "C" fn coin_split(coin: GcPtr<Coin>) -> GcPtr<Coin> {
    let layout = COIN_LAYOUT.load(Ordering::SeqCst);

    assert!(!layout.is_null(), "the layout of `Coin` must be known");

    // SAFETY: the layout of `Coin` of the running program.
    unsafe { GcPtr::from_parts(Coin { cents: coin.cents / 2 }, &*layout) }
}

/// Returns a new string, allocated by the host.
extern "C" fn coin_describe(coin: GcPtr<Coin>, prefix: MolStr) -> MolStr {
    MolStr::new(&format!("{prefix}{} cents", coin.cents))
}

extern "C" fn coin_value(coin: GcPtr<Coin>) -> i32 {
    VALUE_CALLS.set(VALUE_CALLS.get() + 1);

    coin.cents
}

/// Items registered by the host.
pub struct HostItems {
    /// `Valued` as the type of trait objects.
    pub valued_ty: TypeRef,
}

/// A compiler knowing:
///
/// - `struct Coin { cents: i32 }`;
/// - `trait Valued { func value(self) -> i32; }`;
/// - host methods `Coin::doubled(self) -> i32`, `Coin::split(self) -> Coin` and
///   `Coin::describe(self, prefix: string) -> string`;
/// - a host impl of `Valued` for `Coin`.
///
/// # Panics
///
/// Panics if the compiler can't be created or an item can't be registered.
pub fn compiler() -> (Compiler<()>, HostItems) {
    let mut compiler = Compiler::with_symbols((), [
        ("host_coin_doubled", coin_doubled as *const u8),
        ("host_coin_split", coin_split as *const u8),
        ("host_coin_value", coin_value as *const u8),
        ("host_coin_describe", coin_describe as *const u8),
    ])
    .unwrap_or_else(|error| panic!("can't create the compiler: {error}"));

    let context = &mut compiler.type_context;
    let i32 = context.tcx.types.core_types.i32;
    let string = context.tcx.types.core_types.string;
    let coin = AdtBuilder::new_struct(&mut context.tcx, "Coin").field::<i32>("cents").finish();
    let coin = context.tcx.types.get_or_add(Type::Adt(coin, Box::new([])));
    let valued = TraitBuilder::new(&mut context.tcx, "Valued")
        .func("value", Vec::<(&str, TypeRef)>::new(), i32)
        .finish();
    let valued_ty = context.tcx.types.get_or_add(Type::Trait(valued, Box::new([])));

    VTableBuilder::new(context, coin)
        .func("doubled", "host_coin_doubled", [coin], i32)
        .func("split", "host_coin_split", [coin], coin)
        .func("describe", "host_coin_describe", [coin, string], string)
        .finish();

    VTableBuilder::new(context, coin)
        .implements(valued, [])
        .func("value", "host_coin_value", [coin], i32)
        .finish();

    (compiler, HostItems { valued_ty })
}

/// Compiles `source` into `main` returning a value of the type picked by
/// `returns`, and runs it. With `stress`, every allocation collects garbage.
pub fn run<R: Copy>(source: &str, returns: impl FnOnce(&HostItems, &mut Compiler<()>) -> TypeRef, stress: bool) -> R {
    run_kept(source, returns, stress).value
}

/// A program that ran, kept for tests that use its objects or call its code
/// afterwards.
pub struct Kept<R> {
    pub value: R,
    /// Compiled code is valid only while its compiler is alive: writable data
    /// of the JIT module (like the state of the sandbox, read by every
    /// function) is unmapped when the compiler is dropped.
    compiler: Compiler<()>,
    /// Another program must not collect objects of this one meanwhile.
    /// Dropped last.
    _guard: MutexGuard<'static, ()>,
}

/// Like [`run`], but keeps the compiler and the lock of the garbage collector
/// with the result.
///
/// # Panics
///
/// Panics if the program doesn't compile, or traps.
pub fn run_kept<R: Copy>(source: &str, returns: impl FnOnce(&HostItems, &mut Compiler<()>) -> TypeRef, stress: bool) -> Kept<R> {
    let guard = lock();
    let (mut compiler, items) = compiler();
    let returns = returns(&items, &mut compiler);
    let mut provider = compiler.start_compiling();

    if let Err(error) = provider.compile("main", Vec::<(String, TypeRef)>::new(), Some(returns), source) {
        panic!("compilation failed:\n{}\n\nprogram:\n{source}", error.display(&provider.type_context.tcx));
    }

    if let Some(coin) = provider.compiler.find_adt("Coin") {
        COIN_LAYOUT.store(ptr::from_ref(coin.type_layout).cast_mut(), Ordering::SeqCst);
    }

    let main = unsafe { provider.compiler.get_func::<extern "C" fn() -> R>("main") }.expect("`main` must be compiled");

    // The heap is the compiler's, so stress mode ends with it.
    provider.compiler.heap().set_stress(stress);

    let value = provider
        .compiler
        .run(Limits::default(), || main())
        .unwrap_or_else(|trap| panic!("the program trapped: {trap}\n\nprogram:\n{source}"));

    drop(provider);

    Kept {
        value,
        compiler,
        _guard: guard,
    }
}

impl<R> Kept<R> {
    /// Runs `call`, which calls compiled code of the program.
    ///
    /// # Panics
    ///
    /// Panics if the program traps.
    pub fn run<T>(&self, call: impl FnOnce() -> T) -> T {
        self.compiler
            .inner
            .run(Limits::default(), call)
            .unwrap_or_else(|trap| panic!("the program trapped: {trap}"))
    }
}

pub fn run_i32(source: &str) -> i32 {
    run(source, |_, compiler| compiler.type_context.tcx.types.core_types.i32, false)
}

pub fn run_i32_stressed(source: &str) -> i32 {
    run(source, |_, compiler| compiler.type_context.tcx.types.core_types.i32, true)
}
