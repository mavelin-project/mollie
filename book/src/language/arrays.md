# Arrays

Mollie has four kinds of arrays. That may sound like a lot, but most of the time you'll only use the first one.

| Type | What it is |
| --- | --- |
| `T[]` | A growable array. Like a struct, it's shared. |
| `T[3]` | A fixed array of 3 elements. Like a value struct, it's copied. |
| `T[_]` | A fixed array of any size, as a parameter. |
| `T[..]` | A view of any array, as a parameter or a variable. |

All of them read and write elements with `array[index]`. An index past the end stops the program.

An array literal becomes whatever kind its place needs. `let a = [1, 2, 3];` is growable, and the same literal given to a `T[3]` is a fixed array.

Fixed arrays are handy for small things like colors, since they don't allocate anything. They can be at most 4 KiB, and `to_array()` makes a growable array out of one.

Views are the most interesting kind. A function taking `T[..]` accepts any array, without copying it. With `mut`, it can even change the elements:

```mollie
func average(values: f32[..]) -> f32 {
  let mut total = 0.0;

  for value in values { total += value; }

  total / values.len() as f32
}

func normalize(mut values: f32[..]) {
  let length = average(values);
  let mut i = 0usize;

  while i < values.len() {
    values[i] /= length;
    i += 1;
  }
}

let mut color: f32[3] = [0.2, 0.4, 0.6];

normalize(color);   // Changes `color`.
average([1.0, 2.0]); // Nothing is allocated.
```

Since a view only borrows an array, it can't outlive the call. That's why it can't be stored in a field, returned, or captured by a closure. And a view can't change an array's length, so `push` and `pop` are only for `T[]`.

## Variadic Parameters

Remember the parameter that takes any number of arguments? It's written as `..args: T[..]`, and it must be the last one. The function gets the extra arguments as a view:

```mollie
func log(level: i32, ..parts: string[..]) {
  ...
}

log(1);
log(2, "loading ", name, "...");
```

Calling it doesn't allocate anything. A variadic parameter can't have a default, and you can't pass it by name.
