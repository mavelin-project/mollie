# Containers

Remember how we said Mollie is meant for user interfaces? This is where it shows. Some types can take *children*, written right after their fields:

```mollie
Column { spacing: 4, Text { value: "a" } Text { value: "b" } }
```

What children are allowed depends on the type. A `Column` taking `Drawable[]` accepts any number of drawables, one taking just `Drawable` wants exactly one, and one taking `Text[]` accepts only texts.

Children don't have to be written out one by one. You can add them under a condition, for each item of a collection, or from any expression in braces:

```mollie
Column {
  Text { value: "Title" }
  if loaded {
    Button { Text { value: "Play" } }
  } else {
    Text { value: "Loading..." }
  }
  for item in items {
    Row { Text { value: item.name } }
  }
  { footer(game) }
}
```

When a type takes exactly one child, you can't use `for`, and an `if` must have an `else`, so that there's always one.

The easiest way to make such a type is a `view`. It takes its children through the `children` field:

```mollie
view Column {
  spacing: f32 = 0.0,
  children: Drawable[]
}
```

Any other type can take children too, by implementing `Container` itself. `set_children` is called once the value is created:

```mollie
struct Group { items: Drawable[] }

impl Container<Drawable[]> for Group {
  func children(self) -> Drawable[] { self.items }
  func set_children(self, children: Drawable[]) { self.items = children; }
}
```
