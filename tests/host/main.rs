mod deferred;
mod isolation;
mod libraries;
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

static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

#[repr(C)]
pub struct Coin {
    pub cents: i32,
}

static COIN_LAYOUT: AtomicPtr<TypeLayout> = AtomicPtr::new(ptr::null_mut());
thread_local! {
    pub static VALUE_CALLS: Cell<usize> = const { Cell::new(0) };
}

extern "C" fn coin_doubled(coin: GcPtr<Coin>) -> i32 {
    coin.cents * 2
}

extern "C" fn coin_split(coin: GcPtr<Coin>) -> GcPtr<Coin> {
    let layout = COIN_LAYOUT.load(Ordering::SeqCst);

    assert!(!layout.is_null(), "the layout of `Coin` must be known");

    // SAFETY: the layout of `Coin` of the running program.
    unsafe { GcPtr::from_parts(Coin { cents: coin.cents / 2 }, &*layout) }
}

extern "C" fn coin_describe(coin: GcPtr<Coin>, prefix: MolStr) -> MolStr {
    MolStr::new(&format!("{prefix}{} cents", coin.cents))
}

extern "C" fn coin_value(coin: GcPtr<Coin>) -> i32 {
    VALUE_CALLS.set(VALUE_CALLS.get() + 1);

    coin.cents
}

pub struct HostItems {
    pub valued_ty: TypeRef,
}

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

pub fn run<R: Copy>(source: &str, returns: impl FnOnce(&HostItems, &mut Compiler<()>) -> TypeRef, stress: bool) -> R {
    run_kept(source, returns, stress).value
}

pub struct Kept<R> {
    pub value: R,
    compiler: Compiler<()>,
    _guard: MutexGuard<'static, ()>,
}

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
