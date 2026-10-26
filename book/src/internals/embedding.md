# Embedding Mollie

A host is the game or application that runs Mollie programs. It creates a `Compiler`, registers its API with `Host`, compiles programs, and calls them. Everything here is in `mollie::host` unless it says otherwise.

## A compiler per program

```rust,ignore
use mollie::{compiler::Compiler, typed_ast::FileModuleLoader};

let mut compiler = Compiler::with_symbols(
    FileModuleLoader { current_dir: "addons/shop/src".into() },
    [],
)?;
```

A `Compiler` owns a heap, compiled code and the host's functions. Dropping it frees all of them.

Programs of different compilers can't reach each other. Giving a value of one compiler to another is checked, and the program stops with a trap.

One compiler can also hold several programs, since every `compile_script` adds one with its own items. They share the heap and the host's functions, so they can share values and [libraries](#libraries). So it's a choice for the host. A compiler per program keeps programs apart, and gives each one its own memory limit. One compiler for several programs lets them work together.

The module loader finds submodules for `module ui;`. `FileModuleLoader` reads them from files next to the root, and `()` doesn't load any.

## Registering the host's API

`Host::new(&mut compiler)` registers the items programs can use. Rust values cross over through the `HostValue` trait, which is implemented for numbers, `bool`, `MolStr` and `String`, `GcPtr<T>`, `Opaque<T>`, value types, callbacks, trait objects and arrays.

### Functions

```rust,ignore
let mut host = Host::new(&mut compiler);

host.function_named("log", &["message"], |message: MolStr| println!("{message}"));
host.function("random", move || rng.next());
```

The compiler keeps the closures. Compilers can move between threads (see [Threads](#threads)), so closures must be `Send`, and state they share with the host goes in an `Arc`.

If the last parameter's name starts with `..`, the function is variadic, and gets the rest of the arguments as a `Slice` (see [Variadic Parameters](../language/arrays.md#variadic-parameters)):

```rust,ignore
host.function_named("tr", &["key", "..args"], |key: MolStr, args: Slice<MolStr>| {
    translate(&key, &args.iter().map(MolStr::as_str).collect::<Vec<_>>())
});
```

Programs then call `tr("menu.play")` or `tr("options.fov.value", "${fov}")`.

A panic in a host function stops the program with a trap carrying the panic's message. The host itself goes on. To stop a program on purpose, use `raise!`:

```rust,ignore
host.function("check", |level: i32| {
    if level > 15 {
        mollie::raise!("level {level} is over 15");
    }

    level
});
```

### Arrays

Arrays of plain values, which are numbers, `bool` and value types (`PlainValue`), can be passed as a `Vec<T>`. As an argument, the program's array is copied into it. As a result, it's copied into a new array. One call returning a `Vec<f32>` is much cheaper than a call per element. Arrays of anything else are `GcArray<T>`.

A Rust array of plain values, `[T; N]`, is a fixed array `T[N]`. Like a value type, it's passed by pointer and returned by value.

`Slice<T>` is a view (`T[..]`) of any array, given to the host function as a copy of its elements. `SliceMut<T>` is a `mut` view of plain values. The host reads and sets the elements in place, with `len`, `get`, `set` and `copy_from`, and a fixed array given to it gets the changes back after the call. Both are only valid during the call, and host functions can't return them.

### Modules and capabilities

```rust,ignore
host.module("graphics");            // following items: `graphics::...`, visible to every program
host.capability("fs");              // following items: only for programs granted `fs`
host.everywhere();                  // back to the root module
```

A program compiled with `compile_script_with(name, params, source, &["fs"])` can use `fs`. Any other program gets "`fs` isn't available to this addon".

### Libraries

A library is Mollie code that several programs of a compiler use, like a UI library, or an addon other addons depend on. It's loaded and checked once, and each of its functions is compiled once, the first time a program uses it:

```rust,ignore
let source = std::fs::read_to_string("resources/ui/lib.mol")?;

// Its submodules are files in its folder (`module widgets;` is `widgets.mol`).
compiler.load_library("ui", &source, Path::new("resources/ui"), &LibraryOptions::default())?;

compiler.compile_script_in::<Main, ()>("shop", &["data"], &main, &ScriptOptions {
    libraries: &["ui"],
    ..ScriptOptions::default()
})?;
```

Like a capability, a library is only visible to programs that ask for it (`ScriptOptions::libraries`), and to libraries that depend on it (`LibraryOptions::dependencies`). Dependencies must be loaded first.

Programs and libraries only see each other's `pub` items, unless they're in the same *package*. An addon's program and its own library would be one package: give both the same `ScriptOptions::package` and `LibraryOptions::package`. Without a package, each program and each library is a package of its own.

Since programs of one compiler share its heap, they can also pass values to each other. A library uses visibility to keep the parts it doesn't want changed to itself (see [Libraries and Visibility](../language/modules.md#libraries-and-visibility)).

### Types

```rust,ignore
// An object: programs hold references (`GcPtr<Counter>` in Rust).
#[repr(C)]
struct Counter { count: i32 }

host.object::<Counter>("Counter").field::<i32>("count").finish();

// A value type: copied, passed by value (needs `host_value_type!`).
#[repr(C)]
#[derive(Clone, Copy)]
struct Vec2 { x: f32, y: f32 }

mollie::host_value_type!(Vec2);
host.value_type::<Vec2>("Vec2").field::<f32>("x").field::<f32>("y").finish();

// Fields can have defaults: `Block { id: "stone" }`.
host.object::<Block>("Block")
    .field::<MolStr>("id")
    .field_default::<bool>("collidable", true)
    .finish();

// Enums: `#[repr(usize)]`, variants and fields in order.
host.enum_::<Shape>("Shape")
    .variant("Circle").field::<f32>("radius")
    .variant("Rect").field::<f32>("width").field::<f32>("height")
    .finish();

// A handle to host data programs can only pass back.
host.opaque::<DrawContext>("DrawContext");
```

`finish` checks that the declared fields add up to the size and alignment of the Rust type. It can't check each field's offset or type, so be careful with fields that point into the heap: **`MolStr`, `GcPtr` and arrays must be declared as what they are**, or the collector won't see them and will free what they point to.

### Methods and impls

```rust,ignore
host.methods::<GcPtr<Counter>>()
    .method_named("bump", &["by"], |counter: GcPtr<Counter>, by: i32| { /* ... */ })
    .finish();

host.implement::<GcPtr<Counter>>(display_trait, &[])
    .method("show", |counter: GcPtr<Counter>| format!("{}", counter.count))
    .finish();
```

A property looks like a field to programs. Reading `value.name` calls its getter, and `value.name = x` calls its setter. Properties are a good way to group an API without a method for every value, and they work on opaque types too:

```rust,ignore
host.methods::<Opaque<GameApi>>()
    .property("inventory", |_: Opaque<GameApi>| handle(InventoryApi))  // `game.inventory.size`
    .property("fov", |_: Opaque<GameApi>| fov.get())
    .setter("fov", |_: Opaque<GameApi>, value: f32| fov.set(value))      // `game.fov = 70.0`
    .finish();
```

A getter takes only the value, and a setter takes the value and the new value. If the type already has a field with that name, the builder panics.

A setter on a value type gets a copy of the value. That means it can't change the value itself, only state the host keeps somewhere else.

### Traits programs implement

```rust,ignore
enum DrawableTrait {}

host.trait_::<DrawableTrait>("Drawable")
    .method::<(Size, Opaque<DrawContext>), Size>("measure", &["size", "ctx"])
    .method::<(Point, Size, Opaque<DrawContext>), ()>("render", &["origin", "size", "ctx"])
    .finish();
```

Programs implement `Drawable` for their own types. The host gets their values as `ScriptObject<DrawableTrait>` and calls them, as shown [below](#compiling-and-calling-programs).

A function of the trait can have a default body, written in Mollie. Types implementing the trait can then leave it out. Like any default, it can use `self` and the trait's other functions:

```rust,ignore
host.trait_::<BehaviourTrait>("Behaviour")
    .method_default::<(), bool>("ticks", &[], "{ false }")
    .method_default::<(i32,), i32>("speed", &["scale"], "{ if self.ticks() { scale } else { 0 } }")
    .finish();
```

`finish` type checks the defaults, and panics with their errors. So whatever they use from the host must be registered before the trait.

## Compiling and calling programs

```rust,ignore
compiler.compile_script::<(Opaque<DrawContext>,), ScriptObject<DrawableTrait>>("main", &["context"], &source)?;

let main = compiler.script_fn::<(Opaque<DrawContext>,), ScriptObject<DrawableTrait>>("main")?;
let root = main.call((context,), Limits::default())?;
let size: Size = root.call(&compiler, "measure", (window, context), Limits::default())?;
```

A program is a body of code. `params` become its variables, and its last expression is its result. The Rust types in the call are checked against the program, so calling it with the wrong ones is an error, not undefined behaviour.

Type errors come as `CompileError::Type(diagnostics)`, with spans. They can be printed with `ariadne`, through `Diagnostic::add_to_report`. Other errors print with `error.display(&compiler.type_context.tcx)`.

## Values held by the host

| Handle | What it is |
| --- | --- |
| `GcPtr<T>` | A reference to an object. It isn't rooted, so it's only valid during the program's run, unless rooted. |
| `GcRoot<'h, T>` | A rooted `GcPtr`. The collector keeps its object alive. |
| `ScriptCallback<Args, R>` | A function value of a program. `call(args)` during the program's run, `call_with_limits(&compiler, args, limits)` from the host. |
| `ScriptObject<M>` | A trait object of a program, called with `call(&compiler, name, args, limits)`. |
| `GcArray<T>` | An array of a program: `len`, `get`, `set`, `push`, `to_vec`. |
| `Opaque<T>` | A handle to host data. Programs can only pass it back. |

Handles keep their values alive. Using a handle after its compiler was dropped returns an error, never undefined behaviour.

Most methods of handles need proof that the current thread has access to the program. Outside of a run, that's the compiler itself (`&compiler`). Inside a host function called by the program, it's `Running`, as in `array.len(&Running)`.

## Limits

```rust,ignore
let limits = Limits {
    fuel: Some(1_000_000),          // calls and loop iterations
    call_depth: Some(256),          // calls in progress
    heap_bytes: Some(64 << 20),     // live objects of this program
    auto_collect: Some(false),      // collect between frames instead
    ..Limits::default()
};
```

A program that goes over a limit stops with a `Trap`, which has a `kind`, a message, a location and a backtrace. Both the host and the compiler can carry on after it.

When fuel runs out, `Limits::refuel` is asked for more. It's how a host can let a program run longer after deciding it's fine.

[Runtime](runtime.md#limits) lists every limit with its default.

### Garbage collection in a game loop

```rust,ignore
let limits = Limits { auto_collect: Some(false), ..Limits::default() };

main.call(args, limits)?;               // no collection pauses inside
compiler.inner.collect_garbage_if_due(); // between frames
```

`compiler.inner.heap_stats()` reports live bytes, collections and pauses. A bigger threshold, set with `compiler.inner.heap().set_collection_threshold(bytes)`, means fewer but longer pauses. It's 1 MiB by default. [Runtime](runtime.md#garbage-collector) has the numbers.

## Reloading

Compiling a program again with the same name replaces it. That doesn't free the old version, though. To free its code, its heap and its host closures, drop the compiler and create a new one, and keep state that must survive in plain Rust data. `examples/reload.rs` shows both ways.

## Threads

`Compiler` is `Send` but not `Sync`. It can be created and run on one thread, like a loading thread, then moved to another. Only one thread can use it at a time.

Handles are `Send` too. Dropping one on any thread is fine: its value is released at the program's next collection. Using one needs access to its program, as described [above](#values-held-by-the-host).

## Editors

`compiler.write_host_stub(project)` writes the declarations of the host's API to `project/.mollie/host`, which the language server reads. It only writes when something changed. See [Tooling](tooling.md).
