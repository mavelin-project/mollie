# Modules

As a program grows, you'll want to split it into files. `module ui;` loads `ui.mol` from next to the current file. If `ui.mol` then says `module widgets;`, that's `ui/widgets.mol`.

To use something from another module, write its path or import it:

```mollie
module ui;

import { Button, Column } from ui;

let button = ui::Button { ... };
```

Paths start from the current module. `super` is the module above it, `std` is the standard library, and the host's modules are reached by their names, like `import { Size } from graphics;`.

Some of the host's modules are only available to programs the host trusts. If yours isn't one of them, you'll get an error like "`fs` isn't available to this addon".

Every module sees the [standard library's](standard-library.md#the-standard-library) common items, unless it declares or imports something with the same name.

## Libraries and Visibility

A host can also give programs *libraries*: code shared by many programs, like a UI library. You use them just like `std`: `import { Ui, Button } from ui;`.

Code you share with others usually has parts that are nobody else's business. That's what `pub` is for. A program with its modules is one *package*, and so is a library. Inside a package, everything is visible. Outside of it, only what's marked `pub`:

```mollie
pub struct Point {
    pub x: f32,
    pub y: f32,
    cache: f32 = 0.0,
}

impl Point {
    pub func length(self) -> f32 { (self.x * self.x + self.y * self.y).sqrt() }

    func update(self) { ... }
}

pub enum Shape { Circle { radius: f32 }, Square { side: f32 } }

pub trait Draw { func draw(self); }

pub func origin() -> Point { Point { x: 0.0, y: 0.0 } }
pub const LIMIT: i32 = 4;
pub module shapes;
pub import { Square } from shapes;
```

Here, other packages can create a `Point` without giving `cache`, since it has a default, but they can't read it: "`cache` is private to its package". And thanks to `pub import`, they can get `Square` straight from this module.

Variants of an enum are as visible as the enum, and functions of a trait as visible as the trait. The host's own items are visible everywhere.
