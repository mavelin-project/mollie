# The Standard Library

Every program comes with `std`, the standard library. Its most common items are always there, without importing anything:

- `Option<T>`, which is either `Some { value }` or `None`;
- `Result<T, E>`, which is either `Ok { value }` or `Err { error }`;
- `Map<K, V>` and `Set<K>`, for looking things up by key;
- `Range`, `Iterable` and `Iterator`, which make `for` work;
- `Container`, which we'll meet in [Containers](containers.md#containers);
- `Eq` and `Hash`, which `Map` and `Set` need from their keys;
- the constants `PI`, `TAU` and `E` (and `PI_F64`, `TAU_F64` and `E_F64`).

Everything else has to be imported, like `import { Chars } from std::string;`. Methods of numbers, strings and arrays never need an import.

Here's a quick tour of what you get.

`Option` and `Result` have `unwrap_or(default)`, `unwrap()` and `expect(message)`, plus `is_some()`/`is_none()` and `is_ok()`/`is_err()`. A `Result` can also turn into an `Option` with `ok()` or `err()`.

Numbers have `min`, `max` and `clamp`, and signed ones have `abs`. Floats get the rest of the math you'd expect: `sqrt`, `floor`, `ceil`, `trunc`, `round`, `sin`, `cos`, `tan`, `atan2`, `exp`, `ln`, `pow`, `lerp`, `signum`, `to_radians` and `to_degrees`. They give the same results on every computer.

Strings have `len`, `is_empty`, `char_count`, `slice`, `contains`, `starts_with`, `ends_with`, `find`, `split`, `trim`, `to_lower`, `to_upper`, `repeat`, `compare`, `parse_int` and `parse_float`, and you can go through them with `for` using `chars()` or `bytes()`. They can also be compared with `<` and `>`.

Arrays have `len`, `is_empty`, `copy`, `reverse` and `sort_by`, and `contains` and `index_of` when their elements implement `Eq`. Growable arrays also have `push`, `pop`, `insert`, `remove`, `truncate`, `resize` and `clear`. There's more about arrays in [Arrays](arrays.md#arrays).

`Map` and `Set` remember the order things were added in, and that's the order `for` goes through them:

```mollie
let scores: Map<string, i32> = Map::new();

scores.insert("ann", 3);
scores.insert("bob", 5);

for entry in scores {
  println("${entry.key}: ${entry.value}");
}

let names = "bob,ann".split(",");

names.sort_by(|a, b| { a.compare(b) });
```
