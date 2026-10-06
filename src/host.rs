//! A safe interface between the host and scripts.
//!
//! The host registers functions, types and impls from Rust closures and
//! types, which are checked against the Mollie types they get, and calls
//! scripts through [`ScriptFn`]s whose signatures are checked against the
//! compiled programs:
//!
//! ```ignore
//! let mut host = Host::new(&mut compiler);
//!
//! host.function("log", |message: MolStr| println!("{message}"));
//!
//! compiler.compile_script::<(i32, i32), i32>("update", &["state", "frame"], source)?;
//!
//! let update = compiler.script_fn::<(i32, i32), i32>("update")?;
//! let state = update.call((state, frame), Limits::default())?;
//! ```
//!
//! Functions of the host get their arguments by value, except values of value
//! types, which they get by pointer; GC references in arguments stay alive
//! while the function runs.
//!
//! Every program has its own heap: GC objects of one program can't be given to
//! another (that stops the program), strings are copied. A panic in a function
//! of the host stops the program (a [`Trap`] of kind
//! [`TrapKind::Host`](crate::compiler::sandbox::TrapKind::Host)) instead of
//! unwinding through compiled code.

use std::{
    any::{Any, TypeId},
    io,
    marker::PhantomData,
    mem,
    panic::{self, AssertUnwindSafe},
    path::Path,
    ptr::{self, NonNull},
    rc::Weak,
};

use mollie_index::Idx;

pub use crate::compiler::HostTypes;
use crate::{
    AdtBuilder, GcPtr, MolStr, MollieTypeOf, TraitBuilder, VTableBuilder,
    compiler::{
        Compiler, CompilerInner,
        allocator::{self, Array, Heap, TraitFunction},
        error::{CompileError, CompileResult},
        sandbox::{self, Limits, Trap, TrapKind, VmState},
    },
    shared::Span,
    typed_ast::{FunctionBody, ModuleLoader},
    typing::{Func, ModuleId, ModuleItem, PrimitiveType, TraitRef, TyCtxt, Type, TypeRef},
};

/// A Rust type whose values cross the boundary between the host and compiled
/// code.
///
/// # Safety
///
/// `Arg` and `Ret` must have the representation compiled code uses for
/// values of the Mollie type of `Self`, and `LAYOUT` must be the size and
/// alignment of values of that type stored in memory.
pub unsafe trait HostValue: Sized + 'static {
    /// How compiled code passes values to the host (and how the host passes
    /// them to scripts).
    type Arg: Copy + 'static;
    /// How the host gives values back to compiled code: written to the slot
    /// of the result.
    type Ret: 'static;

    /// Size and alignment of values stored in memory (fields of objects and
    /// value types).
    const LAYOUT: (usize, usize);

    /// Whether values are passed by pointer (values of value types). Compiled
    /// code passes them differently than C, so the host calls it through C
    /// functions made for each signature.
    const BY_POINTER: bool = false;

    /// The Mollie type of values.
    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef;

    /// # Safety
    ///
    /// `arg` must be a value passed by compiled code.
    unsafe fn from_arg(arg: Self::Arg) -> Self;

    fn into_ret(self) -> Self::Ret;

    /// The value returned by compiled code (in its memory layout).
    ///
    /// # Safety
    ///
    /// `ret` must be a value returned by compiled code, while its program
    /// runs.
    unsafe fn from_ret(ret: Self::Ret) -> Self;

    /// Values of GC objects referenced by `arg`, kept alive while the host
    /// uses it.
    fn references(_arg: &Self::Arg, _references: &mut Vec<*mut u8>) {}
}

/// A [`HostValue`] that the host can also pass to scripts (arguments of
/// programs, functions of trait objects and callbacks).
pub trait ScriptValue: HostValue {
    /// The value for compiled code. It's called while the program runs.
    fn to_arg(&self) -> Self::Arg;

    /// Whether the value can be given to the program with `heap`: GC objects
    /// must be objects of its heap.
    fn belongs_to(&self, _heap: &Heap) -> bool {
        true
    }
}

/// `string`, or a copy of it in the heap of the running program if it isn't a
/// string of that heap (a string of another program, or a literal).
fn in_running_heap(string: MolStr) -> MolStr {
    sandbox::with_current_heap(|heap| {
        if heap.contains(string.ptr().cast()) {
            string
        } else {
            MolStr::new_in(heap, &string)
        }
    })
    .unwrap_or(string)
}

/// Stops the running program because the host gave it a value of another
/// program.
fn foreign_value() {
    sandbox::raise("a value of another program was given to this program");
}

/// Looks up the Mollie type registered for the Rust type `T`.
fn registered<T: 'static>(types: &HostTypes) -> TypeRef {
    *types
        .get(&TypeId::of::<T>())
        .unwrap_or_else(|| panic!("`{}` must be registered with `Host` before it's used", std::any::type_name::<T>()))
}

macro_rules! plain_values {
    ($($ty:ty),*) => {
        $(
            unsafe impl HostValue for $ty {
                type Arg = Self;
                type Ret = Self;

                const LAYOUT: (usize, usize) = (mem::size_of::<Self>(), mem::align_of::<Self>());

                fn mollie_type(_: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
                    <Self as MollieTypeOf>::mollie_type_of(tcx)
                }

                unsafe fn from_arg(arg: Self::Arg) -> Self {
                    arg
                }

                fn into_ret(self) -> Self::Ret {
                    self
                }

                unsafe fn from_ret(ret: Self::Ret) -> Self {
                    ret
                }
            }

            impl ScriptValue for $ty {
                fn to_arg(&self) -> Self::Arg {
                    *self
                }
            }
        )*
    };
}

plain_values!(i8, u8, i16, u16, i32, u32, i64, u64, isize, usize, f32);

// Booleans are bytes in compiled code.
unsafe impl HostValue for bool {
    type Arg = u8;
    type Ret = u8;

    const LAYOUT: (usize, usize) = (1, 1);

    fn mollie_type(_: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        tcx.types.get_or_add(Type::Primitive(PrimitiveType::Bool))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        arg != 0
    }

    fn into_ret(self) -> Self::Ret {
        u8::from(self)
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        ret != 0
    }
}

impl ScriptValue for bool {
    fn to_arg(&self) -> Self::Arg {
        u8::from(*self)
    }
}

unsafe impl HostValue for () {
    type Arg = ();
    type Ret = ();

    const LAYOUT: (usize, usize) = (0, 1);

    fn mollie_type(_: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        tcx.types.core_types.void
    }

    unsafe fn from_arg((): Self::Arg) -> Self {}

    fn into_ret(self) -> Self::Ret {}

    unsafe fn from_ret((): Self::Ret) -> Self {}
}

impl ScriptValue for () {
    fn to_arg(&self) -> Self::Arg {}
}

unsafe impl HostValue for MolStr {
    type Arg = Self;
    type Ret = Self;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(_: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        tcx.types.get_or_add(Type::Primitive(PrimitiveType::String))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        arg
    }

    fn into_ret(self) -> Self::Ret {
        in_running_heap(self)
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        ret
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.0.cast_mut().cast());
    }
}

impl ScriptValue for MolStr {
    fn to_arg(&self) -> Self::Arg {
        in_running_heap(*self)
    }
}

// Strings are copied from and into GC strings.
unsafe impl HostValue for String {
    type Arg = MolStr;
    type Ret = MolStr;

    const LAYOUT: (usize, usize) = MolStr::LAYOUT;

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        MolStr::mollie_type(types, tcx)
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        arg.as_str().to_owned()
    }

    fn into_ret(self) -> Self::Ret {
        MolStr::new(&self)
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        ret.as_str().to_owned()
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        MolStr::references(arg, references);
    }
}

impl ScriptValue for String {
    fn to_arg(&self) -> Self::Arg {
        MolStr::new(self)
    }
}

// Objects registered with `Host::object`.
unsafe impl<T: 'static> HostValue for GcPtr<T> {
    type Arg = Self;
    type Ret = Self;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, _: &mut TyCtxt) -> TypeRef {
        registered::<T>(types)
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        arg
    }

    fn into_ret(self) -> Self::Ret {
        let belongs = sandbox::with_current_heap(|heap| self.0.is_null() || heap.contains(self.0.cast())).unwrap_or(true);

        if belongs {
            self
        } else {
            foreign_value();

            // Not used: the program stops right after the host function.
            Self(ptr::null_mut())
        }
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        ret
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.0.cast());
    }
}

impl<T: 'static> ScriptValue for GcPtr<T> {
    fn to_arg(&self) -> Self::Arg {
        *self
    }

    fn belongs_to(&self, heap: &Heap) -> bool {
        self.0.is_null() || heap.contains(self.0.cast())
    }
}

/// A handle to a value owned by the host (registered with [`Host::opaque`]),
/// like a drawing context: scripts can only pass it back to the host.
#[repr(transparent)]
pub struct Opaque<T>(NonNull<T>);

impl<T> Clone for Opaque<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Opaque<T> {}

impl<T> Opaque<T> {
    /// # Safety
    ///
    /// `value` must outlive every use of the handle by scripts and by the
    /// host.
    pub unsafe fn new(value: &mut T) -> Self {
        Self(NonNull::from(value))
    }

    /// The value of the handle.
    ///
    /// # Safety
    ///
    /// The value must still be alive, and not borrowed elsewhere.
    pub const unsafe fn get(&mut self) -> &mut T {
        unsafe { self.0.as_mut() }
    }
}

unsafe impl<T: 'static> HostValue for Opaque<T> {
    type Arg = Self;
    type Ret = Self;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, _: &mut TyCtxt) -> TypeRef {
        registered::<T>(types)
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        arg
    }

    fn into_ret(self) -> Self::Ret {
        self
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        ret
    }
}

impl<T: 'static> ScriptValue for Opaque<T> {
    fn to_arg(&self) -> Self::Arg {
        *self
    }
}

/// Implements [`HostValue`] and [`ScriptValue`] for a
/// `#[repr(C)] #[derive(Clone, Copy)]` type registered as a value type with
/// [`Host::value_type`].
///
/// Values are passed by pointer and returned by value. Value types crossing the boundary can't hold GC references.
#[macro_export]
macro_rules! host_value_type {
    ($ty:ty) => {
        unsafe impl $crate::host::HostValue for $ty {
            type Arg = *const $ty;
            type Ret = $ty;

            const BY_POINTER: bool = true;
            const LAYOUT: (usize, usize) = (::std::mem::size_of::<$ty>(), ::std::mem::align_of::<$ty>());

            fn mollie_type(types: &$crate::host::HostTypes, _: &mut $crate::typing::TyCtxt) -> $crate::typing::TypeRef {
                *types
                    .get(&::std::any::TypeId::of::<$ty>())
                    .unwrap_or_else(|| panic!("`{}` must be registered with `Host::value_type` before it's used", stringify!($ty)))
            }

            unsafe fn from_arg(arg: Self::Arg) -> Self {
                unsafe { arg.read() }
            }

            fn into_ret(self) -> Self::Ret {
                self
            }

            unsafe fn from_ret(ret: Self::Ret) -> Self {
                ret
            }
        }

        impl $crate::host::ScriptValue for $ty {
            fn to_arg(&self) -> Self::Arg {
                // Valid while the arguments are passed: they're kept until the
                // call returns.
                self
            }
        }
    };
}

/// A function of a script passed to the host (a closure or a function used
/// as a value), with arguments `Args` and result `R`.
///
/// It keeps its captured
/// variables alive while it's held, and knows its program: it can only run
/// in it, and can't be called once the program's compiler is dropped.
pub struct ScriptCallback<Args, R> {
    code: usize,
    env: *mut u8,
    /// The C function it's called through (see `Heap::callback_entry`), made
    /// when a program using its type is compiled.
    entry: Option<usize>,
    /// State and heap of the program the function belongs to.
    state: NonNull<VmState>,
    heap: *const Heap,
    alive: Weak<()>,
    _marker: PhantomData<fn(Args) -> R>,
}

/// Representation of function values in compiled code: the code and the
/// environment.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct RawFunction {
    code: usize,
    env: *mut u8,
}

/// A trap of the host with `message`.
fn host_trap(message: &str) -> Trap {
    Trap::new(TrapKind::Host, Some(message.to_owned()))
}

impl<Args: ScriptArgs, R: ScriptValue> ScriptCallback<Args, R> {
    /// Calls the function inside the running program it belongs to (from a
    /// function of the host called by that program), with its limits.
    ///
    /// # Errors
    ///
    /// Returns a trap if the function stops its program (the program then
    /// stops when the host function returns), if its program isn't the one
    /// running (see [`ScriptCallback::call_with_limits`]), if it was
    /// unloaded, or if an argument belongs to another program.
    pub fn call(&self, args: Args) -> Result<R, Trap> {
        if self.alive.upgrade().is_none() {
            return Err(host_trap("the program of the callback was unloaded"));
        }

        if sandbox::current_state() != Some(self.state) {
            return Err(host_trap("the program of the callback isn't running: use `call_with_limits`"));
        }

        // SAFETY: the heap is alive (checked above).
        if !args.belong_to(unsafe { &*self.heap }) {
            foreign_value();

            return Err(host_trap("a value of another program was given to this program"));
        }

        // SAFETY: the code was compiled for these types, checked by the type of
        // the parameter the callback came from.
        let value = unsafe { self.invoke(args) }?;
        // SAFETY: the state is alive with the heap.
        let trap = unsafe { self.state.as_ref() }.trap;

        TrapKind::from_code(trap).map_or_else(|| Ok(unsafe { value.read() }), |kind| Err(Trap::new(kind, None)))
    }

    /// Calls the function with `args`, through its C entry if it has one,
    /// returning a reader of its result (only valid if it didn't stop).
    ///
    /// # Safety
    ///
    /// The function must take `Args` and return `R`, and its program must be
    /// running.
    unsafe fn invoke(&self, args: Args) -> Result<Returned<R>, Trap> {
        if let Some(entry) = self.entry {
            // SAFETY: the entry was made for the type of the function.
            return Ok(Returned::Ret(unsafe { args.invoke_method::<R>(entry, self.code, self.env) }));
        }

        if Args::by_pointer() || R::BY_POINTER {
            return Err(host_trap("callbacks with values of value types need a program compiled with `compile_script`"));
        }

        // Without values of value types, compiled code passes values like C.
        Ok(Returned::Arg(unsafe { args.invoke_with_env::<R>(self.code, self.env) }))
    }

    /// Calls the function in a new run of its program with `limits`, e.g. when
    /// the host calls it later, outside of the program.
    ///
    /// # Errors
    ///
    /// Returns the trap that stopped the function, or a trap if its program
    /// was unloaded or an argument belongs to another program.
    pub fn call_with_limits(&self, args: Args, limits: Limits) -> Result<R, Trap> {
        if self.alive.upgrade().is_none() {
            return Err(host_trap("the program of the callback was unloaded"));
        }

        // SAFETY: the heap is alive (checked above).
        if !args.belong_to(unsafe { &*self.heap }) {
            return Err(host_trap("a value of another program was given to this program"));
        }

        // SAFETY: the state belongs to the program of the code, which is alive.
        // The result is read while the program runs, if it didn't stop.
        let value = unsafe {
            sandbox::run(self.state, limits, || {
                let value = self.invoke(args)?;
                let stopped = self.state.as_ref().trap != 0;

                Ok::<_, Trap>((!stopped).then(|| value.read()))
            })
        }??;

        value.ok_or_else(|| host_trap("the program stopped"))
    }
}

/// A result of a call of compiled code: as passed to the host, or written to
/// a slot.
enum Returned<R: HostValue> {
    Arg(R::Arg),
    Ret(mem::MaybeUninit<R::Ret>),
}

impl<R: HostValue> Returned<R> {
    /// # Safety
    ///
    /// The call must have returned (not stopped its program), and its program
    /// must be running.
    unsafe fn read(self) -> R {
        match self {
            Self::Arg(arg) => unsafe { R::from_arg(arg) },
            Self::Ret(ret) => unsafe { R::from_ret(ret.assume_init()) },
        }
    }
}

impl<Args, R> Drop for ScriptCallback<Args, R> {
    fn drop(&mut self) {
        // The root goes with the heap if the program is unloaded.
        if !self.env.is_null() && self.alive.upgrade().is_some() {
            // SAFETY: the heap is alive.
            unsafe { &*self.heap }.unroot(self.env.cast());
        }
    }
}

unsafe impl<Args: ScriptArgs + 'static, R: ScriptValue> HostValue for ScriptCallback<Args, R> {
    type Arg = RawFunction;
    type Ret = RawFunction;

    const LAYOUT: (usize, usize) = (2 * mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let args = Args::mollie_types(types, tcx).into_boxed_slice();
        let returns = R::mollie_type(types, tcx);
        let ty = tcx.types.get_or_add(Type::Func(args, returns));

        // Called through a C function, made before programs are compiled.
        types.callbacks.borrow_mut().push((TypeId::of::<fn(Args) -> R>(), ty));

        ty
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        // Callbacks come from the program calling the host.
        let state = sandbox::current_state().expect("callbacks are given to the host by a running program");
        // SAFETY: the state of a running program is valid, and has a heap.
        let heap = unsafe { state.as_ref() }.heap;
        let heap_ref = unsafe { &*heap };

        // Captured variables stay alive while the host holds the callback.
        if !arg.env.is_null() {
            heap_ref.root(arg.env.cast());
        }

        Self {
            code: arg.code,
            env: arg.env,
            entry: heap_ref.callback_entry(TypeId::of::<fn(Args) -> R>()),
            state,
            heap,
            alive: heap_ref.alive_token(),
            _marker: PhantomData,
        }
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        // Returned by a program to the host, while its run is current.
        unsafe { Self::from_arg(ret) }
    }

    fn into_ret(self) -> Self::Ret {
        if self.alive.upgrade().is_none() || sandbox::current_state() != Some(self.state) {
            foreign_value();

            // Not used: the program stops right after the host function.
            return RawFunction { code: 0, env: ptr::null_mut() };
        }

        let raw = RawFunction {
            code: self.code,
            env: self.env,
        };

        // The value is the program's now. Nothing collects garbage between the
        // return of the host function and the caller holding the value.
        drop(self);

        raw
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.env);
    }
}

/// Arguments of programs and callbacks called by the host: tuples of
/// [`ScriptValue`]s.
pub trait ScriptArgs: Sized {
    fn mollie_types(types: &HostTypes, tcx: &mut TyCtxt) -> Vec<TypeRef>;

    /// Whether every argument can be given to the program with `heap` (see
    /// [`ScriptValue::belongs_to`]).
    fn belong_to(&self, heap: &Heap) -> bool;

    /// Whether an argument is passed by pointer (see
    /// [`HostValue::BY_POINTER`]).
    fn by_pointer() -> bool;

    /// Calls the entry of a program (see `CompilerInner::get_entry`), returning
    /// the slot its result was written to.
    ///
    /// # Safety
    ///
    /// `code` must be the entry of a program taking these arguments and
    /// returning `R`.
    unsafe fn invoke<R: HostValue>(self, code: usize) -> mem::MaybeUninit<R::Ret>;

    /// # Safety
    ///
    /// `code` must be the code of a function value taking these arguments
    /// and its environment `env`, and returning `R`.
    unsafe fn invoke_with_env<R: HostValue>(self, code: usize, env: *mut u8) -> R::Arg;

    /// Calls the function `code` of a vtable with its value `data` (or a
    /// function value with its environment), through `entry` (see
    /// `FuncCompiler::method_entry` and `callback_entry`), returning the slot
    /// its result was written to.
    ///
    /// # Safety
    ///
    /// `entry` must be the method entry for these arguments and `R`, and
    /// `code` the code of a function of a vtable taking the value and these
    /// arguments, and returning `R`.
    unsafe fn invoke_method<R: HostValue>(self, entry: usize, code: usize, data: *mut u8) -> mem::MaybeUninit<R::Ret>;
}

/// A function of the host registered from a closure, with arguments `Args`.
pub trait IntoHostFunction<Args>: Sized + 'static {
    /// Types of the parameters and of the result.
    fn signature(types: &HostTypes, tcx: &mut TyCtxt) -> (Vec<TypeRef>, TypeRef);

    /// Address of the `extern "C" fn(context, result, arguments...)` calling
    /// the closure (the context).
    fn shim() -> usize;
}

/// Message of a panic, for the trap it becomes.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("a function of the host panicked"))
}

macro_rules! arities {
    ($($arg:ident),*) => {
        impl<Func, Ret, $($arg),*> IntoHostFunction<($($arg,)*)> for Func
        where
            Func: Fn($($arg),*) -> Ret + 'static,
            Ret: HostValue,
            $($arg: HostValue,)*
        {
            fn signature(types: &HostTypes, tcx: &mut TyCtxt) -> (Vec<TypeRef>, TypeRef) {
                (vec![$($arg::mollie_type(types, tcx)),*], Ret::mollie_type(types, tcx))
            }

            fn shim() -> usize {
                #[allow(non_snake_case, clippy::too_many_arguments)]
                unsafe extern "C" fn shim<Func, Ret, $($arg),*>(context: *const Func, result: *mut Ret::Ret, $($arg: $arg::Arg),*)
                where
                    Func: Fn($($arg),*) -> Ret + 'static,
                    Ret: HostValue,
                    $($arg: HostValue,)*
                {
                    // References in arguments stay alive while the function
                    // runs, even if the caller doesn't hold them anymore.
                    #[allow(unused_mut)]
                    let mut references = Vec::new();

                    $($arg::references(&$arg, &mut references);)*

                    allocator::pin(&references);

                    // SAFETY: the context is the closure, kept alive by the
                    // compiler.
                    let func = unsafe { &*context };
                    let returned = panic::catch_unwind(AssertUnwindSafe(|| func($(unsafe { $arg::from_arg($arg) }),*)));

                    allocator::unpin(references.len());

                    match returned {
                        Ok(value) => {
                            if mem::size_of::<Ret::Ret>() > 0 {
                                // SAFETY: the slot has room for the result.
                                unsafe { result.write(value.into_ret()) };
                            }
                        }
                        Err(payload) => {
                            sandbox::raise(panic_message(&*payload));
                        }
                    }
                }

                shim::<Func, Ret, $($arg),*> as *const () as usize
            }
        }

        impl<$($arg: ScriptValue,)*> ScriptArgs for ($($arg,)*) {
            #[allow(unused_variables, reason = "unused without arguments")]
            fn mollie_types(types: &HostTypes, tcx: &mut TyCtxt) -> Vec<TypeRef> {
                vec![$($arg::mollie_type(types, tcx)),*]
            }

            #[allow(non_snake_case, unused_variables, reason = "unused without arguments")]
            fn belong_to(&self, heap: &Heap) -> bool {
                let ($($arg,)*) = self;

                true $(&& $arg.belongs_to(heap))*
            }

            fn by_pointer() -> bool {
                false $(|| $arg::BY_POINTER)*
            }

            #[allow(non_snake_case)]
            unsafe fn invoke<R: HostValue>(self, code: usize) -> mem::MaybeUninit<R::Ret> {
                let ($($arg,)*) = self;
                // Zeroed: a program that stops still writes (zeroes) to it.
                let mut result = mem::MaybeUninit::<R::Ret>::zeroed();
                // SAFETY: guaranteed by the caller.
                let code = unsafe { mem::transmute::<usize, extern "C" fn(*mut R::Ret, $($arg::Arg),*)>(code) };

                code(result.as_mut_ptr(), $($arg.to_arg()),*);

                result
            }

            #[allow(non_snake_case)]
            unsafe fn invoke_with_env<R: HostValue>(self, code: usize, env: *mut u8) -> R::Arg {
                let ($($arg,)*) = self;
                // SAFETY: guaranteed by the caller.
                let code = unsafe { mem::transmute::<usize, extern "C" fn($($arg::Arg,)* *mut u8) -> R::Arg>(code) };

                code($($arg.to_arg(),)* env)
            }

            #[allow(non_snake_case)]
            unsafe fn invoke_method<R: HostValue>(self, entry: usize, code: usize, data: *mut u8) -> mem::MaybeUninit<R::Ret> {
                let ($($arg,)*) = self;
                // Zeroed, like the result of `invoke`.
                let mut result = mem::MaybeUninit::<R::Ret>::zeroed();
                // SAFETY: guaranteed by the caller.
                let entry = unsafe { mem::transmute::<usize, extern "C" fn(*mut R::Ret, usize, *mut u8, $($arg::Arg),*)>(entry) };

                entry(result.as_mut_ptr(), code, data, $($arg.to_arg()),*);

                result
            }
        }
    };
}

arities!();
arities!(A);
arities!(A, B);
arities!(A, B, C);
arities!(A, B, C, D);
arities!(A, B, C, D, E);
arities!(A, B, C, D, E, F);
arities!(A, B, C, D, E, F, G);
arities!(A, B, C, D, E, F, G, H);

/// Registers functions, types and impls of the host in a compiler.
///
/// Items are visible to every program, unless they're registered in a
/// capability (see [`Host::capability`]): then only programs compiled with
/// it can use them.
pub struct Host<'c, ML: ModuleLoader> {
    compiler: &'c mut Compiler<ML>,
    /// The module items are registered in: the host's own, or a capability.
    module: ModuleId,
}

impl<'c, ML: ModuleLoader> Host<'c, ML> {
    pub const fn new(compiler: &'c mut Compiler<ML>) -> Self {
        Self {
            compiler,
            module: ModuleId::ZERO,
        }
    }

    /// Registers the following items in the capability `name`: a module only
    /// programs compiled with it can use (`import { read } from fs;`, see
    /// [`CompilerExt::compile_script_with`]). Gives addons only the parts of
    /// the host's API they're allowed to use.
    ///
    /// # Panics
    ///
    /// Panics if an item called `name` already exists.
    pub fn capability(&mut self, name: &str) -> &mut Self {
        self.enter_module(name, true)
    }

    /// Registers the following items in the module `name` of the host,
    /// visible to every program (`import { Size } from graphics;`).
    ///
    /// # Panics
    ///
    /// Panics if an item called `name` already exists.
    pub fn module(&mut self, name: &str) -> &mut Self {
        self.enter_module(name, false)
    }

    fn enter_module(&mut self, name: &str, restricted: bool) -> &mut Self {
        let registry = &self.compiler.type_context.tcx.def_registry;
        let existing = self
            .compiler
            .capabilities
            .get(name)
            .copied()
            .or_else(|| match registry.modules[ModuleId::ZERO].get_item(name) {
                Some(ModuleItem::SubModule(module)) => Some(module),
                _ => None,
            });

        let module = if let Some(module) = existing {
            module
        } else {
            let registry = &mut self.compiler.type_context.tcx.def_registry;
            let module = registry
                .register_module(name, Span::default())
                .unwrap_or_else(|error| panic!("can't register the capability `{name}`: {:?}", error.error));

            if restricted {
                // Hidden until a program is compiled with it.
                registry.restrict(module);
                self.compiler.capabilities.insert(name.to_owned(), module);
            }

            module
        };

        self.module = module;
        self
    }

    /// Registers the following items for every program again (after
    /// [`Host::capability`]).
    pub const fn everywhere(&mut self) -> &mut Self {
        self.module = ModuleId::ZERO;
        self
    }

    /// The type context, e.g. to register traits with [`crate::TraitBuilder`].
    pub const fn tcx(&mut self) -> &mut TyCtxt {
        &mut self.compiler.type_context.tcx
    }

    /// The module items are registered in (see [`Host::module`]), e.g. for
    /// items built with lower-level builders like [`AdtBuilder`].
    pub const fn current_module(&self) -> ModuleId {
        self.module
    }

    /// Uses the Mollie type `ty` (built with lower-level builders, like an
    /// enum) for the Rust type `T`, so `GcPtr<T>` and the like can be used
    /// in functions of the host.
    ///
    /// # Safety
    ///
    /// Values of `ty` must have the memory layout of `T`.
    pub unsafe fn register_type<T: 'static>(&mut self, ty: TypeRef) -> &mut Self {
        self.compiler.host_types.insert(TypeId::of::<T>(), ty);
        self
    }

    /// The Mollie type of values of the Rust type `T`.
    pub fn type_of<T: HostValue>(&mut self) -> TypeRef {
        T::mollie_type(&self.compiler.host_types, &mut self.compiler.type_context.tcx)
    }

    /// Keeps `func` alive with the compiler, returning its address.
    fn keep<F: 'static>(&mut self, func: F) -> usize {
        let func = Box::new(func);
        let context = ptr::from_ref::<F>(&func).addr();

        self.compiler.host_data.push(func);

        context
    }

    /// Registers the function `name`, implemented by `func`. Its parameters
    /// are called `a0`, `a1`... (see [`Host::function_named`]).
    pub fn function<Args, F: IntoHostFunction<Args>>(&mut self, name: &str, func: F) -> &mut Self {
        let (params, _) = F::signature(&self.compiler.host_types, &mut self.compiler.type_context.tcx);
        let names = (0..params.len()).map(|index| format!("a{index}")).collect::<Vec<_>>();

        self.function_named(name, &names.iter().map(String::as_str).collect::<Vec<_>>(), func)
    }

    /// Registers the function `name` with parameters called `arg_names`,
    /// implemented by `func`.
    ///
    /// # Panics
    ///
    /// Panics if the number of names doesn't match the parameters of `func`,
    /// or if an item called `name` already exists.
    pub fn function_named<Args, F: IntoHostFunction<Args>>(&mut self, name: &str, arg_names: &[&str], func: F) -> &mut Self {
        let (params, returns) = F::signature(&self.compiler.host_types, &mut self.compiler.type_context.tcx);

        assert_eq!(params.len(), arg_names.len(), "`{name}` takes {} arguments", params.len());

        let ty = self.compiler.type_context.tcx.types.get_or_add(Type::Func(params.into_boxed_slice(), returns));
        let context = self.keep(func);
        let func_ref = self
            .compiler
            .type_context
            .tcx
            .def_registry
            .register_func_in_module(
                self.module,
                Func {
                    postfix: false,
                    name: name.to_owned(),
                    generics: 0,
                    arg_names: arg_names.iter().map(|&name| name.to_owned()).collect(),
                    ty,
                },
                Span::default(),
            )
            .unwrap_or_else(|error| panic!("can't register `{name}`: {:?}", error.error));

        self.compiler
            .type_context
            .functions
            .insert(func_ref, FunctionBody::Host { code: F::shim(), context });

        self
    }

    /// Registers the struct `name` whose values are GC objects holding values
    /// of `T` (`#[repr(C)]`, with the fields added to the builder, in order).
    /// Values are passed as [`GcPtr<T>`].
    pub fn object<T: 'static>(&mut self, name: &str) -> TypeBuilder<'_, 'c, ML, T> {
        TypeBuilder::new(self, name, Kind::Object)
    }

    /// Registers the value type `name` for the `#[repr(C)]` type `T`, with the
    /// fields added to the builder, in order. `T` needs
    /// [`host_value_type!`](crate::host_value_type).
    pub fn value_type<T: 'static>(&mut self, name: &str) -> TypeBuilder<'_, 'c, ML, T> {
        TypeBuilder::new(self, name, Kind::Value)
    }

    /// Registers the enum `name` whose values are GC objects holding values
    /// of `T`, with the variants and fields added to the builder, in order.
    /// `T` is `#[repr(usize)]` (each variant is a `#[repr(C)]` struct of the
    /// discriminant and its fields, like variants of Mollie enums). Values
    /// are passed as [`GcPtr<T>`].
    pub fn enum_<T: 'static>(&mut self, name: &str) -> EnumBuilder<'_, 'c, ML, T> {
        EnumBuilder::new(self, name.to_owned(), Kind::Object)
    }

    /// Registers the value enum `name` for `T`, like [`Host::enum_`]. `T`
    /// needs [`host_value_type!`](crate::host_value_type).
    pub fn value_enum<T: 'static>(&mut self, name: &str) -> EnumBuilder<'_, 'c, ML, T> {
        EnumBuilder::new(self, name.to_owned(), Kind::Value)
    }

    /// Registers the type `name` of handles to values of `T` owned by the
    /// host ([`Opaque<T>`]).
    pub fn opaque<T: 'static>(&mut self, name: &str) -> TypeRef {
        let adt = AdtBuilder::new_struct(&mut self.compiler.type_context.tcx, name)
            .non_gc_collectable()
            .finish_in_module(self.module);
        let ty = self.compiler.type_context.tcx.types.get_or_add(Type::Adt(adt, Box::new([])));

        self.compiler.host_types.insert(TypeId::of::<T>(), ty);

        ty
    }

    /// Registers the trait `name`, whose trait objects the host gets as
    /// [`ScriptObject<M>`] (`M` is a Rust marker type standing for the trait).
    /// Its functions are added to the builder; scripts implement it.
    pub fn trait_<M: 'static>(&mut self, name: &str) -> HostTraitBuilder<'_, 'c, ML, M> {
        HostTraitBuilder {
            host: self,
            name: name.to_owned(),
            functions: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Adds functions to values of `T` (a registered type). Methods take the
    /// value as their first argument.
    pub fn methods<T: HostValue>(&mut self) -> ImplBuilder<'_, 'c, ML> {
        let target = self.type_of::<T>();

        ImplBuilder::new(self, target, None)
    }

    /// Implements the trait `trait_ref` (with type arguments `trait_args`) for
    /// values of `T`. Every function of the trait must be added.
    pub fn implement<T: HostValue>(&mut self, trait_ref: TraitRef, trait_args: &[TypeRef]) -> ImplBuilder<'_, 'c, ML> {
        let target = self.type_of::<T>();

        ImplBuilder::new(self, target, Some((trait_ref, trait_args.to_vec())))
    }

    /// Declarations of everything registered, in Mollie syntax: a stub for
    /// tools like language servers.
    pub fn declarations(&self) -> String {
        crate::stub::host_stub(self.compiler).to_string()
    }
}

enum Kind {
    Object,
    Value,
}

/// Builds a type registered by [`Host::object`] or [`Host::value_type`].
pub struct TypeBuilder<'h, 'c, ML: ModuleLoader, T> {
    host: &'h mut Host<'c, ML>,
    name: String,
    kind: Kind,
    fields: Vec<(String, TypeRef)>,
    /// C layout of the fields so far: size and alignment.
    size: usize,
    align: usize,
    _marker: PhantomData<T>,
}

impl<'h, 'c, ML: ModuleLoader, T: 'static> TypeBuilder<'h, 'c, ML, T> {
    fn new(host: &'h mut Host<'c, ML>, name: &str, kind: Kind) -> Self {
        Self {
            host,
            name: name.to_owned(),
            kind,
            fields: Vec::new(),
            size: 0,
            align: 1,
            _marker: PhantomData,
        }
    }

    /// Adds the field `name` of type `F`, the next field of `T`.
    #[must_use]
    pub fn field<F: HostValue>(mut self, name: &str) -> Self {
        let ty = self.host.type_of::<F>();
        let (size, align) = F::LAYOUT;

        self.size = self.size.next_multiple_of(align.max(1)) + size;
        self.align = self.align.max(align);
        self.fields.push((name.to_owned(), ty));

        self
    }

    /// Registers the type.
    ///
    /// Only the size and alignment of `T` are checked, like
    /// [`EnumBuilder::finish`] does: fields of GC types (like [`MolStr`])
    /// must match exactly, or the collector misses them.
    ///
    /// # Panics
    ///
    /// Panics if the size or alignment of the fields doesn't match `T`.
    pub fn finish(self) -> TypeRef {
        let size = self.size.next_multiple_of(self.align);

        assert!(
            size == mem::size_of::<T>() && self.align == mem::align_of::<T>(),
            "fields of `{}` don't match the layout of `{}`: is it `#[repr(C)]`, with the same fields in the same order?",
            self.name,
            std::any::type_name::<T>()
        );

        let tcx = &mut self.host.compiler.type_context.tcx;
        let mut builder = AdtBuilder::new_struct(tcx, self.name.as_str());

        for (name, ty) in &self.fields {
            builder = builder.field_ty(name.as_str(), *ty);
        }

        if matches!(self.kind, Kind::Value) {
            builder = builder.value_type();
        }

        let adt = builder.finish_in_module(self.host.module);
        let ty = self.host.compiler.type_context.tcx.types.get_or_add(Type::Adt(adt, Box::new([])));

        self.host.compiler.host_types.insert(TypeId::of::<T>(), ty);

        ty
    }
}

pub type EnumVariant = (String, Vec<(String, TypeRef)>, usize, usize);

/// Builds an enum registered by [`Host::enum_`] or [`Host::value_enum`].
pub struct EnumBuilder<'h, 'c, ML: ModuleLoader, T> {
    host: &'h mut Host<'c, ML>,
    name: String,
    kind: Kind,
    /// Variants with their fields, and the C layout of each one so far (the
    /// discriminant, then the fields): size and alignment.
    variants: Vec<EnumVariant>,
    _marker: PhantomData<T>,
}

impl<'h, 'c, ML: ModuleLoader, T: 'static> EnumBuilder<'h, 'c, ML, T> {
    const fn new(host: &'h mut Host<'c, ML>, name: String, kind: Kind) -> Self {
        Self {
            host,
            name,
            kind,
            variants: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Adds the variant `name`, the next variant of `T`.
    #[must_use]
    pub fn variant(mut self, name: &str) -> Self {
        self.variants
            .push((name.to_owned(), Vec::new(), mem::size_of::<usize>(), mem::align_of::<usize>()));

        self
    }

    /// Adds the field `name` of type `F` to the last variant, the next field
    /// of that variant of `T`.
    ///
    /// # Panics
    ///
    /// Panics if there's no variant yet.
    #[must_use]
    pub fn field<F: HostValue>(mut self, name: &str) -> Self {
        let ty = self.host.type_of::<F>();
        let (size, align) = F::LAYOUT;
        let enum_name = &self.name;
        let (_, fields, variant_size, variant_align) = self
            .variants
            .last_mut()
            .unwrap_or_else(|| panic!("`{name}` of `{enum_name}` must be in a variant"));

        *variant_size = variant_size.next_multiple_of(align.max(1)) + size;
        *variant_align = (*variant_align).max(align);
        fields.push((name.to_owned(), ty));

        self
    }

    /// Registers the enum.
    ///
    /// Only the size and alignment of `T` are checked: a variant smaller than
    /// it should be (missing a field that fits in padding), or a field of
    /// another type with the same layout, isn't caught. Fields of GC types
    /// (like [`MolStr`]) must match exactly, or the collector misses them.
    ///
    /// # Panics
    ///
    /// Panics if it has no variants, or if the size or alignment of the
    /// variants doesn't match `T`.
    #[track_caller]
    pub fn finish(self) -> TypeRef {
        assert!(!self.variants.is_empty(), "`{}` has no variants", self.name);

        let align = self.variants.iter().map(|&(.., align)| align).max().unwrap_or(1);
        let size = self.variants.iter().map(|&(_, _, size, _)| size).max().unwrap_or(0).next_multiple_of(align);

        assert!(
            size == mem::size_of::<T>() && align == mem::align_of::<T>(),
            "variants of `{}` don't match the layout of `{}`: is it `#[repr(usize)]`, with the same variants and fields in the same order?",
            self.name,
            std::any::type_name::<T>()
        );

        let tcx = &mut self.host.compiler.type_context.tcx;
        let mut builder = AdtBuilder::new_enum(tcx, self.name.as_str());

        for (variant, fields, ..) in &self.variants {
            builder = builder.variant(variant.as_str());

            for (name, ty) in fields {
                builder = builder.field_ty(name.as_str(), *ty);
            }
        }

        if matches!(self.kind, Kind::Value) {
            builder = builder.value_type();
        }

        let adt = builder.finish_in_module(self.host.module);
        let ty = self.host.compiler.type_context.tcx.types.get_or_add(Type::Adt(adt, Box::new([])));

        self.host.compiler.host_types.insert(TypeId::of::<T>(), ty);

        ty
    }
}

/// A function of an impl being built: its name, argument names and types,
/// result type and body.
type ImplFunction = (String, Vec<String>, Vec<TypeRef>, TypeRef, FunctionBody);

/// Builds an impl registered by [`Host::methods`] or [`Host::implement`].
pub struct ImplBuilder<'h, 'c, ML: ModuleLoader> {
    host: &'h mut Host<'c, ML>,
    target: TypeRef,
    origin_trait: Option<(TraitRef, Vec<TypeRef>)>,
    functions: Vec<ImplFunction>,
}

impl<'h, 'c, ML: ModuleLoader> ImplBuilder<'h, 'c, ML> {
    const fn new(host: &'h mut Host<'c, ML>, target: TypeRef, origin_trait: Option<(TraitRef, Vec<TypeRef>)>) -> Self {
        Self {
            host,
            target,
            origin_trait,
            functions: Vec::new(),
        }
    }

    /// Adds the method `name`, implemented by `func`, which takes the value
    /// first (`self`). Other parameters are called `a0`, `a1`...
    #[must_use]
    pub fn method<Args, F: IntoHostFunction<Args>>(self, name: &str, func: F) -> Self {
        let (params, _) = F::signature(&self.host.compiler.host_types, &mut self.host.compiler.type_context.tcx);
        let names = (1..params.len()).map(|index| format!("a{}", index - 1)).collect::<Vec<_>>();

        self.method_named(name, &names.iter().map(String::as_str).collect::<Vec<_>>(), func)
    }

    /// Adds the method `name` whose parameters after `self` are called
    /// `arg_names`, implemented by `func`.
    ///
    /// # Panics
    ///
    /// Panics if `func` doesn't take the value first, or if the number of
    /// names doesn't match its other parameters.
    #[must_use]
    pub fn method_named<Args, F: IntoHostFunction<Args>>(mut self, name: &str, arg_names: &[&str], func: F) -> Self {
        let (params, returns) = F::signature(&self.host.compiler.host_types, &mut self.host.compiler.type_context.tcx);

        assert!(
            params
                .first()
                .is_some_and(|&first| self.host.compiler.type_context.tcx.is_same(first, self.target)),
            "`{name}` must take the value as its first argument"
        );
        assert_eq!(params.len() - 1, arg_names.len(), "`{name}` takes {} arguments after `self`", params.len() - 1);

        let context = self.host.keep(func);
        let names = std::iter::once(String::from("self"))
            .chain(arg_names.iter().map(|&name| name.to_owned()))
            .collect();

        self.functions
            .push((name.to_owned(), names, params, returns, FunctionBody::Host { code: F::shim(), context }));

        self
    }

    /// Registers the impl.
    ///
    /// # Panics
    ///
    /// Panics if a function of the implemented trait is missing.
    pub fn finish(self) {
        let Self {
            host,
            target,
            origin_trait,
            functions,
        } = self;
        let mut builder = VTableBuilder::new(&mut host.compiler.type_context, target);

        if let Some((trait_ref, trait_args)) = origin_trait {
            builder = builder.implements(trait_ref, trait_args);
        }

        for (name, names, params, returns, body) in functions {
            builder = builder.func_body(name, names, params, returns, body);
        }

        builder.finish();
    }
}

/// A compiled program with arguments `Args` and result `R`, called by the
/// host. It borrows its compiler: compiled code can't run after the compiler
/// is dropped.
pub struct ScriptFn<'c, Args, R> {
    code: usize,
    compiler: &'c CompilerInner,
    _marker: PhantomData<fn(Args) -> R>,
}

impl<Args: ScriptArgs, R: ScriptValue> ScriptFn<'_, Args, R> {
    /// Runs the program with `limits`.
    ///
    /// # Errors
    ///
    /// Returns the trap that stopped the program.
    pub fn call(&self, args: Args, limits: Limits) -> Result<R, Trap> {
        if !args.belong_to(self.compiler.heap()) {
            return Err(host_trap("a value of another program was given to this program"));
        }

        // SAFETY: the code was compiled for these types, checked by
        // `CompilerExt::script_fn`. Arguments are converted inside the run, so
        // strings are copied into the program's heap, and so is the result,
        // unless the program stopped (its result is zeroes then, which may not
        // be a valid value).
        self.compiler
            .run(limits, || {
                let result = unsafe { args.invoke::<R>(self.code) };
                // SAFETY: the state of a running program is valid.
                let stopped = sandbox::current_state().is_some_and(|state| unsafe { state.as_ref() }.trap != 0);

                (!stopped).then(|| unsafe { R::from_ret(result.assume_init()) })
            })
            .and_then(|value| value.ok_or_else(|| host_trap("the program stopped")))
    }
}

/// Compiling and calling programs with signatures checked against Rust types.
pub trait CompilerExt {
    /// Compiles `source` as the program `name`, whose top-level code gets
    /// variables `params` of types `Args` and evaluates to `R`.
    ///
    /// # Errors
    ///
    /// Returns an error if the program has errors.
    fn compile_script<Args: ScriptArgs + 'static, R: ScriptValue>(&mut self, name: &str, params: &[&str], source: &str) -> CompileResult<()> {
        self.compile_script_with::<Args, R>(name, params, source, &[])
    }

    /// Like [`CompilerExt::compile_script`], with the capabilities
    /// `capabilities` (see [`Host::capability`]): the program can use their
    /// items.
    ///
    /// # Errors
    ///
    /// Returns an error if the program has errors, or if a capability doesn't
    /// exist.
    fn compile_script_with<Args: ScriptArgs + 'static, R: ScriptValue>(
        &mut self,
        name: &str,
        params: &[&str],
        source: &str,
        capabilities: &[&str],
    ) -> CompileResult<()>;

    /// The program `name` compiled with arguments `Args` and result `R`.
    ///
    /// # Errors
    ///
    /// Returns an error if there's no such program, or if it has another
    /// signature.
    fn script_fn<Args: ScriptArgs + 'static, R: ScriptValue>(&self, name: &str) -> CompileResult<ScriptFn<'_, Args, R>>;

    /// Writes the stub of everything the host registered to
    /// `<project>/.mollie/host`, where the language server finds it (see
    /// [`HostStub::update`](crate::stub::HostStub::update)): call it after
    /// registering, e.g. on every start of the game. Returns whether files
    /// were written.
    ///
    /// # Errors
    ///
    /// Returns an error if a file can't be read, written or removed.
    fn write_host_stub(&self, project: &Path) -> io::Result<bool>;
}

/// Compiles a program for [`CompilerExt::compile_script`], with the
/// capabilities already granted.
/// Makes the C functions the host calls callbacks of types met so far through
/// (see [`ScriptCallback`]).
fn prepare_callbacks<ML: ModuleLoader>(compiler: &mut Compiler<ML>) -> CompileResult<()> {
    let pending = mem::take(&mut *compiler.host_types.callbacks.borrow_mut());

    for (key, ty) in pending {
        if compiler.inner.heap().callback_entry(key).is_none() {
            let entry = compiler.start_compiling().callback_entry(ty)?;

            compiler.inner.heap().set_callback_entry(key, entry);
        }
    }

    Ok(())
}

fn compile_checked<ML: ModuleLoader, Args: ScriptArgs + 'static, R: ScriptValue>(
    compiler: &mut Compiler<ML>,
    name: &str,
    params: &[&str],
    source: &str,
) -> CompileResult<()> {
    let tcx = &mut compiler.type_context.tcx;
    let types = Args::mollie_types(&compiler.host_types, tcx);
    let returns = R::mollie_type(&compiler.host_types, tcx);

    if params.len() != types.len() {
        return Err(CompileError::unsupported(format!("program `{name}` takes {} arguments", types.len())));
    }

    let params = params.iter().map(|&name| name.to_owned()).zip(types).collect::<Vec<_>>();

    prepare_callbacks(compiler)?;
    compiler.start_compiling().compile(name, params, Some(returns), source)?;
    compiler.program_signatures.insert(name.to_owned(), TypeId::of::<fn(Args) -> R>());

    Ok(())
}

impl<ML: ModuleLoader> CompilerExt for Compiler<ML> {
    fn compile_script_with<Args: ScriptArgs + 'static, R: ScriptValue>(
        &mut self,
        name: &str,
        params: &[&str],
        source: &str,
        capabilities: &[&str],
    ) -> CompileResult<()> {
        let modules = capabilities
            .iter()
            .map(|&capability| {
                self.capabilities
                    .get(capability)
                    .copied()
                    .ok_or_else(|| CompileError::unsupported(format!("there's no capability called `{capability}`")))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        // Names are resolved while compiling: the capabilities are visible
        // only then.
        for &module in &modules {
            self.type_context.tcx.def_registry.grant(module);
        }

        let result = compile_checked::<ML, Args, R>(self, name, params, source);

        for &module in &modules {
            self.type_context.tcx.def_registry.restrict(module);
        }

        result
    }

    fn write_host_stub(&self, project: &Path) -> io::Result<bool> {
        crate::stub::host_stub(self).update(&project.join(".mollie").join("host"))
    }

    fn script_fn<Args: ScriptArgs + 'static, R: ScriptValue>(&self, name: &str) -> CompileResult<ScriptFn<'_, Args, R>> {
        let signature = self
            .program_signatures
            .get(name)
            .ok_or_else(|| CompileError::unsupported(format!("there's no program called `{name}` compiled by `compile_script`")))?;

        if *signature != TypeId::of::<fn(Args) -> R>() {
            let found = self.inner.program_types.get(name).map(|&ty| self.type_context.tcx.display_of(ty).to_string());

            return Err(CompileError::unsupported(format!(
                "program `{name}` has the type `{}`, not `{}`",
                found.unwrap_or_default(),
                std::any::type_name::<fn(Args) -> R>()
            )));
        }

        let code = self.inner.get_entry(name);

        Ok(ScriptFn {
            code: code.ok_or_else(|| CompileError::unsupported(format!("program `{name}` isn't compiled")))?,
            compiler: &self.inner,
            _marker: PhantomData,
        })
    }
}

/// A function of a trait being registered: its name, names and types of its
/// parameters (after `self`), result type and Rust signature.
type HostTraitFunction = (String, Vec<String>, Vec<TypeRef>, TypeRef, TypeId);

/// Builds a trait registered by [`Host::trait_`].
pub struct HostTraitBuilder<'h, 'c, ML: ModuleLoader, M> {
    host: &'h mut Host<'c, ML>,
    name: String,
    functions: Vec<HostTraitFunction>,
    _marker: PhantomData<M>,
}

impl<ML: ModuleLoader, M: 'static> HostTraitBuilder<'_, '_, ML, M> {
    /// Adds the function `name`, taking `self` and arguments `Args` called
    /// `arg_names`, and returning `R`. The host calls it with
    /// [`ScriptObject::call`].
    ///
    /// # Panics
    ///
    /// Panics if the number of names doesn't match `Args`.
    #[must_use]
    pub fn method<Args: ScriptArgs + 'static, R: ScriptValue>(mut self, name: &str, arg_names: &[&str]) -> Self {
        let compiler = &mut *self.host.compiler;
        let params = Args::mollie_types(&compiler.host_types, &mut compiler.type_context.tcx);
        let returns = R::mollie_type(&compiler.host_types, &mut compiler.type_context.tcx);

        assert_eq!(params.len(), arg_names.len(), "`{name}` takes {} arguments after `self`", params.len());

        self.functions.push((
            name.to_owned(),
            arg_names.iter().map(|&name| name.to_owned()).collect(),
            params,
            returns,
            TypeId::of::<fn(Args) -> R>(),
        ));

        self
    }

    /// Registers the trait.
    ///
    /// # Panics
    ///
    /// Panics if the functions the host calls the trait's functions through
    /// can't be compiled.
    pub fn finish(self) -> TraitRef {
        let Self { host, name, functions, .. } = self;
        let mut builder = TraitBuilder::new(&mut host.compiler.type_context.tcx, name.as_str());

        for (function, names, params, returns, _) in &functions {
            builder = builder.func(function.as_str(), names.iter().map(String::as_str).zip(params.iter().copied()), *returns);
        }

        let trait_ref = builder.finish_in_module(host.module);
        let ty = host.compiler.type_context.tcx.types.get_or_add(Type::Trait(trait_ref, Box::new([])));

        host.compiler.host_types.insert(TypeId::of::<M>(), ty);

        for (index, (function, _, params, returns, signature)) in functions.into_iter().enumerate() {
            // Values of value types are passed to compiled code in chunks, not
            // like C passes them: the host calls through a C function.
            let entry = host
                .compiler
                .start_compiling()
                .method_entry(&params, returns)
                .unwrap_or_else(|error| panic!("`{function}` of `{name}` can't be called by the host: {error:?}"));

            host.compiler
                .inner
                .heap()
                .set_trait_function(TypeId::of::<M>(), function, TraitFunction { index, signature, entry });
        }

        trait_ref
    }
}

/// Representation of trait objects in compiled code: the value and its
/// vtable (the hash of its type, then its functions in the trait's order).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct RawObject {
    data: *mut u8,
    vtable: *const usize,
}

/// A trait object of a script (a value of a trait registered with
/// [`Host::trait_`], marked by `M`).
///
/// It keeps the value alive while it's held,
/// and knows its program: its functions run in it, and can't be called once
/// the program's compiler is dropped.
pub struct ScriptObject<M> {
    raw: RawObject,
    state: NonNull<VmState>,
    heap: *const Heap,
    alive: Weak<()>,
    _marker: PhantomData<fn() -> M>,
}

impl<M: 'static> ScriptObject<M> {
    /// Calls the trait function `name` of the value with `args`, in a new run
    /// of its program with `limits`.
    ///
    /// # Errors
    ///
    /// Returns the trap that stopped the program, or a trap if the function
    /// doesn't exist or has another signature, if its program was unloaded,
    /// or if an argument belongs to another program.
    pub fn call<Args: ScriptArgs + 'static, R: ScriptValue>(&self, name: &str, args: Args, limits: Limits) -> Result<R, Trap> {
        if self.alive.upgrade().is_none() {
            return Err(host_trap("the program of the object was unloaded"));
        }

        // SAFETY: the heap is alive (checked above).
        let heap = unsafe { &*self.heap };
        let Some(TraitFunction { index, signature, entry }) = heap.trait_function(TypeId::of::<M>(), name) else {
            return Err(host_trap(&format!("the trait has no function called `{name}` registered by the host")));
        };

        if signature != TypeId::of::<fn(Args) -> R>() {
            return Err(host_trap(&format!(
                "`{name}` has another signature than `{}`",
                std::any::type_name::<fn(Args) -> R>()
            )));
        }

        if !args.belong_to(heap) {
            return Err(host_trap("a value of another program was given to this program"));
        }

        // SAFETY: vtables of a trait have its functions after the type's hash.
        let code = unsafe { self.raw.vtable.add(1 + index).read() };
        let data = self.raw.data;

        // SAFETY: the state belongs to the program of the value, which is
        // alive.
        unsafe {
            sandbox::run(self.state, limits, || {
                // SAFETY: the function takes the value and `Args` and returns
                // `R` (checked above), and the entry was compiled for them.
                let value = args.invoke_method::<R>(entry, code, data);
                let stopped = self.state.as_ref().trap != 0;

                // SAFETY: written by the function, since it didn't stop.
                (!stopped).then(|| R::from_ret(value.assume_init()))
            })
        }
        .and_then(|value| value.ok_or_else(|| host_trap("the program stopped")))
    }

    /// Hash of the type of the value, as in its vtable.
    pub const fn type_hash(&self) -> u64 {
        // SAFETY: vtables start with the hash of the type.
        unsafe { self.raw.vtable.read() as u64 }
    }
}

impl<M> Drop for ScriptObject<M> {
    fn drop(&mut self) {
        if !self.raw.data.is_null() && self.alive.upgrade().is_some() {
            // SAFETY: the heap is alive.
            unsafe { &*self.heap }.unroot(self.raw.data.cast());
        }
    }
}

/// The program running on this thread: its state, heap and liveness, for
/// handles of its values held by the host.
fn running_program() -> (NonNull<VmState>, *const Heap, Weak<()>) {
    let state = sandbox::current_state().expect("values of programs are given to the host by a running program");
    // SAFETY: the state of a running program is valid, and has a heap.
    let heap = unsafe { state.as_ref() }.heap;

    (state, heap, unsafe { &*heap }.alive_token())
}

unsafe impl<M: 'static> HostValue for ScriptObject<M> {
    type Arg = RawObject;
    type Ret = RawObject;

    const LAYOUT: (usize, usize) = (2 * mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, _: &mut TyCtxt) -> TypeRef {
        registered::<M>(types)
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        let (state, heap, alive) = running_program();

        // The value stays alive while the host holds it.
        unsafe { &*heap }.root(arg.data.cast());

        Self {
            raw: arg,
            state,
            heap,
            alive,
            _marker: PhantomData,
        }
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        unsafe { Self::from_arg(ret) }
    }

    fn into_ret(self) -> Self::Ret {
        if self.alive.upgrade().is_none() || sandbox::current_state() != Some(self.state) {
            foreign_value();

            // Not used: the program stops right after the host function.
            return RawObject {
                data: ptr::null_mut(),
                vtable: ptr::null(),
            };
        }

        // The value is the program's now (see `ScriptCallback::into_ret`).
        let raw = self.raw;

        drop(self);

        raw
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.data);
    }
}

impl<M: 'static> ScriptValue for ScriptObject<M> {
    fn to_arg(&self) -> Self::Arg {
        self.raw
    }

    fn belongs_to(&self, heap: &Heap) -> bool {
        self.raw.data.is_null() || heap.contains(self.raw.data.cast())
    }
}

/// An array of a script (`T[]`), kept alive while the host holds it.
pub struct GcArray<T> {
    array: *mut Array,
    heap: *const Heap,
    alive: Weak<()>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: HostValue> GcArray<T> {
    /// The array, if its program is still loaded.
    fn array(&self) -> Option<&Array> {
        // SAFETY: the array is rooted while its heap is alive.
        self.alive.upgrade().map(|_| unsafe { &*self.array })
    }

    /// Number of elements (0 once the program is unloaded).
    pub fn len(&self) -> usize {
        self.array().map_or(0, |array| array.length)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element at `index`.
    pub fn get(&self, index: usize) -> Option<T> {
        let array = self.array()?;

        // SAFETY: elements are values of `T` in memory, in bounds.
        (index < array.length).then(|| unsafe { T::from_ret(array.ptr.cast::<T::Ret>().add(index).read_unaligned()) })
    }

    /// Replaces the element at `index`. Returns `false` if it's out of
    /// bounds.
    pub fn set(&mut self, index: usize, value: T) -> bool {
        let Some(array) = self.array() else {
            return false;
        };

        if index >= array.length {
            return false;
        }

        // SAFETY: in bounds, and elements are values of `T` in memory.
        unsafe { array.ptr.cast::<T::Ret>().add(index).write_unaligned(value.into_ret()) };

        true
    }

    /// Adds `value` at the end. Garbage may be collected (values the host
    /// holds must be rooted). Returns `false` if the heap limit doesn't allow
    /// it, or if the program is unloaded.
    pub fn push(&mut self, value: T) -> bool {
        if self.alive.upgrade().is_none() {
            return false;
        }

        // SAFETY: the heap is alive.
        let heap = unsafe { &*self.heap };
        let length = self.len();
        let ret = value.into_ret();
        // SAFETY: `Ret` is the memory representation of the element, and
        // `Arg` is `Ret` for every value whose references matter.
        let references = {
            let mut references = Vec::new();

            if mem::size_of::<T::Ret>() == mem::size_of::<T::Arg>() {
                let arg = unsafe { mem::transmute_copy::<T::Ret, T::Arg>(&ret) };

                T::references(&arg, &mut references);
            }

            references
        };

        // The new element isn't stored yet: it's kept alive by hand.
        for &reference in &references {
            heap.root(reference.cast());
        }

        // SAFETY: the array belongs to the heap.
        let grown = unsafe { heap.realloc_array(self.array, length + 1) };

        for &reference in &references {
            heap.unroot(reference.cast());
        }

        if grown {
            // SAFETY: the array has room for it now.
            unsafe { (*self.array).ptr.cast::<T::Ret>().add(length).write_unaligned(ret) };
        }

        grown
    }

    /// The elements.
    pub fn to_vec(&self) -> Vec<T> {
        (0..self.len()).filter_map(|index| self.get(index)).collect()
    }
}

impl<T> Drop for GcArray<T> {
    fn drop(&mut self) {
        if self.alive.upgrade().is_some() {
            // SAFETY: the heap is alive.
            unsafe { &*self.heap }.unroot(self.array.cast());
        }
    }
}

unsafe impl<T: HostValue> HostValue for GcArray<T> {
    type Arg = *mut Array;
    type Ret = *mut Array;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type(types, tcx);

        tcx.types.get_or_add(Type::Array(element, None))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        let (_, heap, alive) = running_program();

        unsafe { &*heap }.root(arg.cast());

        Self {
            array: arg,
            heap,
            alive,
            _marker: PhantomData,
        }
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        unsafe { Self::from_arg(ret) }
    }

    fn into_ret(self) -> Self::Ret {
        let belongs = self.alive.upgrade().is_some() && sandbox::with_current_heap(|heap| ptr::eq(heap, self.heap)).unwrap_or(false);

        if !belongs {
            foreign_value();

            return ptr::null_mut();
        }

        let array = self.array;

        drop(self);

        array
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.cast());
    }
}

impl<T: HostValue> ScriptValue for GcArray<T> {
    fn to_arg(&self) -> Self::Arg {
        self.array
    }

    fn belongs_to(&self, heap: &Heap) -> bool {
        heap.contains(self.array.cast())
    }
}
