# Runtime

## Values

This is how each type looks to compiled code:

| Mollie type | In compiled code |
| --- | --- |
| `i8` … `i64`, `u8` … `u64`, `isize`, `usize` | an integer of that size |
| `f32`, `f64` | a float of that size |
| `bool` | an `i8`, 0 or 1 |
| `string` | a pointer to a GC array of bytes (UTF-8) |
| `T[]` | a pointer to a GC array: `{ length, capacity, pointer to elements }` |
| `T[N]` | its elements, inline, one after another |
| `T[..]` | a pointer to an array header |
| structs, enums, views | a pointer to a GC object |
| value types | their bytes, inline |
| trait objects | two words: a pointer to the value, and a pointer to the vtable |
| function values | two words: the code, and a pointer to the environment |

Every GC object has a header before its value, with its layout and some flags. Compiled code only ever sees pointers to the value, past the header. An enum's value starts with the index of its variant.

Value types are stored with their fields in C layout. In registers, they're split into *chunks* of 8, 4, 2 or 1 bytes (`mollie_ir::chunks`). When a value type is put into a trait object, the trait object points to a copy of it on the heap.

A fixed array is laid out like a value struct whose fields are its elements. Reading or writing an element at an index known only at run time goes through a copy of the array in a stack slot, which is written back to the variable after a write.

A view (`T[..]`) points to an array header. Given a growable array, that's the array itself. Given a fixed array, the array is copied to the stack after a header pointing to it, and a `mut` view's changes are copied back after the call. These headers are never on the heap, so the collector skips them.

A closure's environment is a GC object holding the values it captured. A function that captures nothing has a null environment.

A vtable starts with the hash of the type, followed by the code of the trait's functions in the order the trait declares them.

## Calling conventions

Compiled functions call each other with Cranelift's own convention. Arguments are passed as chunks, and results of more than two chunks go through a return area.

The host never calls compiled code directly. Each entry point gets a wrapper with a C signature, made for its types:

| Called by the host | Wrapper |
| --- | --- |
| A program (`ScriptFn::call`) | `extern "C" fn(result: *mut R, args...)` |
| A trait function (`ScriptObject::call`) | `extern "C" fn(result: *mut R, code, value, args...)` |
| A callback (`ScriptCallback::call*`) | `extern "C" fn(result: *mut R, code, environment, args...)` |

C passes small structs in other registers than compiled code does, and the wrappers translate between the two. Values of value types are passed by pointer, and the result is written through `result` in its memory layout.

Host functions are called the other way around: compiled code calls a wrapper, which calls an `extern "C"` shim holding the Rust closure.

Before compiled code calls the runtime or the host, it records its frame (`exit_push`, `exit_pop`). That's how the collector walks the stack (see [Roots](#roots)).

## Runs, traps and limits

Compiled code only runs inside a *run* (`sandbox::run`). A run sets up the program's state (`VmState`) and limits, and catches whatever stops the program. `VmState` holds the trap code, the fuel left, the stack limit, the call depth and its limit, the program's heap, and the trace of the last trap.

These checks are compiled into the code:

| Check | Where it happens |
| --- | --- |
| Fuel | Every function call and loop iteration takes one unit. At 0, the program stops with `out of fuel`, unless `Limits::refuel` gives more. |
| Call depth | In every function's prologue. Above `call_depth`, the program stops with `stack overflow`. |
| Stack | In every function's prologue, the stack pointer is compared with a limit: the stack left on the thread, minus 128 KiB for the host. |
| Memory | An allocation above `heap_bytes`, even after collecting, fails with `out of memory`. |
| Arithmetic | Division and remainder by zero, and bounds of arrays and string slices. |

When something traps, the trap code is set and the code returns through every frame, since each call checks the code after it returns. A trap is one of: index out of bounds, division by zero, arithmetic overflow, invalid string slice, out of fuel, stack overflow, out of memory, raised by the host (`raise!`), unreachable code, and panic.

The compiler records the source location of every place that can trap and of every call. When a program stops, the trap's site and up to 32 calls it went through are kept in `VmState::trace`, innermost first. `Trap` turns them into a location and a backtrace:

```text
index out of bounds
  --> ui::list:12:9
  in Grid::render (ui::grid:40:13)
  in <main> (main:8:1)
```

### Limits

| Limit | Default | When it's exceeded |
| --- | --- | --- |
| Fuel (function calls and loop iterations) | none (`Limits::fuel`) | `out of fuel` |
| Calls in progress | 1024 (`Limits::call_depth`) | `stack overflow` |
| Stack bytes | the stack left on the thread (`Limits::stack_bytes`) | `stack overflow` |
| Bytes of live objects | none (`Limits::heap_bytes`) | `out of memory` |
| Size of a module's source | 1 MiB | compile error |
| Nesting of expressions, blocks, types and operator chains | 256 levels | compile error |
| Strings interpolated in each other | 32 levels | compile error |
| Generic instances | 20 000, of at most 256 type nodes each | compile error |

The compile-time limits are in `mollie_shared::limits`.

## Garbage collector

Each compiler has its own heap (`allocator::Heap`), with a mark-and-sweep collector that never moves objects.

### Memory

Objects of up to 256 bytes, and the buffers of arrays, are blocks in 16 KiB *pages*. Each page has one size class, in steps of 16 bytes. Pages are aligned to their size, so the page of a pointer is its address with the low bits cleared. A page keeps three bitmaps: blocks in use, marked objects, and arrays.

Bigger objects come from the system allocator, and the heap keeps them in a set.

### Roots

The collector starts from:

- references held by compiled code. Cranelift spills them to the stack around calls, and describes where in *stack maps*. The collector walks the frames of compiled code from every recorded exit, skipping frames of other programs nested through host callbacks;
- values the host rooted, with `GcRoot` or a handle like `ScriptCallback`;
- the arguments of a host function, while it runs;
- fixed arrays copied to the stack for a call (for views and variadic arguments) that hold references. The call may store new objects there that nothing else points to. Compiled code registers them (`molstack_push`, `molstack_pop`), and when a run ends with a trap, the registrations its calls didn't remove are dropped.

### Collecting

First, the roots and everything reachable from them are marked. A pointer is an object of the heap if its page belongs to the heap and its block is in use, or if it's one of the big objects. Marking sets the object's bit in its page.

Then sweeping goes through the bitmaps 64 bits at a time, with `dead = used & !marked`. Dead objects are freed without being read, except arrays, whose buffers are freed with them. Empty pages go back to the system, apart from a few kept for reuse.

Handles the host drops, on any thread, are queued (`Releaser`) and unrooted when the next collection starts.

A collection is due once the program has allocated `max(threshold, live bytes)` since the last one. The threshold is 1 MiB, and `Heap::set_collection_threshold` changes it. With `Limits::auto_collect = Some(false)`, allocations don't collect, and the host collects between frames instead. The heap still collects by itself if it grows to several times its usual size, or would go over its limit.

In the stress test of `examples/ui.rs`, a tree of views is rebuilt every frame, leaving about 21 000 dead objects per collection at 1 MiB. A collection takes about 64 µs, 143 µs at most, and the whole frame about 7.5 µs on average.

### Layouts

A `TypeLayout` tells the collector where an object's fields point to other objects, per variant for enums. Layouts are interned per heap, and freed with it.

## Determinism

A program given the same inputs and limits does the same thing on every machine, and if it traps, it traps at the same point. That's what lets peers in a multiplayer game run the same addon and agree. It takes several things:

- Integers wrap around on overflow. Division and remainder by zero trap, and so does dividing the minimum value by -1.
- Floats are IEEE 754, 32 or 64 bits, without fused operations.
- NaNs get the same bits wherever a program can see them: when stored, returned, given to the host, or bitcast (see `crates/compiler/src/nans.rs`). Canonicalizing every result would be slower.
- Converting a float to an integer saturates, and NaN becomes 0.
- `sin`, `cos`, `tan`, `atan2`, `exp`, `ln` and `pow` are Mollie's own (`crates/compiler/src/math.rs`), not the platform's `libm`. `sqrt`, `floor`, `ceil`, `trunc` and `round` are exact IEEE operations anyway.
- Recursion is limited by call depth, not by stack bytes, which differ between platforms. The stack check is only a safety net.
- `Hash` and `Map` use fixed hash functions, collections iterate in insertion order, and nothing depends on addresses.
- The compiler reports diagnostics in a fixed order.

Mollie has no clock or randomness of its own, so a host must keep its own functions deterministic too. Peers must also run the same version of Mollie.

## Details of the language

The [language chapters](../language/index.md) are an introduction and leave out some exact rules. These are the ones a host author is most likely to ask about.

Numbers:

- Shifts take their amount modulo the width of the type, so `1 << 33` is `2` for an `i32`. Signed integers shift their sign in from the left, and unsigned ones shift in zeros.
- `as` from a float to an integer rounds toward zero and saturates. `f32` to `f64` is exact, and `f64` to `f32` rounds to the nearest. A literal converted to a float gets that type directly, so `0.1 as f64` is `0.1`, not `0.1` rounded to `f32` first.
- `round()` rounds halfway cases away from zero.
- In a format, `0` pads after the sign: `${-7:04}` is `-007`.

Strings:

- `len()`, `slice()` and `find()` work in bytes. Slicing in the middle of a character traps.
- `to_lower()` and `to_upper()` only change ASCII letters.
- Strings are ordered by their bytes.
- `f32` and `f64` implement `Eq`, but not `Hash`.

Arrays:

- `sort_by` is stable, and its function returns a negative number, 0 or a positive number.
- `resize(length, value)` truncates, or adds copies of `value`.
- A fixed array is at most 4 KiB. A literal of the wrong length for a fixed array is an error.
- Elements of a fixed array change only through a `let mut` variable, like fields of value structs. A view changes the array it's given only when it's `mut`, and giving it a fixed array needs a `let mut` variable.
- Views can't be fields, results, elements of arrays or type arguments, and closures can't capture them. A variable can only view a growable array or another view, since viewing a fixed array would see a copy of it.
- `for` goes through every kind of array by index.

Values and calls:

- `mut self` called through a generic bound changes a copy. Called through a trait object, it changes the trait object's own copy.
- A closure captures variables when it's created. An immutable variable declared before a loop can't be assigned inside it.
- Defaults of parameters are computed during the call, after the given arguments. Naming a parameter that doesn't exist, passing one twice, or leaving out one without a default is an error.
- `x.name += value` on a property calls the getter, then the setter, so `x` must be a variable.
- `pub` applies to structs, enums, views, traits, functions, constants, modules, imports, fields of structs and views, and functions of impl blocks. Functions of a trait's impls are as visible as the trait.
