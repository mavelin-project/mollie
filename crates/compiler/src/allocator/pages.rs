//! Memory of small objects and array buffers: pages of blocks of one size
//! class, with bitmaps of the blocks in use, of marked objects and of
//! arrays. Sweeping works on whole words of these bitmaps, so it costs about
//! the pages and the dead arrays (whose buffers it frees), not every dead
//! object, and telling objects from other pointers is a page lookup and a bit.

use std::{
    alloc,
    collections::HashSet,
    mem,
    ptr::{self, NonNull},
};

use super::{Array, GcValue, Hasher, Object, buffer_layout};

/// Bytes of a page, which is aligned to its size: the page of a block is its
/// address with the low bits cleared.
const PAGE_SIZE: usize = 16 * 1024;
/// Blocks of size class `c` take `(c + 1) * GRANULE` bytes.
const GRANULE: usize = 16;
/// Size classes: blocks of up to 256 bytes. Larger memory comes from the
/// system allocator.
const CLASSES: usize = 16;
/// Words of each bitmap: a bit for each block (a page never has more blocks
/// than granules).
const WORDS: usize = PAGE_SIZE / GRANULE / 64;

/// What the blocks of a page hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Objects (a header and a value), swept by collections.
    Objects = 0,
    /// Buffers of arrays, freed with their array.
    Buffers = 1,
}

/// The header of a page, followed by its blocks.
#[repr(C)]
struct Page {
    /// Blocks in use.
    used: [u64; WORDS],
    /// Objects marked by the running collection.
    marked: [u64; WORDS],
    /// Objects that are arrays: their buffers are freed with them.
    arrays: [u64; WORDS],
    kind: Kind,
    block_size: usize,
    /// Offset of the first block.
    first: usize,
    blocks: usize,
    used_count: usize,
}

impl Page {
    const fn layout() -> alloc::Layout {
        match alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE) {
            Ok(layout) => layout,
            Err(_) => panic!("pages have a valid layout"),
        }
    }

    fn base(&self) -> usize {
        ptr::from_ref(self) as usize
    }

    /// The index of the block starting at `address`, if there's one.
    fn index_of(&self, address: usize) -> Option<usize> {
        let offset = address.checked_sub(self.base() + self.first)?;
        let index = offset / self.block_size;

        (offset % self.block_size == 0 && index < self.blocks).then_some(index)
    }

    fn block(&self, index: usize) -> *mut u8 {
        (self.base() + self.first + index * self.block_size) as *mut u8
    }

    const fn words(&self) -> usize {
        self.blocks.div_ceil(64)
    }

    /// Bits of the word `word` that stand for blocks.
    const fn valid(&self, word: usize) -> u64 {
        let start = word * 64;

        if self.blocks >= start + 64 {
            u64::MAX
        } else if self.blocks <= start {
            0
        } else {
            (1 << (self.blocks - start)) - 1
        }
    }

    const fn is_used(&self, index: usize) -> bool {
        self.used[index / 64] & (1 << (index % 64)) != 0
    }

    /// Bytes of its free blocks.
    const fn free_bytes(&self) -> usize {
        (self.blocks - self.used_count) * self.block_size
    }
}

/// Pages of a heap.
pub struct Pages {
    /// Addresses of every page, to tell blocks from other pointers.
    bases: HashSet<usize, Hasher>,
    /// Pages by kind and size class.
    classes: [[Vec<NonNull<Page>>; CLASSES]; 2],
    /// The page allocations of each kind and size class look for a free block
    /// in first (pages before it are full).
    cursors: [[usize; CLASSES]; 2],
    /// Objects in pages.
    pub objects: usize,
}

impl Default for Pages {
    fn default() -> Self {
        Self::new()
    }
}

impl Pages {
    pub fn new() -> Self {
        Self {
            bases: HashSet::default(),
            classes: std::array::from_fn(|_| std::array::from_fn(|_| Vec::new())),
            cursors: [[0; CLASSES]; 2],
            objects: 0,
        }
    }

    /// The size class of blocks for `layout`, if it's small.
    const fn class_of(layout: alloc::Layout) -> Option<usize> {
        if layout.align() <= GRANULE && layout.size() <= GRANULE * CLASSES {
            Some(layout.size().saturating_sub(1) / GRANULE)
        } else {
            None
        }
    }

    /// Whether memory for `layout` comes from pages.
    pub const fn is_small(layout: alloc::Layout) -> bool {
        Self::class_of(layout).is_some()
    }

    /// Bytes memory for `layout` takes.
    pub const fn size_of(layout: alloc::Layout) -> usize {
        match Self::class_of(layout) {
            Some(class) => (class + 1) * GRANULE,
            None => layout.size(),
        }
    }

    fn new_page(&mut self, class: usize, kind: Kind) -> Option<NonNull<Page>> {
        // SAFETY: the layout isn't empty.
        let page = NonNull::new(unsafe { alloc::alloc_zeroed(Page::layout()) }.cast::<Page>())?;
        let block_size = (class + 1) * GRANULE;
        let first = mem::size_of::<Page>().next_multiple_of(GRANULE);

        // SAFETY: the page is allocated and zeroed (empty bitmaps).
        unsafe {
            let page = page.as_ptr();

            (*page).kind = kind;
            (*page).block_size = block_size;
            (*page).first = first;
            (*page).blocks = (PAGE_SIZE - first) / block_size;
            (*page).used_count = 0;
        }

        self.bases.insert(page.as_ptr() as usize);

        Some(page)
    }

    /// Zeroed memory for `layout`, or null. Small memory is a block of a page
    /// of `kind`, other memory comes from the system allocator.
    pub fn alloc(&mut self, layout: alloc::Layout, kind: Kind) -> *mut u8 {
        let Some(class) = Self::class_of(layout) else {
            // SAFETY: objects have a header and empty buffers aren't
            // allocated, so the layout isn't empty.
            return unsafe { alloc::alloc_zeroed(layout) };
        };
        let pages = kind as usize;

        loop {
            let cursor = self.cursors[pages][class];
            let Some(&page) = self.classes[pages][class].get(cursor) else {
                let Some(page) = self.new_page(class, kind) else {
                    return ptr::null_mut();
                };

                // Found at the cursor next.
                self.classes[pages][class].push(page);

                continue;
            };
            // SAFETY: pages of the heap are valid, and not borrowed elsewhere.
            let page = unsafe { &mut *page.as_ptr() };

            if page.used_count < page.blocks {
                for word in 0..page.words() {
                    let free = !page.used[word] & page.valid(word);

                    if free == 0 {
                        continue;
                    }

                    let bit = free.trailing_zeros() as usize;
                    let block = page.block(word * 64 + bit);

                    page.used[word] |= 1 << bit;
                    page.used_count += 1;

                    if kind == Kind::Objects {
                        self.objects += 1;
                    }

                    // SAFETY: the block is in the page, and free.
                    unsafe { block.write_bytes(0, page.block_size) };

                    return block;
                }
            }

            self.cursors[pages][class] += 1;
        }
    }

    /// The page of a block (without checking that it's one).
    const fn page_of(address: usize) -> *mut Page {
        (address & !(PAGE_SIZE - 1)) as *mut Page
    }

    /// Frees memory from [`Self::alloc`] for buffers.
    ///
    /// # Safety
    ///
    /// `ptr` must come from [`Self::alloc`] with `layout`, and not be used
    /// anymore.
    pub unsafe fn free_buffer(&mut self, ptr: *mut u8, layout: alloc::Layout) {
        if !Self::is_small(layout) {
            return unsafe { alloc::dealloc(ptr, layout) };
        }

        // SAFETY: small memory is a block of a page of the heap.
        let page = unsafe { &mut *Self::page_of(ptr as usize) };

        if let Some(index) = page.index_of(ptr as usize) {
            page.used[index / 64] &= !(1 << (index % 64));
            page.used_count -= 1;
        }
    }

    /// The page and index of `object`, if it's an object in a page.
    fn locate(&self, object: Object) -> Option<(*mut Page, usize)> {
        let address = object as usize;
        let page = Self::page_of(address);

        if !self.bases.contains(&(page as usize)) {
            return None;
        }

        // SAFETY: the page is a page of the heap.
        let page_ref = unsafe { &*page };
        let index = page_ref.index_of(address)?;

        (page_ref.kind == Kind::Objects && page_ref.is_used(index)).then_some((page, index))
    }

    /// Whether `object` is an object in a page.
    pub fn contains(&self, object: Object) -> bool {
        self.locate(object).is_some()
    }

    /// Records that the object in a page `object` is an array.
    pub fn set_array(&mut self, object: Object) {
        if let Some((page, index)) = self.locate(object) {
            // SAFETY: the page is a page of the heap.
            unsafe { (*page).arrays[index / 64] |= 1 << (index % 64) };
        }
    }

    /// Marks `object`: `None` if it isn't an object in a page, otherwise
    /// whether it wasn't marked yet.
    pub fn mark(&self, object: Object) -> Option<bool> {
        let (page, index) = self.locate(object)?;
        let (word, bit) = (index / 64, 1 << (index % 64));

        // SAFETY: the page is a page of the heap. Its bitmaps aren't borrowed.
        unsafe {
            if (*page).marked[word] & bit != 0 {
                return Some(false);
            }

            (*page).marked[word] |= bit;
        }

        Some(true)
    }

    /// Frees objects that aren't marked (and buffers of arrays among them),
    /// and unmarks the others. Then frees empty pages while more than
    /// `keep_free` bytes of blocks are free. Returns the number of freed
    /// objects and bytes.
    ///
    /// # Safety
    ///
    /// Objects in pages must be valid.
    pub unsafe fn sweep(&mut self, keep_free: usize) -> (usize, usize) {
        let (mut objects, mut bytes) = (0, 0);

        for class in 0..CLASSES {
            for index in 0..self.classes[Kind::Objects as usize][class].len() {
                let page = self.classes[Kind::Objects as usize][class][index].as_ptr();

                for word in 0..unsafe { (*page).words() } {
                    // SAFETY: the page is a page of the heap. Buffers are in
                    // other pages.
                    let (dead, mut dead_arrays) = unsafe {
                        let dead = (*page).used[word] & !(*page).marked[word];

                        (*page).marked[word] = 0;

                        (dead, dead & (*page).arrays[word])
                    };

                    if dead == 0 {
                        continue;
                    }

                    while dead_arrays != 0 {
                        let block = unsafe { (*page).block(word * 64 + dead_arrays.trailing_zeros() as usize) };
                        // SAFETY: dead objects are still valid.
                        let array = unsafe { &*block.cast::<GcValue<Array>>() };

                        if let Ok(Some(layout)) = buffer_layout(array.layout, array.value.capacity) {
                            unsafe { self.free_buffer(array.value.ptr.cast(), layout) };

                            bytes += Self::size_of(layout);
                        }

                        dead_arrays &= dead_arrays - 1;
                    }

                    let count = dead.count_ones() as usize;

                    unsafe {
                        (*page).used[word] &= !dead;
                        (*page).arrays[word] &= !dead;
                        (*page).used_count -= count;
                        bytes += count * (*page).block_size;
                    }

                    objects += count;
                }
            }
        }

        self.objects -= objects;
        self.release_empty_pages(keep_free);

        (objects, bytes)
    }

    /// Frees empty pages while more than `keep_free` bytes of blocks are free,
    /// and starts allocations from the first pages again.
    fn release_empty_pages(&mut self, keep_free: usize) {
        let mut free = self
            .classes
            .iter()
            .flatten()
            .flatten()
            // SAFETY: pages of the heap are valid.
            .map(|page| unsafe { page.as_ref() }.free_bytes())
            .sum::<usize>();
        let bases = &mut self.bases;

        for pages in self.classes.iter_mut().flatten() {
            pages.retain(|&page| {
                // SAFETY: pages of the heap are valid.
                let page_ref = unsafe { page.as_ref() };

                if page_ref.used_count > 0 || free <= keep_free {
                    return true;
                }

                free -= page_ref.free_bytes();
                bases.remove(&(page.as_ptr() as usize));

                // SAFETY: the page is empty, and forgotten.
                unsafe { alloc::dealloc(page.as_ptr().cast(), Page::layout()) };

                false
            });
        }

        self.cursors = [[0; CLASSES]; 2];
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // Buffers of arrays in pages that come from the system allocator.
        for page in self.classes[Kind::Objects as usize].iter().flatten() {
            // SAFETY: pages of the heap are valid, and so are their objects.
            let page = unsafe { page.as_ref() };

            for word in 0..page.words() {
                let mut arrays = page.used[word] & page.arrays[word];

                while arrays != 0 {
                    let block = page.block(word * 64 + arrays.trailing_zeros() as usize);
                    let array = unsafe { &*block.cast::<GcValue<Array>>() };

                    if let Ok(Some(layout)) = buffer_layout(array.layout, array.value.capacity)
                        && !Self::is_small(layout)
                    {
                        unsafe { alloc::dealloc(array.value.ptr.cast(), layout) };
                    }

                    arrays &= arrays - 1;
                }
            }
        }

        for page in self.classes.iter().flatten().flatten() {
            // SAFETY: pages are only freed here, or when they're forgotten.
            unsafe { alloc::dealloc(page.as_ptr().cast(), Page::layout()) };
        }
    }
}
