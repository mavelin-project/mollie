#![allow(clippy::cast_ptr_alignment)]

use std::{
    alloc,
    any::TypeId,
    cell::{RefCell, UnsafeCell},
    collections::{HashMap, HashSet},
    hash, mem,
    ptr::{self, NonNull},
    sync::{Arc, Mutex, PoisonError, Weak},
    time::{Duration, Instant},
};

use mollie_index::Idx;
use mollie_ir::MollieType;
use mollie_typing::{AdtKind, AdtVariantRef};

use crate::sandbox::{self, TrapKind};

mod pages;

use pages::{Kind, Pages};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum TypeLayoutField {
    Regular,
    Collectable,
    FuncEnv,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(C)]
pub struct TypeLayout {
    pub fields: &'static [(AdtVariantRef, u32, MollieType, TypeLayoutField)],
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
    pub layout: &'static TypeLayout,
    // `usize`, so that the value is aligned to pointer size.
    pub info: GcValueInfo,
    pub value: T,
}

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

pub type LayoutFields = [(AdtVariantRef, u32, MollieType, TypeLayoutField)];

const MIN_COLLECTION_THRESHOLD: usize = 1024 * 1024;

const UNCOLLECTED_GROWTH: usize = 4;

#[derive(Debug, Default)]
struct CodeTable {
    ranges: Vec<(usize, usize)>,
    stack_maps: HashMap<usize, Box<[u32]>, Hasher>,
}

impl CodeTable {
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PausePhases {
    pub roots: Duration,
    pub mark: Duration,
    pub sweep: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeapStats {
    pub allocated_bytes: usize,
    pub deallocated_bytes: usize,
    pub objects: usize,
    pub collections: usize,
    pub last_pause: Duration,
    pub max_pause: Duration,
    pub last_phases: PausePhases,
    pub last_freed_objects: usize,
    pub next_collection_at: usize,
}

thread_local! {
    static EXIT_FRAMES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static PINNED: RefCell<Vec<(*const Heap, *mut u8)>> = const { RefCell::new(Vec::new()) };
    static STACK_ARRAYS: RefCell<Vec<(*const Heap, *const Array, &'static TypeLayout)>> = const { RefCell::new(Vec::new()) };
}

pub fn pin(values: &[*mut u8]) {
    let heap = sandbox::current_heap().map_or(ptr::null(), ptr::from_ref);

    PINNED.with_borrow_mut(|pinned| pinned.extend(values.iter().map(|&value| (heap, value))));
}

pub fn unpin(count: usize) {
    PINNED.with_borrow_mut(|pinned| {
        let length = pinned.len().saturating_sub(count);

        pinned.truncate(length);
    });
}

pub extern "C" fn stack_array_push(array: *const Array, element_layout: &'static TypeLayout) {
    let heap = sandbox::current_heap().map_or(ptr::null(), ptr::from_ref);

    STACK_ARRAYS.with_borrow_mut(|arrays| arrays.push((heap, array, element_layout)));
}

pub extern "C" fn stack_array_pop() {
    STACK_ARRAYS.with_borrow_mut(Vec::pop);
}

pub fn stack_arrays() -> usize {
    STACK_ARRAYS.with_borrow(Vec::len)
}

pub fn truncate_stack_arrays(length: usize) {
    STACK_ARRAYS.with_borrow_mut(|arrays| arrays.truncate(length));
}

pub extern "C" fn exit_push(frame_pointer: usize) {
    EXIT_FRAMES.with_borrow_mut(|frames| frames.push(frame_pointer));
}

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

fn buffer_layout(item_layout: &TypeLayout, capacity: usize) -> Result<Option<alloc::Layout>, ()> {
    let size = item_layout.size.checked_mul(capacity).ok_or(())?;

    if size == 0 {
        Ok(None)
    } else {
        alloc::Layout::from_size_align(size, item_layout.align.max(1)).map(Some).map_err(|_| ())
    }
}

fn out_of_memory(may_collect: bool, layout: alloc::Layout) {
    if may_collect {
        sandbox::set_trap(TrapKind::OutOfMemory);
    } else {
        alloc::handle_alloc_error(layout);
    }
}

const fn compute_capacity(length: usize) -> Option<usize> {
    if length == 0 { Some(0) } else { length.checked_next_power_of_two() }
}

const fn object_of(value_ptr: *const ()) -> Object {
    value_ptr.cast::<u8>().wrapping_sub(HEADER_SIZE).cast_mut().cast()
}

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

#[derive(Clone, Default)]
pub struct Releaser(Arc<Mutex<Vec<usize>>>);

impl Releaser {
    pub fn unroot(&self, value_ptr: *const ()) {
        if !value_ptr.is_null() {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).push(value_ptr.expose_provenance());
        }
    }

    fn take(&self) -> Vec<*const ()> {
        mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
            .into_iter()
            .map(ptr::with_exposed_provenance)
            .collect()
    }
}

struct HeapState {
    pages: Pages,
    large: HashSet<Object, Hasher>,
    released: Releaser,
    roots: HashMap<Object, usize, Hasher>,
    code: CodeTable,
    stress: bool,
    allocated_bytes: usize,
    deallocated_bytes: usize,
    perform_gc_at: usize,
    threshold: usize,
    limit: Option<usize>,
    auto_collect: bool,
    collections: usize,
    last_pause: Duration,
    max_pause: Duration,
    last_phases: PausePhases,
    last_freed_objects: usize,
    layouts: Vec<NonNull<TypeLayout>>,
    fields: Vec<NonNull<LayoutFields>>,
    type_layouts: HashMap<TypeId, &'static TypeLayout>,
    callback_entries: HashMap<TypeId, usize>,
    trait_functions: HashMap<(TypeId, String), TraitFunction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraitFunction {
    pub index: usize,
    pub signature: TypeId,
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

    unsafe fn collect(&mut self, heap: *const Heap) {
        let start = Instant::now();

        for value_ptr in self.released.take() {
            self.unroot(value_ptr);
        }
        let mut roots = self.roots.iter().filter(|&(_, &count)| count > 0).map(|(&root, _)| root).collect::<Vec<_>>();

        PINNED.with_borrow(|pinned| {
            roots.extend(
                pinned
                    .iter()
                    .filter(|&&(pin_heap, value)| ptr::eq(pin_heap, heap) && !value.is_null())
                    .map(|&(_, value)| value.wrapping_sub(HEADER_SIZE).cast::<GcValue<()>>()),
            );
        });

        STACK_ARRAYS.with_borrow(|arrays| {
            for &(array_heap, array, layout) in arrays {
                if !ptr::eq(array_heap, heap) {
                    continue;
                }

                // SAFETY: registered arrays are on the stack until they're
                // unregistered.
                let array = unsafe { &*array };

                for index in 0..array.length {
                    let element = unsafe { array.ptr.cast::<u8>().add(index * layout.size) };

                    unsafe { Self::push_references(layout, element, AdtVariantRef::ZERO, &mut roots) };
                }
            }
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

pub struct Heap {
    state: UnsafeCell<HeapState>,
    alive: Arc<()>,
}

// SAFETY: the heap owns everything its raw pointers point to (objects, pages,
// layouts), so it can move to another thread with them. It isn't `Sync`: one
// thread uses it at a time. Other threads only reach it through handles of
// the host, which never touch it directly: they release objects through a
// `Releaser`, and need the compiler (only on the heap's thread, since it's
// not `Sync` either) or a run of its program to do anything else. Entries of
// the thread-locals (frames, pins, runs) only exist while a program runs, on
// that thread, and a running heap is borrowed, so it can't move.
#[allow(clippy::non_send_fields_in_send_ty, reason = "see the SAFETY comment")]
unsafe impl Send for Heap {}

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
                released: Releaser::default(),
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
            alive: Arc::new(()),
        }
    }

    pub fn alive_token(&self) -> Weak<()> {
        Arc::downgrade(&self.alive)
    }

    pub fn releaser(&self) -> Releaser {
        self.state().released.clone()
    }

    #[allow(clippy::mut_from_ref, reason = "borrows never overlap, see the type's docs")]
    fn state(&self) -> &mut HeapState {
        // SAFETY: the heap is used by one thread, and every borrow of its state
        // ends before another one is made (see the type's docs).
        unsafe { &mut *self.state.get() }
    }

    pub fn root(&self, value_ptr: *const ()) {
        self.state().root(value_ptr);
    }

    pub fn unroot(&self, value_ptr: *const ()) {
        self.state().unroot(value_ptr);
    }

    pub fn callback_entry(&self, key: TypeId) -> Option<usize> {
        self.state().callback_entries.get(&key).copied()
    }

    pub fn set_callback_entry(&self, key: TypeId, entry: usize) {
        self.state().callback_entries.insert(key, entry);
    }

    pub fn trait_function(&self, marker: TypeId, name: &str) -> Option<TraitFunction> {
        self.state().trait_functions.get(&(marker, name.to_owned())).copied()
    }

    pub fn set_trait_function(&self, marker: TypeId, name: String, function: TraitFunction) {
        self.state().trait_functions.insert((marker, name), function);
    }

    pub fn contains(&self, value_ptr: *const ()) -> bool {
        if value_ptr.is_null() {
            return false;
        }

        let state = self.state();
        let object = object_of(value_ptr);

        state.pages.contains(object) || state.large.contains(&object)
    }

    /// # Safety
    ///
    /// `type_layout` must be interned in this heap. If `may_collect`, it may
    /// collect first: objects of this heap held only by the caller (not rooted,
    /// pinned, or on compiled code's stack) are freed. Over the heap's limit it
    /// sets an out-of-memory trap and returns null; without `may_collect`, it
    /// aborts instead. The object is zeroed, not a valid value of its type.
    pub unsafe fn alloc(&self, type_layout: &'static TypeLayout, may_collect: bool) -> *mut () {
        unsafe { self.state().alloc(self, type_layout, may_collect) }
    }

    /// # Safety
    ///
    /// As for [`Heap::alloc`], for an array of `length` zeroed elements of
    /// `item_layout`.
    pub unsafe fn alloc_array(&self, item_layout: &'static TypeLayout, length: usize, may_collect: bool) -> *mut () {
        unsafe { self.state().alloc_array(self, item_layout, length, may_collect) }
    }

    /// # Safety
    ///
    /// `array` must be a live array of this heap. Growing it may collect, as
    /// [`Heap::alloc`] with `may_collect` (the array itself is kept); new
    /// elements are zeroed. On failure it sets an out-of-memory trap, leaves the
    /// array as it was and returns `false`.
    pub unsafe fn realloc_array(&self, array: *mut Array, length: usize) -> bool {
        unsafe { self.state().realloc_array(self, array, length) }
    }

    /// # Safety
    ///
    /// Every object still used must be reachable from the roots: the host's
    /// roots, pinned values, registered stack arrays, or frames of compiled code
    /// of this heap on this thread. Objects held otherwise (a raw pointer kept
    /// by the host) are freed, and pointers to them dangle.
    pub unsafe fn collect(&self) {
        unsafe { self.state().collect(self) };
    }

    pub fn register_code(&self, start: usize, size: usize, stack_maps: impl IntoIterator<Item = (u32, Box<[u32]>)>) {
        let state = self.state();

        state.code.insert(start, start + size);

        for (return_addr, offsets) in stack_maps {
            state.code.stack_maps.insert(start + return_addr as usize, offsets);
        }
    }

    pub fn intern_layout(&self, layout: TypeLayout) -> &'static TypeLayout {
        let layout = NonNull::from(Box::leak(Box::new(layout)));

        self.state().layouts.push(layout);

        // SAFETY: freed with the heap, after its objects and code.
        unsafe { layout.as_ref() }
    }

    pub fn intern_fields(&self, fields: Vec<(AdtVariantRef, u32, MollieType, TypeLayoutField)>) -> &'static LayoutFields {
        if fields.is_empty() {
            return &[];
        }

        let fields = NonNull::from(Box::leak(fields.into_boxed_slice()));

        self.state().fields.push(fields);

        // SAFETY: freed with the heap, after its objects and code.
        unsafe { fields.as_ref() }
    }

    pub fn layout_of<T: 'static>(&self) -> &'static TypeLayout {
        if let Some(&layout) = self.state().type_layouts.get(&TypeId::of::<T>()) {
            return layout;
        }

        let layout = self.intern_layout(TypeLayout::of::<T>());

        self.state().type_layouts.insert(TypeId::of::<T>(), layout);

        layout
    }

    pub fn set_limit(&self, limit: Option<usize>) -> Option<usize> {
        mem::replace(&mut self.state().limit, limit)
    }

    pub fn set_collection_threshold(&self, bytes: usize) -> usize {
        let state = self.state();
        let previous = mem::replace(&mut state.threshold, bytes.max(1));

        state.perform_gc_at = state.allocated_bytes.saturating_add(state.threshold.max(state.allocated_bytes));

        previous
    }

    pub fn set_auto_collect(&self, enabled: bool) -> bool {
        mem::replace(&mut self.state().auto_collect, enabled)
    }

    /// # Safety
    ///
    /// As for [`Heap::collect`].
    pub unsafe fn collect_if_due(&self) -> bool {
        let state = self.state();
        let due = state.allocated_bytes >= state.perform_gc_at;

        if due {
            unsafe { state.collect(self) };
        }

        due
    }

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

fn heap() -> Option<&'static Heap> {
    sandbox::current_heap()
}

pub(crate) unsafe extern "C" fn compiled_alloc(type_layout: &'static TypeLayout) -> *mut () {
    heap().map_or(ptr::null_mut(), |heap| unsafe { heap.alloc(type_layout, true) })
}

pub(crate) unsafe extern "C" fn compiled_alloc_array(item_layout: &'static TypeLayout, length: usize) -> *mut () {
    heap().map_or(ptr::null_mut(), |heap| unsafe { heap.alloc_array(item_layout, length, true) })
}

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
