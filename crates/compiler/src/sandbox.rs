#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::{
    cell::RefCell,
    error::Error,
    fmt, mem,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

use mollie_shared::limits::{DEFAULT_CALL_DEPTH, remaining_stack};

use crate::{
    allocator::{self, Array, Heap},
    strings,
};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct VmState {
    pub trap: u32,
    pub fuel: i64,
    pub stack_limit: usize,
    pub depth: u32,
    pub max_depth: u32,
    pub heap: *const Heap,
    pub trace: [u32; TRACE_CAPACITY],
    pub trace_len: u32,
    pub sites: *const TrapSites,
}

pub const TRACE_CAPACITY: usize = 32;

pub(crate) const TRAP_OFFSET: i32 = mem::offset_of!(VmState, trap) as i32;
pub(crate) const FUEL_OFFSET: i32 = mem::offset_of!(VmState, fuel) as i32;
pub(crate) const STACK_LIMIT_OFFSET: i32 = mem::offset_of!(VmState, stack_limit) as i32;
pub(crate) const HEAP_OFFSET: i32 = mem::offset_of!(VmState, heap) as i32;
pub(crate) const DEPTH_OFFSET: i32 = mem::offset_of!(VmState, depth) as i32;
pub(crate) const MAX_DEPTH_OFFSET: i32 = mem::offset_of!(VmState, max_depth) as i32;
pub(crate) const TRACE_OFFSET: i32 = mem::offset_of!(VmState, trace) as i32;
pub(crate) const TRACE_LEN_OFFSET: i32 = mem::offset_of!(VmState, trace_len) as i32;
pub(crate) const SITES_OFFSET: i32 = mem::offset_of!(VmState, sites) as i32;

impl VmState {
    pub const UNLIMITED: Self = Self {
        trap: 0,
        fuel: i64::MAX,
        stack_limit: 0,
        depth: 0,
        max_depth: u32::MAX,
        heap: ptr::null(),
        trace: [0; TRACE_CAPACITY],
        trace_len: 0,
        sites: ptr::null(),
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TrapKind {
    OutOfBounds = 1,
    DivisionByZero = 2,
    ArithmeticOverflow = 3,
    InvalidSlice = 4,
    OutOfFuel = 5,
    StackOverflow = 6,
    OutOfMemory = 7,
    Host = 8,
    Unreachable = 9,
    Panic = 10,
}

impl TrapKind {
    pub const fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            1 => Self::OutOfBounds,
            2 => Self::DivisionByZero,
            3 => Self::ArithmeticOverflow,
            4 => Self::InvalidSlice,
            5 => Self::OutOfFuel,
            6 => Self::StackOverflow,
            7 => Self::OutOfMemory,
            8 => Self::Host,
            9 => Self::Unreachable,
            10 => Self::Panic,
            _ => return None,
        })
    }
}

impl fmt::Display for TrapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfBounds => "index out of bounds",
            Self::DivisionByZero => "division by zero",
            Self::ArithmeticOverflow => "arithmetic overflow",
            Self::InvalidSlice => "invalid string slice",
            Self::OutOfFuel => "out of fuel",
            Self::StackOverflow => "stack overflow",
            Self::OutOfMemory => "out of memory",
            Self::Host => "aborted by the host",
            Self::Unreachable => "unreachable code reached",
            Self::Panic => "panicked",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    pub module: String,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.module, self.line, self.column)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    pub function: String,
    pub location: Option<Location>,
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.location {
            Some(location) => write!(f, "{} at {location}", self.function),
            None => f.write_str(&self.function),
        }
    }
}

#[derive(Debug, Default)]
pub struct TrapSites {
    sites: Vec<Frame>,
}

impl TrapSites {
    pub fn push(&mut self, frame: Frame) -> u32 {
        let index = u32::try_from(self.sites.len()).unwrap_or(u32::MAX);

        self.sites.push(frame);

        index
    }

    pub fn get(&self, index: u32) -> Option<&Frame> {
        self.sites.get(usize::try_from(index).ok()?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trap {
    pub kind: TrapKind,
    pub message: Option<String>,
    pub location: Option<Location>,
    pub backtrace: Vec<Frame>,
}

impl Trap {
    pub const fn new(kind: TrapKind, message: Option<String>) -> Self {
        Self {
            kind,
            message,
            location: None,
            backtrace: Vec::new(),
        }
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.message {
            Some(message) => write!(f, "{}: {message}", self.kind)?,
            None => self.kind.fmt(f)?,
        }

        if let Some(location) = &self.location {
            write!(f, "\n  --> {location}")?;
        }

        for frame in &self.backtrace {
            write!(f, "\n  in {frame}")?;
        }

        Ok(())
    }
}

impl Error for Trap {}

#[derive(Default)]
pub struct Limits {
    pub fuel: Option<u64>,
    pub refuel: Option<Box<dyn FnMut() -> u64>>,
    pub call_depth: Option<u32>,
    pub stack_bytes: Option<usize>,
    pub heap_bytes: Option<usize>,
    pub auto_collect: Option<bool>,
}

const STACK_MARGIN: usize = 128 * 1024;

struct Run {
    state: ptr::NonNull<VmState>,
    refuel: Option<Box<dyn FnMut() -> u64>>,
    message: Option<String>,
}

thread_local! {
    static RUNS: RefCell<Vec<Run>> = const { RefCell::new(Vec::new()) };
}

/// # Safety
///
/// `state` must be the `VmState` of the compiler whose code `call` runs,
/// valid and not used elsewhere for the whole call.
pub unsafe fn run<R>(state: ptr::NonNull<VmState>, limits: Limits, call: impl FnOnce() -> R) -> Result<R, Trap> {
    // A program may be run again by the host while it's running (in a
    // callback), so the outer state is restored afterwards.
    let outer = unsafe { state.read() };
    let marker = 0u8;
    // The stack left on the thread, minus room for the runtime and the host
    // called by the program, is always a limit.
    let available = remaining_stack().map(|bytes| bytes.saturating_sub(STACK_MARGIN));
    let stack_bytes = match (limits.stack_bytes, available) {
        (Some(bytes), Some(available)) => Some(bytes.min(available)),
        (bytes, available) => bytes.or(available),
    };
    let stack_limit = stack_bytes.map_or(0, |bytes| ptr::from_ref(&marker).addr().saturating_sub(bytes));

    unsafe {
        state.write(VmState {
            trap: 0,
            fuel: limits.fuel.map_or(i64::MAX, |fuel| i64::try_from(fuel).unwrap_or(i64::MAX)),
            stack_limit,
            // A program run again by the host while it runs (in a callback)
            // continues its depth.
            depth: outer.depth,
            max_depth: outer.depth.saturating_add(limits.call_depth.unwrap_or(DEFAULT_CALL_DEPTH)),
            heap: outer.heap,
            trace: [0; TRACE_CAPACITY],
            trace_len: 0,
            sites: outer.sites,
        });
    }

    // SAFETY: the heap lives as long as the compiler owning the state.
    let heap = unsafe { outer.heap.as_ref() };
    let outer_heap_limit = heap.zip(limits.heap_bytes).map(|(heap, limit)| heap.set_limit(Some(limit)));
    let outer_auto_collect = heap.zip(limits.auto_collect).map(|(heap, enabled)| heap.set_auto_collect(enabled));

    RUNS.with_borrow_mut(|runs| {
        runs.push(Run {
            state,
            refuel: limits.refuel,
            message: None,
        });
    });

    let stack_arrays = allocator::stack_arrays();
    let result = call();

    // A trap returns early through frames that registered arrays on the
    // stack, without ending their registrations.
    allocator::truncate_stack_arrays(stack_arrays);

    // Runs are pushed and popped in pairs, so this is the run pushed above.
    let message = RUNS.with_borrow_mut(Vec::pop).and_then(|run| run.message);
    let stopped = unsafe { state.read() };
    let trap = stopped.trap;

    if let (Some(heap), Some(limit)) = (heap, outer_heap_limit) {
        heap.set_limit(limit);
    }

    if let (Some(heap), Some(enabled)) = (heap, outer_auto_collect) {
        heap.set_auto_collect(enabled);
    }

    unsafe { state.write(outer) };

    TrapKind::from_code(trap).map_or_else(
        || Ok(result),
        |kind| {
            // SAFETY: the sites live as long as the compiler of the program.
            let sites = unsafe { stopped.sites.as_ref() };
            let length = usize::try_from(stopped.trace_len).unwrap_or(0).min(TRACE_CAPACITY);
            let backtrace = sites.map_or_else(Vec::new, |sites| {
                stopped.trace[..length].iter().filter_map(|&site| sites.get(site).cloned()).collect::<Vec<_>>()
            });

            Err(Trap {
                kind,
                message,
                location: backtrace.iter().find_map(|frame| frame.location.clone()),
                backtrace,
            })
        },
    )
}

pub(crate) fn current_heap() -> Option<&'static Heap> {
    RUNS.with_borrow(|runs| {
        // SAFETY: the state and its heap are valid while the program runs; the
        // reference isn't kept longer by the runtime.
        runs.last().and_then(|run| unsafe { (*run.state.as_ptr()).heap.as_ref() })
    })
}

pub fn with_current_heap<R>(f: impl FnOnce(&Heap) -> R) -> Option<R> {
    current_heap().map(f)
}

pub fn current_state() -> Option<ptr::NonNull<VmState>> {
    RUNS.with_borrow(|runs| runs.last().map(|run| run.state))
}

pub(crate) fn set_trap(kind: TrapKind) -> bool {
    RUNS.with_borrow(|runs| {
        runs.last().is_some_and(|run| {
            // SAFETY: the state is valid while the program runs.
            let state = unsafe { &mut *run.state.as_ptr() };

            if state.trap == 0 {
                state.trap = kind as u32;
            }

            true
        })
    })
}

pub fn raise(message: impl Into<String>) -> bool {
    let message = message.into();

    RUNS.with_borrow_mut(|runs| {
        runs.last_mut().is_some_and(|run| {
            run.message.get_or_insert(message);

            true
        })
    }) && set_trap(TrapKind::Host)
}

pub(crate) unsafe extern "C" fn panic(message: *const Array) {
    let message = unsafe { strings::as_str(message) }.to_owned();

    RUNS.with_borrow_mut(|runs| {
        if let Some(run) = runs.last_mut() {
            run.message.get_or_insert(message);
        }
    });
}

pub(crate) extern "C" fn out_of_fuel() {
    // The callback is taken out while it runs, so it can't be called again
    // reentrantly.
    let Some((state, refuel)) = RUNS.with_borrow_mut(|runs| runs.last_mut().map(|run| (run.state, run.refuel.take()))) else {
        return;
    };

    // A panic must not unwind through compiled code: it stops the program.
    let (fuel, refuel) = refuel.map_or_default(|mut refuel| {
        if let Ok(fuel) = catch_unwind(AssertUnwindSafe(&mut refuel)) {
            (fuel, Some(refuel))
        } else {
            RUNS.with_borrow_mut(|runs| {
                if let Some(run) = runs.last_mut() {
                    run.message.get_or_insert_with(|| String::from("refueling panicked"));
                }
            });

            (0, Some(refuel))
        }
    });

    RUNS.with_borrow_mut(|runs| {
        if let Some(run) = runs.last_mut() {
            run.refuel = refuel;
        }
    });

    // SAFETY: the state is valid while the program runs.
    let state = unsafe { &mut *state.as_ptr() };

    if fuel == 0 {
        if state.trap == 0 {
            state.trap = TrapKind::OutOfFuel as u32;
        }
    } else {
        state.fuel = i64::try_from(fuel).unwrap_or(i64::MAX);
    }
}
