use std::{
    any::{Any, TypeId},
    io,
    marker::PhantomData,
    mem,
    panic::{self, AssertUnwindSafe},
    path::Path,
    ptr::{self, NonNull},
    sync::Weak,
};

use mollie_index::Idx;

pub use crate::compiler::HostTypes;
use crate::{
    AdtBuilder, GcPtr, MolStr, MollieTypeOf, TraitBuilder, VTableBuilder,
    compiler::{
        Compiler, CompilerInner,
        allocator::{self, Array, Heap, Releaser, TraitFunction},
        error::{CompileError, CompileResult},
        sandbox::{self, Limits, Trap, TrapKind, VmState},
    },
    constants::ConstantValue,
    shared::Span,
    typed_ast::{FileModuleLoader, FunctionBody, ModuleLoader},
    typing::{Accessor, ArrayKind, DefRegistry, Func, ModuleId, ModuleItem, PrimitiveType, TraitFuncRef, TraitRef, TyCtxt, Type, TypeRef},
};

/// # Safety
///
/// Implementations must pass and return values the way compiled code does for
/// `mollie_type`. `Arg` and `Ret` must have its layout, `LAYOUT` its size and
/// alignment, and `BY_POINTER` must say whether it's passed by pointer.
/// `references` must list every object of the heap an argument holds, or the
/// collector may free them while the host uses them.
pub unsafe trait HostValue: Sized + 'static {
    /// What compiled code passes as an argument of this type.
    type Arg: Copy + 'static;
    /// What compiled code returns as a result of this type, in its memory
    /// layout.
    type Ret: 'static;

    /// The size and alignment of values in compiled code's memory.
    const LAYOUT: (usize, usize);

    /// Whether values are passed by pointer, as value types are. Compiled code
    /// passes them differently than C, so the host calls programs through C
    /// functions made for each signature.
    const BY_POINTER: bool = false;

    /// The Mollie type of these values, made in `tcx`. Types of the host are
    /// looked up in `types`.
    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef;

    /// # Safety
    ///
    /// `arg` must be a value of this type passed by compiled code.
    unsafe fn from_arg(arg: Self::Arg) -> Self;

    /// The value, as compiled code returns it.
    fn into_ret(self) -> Self::Ret;

    /// # Safety
    ///
    /// `ret` must be a value of this type returned by compiled code.
    unsafe fn from_ret(ret: Self::Ret) -> Self;

    /// Objects of the heap that `arg` holds, which must stay alive while the
    /// host has it.
    fn references(_arg: &Self::Arg, _references: &mut Vec<*mut u8>) {}
}

/// A [`HostValue`] the host can also give to programs, as an argument of a
/// program, a callback or a function of a trait object.
pub trait ScriptValue: HostValue {
    /// The value, as compiled code takes it.
    fn to_arg(&self) -> Self::Arg;

    /// Whether the value can be given to programs of `heap`. An object can only
    /// be given to programs of its own heap.
    fn belongs_to(&self, _heap: &Heap) -> bool {
        true
    }
}

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

fn foreign_value() {
    sandbox::raise("a value of another program was given to this program");
}

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

plain_values!(i8, u8, i16, u16, i32, u32, i64, u64, isize, usize, f32, f64);

/// # Safety
///
/// Values must be stored in compiled code's memory exactly as `Self`, without
/// references to objects of the heap. Arrays of them are read and written in
/// place.
pub unsafe trait PlainValue: HostValue {}

macro_rules! plain_value_markers {
    ($($ty:ty),*) => {
        $(
            // SAFETY: numbers and booleans are stored as themselves.
            unsafe impl PlainValue for $ty {}
        )*
    };
}

plain_value_markers!(i8, u8, i16, u16, i32, u32, i64, u64, isize, usize, f32, f64, bool);

// Arrays of plain values are copied: from compiled code's arrays for
// arguments, into new arrays for results.
unsafe impl<T: PlainValue> HostValue for Vec<T> {
    type Arg = *mut Array;
    type Ret = *mut Array;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type(types, tcx);

        tcx.types.get_or_add(Type::Array(element, ArrayKind::Growable))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        // SAFETY: an array of compiled code, whose elements are values of `T`
        // in memory.
        unsafe {
            let array = &*arg;

            (0..array.length)
                .map(|index| T::from_ret(array.ptr.cast::<T::Ret>().add(index).read_unaligned()))
                .collect()
        }
    }

    fn into_ret(self) -> Self::Ret {
        sandbox::with_current_heap(|heap| {
            // No collection here: the array is only referenced by this
            // function until it's returned (like new strings).
            // SAFETY: elements hold no references, so their layout is just
            // their size (see `PlainValue`).
            let array = unsafe { heap.alloc_array(heap.layout_of::<T::Ret>(), self.len(), false) }.cast::<Array>();

            if array.is_null() {
                return array;
            }

            // SAFETY: the array has room for `self.len()` elements.
            unsafe {
                let data = (*array).ptr.cast::<T::Ret>();

                for (index, value) in self.into_iter().enumerate() {
                    data.add(index).write_unaligned(value.into_ret());
                }
            }

            array
        })
        // Only returned to a running program.
        .unwrap_or(ptr::null_mut())
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        unsafe { Self::from_arg(ret) }
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.cast());
    }
}

// Fixed arrays of plain values (`T[N]`) are value types: passed by pointer and
// returned by value, like value structs. Their elements are laid out like
// Rust's.
unsafe impl<T: PlainValue + Copy, const N: usize> HostValue for [T; N] {
    type Arg = *const Self;
    type Ret = Self;

    const BY_POINTER: bool = true;
    const LAYOUT: (usize, usize) = (mem::size_of::<Self>(), mem::align_of::<Self>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type(types, tcx);
        let size = tcx.types.get_or_add(Type::Size(N));

        tcx.types.get_or_add(Type::Array(element, ArrayKind::Fixed(size)))
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

impl<T: PlainValue + Copy, const N: usize> ScriptValue for [T; N] {
    fn to_arg(&self) -> Self::Arg {
        // Valid while the arguments are passed: they're kept until the call
        // returns.
        self
    }
}

// SAFETY: the elements hold no GC references.
unsafe impl<T: PlainValue + Copy, const N: usize> PlainValue for [T; N] {}

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

/// A Rust value given to programs by reference. Programs can't see inside it,
/// only pass it back to the host.
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
    /// `value` must outlive every use of the handle. Handles are `Copy` and
    /// programs can keep them, so that includes every copy.
    pub unsafe fn new(value: &mut T) -> Self {
        Self(NonNull::from(value))
    }

    /// # Safety
    ///
    /// The value must still be alive, and must not be borrowed anywhere else
    /// while the result lives, including through another copy of the handle.
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

/// Stops the running program with an error from the host. The message is
/// formatted like [`format!`].
#[macro_export]
macro_rules! raise {
    ($($message:tt)+) => {
        $crate::compiler::sandbox::raise(::std::format!($($message)+))
    };
}

pub use crate::raise;

/// Implements [`HostValue`] and [`ScriptValue`] for a value type registered
/// with [`Host::value_type`]. Its values are copied, and passed by pointer.
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

        // SAFETY: value types crossing the boundary hold no GC references.
        unsafe impl $crate::host::PlainValue for $ty {}
    };
}

/// A function value of a program, kept by the host to call later.
pub struct ScriptCallback<Args, R> {
    code: usize,
    env: *mut u8,
    entry: Option<usize>,
    state: NonNull<VmState>,
    heap: *const Heap,
    alive: Weak<()>,
    releaser: Releaser,
    _marker: PhantomData<fn(Args) -> R>,
}

// SAFETY: the callback only uses its heap given proof that its program is
// used by the current thread (a run of it, or its compiler), and unroots its
// environment through a `Releaser` (see `ProgramAccess`).
unsafe impl<Args, R> Send for ScriptCallback<Args, R> {}

/// Proof that the current thread has access to a program's heap, because the
/// program is running or its compiler is borrowed. Values of a program can only
/// be used with it.
pub trait ProgramAccess {
    /// Whether it proves access to `heap`.
    fn grants(&self, heap: *const Heap) -> bool;
}

impl<ML: ModuleLoader> ProgramAccess for Compiler<ML> {
    fn grants(&self, heap: *const Heap) -> bool {
        self.inner.grants(heap)
    }
}

impl ProgramAccess for CompilerInner {
    fn grants(&self, heap: *const Heap) -> bool {
        ptr::eq(&raw const *self.heap, heap)
    }
}

/// Access to the heap of the program running on this thread, if any.
#[derive(Debug, Clone, Copy)]
pub struct Running;

impl ProgramAccess for Running {
    fn grants(&self, heap: *const Heap) -> bool {
        sandbox::with_current_heap(|current| ptr::eq(current, heap)).unwrap_or(false)
    }
}

fn no_access() -> Trap {
    host_trap("the program of this value isn't used by this thread: pass its compiler")
}

/// A function value as compiled code passes it, as its code and its environment
/// of captured variables.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct RawFunction {
    code: usize,
    env: *mut u8,
}

fn host_trap(message: &str) -> Trap {
    Trap::new(TrapKind::Host, Some(message.to_owned()))
}

impl<Args: ScriptArgs, R: ScriptValue> ScriptCallback<Args, R> {
    /// Calls it from a host function its program called, under the limits of
    /// that run.
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

    /// Calls it with `limits`. It needs access to its program.
    pub fn call_with_limits(&self, access: &impl ProgramAccess, args: Args, limits: Limits) -> Result<R, Trap> {
        if self.alive.upgrade().is_none() {
            return Err(host_trap("the program of the callback was unloaded"));
        }

        if !access.grants(self.heap) {
            return Err(no_access());
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

enum Returned<R: HostValue> {
    Arg(R::Arg),
    Ret(mem::MaybeUninit<R::Ret>),
}

impl<R: HostValue> Returned<R> {
    unsafe fn read(self) -> R {
        match self {
            Self::Arg(arg) => unsafe { R::from_arg(arg) },
            Self::Ret(ret) => unsafe { R::from_ret(ret.assume_init()) },
        }
    }
}

impl<Args, R> Drop for ScriptCallback<Args, R> {
    fn drop(&mut self) {
        // From any thread: unrooted before the next collection.
        self.releaser.unroot(self.env.cast());
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
            releaser: heap_ref.releaser(),
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

        // The value is the program's now (its root is released at the next
        // collection, when the program holds it).
        drop(self);

        raw
    }

    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.env);
    }
}

/// The arguments the host gives when it calls a program or a callback.
pub trait ScriptArgs: Sized {
    /// Their Mollie types, in order.
    fn mollie_types(types: &HostTypes, tcx: &mut TyCtxt) -> Vec<TypeRef>;

    /// Whether every argument can be given to programs of `heap` (see
    /// [`ScriptValue::belongs_to`]).
    fn belong_to(&self, heap: &Heap) -> bool;

    /// Whether an argument is passed by pointer (see
    /// [`HostValue::BY_POINTER`]).
    fn by_pointer() -> bool;

    /// Calls the entry of a program, returning the slot its result was written
    /// to.
    ///
    /// # Safety
    ///
    /// `code` must be the entry of a program taking these arguments and
    /// returning `R`.
    unsafe fn invoke<R: HostValue>(self, code: usize) -> mem::MaybeUninit<R::Ret>;

    /// Calls a function value with its environment `env`.
    ///
    /// # Safety
    ///
    /// `code` must be the code of a function value taking these arguments and
    /// its environment `env`, and returning `R`.
    unsafe fn invoke_with_env<R: HostValue>(self, code: usize, env: *mut u8) -> R::Arg;

    /// Calls a function of a vtable with its value `data` through `entry`,
    /// returning the slot its result was written to.
    ///
    /// # Safety
    ///
    /// `entry` must be the method entry for these arguments and `R`, and `code`
    /// the code of a function of a vtable taking the value and these arguments,
    /// and returning `R`.
    unsafe fn invoke_method<R: HostValue>(self, entry: usize, code: usize, data: *mut u8) -> mem::MaybeUninit<R::Ret>;
}

/// Rust functions and closures the host gives programs, taking `Args`.
pub trait IntoHostFunction<Args>: Sized + Send + 'static {
    /// The Mollie types of its parameters, and of what it returns.
    fn signature(types: &HostTypes, tcx: &mut TyCtxt) -> (Vec<TypeRef>, TypeRef);

    /// The C function compiled code calls it through.
    fn shim() -> usize;
}

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
            Func: Fn($($arg),*) -> Ret + Send + 'static,
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

/// Registers functions, types and traits of the host in a compiler, for its
/// programs.
pub struct Host<'c, ML: ModuleLoader> {
    compiler: &'c mut Compiler<ML>,
    module: ModuleId,
}

impl<'c, ML: ModuleLoader> Host<'c, ML> {
    /// Registers into the root module of `compiler`, which every program sees.
    pub const fn new(compiler: &'c mut Compiler<ML>) -> Self {
        Self {
            compiler,
            module: ModuleId::ZERO,
        }
    }

    /// Registers into the capability `name` from now on. A capability is a
    /// module that only programs compiled with it can see (see
    /// [`ScriptOptions::capabilities`]).
    pub fn capability(&mut self, name: &str) -> &mut Self {
        self.enter_module(name, true)
    }

    /// Registers into the module `name` from now on, seen by every program.
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

    /// Registers into the root module from now on.
    pub const fn everywhere(&mut self) -> &mut Self {
        self.module = ModuleId::ZERO;
        self
    }

    /// The compiler's types.
    pub const fn tcx(&mut self) -> &mut TyCtxt {
        &mut self.compiler.type_context.tcx
    }

    /// The module that registrations go to.
    pub const fn current_module(&self) -> ModuleId {
        self.module
    }

    /// # Safety
    ///
    /// `ty` must be laid out in compiled code exactly as `T`, because host
    /// functions taking a `T` read it from values of `ty`.
    pub unsafe fn register_type<T: 'static>(&mut self, ty: TypeRef) -> &mut Self {
        self.compiler.host_types.insert(TypeId::of::<T>(), ty);
        self
    }

    /// The Mollie type of values of `T`.
    pub fn type_of<T: HostValue>(&mut self) -> TypeRef {
        T::mollie_type(&self.compiler.host_types, &mut self.compiler.type_context.tcx)
    }

    fn keep<F: Send + 'static>(&mut self, func: F) -> usize {
        let func = Box::new(func);
        let context = ptr::from_ref::<F>(&func).addr();

        self.compiler.host_data.push(func);

        context
    }

    /// Registers the function `name`, with its arguments named `a0`, `a1` and
    /// so on.
    pub fn function<Args, F: IntoHostFunction<Args>>(&mut self, name: &str, func: F) -> &mut Self {
        let (params, _) = F::signature(&self.compiler.host_types, &mut self.compiler.type_context.tcx);
        let names = (0..params.len()).map(|index| format!("a{index}")).collect::<Vec<_>>();

        self.function_named(name, &names.iter().map(String::as_str).collect::<Vec<_>>(), func)
    }

    /// Registers the function `name`, with its arguments named `arg_names`. If
    /// the last name starts with `..`, the function is variadic, and that
    /// parameter must be a [`Slice`]. It panics if there are more or fewer
    /// names than arguments, or if it returns a view.
    pub fn function_named<Args, F: IntoHostFunction<Args>>(&mut self, name: &str, arg_names: &[&str], func: F) -> &mut Self {
        let (params, returns) = F::signature(&self.compiler.host_types, &mut self.compiler.type_context.tcx);

        assert_eq!(params.len(), arg_names.len(), "`{name}` takes {} arguments", params.len());

        let types = &self.compiler.type_context.tcx.types;
        let is_view = |ty: TypeRef| matches!(types[ty], Type::Array(_, ArrayKind::View { .. }));

        assert!(!is_view(returns), "`{name}` can't return a view (`Slice`): it would outlive the array");

        // `..args`: the last parameter is variadic, a view of the arguments
        // past the others.
        let variadic = arg_names.last().is_some_and(|name| name.starts_with(".."));

        assert!(
            !variadic || params.last().is_some_and(|&ty| is_view(ty)),
            "the variadic parameter of `{name}` must be a `Slice`"
        );

        let arg_names = arg_names.iter().map(|name| name.trim_start_matches("..").to_owned()).collect();

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
                    arg_names,
                    ty,
                },
                Span::default(),
            )
            .unwrap_or_else(|error| panic!("can't register `{name}`: {:?}", error.error));

        if variadic {
            self.compiler.type_context.tcx.def_registry.variadic_functions.insert(func_ref);
        }

        self.compiler
            .type_context
            .functions
            .insert(func_ref, FunctionBody::Host { code: F::shim(), context });

        self
    }

    /// Registers the struct `name` for `T`. Programs share its values by
    /// reference.
    pub fn object<T: 'static>(&mut self, name: &str) -> TypeBuilder<'_, 'c, ML, T> {
        TypeBuilder::new(self, name, Kind::Object)
    }

    /// Registers the struct `name` for `T` as a value type, which programs
    /// copy.
    pub fn value_type<T: 'static>(&mut self, name: &str) -> TypeBuilder<'_, 'c, ML, T> {
        TypeBuilder::new(self, name, Kind::Value)
    }

    /// Registers the enum `name` for `T`. Programs share its values by
    /// reference.
    pub fn enum_<T: 'static>(&mut self, name: &str) -> EnumBuilder<'_, 'c, ML, T> {
        EnumBuilder::new(self, name.to_owned(), Kind::Object)
    }

    /// Registers the enum `name` for `T` as a value type, which programs copy.
    pub fn value_enum<T: 'static>(&mut self, name: &str) -> EnumBuilder<'_, 'c, ML, T> {
        EnumBuilder::new(self, name.to_owned(), Kind::Value)
    }

    /// Registers `name` for `T` as an [`Opaque`] type, which programs can't see
    /// inside.
    pub fn opaque<T: 'static>(&mut self, name: &str) -> TypeRef {
        let adt = AdtBuilder::new_opaque(&mut self.compiler.type_context.tcx, name).finish_in_module(self.module);
        let ty = self.compiler.type_context.tcx.types.get_or_add(Type::Adt(adt, Box::new([])));

        self.compiler.host_types.insert(TypeId::of::<T>(), ty);

        ty
    }

    /// Registers the trait `name` for `M`, which programs implement and the
    /// host calls (see [`ScriptObject`]).
    pub fn trait_<M: 'static>(&mut self, name: &str) -> HostTraitBuilder<'_, 'c, ML, M> {
        HostTraitBuilder {
            host: self,
            name: name.to_owned(),
            functions: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Adds methods and properties to `T`.
    pub fn methods<T: HostValue>(&mut self) -> ImplBuilder<'_, 'c, ML> {
        let target = self.type_of::<T>();

        ImplBuilder::new(self, target, None)
    }

    /// Implements `trait_ref`, with the arguments `trait_args`, for `T`.
    pub fn implement<T: HostValue>(&mut self, trait_ref: TraitRef, trait_args: &[TypeRef]) -> ImplBuilder<'_, 'c, ML> {
        let target = self.type_of::<T>();

        ImplBuilder::new(self, target, Some((trait_ref, trait_args.to_vec())))
    }

    /// The [stub](crate::stub) of what's registered so far, as Mollie source.
    pub fn declarations(&self) -> String {
        crate::stub::host_stub(self.compiler).to_string()
    }
}

enum Kind {
    Object,
    Value,
}

/// Registers a struct of the host, made by [`Host::object`] or
/// [`Host::value_type`]. Its fields must be the fields of `T` in the same
/// order, and `T` must be `#[repr(C)]`.
pub struct TypeBuilder<'h, 'c, ML: ModuleLoader, T> {
    host: &'h mut Host<'c, ML>,
    name: String,
    kind: Kind,
    fields: Vec<(String, TypeRef, Option<ConstantValue>)>,
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

    /// Adds a field called `name` of `F`, which must be given.
    #[must_use]
    pub fn field<F: HostValue>(self, name: &str) -> Self {
        self.push_field::<F>(name, None)
    }

    /// Adds a field called `name` of `F`. When a program leaves it out, it's
    /// `default`.
    #[must_use]
    pub fn field_default<F: HostValue + Into<ConstantValue>>(self, name: &str, default: F) -> Self {
        self.push_field::<F>(name, Some(default.into()))
    }

    /// Adds a field called `name` of `F`, with a default of another type, like
    /// a `&str` for a string field. When a program leaves the field out, it's
    /// `default`.
    #[must_use]
    pub fn field_constant<F: HostValue>(self, name: &str, default: impl Into<ConstantValue>) -> Self {
        self.push_field::<F>(name, Some(default.into()))
    }

    fn push_field<F: HostValue>(mut self, name: &str, default: Option<ConstantValue>) -> Self {
        let ty = self.host.type_of::<F>();
        let (size, align) = F::LAYOUT;

        self.size = self.size.next_multiple_of(align.max(1)) + size;
        self.align = self.align.max(align);
        self.fields.push((name.to_owned(), ty, default));

        self
    }

    /// Registers it. It panics if its fields don't match `T`'s layout.
    pub fn finish(self) -> TypeRef {
        let size = self.size.next_multiple_of(self.align);

        assert!(
            size == mem::size_of::<T>() && self.align == mem::align_of::<T>(),
            "fields of `{}` don't match the layout of `{}`: is it `#[repr(C)]`, with the same fields in the same order?",
            self.name,
            std::any::type_name::<T>()
        );

        let tcx = &mut self.host.compiler.type_context.tcx;
        let mut builder = match self.kind {
            Kind::Object => AdtBuilder::new_struct(tcx, self.name.as_str()),
            Kind::Value => AdtBuilder::new_value_struct(tcx, self.name.as_str()),
        };

        for (name, ty, default) in &self.fields {
            builder = builder.field_ty_default(name.as_str(), *ty, default.clone());
        }

        let adt = builder.finish_in_module(self.host.module);
        let ty = self.host.compiler.type_context.tcx.types.get_or_add(Type::Adt(adt, Box::new([])));

        self.host.compiler.host_types.insert(TypeId::of::<T>(), ty);

        ty
    }
}

/// A variant of an [`EnumBuilder`], with its name, fields, size and alignment.
pub type EnumVariant = (String, Vec<(String, TypeRef)>, usize, usize);

/// Registers an enum of the host, made by [`Host::enum_`] or
/// [`Host::value_enum`]. Its variants must be the variants of `T` in the same
/// order, and `T` must be `#[repr(usize)]`.
pub struct EnumBuilder<'h, 'c, ML: ModuleLoader, T> {
    host: &'h mut Host<'c, ML>,
    name: String,
    kind: Kind,
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

    /// Adds a variant called `name`. Fields added after it belong to it.
    #[must_use]
    pub fn variant(mut self, name: &str) -> Self {
        self.variants
            .push((name.to_owned(), Vec::new(), mem::size_of::<usize>(), mem::align_of::<usize>()));

        self
    }

    /// Adds a field called `name` of `F` to the last variant. It panics if
    /// there's no variant yet.
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

    /// Registers it. It panics if it has no variants, or they don't match `T`'s
    /// layout.
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
        let mut builder = match self.kind {
            Kind::Object => AdtBuilder::new_enum(tcx, self.name.as_str()),
            Kind::Value => AdtBuilder::new_value_enum(tcx, self.name.as_str()),
        };

        for (variant, fields, ..) in &self.variants {
            builder = builder.variant(variant.as_str());

            for (name, ty) in fields {
                builder = builder.field_ty(name.as_str(), *ty);
            }
        }

        let adt = builder.finish_in_module(self.host.module);
        let ty = self.host.compiler.type_context.tcx.types.get_or_add(Type::Adt(adt, Box::new([])));

        self.host.compiler.host_types.insert(TypeId::of::<T>(), ty);

        ty
    }
}

type ImplFunction = (String, Vec<String>, Vec<TypeRef>, TypeRef, FunctionBody, Option<Accessor>);

/// Adds functions to a type of the host, made by [`Host::methods`] or
/// [`Host::implement`].
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

    /// Adds the method `name`. It takes the value first, and its other
    /// arguments are named `a0`, `a1` and so on.
    #[must_use]
    pub fn method<Args, F: IntoHostFunction<Args>>(self, name: &str, func: F) -> Self {
        let (params, _) = F::signature(&self.host.compiler.host_types, &mut self.host.compiler.type_context.tcx);
        let names = (1..params.len()).map(|index| format!("a{}", index - 1)).collect::<Vec<_>>();

        self.method_named(name, &names.iter().map(String::as_str).collect::<Vec<_>>(), func)
    }

    /// Adds the method `name`. It takes the value first, and its other
    /// arguments are named `arg_names`.
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
            .push((name.to_owned(), names, params, returns, FunctionBody::Host { code: F::shim(), context }, None));

        self
    }

    fn assert_no_field(&self, name: &str) {
        let tcx = &self.host.compiler.type_context.tcx;

        if let Type::Adt(adt, _) = &tcx.types[self.target] {
            let adt = &tcx.def_registry.adt_types[*adt];

            assert!(
                adt.variants.values().all(|variant| variant.fields.values().all(|field| field.name != name)),
                "`{name}` is a field of `{}` already: a property can't have the same name",
                adt.name
            );
        }
    }

    /// Adds the property `name`, read with `getter`, which takes only the
    /// value. It panics if a field already has the name.
    #[must_use]
    pub fn property<Args, F: IntoHostFunction<Args>>(self, name: &str, getter: F) -> Self {
        let (params, returns) = F::signature(&self.host.compiler.host_types, &mut self.host.compiler.type_context.tcx);
        let void = self.host.compiler.type_context.tcx.types.core_types.void;

        assert!(params.len() == 1 && returns != void, "the getter of `{name}` takes only the value, and gives something");
        self.assert_no_field(name);

        let mut builder = self.method_named(name, &[], getter);

        if let Some(function) = builder.functions.last_mut() {
            function.5 = Some(Accessor::Getter);
        }

        builder
    }

    /// Adds a setter to the property `name`, which takes the value and the new
    /// value. It panics if a field already has the name.
    #[must_use]
    pub fn setter<Args, F: IntoHostFunction<Args>>(self, name: &str, setter: F) -> Self {
        let (params, returns) = F::signature(&self.host.compiler.host_types, &mut self.host.compiler.type_context.tcx);
        let void = self.host.compiler.type_context.tcx.types.core_types.void;

        assert!(params.len() == 2 && returns == void, "the setter of `{name}` takes the value and the new value, and gives nothing");
        self.assert_no_field(name);

        let mut builder = self.method_named(&Accessor::setter_name(name), &["value"], setter);

        if let Some(function) = builder.functions.last_mut() {
            function.5 = Some(Accessor::Setter);
        }

        builder
    }

    /// Registers the functions.
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

        for (name, names, params, returns, body, accessor) in functions {
            builder = builder.func_body(name, names, params, returns, body);

            if let Some(accessor) = accessor {
                builder = builder.accessor(accessor);
            }
        }

        builder.finish();
    }
}

/// A program compiled by [`CompilerExt::compile_script`], ready to be called.
pub struct ScriptFn<'c, Args, R> {
    code: usize,
    compiler: &'c CompilerInner,
    _marker: PhantomData<fn(Args) -> R>,
}

impl<Args: ScriptArgs, R: ScriptValue> ScriptFn<'_, Args, R> {
    /// Runs the program with `args` and `limits`.
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

/// What a program is compiled with (see [`CompilerExt::compile_script_in`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScriptOptions<'a> {
    /// Capabilities it can use (see [`Host::capability`]).
    pub capabilities: &'a [&'a str],
    /// Libraries it can use (see [`CompilerExt::load_library`]).
    pub libraries: &'a [&'a str],
    /// The package it's part of. It sees the package's private items.
    pub package: Option<&'a str>,
}

/// How a library is loaded (see [`CompilerExt::load_library`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct LibraryOptions<'a> {
    /// The package it's part of. It sees the package's private items.
    pub package: Option<&'a str>,
    /// Libraries it uses, loaded before it.
    pub dependencies: &'a [&'a str],
}

/// Compiling programs that take `Args` and return `R`, and calling them.
pub trait CompilerExt {
    /// Compiles `source` as the program `name`, whose parameters are named
    /// `params`.
    fn compile_script<Args: ScriptArgs + 'static, R: ScriptValue>(&mut self, name: &str, params: &[&str], source: &str) -> CompileResult<()> {
        self.compile_script_with::<Args, R>(name, params, source, &[])
    }

    /// Compiles `source` as the program `name`, whose parameters are named
    /// `params`, and which can use `capabilities`.
    fn compile_script_with<Args: ScriptArgs + 'static, R: ScriptValue>(
        &mut self,
        name: &str,
        params: &[&str],
        source: &str,
        capabilities: &[&str],
    ) -> CompileResult<()> {
        self.compile_script_in::<Args, R>(name, params, source, &ScriptOptions {
            capabilities,
            ..ScriptOptions::default()
        })
    }

    /// Compiles `source` as the program `name`, whose parameters are named
    /// `params`, with `options`.
    fn compile_script_in<Args: ScriptArgs + 'static, R: ScriptValue>(&mut self, name: &str, params: &[&str], source: &str, options: &ScriptOptions<'_>) -> CompileResult<()>;

    /// Loads `source` as the library `name`, whose submodules are files in
    /// `folder`. Only programs given it in [`ScriptOptions::libraries`] can use
    /// it.
    fn load_library(&mut self, name: &str, source: &str, folder: &Path, options: &LibraryOptions<'_>) -> CompileResult<()>;

    /// The program `name`, if it takes `Args` and returns `R`.
    fn script_fn<Args: ScriptArgs + 'static, R: ScriptValue>(&self, name: &str) -> CompileResult<ScriptFn<'_, Args, R>>;

    /// Writes the stub of the host to `project/.mollie/host`, for tools like
    /// the language server. Returns whether it changed.
    fn write_host_stub(&self, project: &Path) -> io::Result<bool>;

    /// Writes the stub of the host to `project/.mollie/host`, declaring the
    /// parameters programs take, named `params`, as `extern let` variables.
    /// Returns whether it changed.
    fn write_host_stub_for<Args: ScriptArgs + 'static>(&mut self, project: &Path, params: &[&str]) -> io::Result<bool>;
}

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

fn grant_libraries<'n>(registry: &mut DefRegistry, names: &[&'n str]) -> CompileResult<Vec<&'n str>> {
    let mut granted = Vec::new();

    for &name in names {
        if registry.grant_root(name) {
            granted.push(name);
        } else if !registry.extern_roots.contains_key(name) {
            for &name in &granted {
                registry.restrict_root(name);
            }

            return Err(CompileError::unsupported(format!("there's no library called `{name}`")));
        }
    }

    Ok(granted)
}

impl<ML: ModuleLoader> CompilerExt for Compiler<ML> {
    fn load_library(&mut self, name: &str, source: &str, folder: &Path, options: &LibraryOptions<'_>) -> CompileResult<()> {
        let registry = &self.type_context.tcx.def_registry;

        if registry.extern_roots.contains_key(name) || registry.restricted_roots.contains_key(name) {
            return Err(CompileError::unsupported(format!("the library `{name}` is already loaded")));
        }

        if let Some(missing) = options
            .dependencies
            .iter()
            .find(|&&dependency| !registry.extern_roots.contains_key(dependency) && !registry.restricted_roots.contains_key(dependency))
        {
            return Err(CompileError::unsupported(format!("`{name}` depends on `{missing}`, which isn't loaded")));
        }

        let loader = FileModuleLoader {
            current_dir: folder.to_owned(),
        };

        self.type_context.load_library(name, source, loader, options.package, options.dependencies);

        if self.type_context.diagnostics.is_empty() {
            Ok(())
        } else {
            Err(CompileError::Type(mem::take(&mut self.type_context.diagnostics.errors).into_values().collect()))
        }
    }

    fn compile_script_in<Args: ScriptArgs + 'static, R: ScriptValue>(&mut self, name: &str, params: &[&str], source: &str, options: &ScriptOptions<'_>) -> CompileResult<()> {
        let capabilities = options.capabilities;
        let modules = capabilities
            .iter()
            .map(|&capability| {
                self.capabilities
                    .get(capability)
                    .copied()
                    .ok_or_else(|| CompileError::unsupported(format!("there's no capability called `{capability}`")))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        // Names are resolved while compiling: the capabilities and libraries
        // are visible only then.
        let libraries = grant_libraries(&mut self.type_context.tcx.def_registry, options.libraries)?;

        for &module in &modules {
            self.type_context.tcx.def_registry.grant(module);
        }

        self.type_context.program_package = options.package.map(str::to_owned);

        let result = compile_checked::<ML, Args, R>(self, name, params, source);

        self.type_context.program_package = None;

        for &module in &modules {
            self.type_context.tcx.def_registry.restrict(module);
        }

        for library in libraries {
            self.type_context.tcx.def_registry.restrict_root(library);
        }

        result
    }

    fn write_host_stub(&self, project: &Path) -> io::Result<bool> {
        crate::stub::host_stub(self).update(&project.join(".mollie").join("host"))
    }

    fn write_host_stub_for<Args: ScriptArgs + 'static>(&mut self, project: &Path, params: &[&str]) -> io::Result<bool> {
        let types = Args::mollie_types(&self.host_types, &mut self.type_context.tcx);

        if params.len() != types.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("programs take {} arguments, not {}", types.len(), params.len()),
            ));
        }

        let variables = params.iter().copied().zip(types).collect::<Vec<_>>();

        crate::stub::host_stub_with(self, &variables).update(&project.join(".mollie").join("host"))
    }

    fn script_fn<Args: ScriptArgs + 'static, R: ScriptValue>(&self, name: &str) -> CompileResult<ScriptFn<'_, Args, R>> {
        let signature = self
            .program_signatures
            .get(name)
            .ok_or_else(|| CompileError::unsupported(format!("there's no program called `{name}` compiled by `compile_script`")))?;

        if *signature != TypeId::of::<fn(Args) -> R>() {
            let found = self.inner.program_types.get(name).map_or_else(
                || String::from("other types"),
                |&ty| format!("the type `{}`", self.type_context.tcx.display_of(ty)),
            );

            return Err(CompileError::unsupported(format!(
                "program `{name}` has {found}, not `{}`",
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

type HostTraitFunction = (String, Vec<String>, Vec<TypeRef>, TypeRef, TypeId, Option<String>);

/// Registers a trait of the host (see [`Host::trait_`]).
pub struct HostTraitBuilder<'h, 'c, ML: ModuleLoader, M> {
    host: &'h mut Host<'c, ML>,
    name: String,
    functions: Vec<HostTraitFunction>,
    _marker: PhantomData<M>,
}

impl<ML: ModuleLoader, M: 'static> HostTraitBuilder<'_, '_, ML, M> {
    /// Adds the function `name`, which takes `self` and then `Args`, named
    /// `arg_names`, and returns `R`.
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
            None,
        ));

        self
    }

    /// Adds the function `name` with a default, whose body is the Mollie source
    /// `body`.
    #[must_use]
    pub fn method_default<Args: ScriptArgs + 'static, R: ScriptValue>(self, name: &str, arg_names: &[&str], body: &str) -> Self {
        let mut builder = self.method::<Args, R>(name, arg_names);

        if let Some(function) = builder.functions.last_mut() {
            function.5 = Some(body.to_owned());
        }

        builder
    }

    /// Registers the trait.
    pub fn finish(self) -> TraitRef {
        let Self { host, name, functions, .. } = self;
        let mut builder = TraitBuilder::new(&mut host.compiler.type_context.tcx, name.as_str());

        for (function, names, params, returns, _, default) in &functions {
            builder = builder.func(function.as_str(), names.iter().map(String::as_str).zip(params.iter().copied()), *returns);

            if default.is_some() {
                builder = builder.with_default();
            }
        }

        let trait_ref = builder.finish_in_module(host.module);
        let ty = host.compiler.type_context.tcx.types.get_or_add(Type::Trait(trait_ref, Box::new([])));

        host.compiler.host_types.insert(TypeId::of::<M>(), ty);

        // Defaults may use `std`, like any code.
        if functions.iter().any(|function| function.5.is_some()) && host.compiler.type_context.use_std {
            host.compiler.type_context.load_std();
        }

        for (index, (function, .., default)) in functions.iter().enumerate() {
            let Some(source) = default else {
                continue;
            };
            let func = TraitFuncRef::new(index);

            if let Err(errors) = host.compiler.type_context.check_trait_default(host.module, trait_ref, func, source) {
                let tcx = &host.compiler.type_context.tcx;
                let messages = errors.iter().map(|error| error.message(tcx)).collect::<Vec<_>>();

                panic!("the default of `{function}` of `{name}` has errors:\n{}", messages.join("\n"));
            }

            if let Some(default) = host.compiler.type_context.tcx.def_registry.traits[trait_ref].functions[func].default {
                host.compiler.host_defaults.insert(default, source.clone());
            }
        }

        for (index, (function, _, params, returns, signature, _)) in functions.into_iter().enumerate() {
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

/// A trait object as compiled code passes it, as the value and its vtable.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct RawObject {
    data: *mut u8,
    vtable: *const usize,
}

/// A value of a program that implements a trait of the host, registered with
/// [`Host::trait_`]. The host keeps it to call its functions.
pub struct ScriptObject<M> {
    raw: RawObject,
    state: NonNull<VmState>,
    heap: *const Heap,
    alive: Weak<()>,
    releaser: Releaser,
    _marker: PhantomData<fn() -> M>,
}

// SAFETY: like `ScriptCallback`.
unsafe impl<M> Send for ScriptObject<M> {}

impl<M: 'static> ScriptObject<M> {
    /// Calls its function `name` with `args` and `limits`. It needs access to
    /// its program.
    pub fn call<Args: ScriptArgs + 'static, R: ScriptValue>(&self, access: &impl ProgramAccess, name: &str, args: Args, limits: Limits) -> Result<R, Trap> {
        if self.alive.upgrade().is_none() {
            return Err(host_trap("the program of the object was unloaded"));
        }

        if !access.grants(self.heap) {
            return Err(no_access());
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

    /// The hash of its type.
    pub const fn type_hash(&self) -> u64 {
        // SAFETY: vtables start with the hash of the type.
        unsafe { self.raw.vtable.read() as u64 }
    }
}

impl<M> Drop for ScriptObject<M> {
    fn drop(&mut self) {
        // From any thread: unrooted before the next collection.
        self.releaser.unroot(self.raw.data.cast());
    }
}

fn running_program() -> (NonNull<VmState>, *const Heap, Weak<()>, Releaser) {
    let state = sandbox::current_state().expect("values of programs are given to the host by a running program");
    // SAFETY: the state of a running program is valid, and has a heap.
    let heap = unsafe { state.as_ref() }.heap;
    let heap_ref = unsafe { &*heap };

    (state, heap, heap_ref.alive_token(), heap_ref.releaser())
}

unsafe impl<M: 'static> HostValue for ScriptObject<M> {
    type Arg = RawObject;
    type Ret = RawObject;

    const LAYOUT: (usize, usize) = (2 * mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, _: &mut TyCtxt) -> TypeRef {
        registered::<M>(types)
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        let (state, heap, alive, releaser) = running_program();

        // The value stays alive while the host holds it.
        unsafe { &*heap }.root(arg.data.cast());

        Self {
            raw: arg,
            state,
            heap,
            alive,
            releaser,
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

/// An array of a program, kept by the host. Reading and writing it needs access
/// to its program.
pub struct GcArray<T> {
    array: *mut Array,
    heap: *const Heap,
    alive: Weak<()>,
    releaser: Releaser,
    _marker: PhantomData<fn() -> T>,
}

// SAFETY: like `ScriptCallback`.
unsafe impl<T> Send for GcArray<T> {}

impl<T: HostValue> GcArray<T> {
    fn array(&self, access: &impl ProgramAccess) -> Option<&Array> {
        // SAFETY: the array is rooted while its heap is alive, and the heap is
        // used by this thread.
        (self.alive.upgrade().is_some() && access.grants(self.heap)).then(|| unsafe { &*self.array })
    }

    /// How many elements it has, or 0 without access.
    pub fn len(&self, access: &impl ProgramAccess) -> usize {
        self.array(access).map_or(0, |array| array.length)
    }

    /// Whether it has no elements. It's also `true` without access.
    pub fn is_empty(&self, access: &impl ProgramAccess) -> bool {
        self.len(access) == 0
    }

    /// The element at `index`, if it's in bounds and there's access.
    pub fn get(&self, access: &impl ProgramAccess, index: usize) -> Option<T> {
        let array = self.array(access)?;

        // SAFETY: elements are values of `T` in memory, in bounds.
        (index < array.length).then(|| unsafe { T::from_ret(array.ptr.cast::<T::Ret>().add(index).read_unaligned()) })
    }

    /// Replaces the element at `index`. Returns `false` if it's out of bounds
    /// or there's no access.
    pub fn set(&mut self, access: &impl ProgramAccess, index: usize, value: T) -> bool {
        let Some(array) = self.array(access) else {
            return false;
        };

        if index >= array.length {
            return false;
        }

        // SAFETY: in bounds, and elements are values of `T` in memory.
        unsafe { array.ptr.cast::<T::Ret>().add(index).write_unaligned(value.into_ret()) };

        true
    }

    /// Adds `value` at the end. Returns `false` without access, or when out of
    /// memory.
    pub fn push(&mut self, access: &impl ProgramAccess, value: T) -> bool {
        let Some(length) = self.array(access).map(|array| array.length) else {
            return false;
        };

        // SAFETY: the heap is alive, and used by this thread (checked above).
        let heap = unsafe { &*self.heap };
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

    /// Its elements, or none without access.
    pub fn to_vec(&self, access: &impl ProgramAccess) -> Vec<T> {
        (0..self.len(access)).filter_map(|index| self.get(access, index)).collect()
    }
}

impl<T> Drop for GcArray<T> {
    fn drop(&mut self) {
        // From any thread: unrooted before the next collection.
        self.releaser.unroot(self.array.cast());
    }
}

/// A view of a program's array, `T[..]`, given to a host function as a copy of
/// its elements. Variadic parameters are passed as one too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice<T>(Vec<T>);

impl<T> Slice<T> {
    /// Its elements.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T> std::ops::Deref for Slice<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<T> IntoIterator for Slice<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// Views are only given to the host: functions of the host can't return them
// (see `Host::function_named`).
unsafe impl<T: HostValue> HostValue for Slice<T> {
    type Arg = *const Array;
    type Ret = *const Array;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type(types, tcx);

        tcx.types.get_or_add(Type::Array(element, ArrayKind::View { writable: false }))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        // SAFETY: a view is a valid array header while the call runs.
        let array = unsafe { &*arg };

        // SAFETY: elements are values of `T` in memory, in bounds.
        Self(
            (0..array.length)
                .map(|index| unsafe { T::from_ret(array.ptr.cast::<T::Ret>().add(index).read_unaligned()) })
                .collect(),
        )
    }

    fn into_ret(self) -> Self::Ret {
        unreachable!("functions of the host can't return views")
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        unsafe { Self::from_arg(ret) }
    }

    // A growable array viewed is kept alive with its elements; elements of a
    // fixed array on the stack are roots already (see
    // `allocator::stack_array_push`), and its header isn't an object.
    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.cast_mut().cast());
    }
}

/// A `mut` view of a program's array, `mut T[..]`, given to a host function. It
/// reads and changes the elements in place.
#[derive(Debug)]
pub struct SliceMut<T> {
    array: *mut Array,
    _elements: PhantomData<(T, *mut ())>,
}

impl<T: PlainValue> SliceMut<T> {
    /// How many elements it has.
    #[must_use]
    pub fn len(&self) -> usize {
        // SAFETY: a view is a valid array header while the call runs.
        unsafe { (*self.array).length }
    }

    /// Whether it has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element at `index`, if it's in bounds.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<T> {
        // SAFETY: elements are values of `T` in memory, in bounds.
        (index < self.len()).then(|| unsafe { T::from_ret((*self.array).ptr.cast::<T::Ret>().add(index).read_unaligned()) })
    }

    /// Replaces the element at `index`. Returns `false` if it's out of bounds.
    pub fn set(&mut self, index: usize, value: T) -> bool {
        let inside = index < self.len();

        if inside {
            // SAFETY: elements are values of `T` in memory, in bounds, and
            // hold no references (no write barrier).
            unsafe { (*self.array).ptr.cast::<T::Ret>().add(index).write_unaligned(value.into_ret()) };
        }

        inside
    }

    /// Copies `values` over the elements, starting from the first, as many as
    /// fit. Returns how many were copied.
    pub fn copy_from(&mut self, values: impl IntoIterator<Item = T>) -> usize {
        let mut count = 0;

        for value in values.into_iter().take(self.len()) {
            self.set(count, value);
            count += 1;
        }

        count
    }

    /// Its elements.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        (0..self.len()).filter_map(|index| self.get(index)).collect()
    }
}

// Views are only given to the host: functions of the host can't return them
// (see `Host::function_named`).
unsafe impl<T: PlainValue> HostValue for SliceMut<T> {
    type Arg = *mut Array;
    type Ret = *mut Array;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type(types, tcx);

        tcx.types.get_or_add(Type::Array(element, ArrayKind::View { writable: true }))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        Self {
            array: arg,
            _elements: PhantomData,
        }
    }

    fn into_ret(self) -> Self::Ret {
        unreachable!("functions of the host can't return views")
    }

    unsafe fn from_ret(ret: Self::Ret) -> Self {
        unsafe { Self::from_arg(ret) }
    }

    // A growable array viewed is kept alive; a fixed array's header on the
    // stack isn't an object.
    fn references(arg: &Self::Arg, references: &mut Vec<*mut u8>) {
        references.push(arg.cast());
    }
}

unsafe impl<T: HostValue> HostValue for GcArray<T> {
    type Arg = *mut Array;
    type Ret = *mut Array;

    const LAYOUT: (usize, usize) = (mem::size_of::<usize>(), mem::align_of::<usize>());

    fn mollie_type(types: &HostTypes, tcx: &mut TyCtxt) -> TypeRef {
        let element = T::mollie_type(types, tcx);

        tcx.types.get_or_add(Type::Array(element, ArrayKind::Growable))
    }

    unsafe fn from_arg(arg: Self::Arg) -> Self {
        let (_, heap, alive, releaser) = running_program();

        unsafe { &*heap }.root(arg.cast());

        Self {
            array: arg,
            heap,
            alive,
            releaser,
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
