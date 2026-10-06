//! Programs run in stress mode, where every allocation collects garbage: an
//! object missing from stack maps is freed while still used.

use mollie_compiler::{allocator::Heap, sandbox::Limits};
use mollie_typing::TypeRef;

use crate::{compiler, lock, run_i32_stressed};

const POINT: &str = "struct Point { x: i32, y: i32 }\n";

#[test]
fn linked_list_survives_collections() {
    assert_eq!(
        run_i32_stressed(
            "enum List { Cons { value: i32, next: List }, Nil }

func build(n: i32) -> List {
    let mut list = List::Nil;
    let mut i = 0;

    while i < n {
        list = List::Cons { value: i, next: list };
        i += 1;
    }

    list
}

func sum(list: List) -> i32 {
    let mut total = 0;
    let mut current = list;

    while current is List::Cons { value, next } {
        total += value;
        current = next;
    }

    total
}

sum(build(100))"
        ),
        4950
    );
}

#[test]
fn temporaries_survive_collections() {
    // The first `Point` is only a temporary while the second one and the `Pair`
    // are allocated.
    assert_eq!(
        run_i32_stressed(&format!(
            "{POINT}struct Pair {{ a: Point, b: Point }}

func make(i: i32) -> Pair {{
    Pair {{ a: Point {{ x: i, y: i }}, b: Point {{ x: i * 2, y: i * 2 }} }}
}}

let mut total = 0;
let mut i = 0;

while i < 200 {{
    let pair = make(i);
    total += pair.a.x + pair.b.y;
    i += 1;
}}

total"
        )),
        59700
    );
}

#[test]
fn arrays_of_objects_survive_collections() {
    assert_eq!(
        run_i32_stressed(&format!(
            "{POINT}let items = [Point {{ x: 0, y: 0 }}];
let mut i = 1;

while i < 100 {{
    items.push(Point {{ x: i, y: i }});
    i += 1;
}}

let mut total = 0;
let mut j = 0;

while j < 100 {{
    total += items[j].x;
    j += 1;
}}

total"
        )),
        4950
    );
}

#[test]
fn large_objects_and_buffers_survive_collections() {
    // Objects and buffers of more than 256 bytes don't fit in pages: they come
    // from the system allocator, next to small ones.
    let fields = (0..40).map(|i| format!("f{i}: i64")).collect::<Vec<_>>().join(", ");
    let values = (0..40).map(|i| format!("f{i}: i as i64")).collect::<Vec<_>>().join(", ");

    assert_eq!(
        run_i32_stressed(&format!(
            "{POINT}struct Large {{ point: Point, {fields} }}

let mut i = 0;
let kept = [Large {{ point: Point {{ x: 0, y: 0 }}, {values} }}];
let numbers = [0];

i = 1;

while i < 100 {{
    kept.push(Large {{ point: Point {{ x: i, y: i }}, {values} }});
    numbers.push(i);
    // Garbage of both sizes.
    let garbage = Large {{ point: Point {{ x: 0, y: 0 }}, {values} }};
    let small = [i, i];
    i += 1;
}}

let mut total = 0;

for large in kept {{
    total += large.point.x + large.f39 as i32;
}}

for number in numbers {{
    total += number;
}}

total"
        )),
        // Points and `f39` of every kept object, then the numbers.
        2 * 4950 + 4950
    );
}

#[test]
fn closure_environments_survive_collections() {
    assert_eq!(
        run_i32_stressed(&format!(
            "{POINT}func adder(point: Point) -> func(i32) -> i32 {{
    |x| {{ x + point.x }}
}}

let add = adder(Point {{ x: 40, y: 0 }});
let mut i = 0;

while i < 100 {{
    let garbage = Point {{ x: i, y: i }};
    i += 1;
}}

add(2)"
        )),
        42
    );
}

#[test]
fn unreachable_objects_are_freed() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let source = format!(
        "{POINT}let mut i = 0;

while i < 1000 {{
    let garbage = Point {{ x: i, y: i }};
    i += 1;
}}

0"
    );
    let mut provider = compiler.start_compiling();

    if let Err(error) = provider.compile("main", Vec::<(String, TypeRef)>::new(), Some(i32), &source) {
        panic!("compilation failed:\n{}", error.display(&provider.type_context.tcx));
    }

    let main = unsafe { compiler.inner.get_func::<extern "C" fn() -> i32>("main") }.expect("`main` must be compiled");

    assert_eq!(compiler.inner.run(Limits::default(), || main()), Ok(0));
    // Too few objects for a collection while the program ran.
    assert!(compiler.inner.heap_stats().objects >= 1000);

    compiler.inner.collect_garbage();

    // No compiled code is running and nothing is rooted by the host.
    assert_eq!(compiler.inner.heap_stats().objects, 0);
}

#[test]
fn host_roots_keep_objects_alive() {
    let heap = Heap::new();
    let value = unsafe { heap.alloc(heap.layout_of::<u64>(), false) };

    heap.root(value);
    unsafe { heap.collect() };

    assert!(heap.contains(value));

    heap.unroot(value);
    unsafe { heap.collect() };

    assert!(!heap.contains(value));
}

#[test]
fn heaps_are_collected_separately() {
    let (first, second) = (Heap::new(), Heap::new());
    let value = unsafe { first.alloc(first.layout_of::<u64>(), false) };

    first.root(value);
    // Collecting the other heap doesn't see the object, or its root.
    unsafe { second.collect() };

    assert!(first.contains(value));
    assert!(!second.contains(value));
    assert_eq!(second.stats().objects, 0);

    first.unroot(value);
    unsafe { first.collect() };

    assert!(!first.contains(value));
}

/// Runs a program allocating `count` objects of garbage with `auto_collect`,
/// and returns its compiler (for its heap).
fn allocate_garbage(count: i32, auto_collect: bool) -> mollie_compiler::Compiler<()> {
    allocate_garbage_with(count, auto_collect, None)
}

/// Like [`allocate_garbage`], with a collection threshold.
fn allocate_garbage_with(count: i32, auto_collect: bool, threshold: Option<usize>) -> mollie_compiler::Compiler<()> {
    let mut compiler = compiler();

    if let Some(threshold) = threshold {
        compiler.inner.heap().set_collection_threshold(threshold);
    }

    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let source = format!(
        "{POINT}let mut i = 0;

while i < {count} {{
    let garbage = Point {{ x: i, y: i }};
    i += 1;
}}

0"
    );
    let mut provider = compiler.start_compiling();

    if let Err(error) = provider.compile("main", Vec::<(String, TypeRef)>::new(), Some(i32), &source) {
        panic!("compilation failed:\n{}", error.display(&provider.type_context.tcx));
    }

    let main = unsafe { compiler.inner.get_func::<extern "C" fn() -> i32>("main") }.expect("`main` must be compiled");
    let limits = Limits {
        auto_collect: Some(auto_collect),
        ..Limits::default()
    };

    assert_eq!(compiler.inner.run(limits, || main()), Ok(0));

    compiler
}

#[test]
fn collections_can_be_left_to_the_host() {
    let _guard = lock();
    // More than a collection is due for, less than the safety valve.
    let compiler = allocate_garbage(50_000, false);
    let stats = compiler.inner.heap_stats();

    assert_eq!(stats.collections, 0, "{stats:?}");
    assert!(stats.allocated_bytes >= stats.next_collection_at, "{stats:?}");

    // Between frames, the host collects.
    assert!(compiler.inner.collect_garbage_if_due());
    assert_eq!(compiler.inner.heap_stats().objects, 0);
    assert!(!compiler.inner.collect_garbage_if_due());

    // Allocations collect by default.
    assert!(allocate_garbage(50_000, true).inner.heap_stats().collections > 0);
}

#[test]
fn smaller_thresholds_give_more_collections() {
    let _guard = lock();
    let default = allocate_garbage(50_000, true).inner.heap_stats();
    // 50 000 points of 24 bytes (plus headers) over 64 KiB thresholds.
    let small = allocate_garbage_with(50_000, true, Some(64 * 1024)).inner.heap_stats();

    assert!(small.collections > 4 * default.collections.max(1), "{small:?} vs {default:?}");
    // Only the garbage since the last collection is left.
    assert!(small.allocated_bytes < 2 * 64 * 1024, "{small:?}");
}

#[test]
fn thresholds_grow_with_live_objects() {
    let _guard = lock();
    let compiler = compiler();
    let heap = compiler.inner.heap();

    let live = compiler.inner.heap_stats().allocated_bytes;

    assert_eq!(heap.set_collection_threshold(4096), 1024 * 1024);
    // Due after 4 KiB more, or as many bytes as are live.
    assert_eq!(compiler.inner.heap_stats().next_collection_at, live + live.max(4096));
    assert_eq!(heap.set_collection_threshold(0), 4096);
    assert_eq!(compiler.inner.heap_stats().next_collection_at, live + live.max(1));
}

#[test]
fn heaps_left_to_the_host_still_have_bounds() {
    let _guard = lock();
    // The host never collects: allocations do once the heap grows too much.
    let stats = allocate_garbage(400_000, false).inner.heap_stats();

    assert!(stats.collections > 0, "{stats:?}");
    assert!(stats.allocated_bytes < 8 * stats.next_collection_at.max(1024 * 1024), "{stats:?}");
}
