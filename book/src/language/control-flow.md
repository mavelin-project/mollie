# Control Flow

## If-else and Loops

They are combined into a single section because, essentially, both affect the "flow" of the resulting code. Let’s start with if-else, then move on to loops, and finish with a couple of examples.

If you’re mostly familiar with C-like languages such as JavaScript or C itself, you may have noticed that in Mollie, the if condition doesn’t need to be enclosed in parentheses. This is an intentional and final decision in the language’s design.

What about loops? Here, you have several options.

- The familiar `while` loop, which executes code as long as the condition is true.
- `for in` for iterating over array elements, [ranges](#ranges) and anything else implementing `Iterable`.
- An infinite loop using `loop`.

In each of these, you can use `break` to force the loop to terminate and `continue` to skip the rest of the current iteration and move on to the next one.

`loop` can even give back a value, with `break value`. `while` and `for` can't do that, because they can finish without ever reaching a `break`, and then there'd be no value to give.

When loops are nested, a plain `break` only leaves the innermost one. To leave an outer loop, give it a label like `'rows:` and write `break 'rows` (or `continue 'rows`).

Now, let’s look at some examples!

```mollie
if 4 > 4 {
  println("How is that possible?! :fearful:");
}
```

```mollie
while true {
  println("Why not use a loop?");
}
```

```mollie
for num in [1, 2, 3] {
  if num == 2 {
    continue; // I don't like this number, let's skip it
  }

  println(num);
}
```

```mollie
let mut attempt = 0;
let found = loop {
  attempt += 1;

  if attempt * attempt > 50 {
    break attempt; // `found` is 8
  }
};
```

```mollie
'rows: for row in grid {
  for cell in row {
    if cell == 0 {
      continue 'rows; // skip the rest of this row
    }
  }
}
```

## Ranges

You'll often want to count from one number to another. That's what ranges are for. `0..3` counts from 0 up to 3, but doesn't include 3 itself, and `0..=3` does include it:

```mollie
for i in 0..3 {
  println(i); // 0, 1, 2
}
```

A range is a value like any other, of type `Range<T>`, so you can keep it in a variable and ask it things, like `(1..=5).contains(5)` or `is_empty()`. Both of its ends must be of the same type.

## Pattern Matching

`match` looks at a value and picks the first arm whose pattern fits it, going from top to bottom:

```mollie
let area = match shape {
  Circle { radius } => radius * radius * 3.14,
  Rect { width, height } if width == height => width * width,
  Rect { width, height } => width * height,
  Empty => 0.0,
};
```

Mollie makes sure you don't forget anything. If some possible value isn't handled by any arm, the program won't compile.

Here's what you can use as a pattern:

- a literal, like `0`, `"name"` or `true`;
- `_`, which matches anything and ignores it;
- a name, which matches anything and lets you use the value under that name;
- an enum variant or a struct, with patterns for its fields, like `Some { value: 0 }`. If you write just a field's name, as in `Point { x, y }`, you get a variable with that name.

As you can see in the example, an arm can also have a guard (`if width == height`), which is checked only after its pattern fits.

You may have noticed that we wrote `Circle` and not `Shape::Circle`. When Mollie already knows the type, you can leave the enum's name out. The same works when you create a value, for example in a function's result:

```mollie
func first(values: i32[]) -> Option<i32> {
  if values.len() == 0 { None } else { Some { value: values[0] } }
}
```

And when all you need is a yes or no, there's `is`. It uses the same patterns: `if value is Some { value } { ... }`.
