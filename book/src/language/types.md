# Type System

Every value in Mollie has a type, known before the program runs. Most of the time, you don't have to write it, because Mollie figures it out. You write types for parameters, results, fields and constants, and wherever Mollie can't guess, like in `let x: Option<i32> = None;`.

## Primitives

| Type | Values |
| --- | --- |
| `i8`, `i16`, `i32`, `i64`, `isize` | signed integers |
| `u8`, `u16`, `u32`, `u64`, `usize` | unsigned integers |
| `f32`, `f64` | 32- and 64-bit floating-point numbers |
| `bool` | `true` and `false` |
| `string` | immutable text (UTF-8) |

A number without a suffix takes whatever type it's needed as, and if nothing says, it's an `i32` or an `f32`. So `let x: f64 = 0.1;` is an `f64`, but `let y = 0.1;` is an `f32`. You can always pick the type yourself with a suffix, like `255u8` or `0.5f64`.

When an integer gets too big for its type, it wraps around instead of stopping the program.

To turn one kind of number into another, use `as`: `x as f32`, `3.9 as i32`. Converting a float to an integer cuts off the fraction, so `3.9 as i32` is `3`.

## Structures

```mollie
struct Player {
  name: string,
  health: i32 = 100,
}

let player = Player { name: "Ann" };

player.health -= 10;
```

Fields can have defaults, so you don't have to write them every time.

Keep in mind that a struct is shared, not copied. If you give `player` to a function and it changes `health`, you'll see the change too.

Functions are added to a type with `impl`. A function that takes `self` is a method, called on a value. One without `self` is called through the type:

```mollie
impl Player {
  func new(name: string) -> Player { Player { name } }

  func heal(self, amount: i32) {
    self.health += amount;
  }
}

let player = Player::new("Ann");

player.heal(5);
```

Structs and their `impl`s can be generic too: `struct Pair<T> { a: T, b: T }`, `impl<T> Pair<T> { ... }`.

## Value Types

Sometimes sharing isn't what you want. A position, a size or a color should behave like a number: if you change your copy, nobody else's should change. That's what `value struct` and `value enum` are for:

```mollie
value struct Point { x: f32, y: f32 }

let a = Point { x: 1.0, y: 2.0 };
let mut b = a;

b.x = 5.0; // `a` is still (1, 2)
```

As a bonus, they don't need the garbage collector at all.

Since a value type is a value, you change it the way you change a number: through a `let mut` variable. A method that changes it takes `mut self`:

```mollie
impl Point {
  func move_by(mut self, dx: f32) {
    self.x += dx;
  }
}

let mut p = Point { x: 0.0, y: 0.0 };

p.move_by(2.0); // `p.x` is 2
```

The same goes for property setters on value types: they take `mut self`.

## Enumerations

An enum is a value that's one of several variants, and each variant can have its own fields:

```mollie
enum Shape {
  Circle { radius: f32 },
  Rect { width: f32, height: f32 },
  Empty,
}

let shape = Shape::Circle { radius: 2.0 };
```

To find out which variant you've got, use [`match`](control-flow.md#pattern-matching) or `is`. And as with patterns, you can leave out `Shape::` when the type is already known: `let shape: Shape = Circle { radius: 2.0 };`.

## Views

Finally, the `view` we promised at the beginning! A view is a struct made for user interfaces. Values written inside it become its children (see [Containers](containers.md#containers)), so a tree of views reads just like a layout:

```mollie
Column {
  spacing: 8.0,

  Text { content: "Hello" }
  Button { on_click => { start(); } Text { content: "Start" } }
}
```

How views get on the screen is up to the program and its host. Usually, the host gives a trait like `Drawable`, and each view implements it.

## Trait Objects

A trait can also be used as a type. Such a value can hold any type implementing the trait, and calls its functions:

```mollie
let shapes: Shape[] = [Square { side: 2.0 }, Circle { radius: 1.0 }];

for shape in shapes {
  println("${shape.area()}");
}
```

When a value type is put into a trait object, the trait object gets its own copy.

## Functions and Closures

Functions are values too. Their type looks like `func(i32, i32) -> i32`, or just `func(string)` when there's no result.

A closure is a function you write right where you need it, like `|a, b| { a + b }`, or `|| { ... }` without parameters. It can use the variables around it:

```mollie
func apply(f: func(i32) -> i32, x: i32) -> i32 {
  f(x)
}

let base = 10;
let shifted = apply(|x| { x + base }, 5); // 15
```

Note that a closure copies those variables when it's created. If it needs to change something and have the change stick, keep it in a struct, because structs are shared:

```mollie
struct Counter { count: i32 }

let counter = Counter { count: 0 };
let tick = || { counter.count += 1; };
```
