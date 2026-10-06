## Before You Begin

This documentation does NOT describe or cover specific language implementations in detail; it merely touches on some of them.

It is primarily intended to provide an introduction to the language’s structure for those who are just beginning to learn it and/or are unfamiliar with other programming languages.

For more detailed information about its implementation, see [`docs`](../docs/README.md).

## Navigation

- [What is Mollie?](#what-is-mollie)
- [Syntax](#syntax)
  - [Expressions](#expressions)
    - [Primitive Values](#primitive-values)
    - [Strings](#strings)
    - [If-else and Loops](#if-else-and-loops)
    - [Ranges](#ranges)
    - [Pattern Matching](#pattern-matching)
    - [Calling Functions](#calling-functions)
    - ...
  - [Statements](#statements)
    - [Variables](#variables)
    - [Constants](#constants)
  - [Traits](#traits)
    - [Default Functions](#default-functions)
    - [Bounds of Generics](#bounds-of-generics)
    - ...
- [Limits and Determinism](#limits-and-determinism)
- [Tools](#tools)
- Type System
  - Primitives
  - Structures
  - Enumerations
  - Views
  - Traits
  - Functions
- ...

## What is Mollie?

Mollie is a scripting language with a strict type system, much like the Rust programming language.

However, unlike Rust, Mollie has a garbage collector. This is largely because implementing a full-fledged ownership and lifetime system in a language designed to be relatively easy to learn and use would be somewhat impractical.

Also, one of Mollie’s features is a separate type designed primarily for describing user interfaces in a Qt-like style. It’s called `view`. We’ll talk about it in more detail a little later. For now, let’s take a look at...

## Syntax

It’s hard to call Mollie an "ideal language" with a well-thought-out syntax that has no unnecessary elements. If you’re already familiar with languages like JavaScript, C#, Go, and Rust, Mollie’s syntax will seem like an attempt to borrow a little bit from each of them. And that’s exactly what it is. Perhaps the language’s creators could have been a bit more creative, but let’s get to the point.

Mollie consists of two parts: expressions and statements. In fact, most of the syntax consists of expressions. So let’s start with those.

### Expressions

The core functionality of the language, allowing you to do everything you’re used to. Function calls, arrays, loops, code blocks. All of this is an expression! I’ll also note that if an expression doesn’t end with a code block or isn’t a value returned from a function, it must end with a semicolon.

#### Primitive Values

As in most languages, it all starts with numbers. Well, or strings! Essentially, there’s nothing special here:

- `1`: Integers are written as usual.
- `1.0`: Floating-point numbers use a `.` to separate the integer and fractional parts.
- `"Hello, World!"`: Strings are enclosed in double quotes.
- `true`, `false`: Boolean values use `true` and `false`.

In the future, there may be a character as a separate type, whose value will be created via a character enclosed in single quotes, but this is not currently the case.

It’s also worth noting that numbers can contain an optional suffix indicating either their specific type (`1u64`) or a suffix function (`1.0px`).

Numbers support the usual operators: `+`, `-`, `*`, `/` and `%` (the remainder of a division, for integers), with their assigning forms `+=`, `-=`, `*=`, `/=` and `%=`. Dividing an integer by zero stops the program.

#### Strings

Strings are immutable. You can join them with `+`, and put values right into them with `${...}`:

```rust
let name = "Ann";
let count = 3;

let greeting = "Hello, " + name + "!";
let summary = "${name} has ${count} items, that's ${count > 1}";
```

Only strings, numbers and booleans can be put into a string this way.

A value can be followed by a format, after `:`: `${value:format}`. A format is written as `[align][0][width][.precision][kind]`, and every part is optional:

- `width` pads the value with spaces up to this many characters. Numbers are aligned to the right and everything else to the left, unless `align` says otherwise: `<` (left), `>` (right) or `^` (center).
- `0` pads numbers with zeros instead, after their sign.
- `.precision` is the number of digits after the point, for `f32` only.
- `kind` writes integers in hexadecimal (`x`, or `X` for uppercase digits) or binary (`b`).

```rust
let price = 3.14159;
let id = 255;

println("${price:.2}");     // 3.14
println("[${"ok":>4}]");     // [  ok]
println("${7:03}");         // 007
println("${id:x} ${id:b}"); // ff 11111111
```

A format that doesn't fit the value, like `${name:.2}` for a string, isn't compiled. To write `${` itself, escape the dollar sign: `"\${not a value}"`. Other escapes are `\n`, `\t`, `\r`, `\0`, `\"` and `\\`.

Strings also know their length in bytes (`"héllo".len()` is `6`), and can be sliced by byte offsets: `"hello".slice(1, 3)` is `"el"`. Slicing in the middle of a character stops the program.

### If-else and Loops

They are combined into a single section because, essentially, both affect the "flow" of the resulting code. Let’s start with if-else, then move on to loops, and finish with a couple of examples.

If you’re mostly familiar with C-like languages such as JavaScript or C itself, you may have noticed that in Mollie, the if condition doesn’t need to be enclosed in parentheses. This is an intentional and final decision in the language’s design.

What about loops? Here, you have several options.

- The familiar `while` loop, which executes code as long as the condition is true.
- `for in` for iterating over array elements, [ranges](#ranges) and anything else implementing `Iterable`.
- An infinite loop using `loop`.

In each of these, you can use `break` to force the loop to terminate and `continue` to skip the rest of the current iteration and move on to the next one.

`loop` can also produce a value: `break value` ends the loop with it. `while` and `for` may end without `break`, so they can't.

Loops can be labeled with `'name:`. Then `break 'name` and `continue 'name` apply to that loop instead of the innermost one, which is handy for nested loops.

Now, let’s look at some examples!

```rust
if 4 > 4 {
  println("How is that possible?! :fearful:");
}
```

```rust
while true {
  println("Why not use a loop?");
}
```

```rust
for num in [1, 2, 3] {
  if num == 2 {
    continue; // I don't like this number, let's skip it
  }

  println(num);
}
```

```rust
let mut attempt = 0;
let found = loop {
  attempt += 1;

  if attempt * attempt > 50 {
    break attempt; // `found` is 8
  }
};
```

```rust
'rows: for row in grid {
  for cell in row {
    if cell == 0 {
      continue 'rows; // skip the rest of this row
    }
  }
}
```

### Ranges

`start..end` are the integers from `start` up to `end`, without it, and `start..=end` includes `end` too. They're mostly used with `for`:

```rust
for i in 0..3 {
  println(i); // 0, 1, 2
}
```

A range is a value of type `Range<T>` of the standard library, with fields `start`, `end` and `inclusive`, and functions `contains(value)` and `is_empty()`: `(1..=5).contains(5)` is `true`. Both sides must have the same type.
### Pattern Matching

`match` compares a value with patterns, top to bottom, and evaluates the first arm that matches. It must handle every possible value, otherwise the program isn't compiled.

```rust
let area = match shape {
  Circle { radius } => radius * radius * 3.14,
  Rect { width, height } if width == height => width * width,
  Rect { width, height } => width * height,
  Empty => 0.0,
};
```

Patterns can be:

- literals: `0`, `"name"`, `true`;
- `_`, which matches anything;
- a name, which matches anything and gives it a name: `other => ...`;
- variants of enums and structs, with patterns of their fields: `Some { value: 0 }`, `Point { x, y }` (a field without a pattern is given a variable named like it). The name of the enum (`Option::Some`) can be left out when the type of the value is known.

The name of the enum can also be left out when creating a variant where its type is known, like in an annotated variable, an argument or a returned value:

```rust
func first(values: i32[]) -> Option<i32> {
  if values.len() == 0 { None } else { Some { value: values[0] } }
}
```

An arm can have a guard, `if condition`, checked after its pattern matches. The same patterns work with `is`, which tells whether a value matches: `if value is Some { value } { ... }`.

### Fields and Methods

`value.name` is always the field `name`, while `value.name(...)` calls the method `name`. If there's no such method but there's a field holding a function, the field is called, so callbacks read naturally:

```rust
struct Button { on_click: func() }

button.on_click();
```

### Calling Functions

Parameters of functions can have default values, written after their type. A call can leave such arguments out, and the default is computed during the call. Defaults may use the parameters before them, including `self`:

```rust
func button(text: string, enabled: bool = true, width: i32 = text.len() as i32 * 8) -> Button {
  ...
}
```

Arguments can be passed by name, in any order, after the ones passed by position:

```rust
button("OK");
button("Cancel", enabled: false);
button(width: 120, text: "Save");
```

Arguments are evaluated in the order they're written, then the missing ones are filled in with their defaults. Naming a parameter that doesn't exist, passing one twice, or leaving out one without a default isn't compiled. Functions stored in variables and fields (like closures) only take arguments by position.
### The Standard Library

`std` comes with every program. Items of its prelude can be used without importing them: `Option`, `Result`, `Iterable`, `Iterator`, `Container`, `Range`, `Number`, `Float`, `PI`, `TAU`, `E`, `Eq`, `Hash`, `ArrayMethods`, `Search`, `Text`, `Map`, `Set` and `Entry`. Everything else is imported from `std`: `import { Chars } from std::string;`.

- `Option<T>` is `Some { value: T }` or `None`, with `is_some()`, `is_none()` and `unwrap_or(default)`.
- `Result<T, E>` is `Ok { value: T }` or `Err { error: E }`, with `is_ok()`, `is_err()`, `ok()`, `err()` and `unwrap_or(default)`.
- `Container<C>` is implemented by types with children of type `C`.
- `Range<T>` is the value of `start..end` (see [Ranges](#ranges)). `std::range::Step` is implemented by integer types, which ranges can go through.
- Numbers (`Number`): `abs()`, `min(other)`, `max(other)` and `clamp(low, high)`. Floats also have `sqrt()`, `floor()`, `ceil()`, `trunc()`, `round()` (halfway cases away from zero), `sin()`, `cos()`, `tan()`, `atan2(x)`, `exp()`, `ln()`, `pow(exponent)`, `lerp(to, t)`, `signum()`, `to_radians()` and `to_degrees()`. They give the same results on every platform.
- Arrays: `push(item)`, `len()`, `truncate(length)`, `is_empty()`, `pop() -> Option<T>`, `insert(index, value)`, `remove(index) -> T`, `clear()`, `reverse()`, `copy()` and `sort_by(|a, b| { ... })` (stable; `compare` returns a negative number, 0 or a positive number). With elements that implement `Eq`: `contains(value)` and `index_of(value) -> Option<usize>`.
- Strings: `len()` (in bytes), `char_count()`, `slice(start, end)` (byte offsets), `contains(part)`, `starts_with(part)`, `ends_with(part)`, `find(part) -> Option<usize>`, `split(separator) -> string[]`, `trim()`, `to_lower()`, `to_upper()` (ASCII), `repeat(count)`, `parse_int() -> Option<i64>`, `parse_float() -> Option<f32>`, `chars()` and `bytes()` (iterated with `for`). Strings are ordered by their bytes with `<`, `<=`, `>` and `>=`.
- `Eq` and `Hash` are implemented by numbers, `bool` and `string` (`Hash` isn't implemented by `f32`). Hashes are the same on every platform.
- `Map<K, V>` (keys implementing `Hash` and `Eq`): `Map::new()`, `insert(key, value) -> Option<V>`, `get(key) -> Option<V>`, `contains(key)`, `remove(key) -> Option<V>`, `len()`, `keys()`, `values()`, and `for entry in map { entry.key ... entry.value }`. Entries are iterated in insertion order. `Set<K>` has `Set::new()`, `insert(value) -> bool`, `contains(value)`, `remove(value) -> bool`, `len()` and `values()`.

```mollie
let scores: Map<string, i32> = Map::new();

scores.insert("ann", 3);
scores.insert("bob", 5);

let names = "ann,bob".split(",");

names.sort_by(|a, b| { a.compare(b) });
```

### Returning Early and Errors

`return value` (or `return` alone) leaves the function (or closure) right away.

Operations that can fail return a `Result`, and the `?` operator passes failures on: `value?` is the value of `Ok`, or returns the `Err` from the function. It works the same with `Option`, returning `None`, and it must match what the function returns.

```rust
func sum(a: string, b: string) -> Result<i32, string> {
  Ok { value: parse(a)? + parse(b)? }
}
```

`panic(message)` stops the program with a message, for situations that should never happen. `unwrap()` and `expect(message)` of `Option` and `Result` return the value, and panic without it.

### Generic Methods

Functions of an `impl` block can have their own generic parameters, inferred where they're called:

```rust
impl<T> Holder<T> {
  func map<U>(self, f: func(T) -> U) -> Holder<U> {
    Holder { value: f(self.value) }
  }
}

let text = Holder { value: 1 }.map(|value| { "${value}" });
```

Functions of trait impls can't, because trait objects couldn't call them.

### Containers

Values of types implementing `Container<C>` can be created with children, written after their fields:

```rust
Column { spacing: 4, Text { value: "a" } Text { value: "b" } }
```

The type of children decides what can be passed: with `Drawable[]`, any number of drawables; with `Drawable`, exactly one; with `Text[]`, only texts.

A `view` is a struct that implements `Container` automatically, from its `children` property:

```rust
view Column {
  spacing: f32 = 0.0,
  children: Drawable[]
}
```

Any other type can take children by implementing `Container` itself: `set_children` is called with the children once the value is created.

```rust
struct Group { items: Drawable[] }

impl Container<Drawable[]> for Group {
  func children(self) -> Drawable[] { self.items }
  func set_children(self, children: Drawable[]) { self.items = children; }
}
```

### Statements

These are just as important as expressions. They are what you use to declare everything: variables, functions, and types. A semicolon is required only if it is a variable declaration or an expression that does not end a code block and is not a return value at the end of a function.

#### Variables

They are declared using `let`, and can't be changed afterwards, unless they're declared with `let mut`. The value of a variable can be *almost* anything: if-else expressions, code blocks, and even loops (`loop` with `break value`). Also, every variable declaration requires a semicolon at the end. Here are a couple of examples:

```rust
let protected = "I'm protected by the language rules!";
```


```rust
let mut dynamic = true;

dynamic = false;
```

```rust
let should_be_true = if true == true {
  "Reality is stable!"
} else {
  "It seems we're in danger..."
};
```

#### Constants

`const` declares a constant of a module, next to its functions and types. Its value is computed when the program is compiled, so it can only use literals, operators, other constants and similar values that don't need the program to run. Its type can be written out, or is inferred from the value:

```rust
const COLUMNS: i32 = 12;
const GAP = 4.0;
const WIDTH = COLUMNS * 80;
```

Constants can be used anywhere in their module, even before their declaration, and imported by other modules like any other item.

### Traits

A trait describes functions that types can implement. A value of a trait type can hold any type implementing it.

```rust
trait Shape {
  func area(self) -> f32;
}

struct Square { side: f32 }

impl Shape for Square {
  func area(self) -> f32 { self.side * self.side }
}
```

Inside a trait, `Self` is the type implementing it, and inside an `impl` block, `Self` is the type of the block.

#### Default Functions

A function of a trait can have a body. Types implementing the trait then get it without writing it, and can still write their own. Such a function can use the other functions of the trait on `self`. A function overriding a default can call the default with `super.name(...)`:

```rust
trait Shape {
  func area(self) -> f32;

  func describe(self) -> string {
    "a shape of area ${self.area():.1}"
  }
}

impl Shape for Circle {
  func area(self) -> f32 { self.radius * self.radius * 3.14 }

  func describe(self) -> string {
    "round, " + super.describe()
  }
}
```

#### Bounds of Generics

Generic parameters of functions and `impl` blocks can require traits, after `:` (and `+` for several of them). Functions of these traits can then be called on values of the parameter, and the parameter can only be given types implementing them:

```rust
func total_area<T: Shape>(shapes: T[]) -> f32 {
  let mut total = 0.0;

  for shape in shapes {
    total += shape.area();
  }

  total
}

impl<T: Shape> Shape for Scaled<T> {
  func area(self) -> f32 { self.inner.area() * self.factor * self.factor }
}
```

An `impl` with bounds only applies to types satisfying them: `Scaled<Square>` implements `Shape` above, `Scaled<i32>` doesn't.

## Limits and Determinism

Programs (addons) may come from anyone, so the host runs them with limits, and
they behave the same on every machine running the same version of Mollie.

### Limits

| Limit | Default | Trap or error |
| --- | --- | --- |
| Fuel: function calls and loop iterations | none (`Limits::fuel`) | `out of fuel` |
| Calls in progress (recursion) | 1024 (`Limits::call_depth`) | `stack overflow` |
| Stack bytes | the stack left on the thread | `stack overflow` |
| Bytes of live objects, per program | none (`Limits::heap_bytes`) | `out of memory` |
| Size of a module's source | 1 MiB | compile error |
| Nesting of code (expressions, blocks, types, operator chains) | 256 levels | compile error |
| Nesting of interpolated strings | 32 levels | compile error |
| Size of types of generic instances, and their number | 256 type nodes, 20 000 instances | compile error |

Every program has its own heap: its memory limit, and when its garbage is
collected, only depend on what it does.

Garbage is collected when allocations make it due. A host can instead keep
collections out of programs' runs (`Limits::auto_collect = Some(false)`) and
collect between frames (`collect_garbage_if_due`). The heap then still
collects by itself if it grows to several times its usual size, or would
exceed its limit. `heap_stats` reports the pauses.

### Determinism

The same program with the same inputs and limits does the same thing (and
stops at the same point) on every machine:

- Integer arithmetic wraps on overflow; division and remainder by zero (and
  the minimum value divided by -1) trap.
- Floats are 32-bit IEEE 754, with no fused operations. Every NaN has the same
  bits.
- Converting a float to an integer saturates (NaN becomes 0).
- Recursion is limited by the call depth, not by the size of the stack (which
  differs between platforms).
- Collections iterate in insertion order, and nothing depends on addresses.
- Programs have no clock or randomness, unless the host gives them.

Peers must run the same version of Mollie (and of the host's functions).

## Tools

The language server (`mollie-lsp`) checks programs as they're edited, with hover, go to definition and completion.

It needs to know the host's API, which a host writes as a *stub*: declarations of everything it registered, with functions and methods declared without bodies.

```rust
mollie::stub::host_stub(&compiler).write_to(Path::new(".mollie/host"))?;
```

```mollie
// .mollie/host/lib.mol
module graphics;

extern func log(message: string);

impl graphics::Size {
    extern func area(self) -> f32;
}
```

`extern` declarations are only allowed in stubs. A project configures the language server with `mollie.toml` next to its files:

```toml
# Programs (addons): other files are their submodules.
entries = ["addons/shop/main.mol", "addons/quests/main.mol"]
# The stub of the host's API (this is the default).
host = ".mollie/host"
```

A submodule opened on its own is checked as part of the entry whose program contains it, and declarations of `std` can be opened from the editor.

