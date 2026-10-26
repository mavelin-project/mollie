# Syntax

It’s hard to call Mollie an "ideal language" with a well-thought-out syntax that has no unnecessary elements. If you’re already familiar with languages like JavaScript, C#, Go, and Rust, Mollie’s syntax will seem like an attempt to borrow a little bit from each of them. And that’s exactly what it is. Perhaps the language’s creators could have been a bit more creative, but let’s get to the point.

Mollie consists of two parts: expressions and statements. In fact, most of the syntax consists of expressions. So let’s start with those.

## Expressions

The core functionality of the language, allowing you to do everything you’re used to. Function calls, arrays, loops, code blocks. All of this is an expression! I’ll also note that if an expression doesn’t end with a code block or isn’t a value returned from a function, it must end with a semicolon.

### Primitive Values

As in most languages, it all starts with numbers. Well, or strings! Essentially, there’s nothing special here:

- `1`: Integers are written as usual.
- `1.0`: Floating-point numbers use a `.` to separate the integer and fractional parts.
- `"Hello, World!"`: Strings are enclosed in double quotes.
- `true`, `false`: Boolean values use `true` and `false`.

In the future, there may be a character as a separate type, whose value will be created via a character enclosed in single quotes, but this is not currently the case.

It’s also worth noting that numbers can contain an optional suffix indicating either their specific type (`1u64`) or a suffix function (`1.0px`).

You can do math with all the usual operators: `+`, `-`, `*`, `/`, and `%` for the remainder of a division. Each of them also has a form that changes a variable in place, like `+=` or `%=`. Just don't divide an integer by zero, because that stops the program.

Integers also have bitwise operators, if you need them: `&` (and), `|` (or), `^` (xor), and the shifts `<<` and `>>`. The first three work on booleans too. They follow the same precedence as in Rust, so `1 + 2 << 3` is `24`.

### Strings

Strings can't be changed once they're created, but you can always make a new one. The easiest way is to join them with `+`, or to put values right inside with `${...}`:

```mollie
let name = "Ann";
let count = 3;

let greeting = "Hello, " + name + "!";
let summary = "${name} has ${count} items, that's ${count > 1}";
```

Only strings, numbers and booleans can go inside `${...}` like this.

Sometimes you want a number to look a certain way. For that, put a format after a colon:

```mollie
let price = 3.14159;
let id = 255;

println("${price:.2}");     // 3.14
println("[${"ok":>4}]");     // [  ok]
println("${7:03}");         // 007
println("${id:x} ${id:b}"); // ff 11111111
```

A format is made of a few optional parts, written in this order:

- How to align the value: `<` for left, `>` for right, `^` for center. Numbers are aligned to the right by default, and everything else to the left.
- `0`, if you want a number padded with zeros instead of spaces.
- The width to pad the value up to.
- A `.` and the number of digits after the point, for floats.
- `x` or `X` to write an integer in hexadecimal, or `b` for binary.

If a format makes no sense for its value, like `.2` on a string, the program won't compile.

What if you want to write `${` itself? Just escape the dollar sign: `"\${not a value}"`. The other escapes are the ones you probably know already: `\n`, `\t`, `\r`, `\0`, `\"` and `\\`.

One thing that may surprise you is that `len()` counts bytes, not letters, so `"héllo".len()` is `6`. The same goes for `slice`: `"hello".slice(1, 3)` is `"el"`, and slicing through the middle of a letter stops the program. If you need letters, there's `char_count()` and `chars()`.
