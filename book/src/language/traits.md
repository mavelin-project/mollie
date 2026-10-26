# Traits

A trait describes what a type can do, as a list of functions. Any type can implement it:

```mollie
trait Shape {
  func area(self) -> f32;
}

struct Square { side: f32 }

impl Shape for Square {
  func area(self) -> f32 { self.side * self.side }
}
```

Inside a trait, `Self` means whatever type implements it. Inside an `impl` block, it's the type of the block.

## Default Functions

A trait can also give a function a body. Then types implementing the trait get it for free, and can still write their own. If they do, `super.name(...)` calls the default one:

```mollie
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

## Bounds of Generics

What if you want a function that works with any shape? Give it a generic parameter, and require the trait after a colon. Several traits are joined with `+`:

```mollie
func total_area<T: Shape>(shapes: T[]) -> f32 {
  let mut total = 0.0;

  for shape in shapes {
    total += shape.area();
  }

  total
}
```

Now `total_area` can call `area()`, and it can only be given types that implement `Shape`.

The same works for `impl` blocks. Here, `Scaled<Square>` is a `Shape`, but `Scaled<i32>` isn't:

```mollie
impl<T: Shape> Shape for Scaled<T> {
  func area(self) -> f32 { self.inner.area() * self.factor * self.factor }
}
```
