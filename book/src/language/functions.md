# Fields, Methods and Calls

## Fields and Methods

`value.name` reads a field, and `value.name(...)` calls a method. But what if a field holds a function? Then you can call it just like a method, which is really handy for callbacks:

```mollie
struct Button { on_click: func() }

button.on_click();
```

There's also a shorter way to put a closure into a field. Write the field's name, its arguments, and then the body:

```mollie
Button { on_click => { game.play(); } }
Slider { on_change(value) => { settings.fov = value; } }
```

Sometimes a closure needs a value from the moment it was created, not from the moment it's called. Put it in square brackets before the arguments:

```mollie
// `previous` is the FOV when the slider was created, not when it changes.
Slider { on_change[previous = settings.fov, game](value) => { game.log("${previous} -> ${value}"); } }
```

Here `game` is short for `game = game`.

### Properties

A property looks just like a field from the outside, but reading or changing it runs your own code. You write a getter with `get func`, and a setter with `set func`:

```mollie
struct Temperature { celsius: f32 }

impl Temperature {
  get func fahrenheit(self) -> f32 { self.celsius * 1.8 + 32.0 }
  set func fahrenheit(self, value: f32) { self.celsius = (value - 32.0) / 1.8; }
}

let t = Temperature { celsius: 20.0 };

t.fahrenheit = 100.0;   // calls the setter
t.fahrenheit += 10.0;   // the getter, then the setter
let f = t.fahrenheit;   // calls the getter
```

A property without a setter is read-only. Two more things to keep in mind: a property can't share its name with a field of the same type, and traits can't have properties.

## Calling Functions

Parameters can have default values. If a call leaves one out, the default is used instead. A default can even use the parameters before it:

```mollie
func button(text: string, enabled: bool = true, width: i32 = text.len() as i32 * 8) -> Button {
  ...
}
```

You can also pass arguments by name, in any order you like, as long as they come after the ones passed by position:

```mollie
button("OK");
button("Cancel", enabled: false);
button(width: 120, text: "Save");
```

Arguments are evaluated in the order you wrote them. Naming works only when calling a function directly: a function you keep in a variable or a field takes its arguments by position.

The last parameter of a function can also take any number of arguments. We'll get to that in [Variadic Parameters](arrays.md#variadic-parameters).

## Returning Early and Errors

Like in most languages, `return` leaves a function right away, with or without a value.

Things that can fail usually give back a `Result`. Checking every one of them by hand gets tiring fast, so there's the `?` operator. `value?` gives you what's inside an `Ok`, or leaves the function with the `Err`:

```mollie
func sum(a: string, b: string) -> Result<i32, string> {
  Ok { value: parse(a)? + parse(b)? }
}
```

It works with `Option` too, leaving with `None`. Just make sure it matches what your function returns.

And if something happens that never should, `panic(message)` stops the program. `unwrap()` and `expect(message)` do the same when there's no value.

Speaking of stopping: the host running your program can stop it too, for example when it runs for too long or uses too much memory. You'll find the details in [`docs`](../internals/runtime.md#runs-traps-and-limits).

## Generic Methods

A function in an `impl` block can have its own generic parameters, and you usually don't have to write them out when calling it:

```mollie
impl<T> Holder<T> {
  func map<U>(self, f: func(T) -> U) -> Holder<U> {
    Holder { value: f(self.value) }
  }
}

let text = Holder { value: 1 }.map(|value| { "${value}" });
```

The only exception is functions implementing a trait. They can't have generic parameters of their own.
