//! Mark & sweep garbage collector used by compiled code.
//!
//! Every program (an addon) has its own [`Heap`], owned by its compiler:
//! objects of one heap never reference objects of another, so heaps are
//! collected, limited and freed independently.
//!
//! Every object starts with a [`GcValue`] header, compiled code only sees
//! pointers to the value right after it.
//!
//! Objects of up to 256 bytes (and array buffers) are blocks of size-classed
//! pages (see [`pages`]): marks are bits of their page, and sweeping frees
//! dead objects a bitmap word at a time. Larger objects come from the system
//! allocator, and are marked in their header.
//!
//! # Roots
//!
//! - Objects kept alive by the host with [`Heap::root`] (see `GcRoot`).
//! - GC references held by compiled code. Cranelift spills them to the stack
//!   around calls and describes where they are with stack maps, registered with
//!   [`Heap::register_code`]. Calls from compiled code to the runtime and to
//!   the host go through wrappers recording their frame pointer ([`exit_push`]
//!   and [`exit_pop`]). A collection walks the frames of compiled code of its
//!   heap from every recorded exit, following frame pointers, until it reaches
//!   a frame of something else (the host, or another program).
//! - Values pinned by the host while it uses them ([`pin`]).
//!
//! Collections only happen in allocations made by compiled code, where every
//! frame of compiled code is described, or when the host asks for one outside
//! of compiled code. Allocations made by the host never collect.
//!
//! Compiled code and the runtime find the heap of the program they belong to
//! through the innermost run on the thread (see [`crate::sandbox::run`]).
#![allow(clippy::cast_ptr_alignment)]

use std::{
    alloc,
    any::TypeId,
    cell::{RefCell, UnsafeCell},
    collections::{HashMap, HashSet},
    hash, mem,
    ptr::{self, NonNull},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typing::{AdtKind, AdtVariantRef};

use crate::sandbox::{self, TrapKind};

mod pages;

use pages::{Kind, Pages};

/// How a field references other GC objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum TypeLayoutField {
    /// Doesn't reference GC objects.
    Regular,
    /// A pointer to a GC object. Null and host pointers are skipped.
    Collectable,
    /// A function value: a code pointer followed by a pointer to its
    /// environment (a GC object, or null).
    FuncEnv,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(C)]
pub struct TypeLayout {
    /// Fields that may reference GC objects: the variant they belong to, their
    /// offset, representation and kind.
    pub fields: &'static [(AdtVariantRef, u32, MollieType, TypeLayoutField)],
    /// Hash of the ADT type, if this is a layout of an ADT.
    pub adt_ty: Option<u64>,
    pub size: usize,
    pub align: usize,
    pub kind: Option<AdtKind>,
}

impl TypeLayout {
    pub const fn of<T>() -> Self {
        Self {
            size: mem::size_of::<T>(),
            align: mem::align_of::<T>(),
            fields: &[],
            kind: None,
            adt_ty: None,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct GcValueInfo: usize {
        const MARKED = 1;
        const ARRAY = 1 << 1;
    }
}

#[derive(Debug)]
#[repr(C)]
pub struct GcValue<T> {
    /// Layout of the value, or of its elements for arrays.
    pub layout: &'static TypeLayout,
    // `usize`, so that the value is aligned to pointer size.
    pub info: GcValueInfo,
    pub value: T,
}

/// Size of the object header, which is the offset of the value.
pub const HEADER_SIZE: usize = mem::offset_of!(GcValue<()>, value);

#[derive(Debug)]
#[repr(C)]
pub struct Array {
    pub length: usize,
    pub capacity: usize,
    pub ptr: *mut (),
}

type Object = *mut GcValue<()>;
type Hasher = hash::BuildHasherDefault<AddressHasher>;

/// Hashes addresses (of objects and code).
///
/// It's cheaper than the default hasher, which matters since collections look
/// up every reference. Addresses are
/// aligned, so their bits are mixed (a multiplication alone would keep the
/// low bits zero, which hash tables use to pick buckets).
#[derive(Default)]
pub struct AddressHasher(u64);

impl hash::Hasher for AddressHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let value = u64::from(byte) ^ self.0.rotate_left(8);

            hash::Hasher::write_u64(self, value);
        }
    }

    fn write_u64(&mut self, value: u64) {
        // The finalizer of MurmurHash3.
        let mut value = value ^ self.0;

        value ^= value >> 33;
        value = value.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        value ^= value >> 33;
        value = value.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        value ^= value >> 33;

        self.0 = value;
    }

    fn write_usize(&mut self, value: usize) {
        hash::Hasher::write_u64(self, value as u64);
    }
}

/// Fields of a [`TypeLayout`] (see [`TypeLayout::fields`]).
pub type LayoutFields = [(AdtVariantRef, u32, MollieType, TypeLayoutField)];

/// Default of [`Heap::set_collection_threshold`].
const MIN_COLLECTION_THRESHOLD: usize = 1024 * 1024;

/// How much more than usual the heap may grow when allocations don't collect
/// garbage (see [`Heap::set_auto_collect`]) before they do anyway.
const UNCOLLECTED_GROWTH: usize = 4;

/// Code of compiled functions and their stack maps.
#[derive(Debug, Default)]
struct CodeTable {
    /// Address ranges of compiled functions, sorted (they don't overlap).
    ranges: Vec<(usize, usize)>,
    /// Offsets from the stack pointer of GC references live at each return
    /// address in compiled code.
    stack_maps: HashMap<usize, Box<[u32]>, Hasher>,
}

impl CodeTable {
    /// Whether `pc` is in a compiled function: looked up for every frame
    /// walked by a collection.
    fn contains(&self, pc: usize) -> bool {
        // The last range starting at or before `pc`.
        let index = self.ranges.partition_point(|&(start, _)| start <= pc);

        index > 0 && pc < self.ranges[index - 1].1
    }

    fn insert(&mut self, start: usize, end: usize) {
        let index = self.ranges.partition_point(|&(other, _)| other < start);

        self.ranges.insert(index, (start, end));
    }
}

/// Durations of the parts of a collection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PausePhases {
    pub roots: Duration,
    pub mark: Duration,
    pub sweep: Duration,
}

/// Statistics of a [`Heap`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes of live objects (and objects not collected yet).
    pub allocated_bytes: usize,
    /// Bytes freed by all collections.
    pub deallocated_bytes: usize,
    /// Number of objects (live, or not collected yet).
    pub objects: usize,
    pub collections: usize,
    pub last_pause: Duration,
    pub max_pause: Duration,
    /// Parts of the last pause: finding roots (with the stack), marking and
    /// sweeping.
    pub last_phases: PausePhases,
    /// Objects freed by the last collection.
    pub last_freed_objects: usize,
    /// Bytes allocated at which the next collection is due.
    pub next_collection_at: usize,
}

thread_local! {
    /// Frame pointers of the wrappers of calls from compiled code to the
    /// runtime or the host, oldest first.
    static EXIT_FRAMES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// Values (pointers to values of GC objects) kept alive by the host while
    /// it uses them, e.g. arguments of a host function while it runs: the
    /// frame calling it may not hold them anymore. With the heap they belong
    /// to.
    static PINNED: RefCell<Vec<(*const Heap, *mut u8)>> = const { RefCell::new(Vec::new()) };
}

/// Keeps the GC objects with the values `values` alive until [`unpin`] is
/// called with their number. They belong to the heap of the program running
/// on this thread. Null and host pointers are allowed.
pub fn pin(values: &[*mut u8]) {
    let heap = sandbox::current_heap().map_or(ptr::null(), ptr::from_ref);

    PINNED.with_borrow_mut(|pinned| pinned.extend(values.iter().map(|&value| (heap, value))));
}

/// Ends the last `count` pins made by [`pin`].
pub fn unpin(count: usize) {
    PINNED.with_borrow_mut(|pinned| {
        let length = pinned.len().saturating_sub(count);

        pinned.truncate(length);
    });
}

/// Records a call from compiled code to the host. `frame_pointer` is the
/// frame pointer of the wrapper making the call.
pub extern "C" fn exit_push(frame_pointer: usize) {
    EXIT_FRAMES.with_borrow_mut(|frames| frames.push(frame_pointer));
}

/// Ends the last call recorded by [`exit_push`].
pub extern "C" fn exit_pop() {
    EXIT_FRAMES.with_borrow_mut(Vec::pop);
}

fn object_layout(type_layout: &TypeLayout) -> alloc::Layout {
    let align = type_layout.align.max(mem::align_of::<GcValue<()>>());

    alloc::Layout::from_size_align(HEADER_SIZE + type_layout.size, align)
        .unwrap_or_else(|_| alloc::Layout::new::<GcValue<()>>())
        .pad_to_align()
}

const fn array_object_layout() -> alloc::Layout {
    alloc::Layout::new::<GcValue<Array>>()
}

/// Layout of an array buffer: `Ok(None)` if it takes no memory, `Err(())` if
/// it's too large to allocate.
fn buffer_layout(item_layout: &TypeLayout, capacity: usize) -> Result<Option<alloc::Layout>, ()> {
    let size = item_layout.size.checked_mul(capacity).ok_or(())?;

    if size == 0 {
        Ok(None)
    } else {
        alloc::Layout::from_size_align(size, item_layout.align.max(1)).map(Some).map_err(|_| ())
    }
}

/// Stops the program with [`TrapKind::OutOfMemory`] after a failed allocation
/// of compiled code, or aborts for allocations of the host (like Rust's own
/// allocations do).
fn out_of_memory(may_collect: bool, layout: alloc::Layout) {
    if may_collect {
        sandbox::set_trap(TrapKind::OutOfMemory);
    } else {
        alloc::handle_alloc_error(layout);
    }
}

/// Capacity of an array of `length` elements, or `None` if it's too large.
const fn compute_capacity(length: usize) -> Option<usize> {
    if length == 0 { Some(0) } else { length.checked_next_power_of_two() }
}

/// The object of a value pointer.
const fn object_of(value_ptr: *const ()) -> Object {
    value_ptr.cast::<u8>().wrapping_sub(HEADER_SIZE).cast_mut().cast()
}

/// Frees an object that isn't in a page (a large one).
///
/// # Safety
///
/// `object` must be a live object of a heap, which forgets it.
unsafe fn free_large(object: Object) -> usize {
    let header = unsafe { &*object };
    let mut freed = 0;
    let layout = if header.info.contains(GcValueInfo::ARRAY) {
        let array = unsafe { &*object.cast::<GcValue<Array>>() };

        if let Ok(Some(layout)) = buffer_layout(header.layout, array.value.capacity) {
            unsafe { Pages::free_buffer(array.value.ptr.cast(), layout) };

            freed += Pages::size_of(layout);
        }

        array_object_layout()
    } else {
        object_layout(header.layout)
    };

    unsafe { alloc::dealloc(object.cast(), layout) };

    freed + layout.size()
}

/// State of a [`Heap`].
struct HeapState {
    /// Small objects (and array buffers).
    pages: Pages,
    /// Objects too large for pages.
    large: HashSet<Object, Hasher>,
    /// Objects kept alive by the host, with the number of times they were
    /// rooted.
    roots: HashMap<Object, usize, Hasher>,
    code: CodeTable,
    /// Collect on every allocation made by compiled code, to find objects
    /// collected too early (for testing).
    stress: bool,
    allocated_bytes: usize,
    deallocated_bytes: usize,
    perform_gc_at: usize,
    /// Bytes allocated since the last collection that make the next one due
    /// (at least as many as there are live ones, see
    /// [`Heap::set_collection_threshold`]).
    threshold: usize,
    /// Allocations of compiled code fail if live objects would take more
    /// bytes (see [`crate::sandbox::Limits::heap_bytes`]).
    limit: Option<usize>,
    /// Whether allocations collect garbage when it's due (see
    /// [`Heap::set_auto_collect`]).
    auto_collect: bool,
    collections: usize,
    last_pause: Duration,
    max_pause: Duration,
    last_phases: PausePhases,
    last_freed_objects: usize,
    /// Layouts used by objects and compiled code of this heap, freed with it.
    layouts: Vec<NonNull<TypeLayout>>,
    fields: Vec<NonNull<LayoutFields>>,
    /// Layouts of Rust types (see [`Heap::layout_of`]).
    type_layouts: HashMap<TypeId, &'static TypeLayout>,
    /// C functions the host calls function values of the program through, by
    /// their Rust signature (see [`Heap::callback_entry`]).
    callback_entries: HashMap<TypeId, usize>,
    /// Functions of traits of the host, by the Rust marker type of the trait
    /// and name (see [`Heap::trait_function`]).
    trait_functions: HashMap<(TypeId, String), TraitFunction>,
}

/// A function of a trait registered by the host, called on trait objects of
/// programs (see [`Heap::trait_function`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraitFunction {
    /// Index in the trait (and in vtables, after the type's hash).
    pub index: usize,
    /// The Rust type of its arguments and result (`fn(Args) -> R`).
    pub signature: TypeId,
    /// The C function the host calls it through.
    pub entry: usize,
}

impl HeapState {
    fn root(&mut self, value_ptr: *const ()) {
        if !value_ptr.is_null() {
            *self.roots.entry(object_of(value_ptr)).or_insert(0) += 1;
        }
    }

    fn unroot(&mut self, value_ptr: *const ()) {
        if value_ptr.is_null() {
            return;
        }

        let object = object_of(value_ptr);

        if let Some(count) = self.roots.get_mut(&object) {
            *count = count.saturating_sub(1);

            if *count == 0 {
                self.roots.remove(&object);
            }
        }
    }

    fn collect_if_needed(&mut self, heap: *const Heap, additional: usize) {
        let allocated = self.allocated_bytes.saturating_add(additional);
        // Without collections by allocations (see `Heap::set_auto_collect`),
        // the heap still doesn't grow without bounds.
        let due = if self.auto_collect {
            allocated >= self.perform_gc_at
        } else {
            allocated >= self.perform_gc_at.saturating_mul(UNCOLLECTED_GROWTH)
        };

        if self.stress || due {
            // SAFETY: objects are only created by this heap and point to valid
            // data.
            unsafe { self.collect(heap) };
        }
    }

    /// Prepares an allocation of `additional` bytes by compiled code,
    /// collecting garbage if needed. Returns `false` (and stops the program)
    /// if the heap limit would be exceeded even after a collection.
    fn make_room(&mut self, heap: *const Heap, additional: usize) -> bool {
        self.collect_if_needed(heap, additional);

        let Some(limit) = self.limit else {
            return true;
        };

        if self.allocated_bytes.saturating_add(additional) > limit {
            // SAFETY: objects are only created by this heap and point to valid
            // data.
            unsafe { self.collect(heap) };
        }

        if self.allocated_bytes.saturating_add(additional) > limit {
            sandbox::set_trap(TrapKind::OutOfMemory);

            false
        } else {
            true
        }
    }

    /// Pushes GC objects referenced by fields of a value onto `stack`.
    ///
    /// # Safety
    ///
    /// `value` must point to a valid value with the given layout.
    unsafe fn push_references(layout: &TypeLayout, value: *const u8, variant: AdtVariantRef, stack: &mut Vec<Object>) {
        for &(field_variant, offset, _, kind) in layout.fields {
            if field_variant != variant {
                continue;
            }

            let field = unsafe { value.add(offset as usize) };
            let pointer = match kind {
                TypeLayoutField::Regular => continue,
                TypeLayoutField::Collectable => unsafe { field.cast::<*mut u8>().read_unaligned() },
                TypeLayoutField::FuncEnv => unsafe { field.add(mem::size_of::<usize>()).cast::<*mut u8>().read_unaligned() },
            };

            if !pointer.is_null() {
                stack.push(pointer.wrapping_sub(HEADER_SIZE).cast());
            }
        }
    }

    /// Marks `roots` and every object reachable from them, with one stack for
    /// all of them.
    ///
    /// # Safety
    ///
    /// All objects should point to valid data.
    unsafe fn mark(&self, roots: Vec<Object>) {
        let mut stack = roots;

        while let Some(object) = stack.pop() {
            // Host pointers, static strings and dangling pointers aren't
            // objects of this heap.
            match self.pages.mark(object) {
                Some(true) => (),
                Some(false) => continue,
                None => {
                    if !self.large.contains(&object) {
                        continue;
                    }

                    let header = unsafe { &mut *object };

                    if header.info.contains(GcValueInfo::MARKED) {
                        continue;
                    }

                    header.info.insert(GcValueInfo::MARKED);
                }
            }

            let header = unsafe { &*object };

            let layout = header.layout;
            let value = object.cast::<u8>().wrapping_add(HEADER_SIZE);

            if header.info.contains(GcValueInfo::ARRAY) {
                if layout.fields.is_empty() {
                    continue;
                }

                let array = unsafe { &*value.cast::<Array>() };

                for index in 0..array.length {
                    let element = array.ptr.cast::<u8>().wrapping_add(index * layout.size);

                    unsafe { Self::push_references(layout, element, AdtVariantRef::ZERO, &mut stack) };
                }
            } else {
                let variant = if matches!(layout.kind, Some(AdtKind::Enum)) {
                    // The discriminant is the first field of every variant.
                    AdtVariantRef::new(unsafe { value.cast::<usize>().read() })
                } else {
                    AdtVariantRef::ZERO
                };

                unsafe { Self::push_references(layout, value, variant, &mut stack) };
            }
        }
    }

    /// # Safety
    ///
    /// All objects should point to valid data.
    unsafe fn sweep(&mut self) -> usize {
        let mut freed = 0;
        let mut freed_objects = 0;
        let Self { large, .. } = self;

        // Before pages: buffers of large arrays may be in pages.
        large.retain(|&object| {
            let header = unsafe { &mut *object };

            if header.info.contains(GcValueInfo::MARKED) {
                header.info.remove(GcValueInfo::MARKED);

                return true;
            }

            freed += unsafe { free_large(object) };
            freed_objects += 1;

            false
        });

        // Free blocks for about as many bytes as allocations make before the
        // next collection are kept.
        let (objects, bytes) = unsafe { self.pages.sweep(self.threshold.max(self.allocated_bytes)) };

        freed += bytes;
        freed_objects += objects;
        self.allocated_bytes -= freed;
        self.deallocated_bytes += freed;

        freed_objects
    }

    /// Collects garbage of the heap at `heap` (this state).
    ///
    /// # Safety
    ///
    /// All objects should point to valid data, and frames recorded with
    /// [`exit_push`] must still be on the stack.
    unsafe fn collect(&mut self, heap: *const Heap) {
        let start = Instant::now();
        let mut roots = self.roots.iter().filter(|&(_, &count)| count > 0).map(|(&root, _)| root).collect::<Vec<_>>();

        PINNED.with_borrow(|pinned| {
            roots.extend(
                pinned
                    .iter()
                    .filter(|&&(pin_heap, value)| ptr::eq(pin_heap, heap) && !value.is_null())
                    .map(|&(_, value)| value.wrapping_sub(HEADER_SIZE).cast::<GcValue<()>>()),
            );
        });

        roots.extend(
            unsafe { self.stack_roots() }
                .into_iter()
                .filter(|root| !root.is_null())
                .map(|root| root.wrapping_sub(HEADER_SIZE).cast::<GcValue<()>>()),
        );

        let rooted = Instant::now();

        unsafe { self.mark(roots) };

        let marked = Instant::now();
        let freed_objects = unsafe { self.sweep() };

        self.last_phases = PausePhases {
            roots: rooted - start,
            mark: marked - rooted,
            sweep: marked.elapsed(),
        };
        self.last_freed_objects = freed_objects;

        // Collections cost about the garbage they free (and the live objects
        // they mark): the next one is due after `threshold` new bytes, or as
        // many as are live, so a large heap isn't marked over and over.
        self.perform_gc_at = self.allocated_bytes.saturating_add(self.threshold.max(self.allocated_bytes));
        self.collections += 1;
        self.last_pause = start.elapsed();
        self.max_pause = self.max_pause.max(self.last_pause);
    }

    /// GC references held by frames of compiled code of this heap on the
    /// current thread.
    ///
    /// # Safety
    ///
    /// Frames recorded with [`exit_push`] must still be on the stack.
    unsafe fn stack_roots(&self) -> Vec<*mut u8> {
        let mut roots = Vec::new();

        // Frames are walked through frame pointers: `[fp]` is the frame pointer
        // of the caller, `[fp + 8]` is the return address into the caller, and
        // the stack pointer of the caller (at that return address) is
        // `fp + 16`. This is how Cranelift lays out frames on x86-64 and
        // AArch64.
        if !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            return roots;
        }

        EXIT_FRAMES.with_borrow(|frames| {
            for &exit_frame in frames.iter().rev() {
                let mut frame_pointer = exit_frame;

                loop {
                    let return_addr = unsafe { (frame_pointer as *const usize).add(1).read() };

                    // The caller isn't code of this heap (the host, or another
                    // program called by the host): the frames of this exit
                    // end here. Exits of other programs are skipped this way
                    // too.
                    if !self.code.contains(return_addr) {
                        break;
                    }

                    let stack_pointer = frame_pointer + 2 * mem::size_of::<usize>();

                    if let Some(offsets) = self.code.stack_maps.get(&return_addr) {
                        for &offset in offsets {
                            roots.push(unsafe { ((stack_pointer + offset as usize) as *const *mut u8).read() });
                        }
                    }

                    frame_pointer = unsafe { (frame_pointer as *const usize).read() };
                }
            }
        });

        roots.retain(|root| !root.is_null());

        roots
    }

    fn register(&mut self, object: Object, layout: alloc::Layout, size: usize) {
        if !Pages::is_small(layout) {
            self.large.insert(object);
        }

        self.allocated_bytes += size;
    }

    unsafe fn alloc(&mut self, heap: *const Heap, type_layout: &'static TypeLayout, may_collect: bool) -> *mut () {
        let layout = object_layout(type_layout);

        // Collect before allocating, so the new object can't be collected right
        // away.
        if may_collect && !self.make_room(heap, layout.size()) {
            return ptr::null_mut();
        }

        let object: Object = self.pages.alloc(layout, Kind::Objects).cast();

        if object.is_null() {
            out_of_memory(may_collect, layout);

            return ptr::null_mut();
        }

        unsafe {
            ptr::addr_of_mut!((*object).layout).write(type_layout);
            ptr::addr_of_mut!((*object).info).write(GcValueInfo::empty());
        }

        self.register(object, layout, Pages::size_of(layout));

        object.cast::<u8>().wrapping_add(HEADER_SIZE).cast()
    }

    unsafe fn alloc_array(&mut self, heap: *const Heap, item_layout: &'static TypeLayout, length: usize, may_collect: bool) -> *mut () {
        let header_layout = array_object_layout();
        let Some(capacity) = compute_capacity(length) else {
            out_of_memory(may_collect, header_layout);

            return ptr::null_mut();
        };
        let Ok(buffer) = buffer_layout(item_layout, capacity) else {
            out_of_memory(may_collect, header_layout);

            return ptr::null_mut();
        };

        let size = Pages::size_of(header_layout) + buffer.map_or(0, Pages::size_of);

        if may_collect && !self.make_room(heap, size) {
            return ptr::null_mut();
        }

        let buffer_ptr: *mut () = match buffer {
            None => NonNull::<u8>::dangling().as_ptr().cast(),
            Some(layout) => {
                let buffer_ptr = self.pages.alloc(layout, Kind::Buffers);

                if buffer_ptr.is_null() {
                    out_of_memory(may_collect, layout);

                    return ptr::null_mut();
                }

                buffer_ptr.cast()
            }
        };

        let object = self.pages.alloc(header_layout, Kind::Objects).cast::<GcValue<Array>>();

        if object.is_null() {
            if let Some(layout) = buffer {
                unsafe { Pages::free_buffer(buffer_ptr.cast(), layout) };
            }

            out_of_memory(may_collect, header_layout);

            return ptr::null_mut();
        }

        unsafe {
            object.write(GcValue {
                layout: item_layout,
                info: GcValueInfo::ARRAY,
                value: Array {
                    length,
                    capacity,
                    ptr: buffer_ptr,
                },
            });
        }

        self.register(object.cast(), header_layout, size);
        self.pages.set_array(object.cast());

        object.cast::<u8>().wrapping_add(HEADER_SIZE).cast()
    }

    unsafe fn realloc_array(&mut self, heap: *const Heap, array: *mut Array, length: usize) -> bool {
        let object = array.cast::<u8>().wrapping_sub(HEADER_SIZE).cast::<GcValue<Array>>();
        let item_layout = unsafe { (*object).layout };
        let (capacity, ptr) = unsafe { ((*object).value.capacity, (*object).value.ptr) };

        if length > capacity {
            let Some(new_capacity) = compute_capacity(length) else {
                sandbox::set_trap(TrapKind::OutOfMemory);

                return false;
            };
            let (Ok(old_layout), Ok(new_layout)) = (buffer_layout(item_layout, capacity), buffer_layout(item_layout, new_capacity)) else {
                sandbox::set_trap(TrapKind::OutOfMemory);

                return false;
            };

            if let Some(new_layout) = new_layout {
                let old_size = old_layout.map_or(0, |layout| layout.size());

                // The array may only be referenced by the caller's arguments,
                // which aren't roots.
                self.root(array.cast());

                let room = self.make_room(heap, new_layout.size() - old_size);

                self.unroot(array.cast());

                if !room {
                    return false;
                }

                let new_ptr = match old_layout {
                    None => self.pages.alloc(new_layout, Kind::Buffers),
                    // Small buffers are blocks: moved to a block of the new size.
                    Some(old_layout) if Pages::is_small(old_layout) || Pages::is_small(new_layout) => {
                        let new_ptr = self.pages.alloc(new_layout, Kind::Buffers);

                        if !new_ptr.is_null() {
                            unsafe {
                                new_ptr.copy_from_nonoverlapping(ptr.cast(), old_size);
                                
                                Pages::free_buffer(ptr.cast(), old_layout);
                            }
                        }

                        new_ptr
                    }
                    Some(old_layout) => {
                        let new_ptr = unsafe { alloc::realloc(ptr.cast(), old_layout, new_layout.size()) };

                        if !new_ptr.is_null() {
                            // `realloc` doesn't zero the new part.
                            unsafe { new_ptr.add(old_size).write_bytes(0, new_layout.size() - old_size) };
                        }

                        new_ptr
                    }
                };

                if new_ptr.is_null() {
                    // The old buffer is still valid and still counted.
                    sandbox::set_trap(TrapKind::OutOfMemory);

                    return false;
                }

                self.allocated_bytes = self.allocated_bytes - old_layout.map_or(0, Pages::size_of) + Pages::size_of(new_layout);

                unsafe { (*object).value.ptr = new_ptr.cast() };
            }

            unsafe { (*object).value.capacity = new_capacity };
        }

        unsafe { (*object).value.length = length };

        true
    }
}

/// The GC heap of a program: its objects, roots, limit and the code of its
/// compiled functions. It's owned by the compiler of the program.
///
/// A heap is only used by the thread running its program. Its methods take
/// `&self`: the state is only borrowed inside of each method, which never
/// calls compiled code or the host.
pub struct Heap {
    state: UnsafeCell<HeapState>,
    /// Dropped with the heap: handles of the host holding a [`Weak`] to it
    /// (see [`Heap::alive_token`]) know when their program is gone.
    alive: Rc<()>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        Self {
            state: UnsafeCell::new(HeapState {
                pages: Pages::new(),
                large: HashSet::default(),
                roots: HashMap::default(),
                code: CodeTable::default(),
                stress: false,
                allocated_bytes: 0,
                deallocated_bytes: 0,
                perform_gc_at: MIN_COLLECTION_THRESHOLD,
                threshold: MIN_COLLECTION_THRESHOLD,
                limit: None,
                auto_collect: true,
                collections: 0,
                last_pause: Duration::ZERO,
                max_pause: Duration::ZERO,
                last_phases: PausePhases::default(),
                last_freed_objects: 0,
                layouts: Vec::new(),
                fields: Vec::new(),
                type_layouts: HashMap::new(),
                callback_entries: HashMap::new(),
                trait_functions: HashMap::new(),
            }),
            alive: Rc::new(()),
        }
    }

    /// A token that can't be upgraded anymore once the heap is dropped, for
    /// handles that can't borrow the heap (like callbacks kept by the host).
    pub fn alive_token(&self) -> Weak<()> {
        Rc::downgrade(&self.alive)
    }

    #[allow(clippy::mut_from_ref, reason = "borrows never overlap, see the type's docs")]
    fn state(&self) -> &mut HeapState {
        // SAFETY: the heap is used by one thread, and every borrow of its state
        // ends before another one is made (see the type's docs).
        unsafe { &mut *self.state.get() }
    }

    /// Makes the object with the value at `value_ptr` a root. Null and host
    /// pointers are allowed, they're ignored when marking.
    pub fn root(&self, value_ptr: *const ()) {
        self.state().root(value_ptr);
    }

    /// Reverts one [`Heap::root`] of the object.
    pub fn unroot(&self, value_ptr: *const ()) {
        self.state().unroot(value_ptr);
    }

    /// The C function the host calls function values of the program through,
    /// for the Rust signature `key` (`fn(Args) -> R`), if it was made. It's
    /// kept here since the host only reaches the heap of a running program.
    pub fn callback_entry(&self, key: TypeId) -> Option<usize> {
        self.state().callback_entries.get(&key).copied()
    }

    /// Records the C function of [`Heap::callback_entry`] for `key`.
    pub fn set_callback_entry(&self, key: TypeId, entry: usize) {
        self.state().callback_entries.insert(key, entry);
    }

    /// The function `name` of the trait of the host marked by the Rust type
    /// `marker`. It's kept here, like [`Heap::callback_entry`], so handles of
    /// trait objects find it.
    pub fn trait_function(&self, marker: TypeId, name: &str) -> Option<TraitFunction> {
        self.state().trait_functions.get(&(marker, name.to_owned())).copied()
    }

    /// Records a function of [`Heap::trait_function`].
    pub fn set_trait_function(&self, marker: TypeId, name: String, function: TraitFunction) {
        self.state().trait_functions.insert((marker, name), function);
    }

    /// Whether `value_ptr` points to the value of an object of this heap.
    pub fn contains(&self, value_ptr: *const ()) -> bool {
        if value_ptr.is_null() {
            return false;
        }

        let state = self.state();
        let object = object_of(value_ptr);

        state.pages.contains(object) || state.large.contains(&object)
    }

    /// Allocates a zeroed object and returns a pointer to its value. With
    /// `may_collect`, garbage may be collected first, and a null pointer is
    /// returned (and the program stopped) if the heap limit is exceeded.
    ///
    /// # Safety
    ///
    /// The layout must be valid, and live as long as the heap (see
    /// [`Heap::intern_layout`]). With `may_collect`, every frame of compiled
    /// code on the stack must be described (see the module docs).
    pub unsafe fn alloc(&self, type_layout: &'static TypeLayout, may_collect: bool) -> *mut () {
        unsafe { self.state().alloc(self, type_layout, may_collect) }
    }

    /// Allocates an array of `length` zeroed elements and returns a pointer to
    /// its [`Array`] value. See [`Heap::alloc`].
    ///
    /// # Safety
    ///
    /// See [`Heap::alloc`].
    pub unsafe fn alloc_array(&self, item_layout: &'static TypeLayout, length: usize, may_collect: bool) -> *mut () {
        unsafe { self.state().alloc_array(self, item_layout, length, may_collect) }
    }

    /// Changes the length of an array, growing its buffer if needed. New
    /// elements are zeroed. Garbage may be collected first (the array itself
    /// is kept), and the heap limit is checked like for other allocations of
    /// compiled code. Returns `false` (and stops the program) if the array
    /// can't grow; it's left unchanged then.
    ///
    /// # Safety
    ///
    /// `array` must point to the value of an array object of this heap, and
    /// every frame of compiled code on the stack must be described (see the
    /// module docs).
    pub unsafe fn realloc_array(&self, array: *mut Array, length: usize) -> bool {
        unsafe { self.state().realloc_array(self, array, length) }
    }

    /// Collects garbage now.
    ///
    /// # Safety
    ///
    /// Compiled code of this heap on the stack (if any) must have called the
    /// host through a recorded wrapper, which is the case for functions it
    /// imports.
    pub unsafe fn collect(&self) {
        unsafe { self.state().collect(self) };
    }

    /// Registers a compiled function: its address, size and stack maps (code
    /// offsets of return addresses, with the offsets from the stack pointer of
    /// GC references live there).
    pub fn register_code(&self, start: usize, size: usize, stack_maps: impl IntoIterator<Item = (u32, Box<[u32]>)>) {
        let state = self.state();

        state.code.insert(start, start + size);

        for (return_addr, offsets) in stack_maps {
            state.code.stack_maps.insert(start + return_addr as usize, offsets);
        }
    }

    /// Keeps `layout` as long as the heap, for objects and compiled code of
    /// the heap.
    pub fn intern_layout(&self, layout: TypeLayout) -> &'static TypeLayout {
        let layout = NonNull::from(Box::leak(Box::new(layout)));

        self.state().layouts.push(layout);

        // SAFETY: freed with the heap, after its objects and code.
        unsafe { layout.as_ref() }
    }

    /// Keeps `fields` as long as the heap (see [`Heap::intern_layout`]).
    pub fn intern_fields(&self, fields: Vec<(AdtVariantRef, u32, MollieType, TypeLayoutField)>) -> &'static LayoutFields {
        if fields.is_empty() {
            return &[];
        }

        let fields = NonNull::from(Box::leak(fields.into_boxed_slice()));

        self.state().fields.push(fields);

        // SAFETY: freed with the heap, after its objects and code.
        unsafe { fields.as_ref() }
    }

    /// Layout of objects holding a Rust value of type `T` (with no GC
    /// references).
    pub fn layout_of<T: 'static>(&self) -> &'static TypeLayout {
        if let Some(&layout) = self.state().type_layouts.get(&TypeId::of::<T>()) {
            return layout;
        }

        let layout = self.intern_layout(TypeLayout::of::<T>());

        self.state().type_layouts.insert(TypeId::of::<T>(), layout);

        layout
    }

    /// Allocations of compiled code fail if live objects would take more than
    /// `limit` bytes. Returns the previous limit.
    pub fn set_limit(&self, limit: Option<usize>) -> Option<usize> {
        mem::replace(&mut self.state().limit, limit)
    }

    /// Bytes allocated (since the last collection) that make the next
    /// collection due, if fewer bytes are live (1 MiB by default). A smaller
    /// threshold gives shorter, more frequent pauses: they mostly cost the
    /// garbage they free. Returns the previous threshold.
    pub fn set_collection_threshold(&self, bytes: usize) -> usize {
        let state = self.state();
        let previous = mem::replace(&mut state.threshold, bytes.max(1));

        state.perform_gc_at = state.allocated_bytes.saturating_add(state.threshold.max(state.allocated_bytes));

        previous
    }

    /// Whether allocations of compiled code collect garbage when it's due
    /// (the default). Without it, garbage is collected when the host asks
    /// (see [`Heap::collect_if_due`], e.g. between frames of a game), when
    /// the heap limit would be exceeded, or when the heap grows to several
    /// times its usual size. Returns the previous setting.
    pub fn set_auto_collect(&self, enabled: bool) -> bool {
        mem::replace(&mut self.state().auto_collect, enabled)
    }

    /// Collects garbage if it's due (as an allocation would), returning
    /// whether it did. Meant for the host, at moments when a pause doesn't
    /// matter, with [`Heap::set_auto_collect`] off.
    ///
    /// # Safety
    ///
    /// See [`Heap::collect`].
    pub unsafe fn collect_if_due(&self) -> bool {
        let state = self.state();
        let due = state.allocated_bytes >= state.perform_gc_at;

        if due {
            unsafe { state.collect(self) };
        }

        due
    }

    /// Makes every allocation of compiled code collect garbage (or stops doing
    /// so). Objects freed while still used then cause failures right away,
    /// instead of once in a while.
    pub fn set_stress(&self, enabled: bool) {
        self.state().stress = enabled;
    }

    pub fn stats(&self) -> HeapStats {
        let state = self.state();

        HeapStats {
            allocated_bytes: state.allocated_bytes,
            deallocated_bytes: state.deallocated_bytes,
            objects: state.pages.objects + state.large.len(),
            collections: state.collections,
            last_pause: state.last_pause,
            max_pause: state.max_pause,
            last_phases: state.last_phases,
            last_freed_objects: state.last_freed_objects,
            next_collection_at: state.perform_gc_at,
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let state = self.state.get_mut();

        for object in state.large.drain() {
            // SAFETY: objects of the heap are valid until it's dropped. Pages
            // are freed after them (with their objects).
            unsafe { free_large(object) };
        }

        for layout in state.layouts.drain(..) {
            // SAFETY: interned layouts are only used by objects and code of the
            // heap, which are gone.
            drop(unsafe { Box::from_raw(layout.as_ptr()) });
        }

        for fields in state.fields.drain(..) {
            drop(unsafe { Box::from_raw(fields.as_ptr()) });
        }
    }
}

/// The heap of the program running on this thread, for the runtime.
fn heap() -> Option<&'static Heap> {
    sandbox::current_heap()
}

/// Allocation by compiled code, through a wrapper recording its frame.
/// Returns null if the heap limit is exceeded, which stops the program.
pub(crate) unsafe extern "C" fn compiled_alloc(type_layout: &'static TypeLayout) -> *mut () {
    heap().map_or(ptr::null_mut(), |heap| unsafe { heap.alloc(type_layout, true) })
}

/// Array allocation by compiled code, through a wrapper recording its frame.
/// Returns null if the heap limit is exceeded, which stops the program.
pub(crate) unsafe extern "C" fn compiled_alloc_array(item_layout: &'static TypeLayout, length: usize) -> *mut () {
    heap().map_or(ptr::null_mut(), |heap| unsafe { heap.alloc_array(item_layout, length, true) })
}

/// Growing of arrays by compiled code, through a wrapper recording its frame.
/// Returns `false` (and stops the program) if the array can't grow.
///
/// # Safety
///
/// `array` must point to the value of an array object.
pub(crate) unsafe extern "C" fn realloc_array(array: *mut Array, length: usize) -> bool {
    heap().is_some_and(|heap| unsafe { heap.realloc_array(array, length) })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, hash::BuildHasherDefault};

    use super::{AddressHasher, CodeTable};

    #[test]
    fn code_is_found_by_address() {
        let mut code = CodeTable::default();

        for (start, end) in [(300, 400), (100, 200), (500, 510), (200, 250)] {
            code.insert(start, end);
        }

        for pc in [100, 199, 200, 249, 300, 399, 505] {
            assert!(code.contains(pc), "{pc}");
        }

        for pc in [0, 99, 250, 299, 400, 499, 510, 1000] {
            assert!(!code.contains(pc), "{pc}");
        }
    }

    #[test]
    fn aligned_addresses_spread() {
        let mut set: HashSet<usize, BuildHasherDefault<AddressHasher>> = HashSet::default();

        for index in 0..10_000 {
            assert!(set.insert(0x7F00_0000_0000 + index * 16));
        }

        assert_eq!(set.len(), 10_000);
        assert!(set.contains(&(0x7F00_0000_0000 + 5_000 * 16)));
        assert!(!set.contains(&(0x7F00_0000_0000 + 5_000 * 16 + 8)));
    }
}
