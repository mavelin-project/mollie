//! Default implementations of trait functions and `super.name()`.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

const SHAPE: &str = "trait Shape {
    func area(self) -> i32;

    func describe(self) -> string {
        \"area ${self.area()}\"
    }

    func doubled(self) -> i32 { self.area() * 2 }
}

struct Square { side: i32 }

impl Shape for Square {
    func area(self) -> i32 { self.side * self.side }
}
";

#[test]
fn impls_use_defaults() {
    assert_no_errors(check(&format!(
        "{SHAPE}struct Holder {{ shape: Shape }}

let square = Square {{ side: 2 }};
let text: string = square.describe();
let doubled: i32 = square.doubled();
let holder = Holder {{ shape: square }};
let dynamic: string = holder.shape.describe();"
    )));
}

#[test]
fn overrides_call_defaults_with_super() {
    assert_no_errors(check(&format!(
        "{SHAPE}struct Circle {{ radius: i32 }}

impl Shape for Circle {{
    func area(self) -> i32 {{ self.radius * self.radius * 3 }}

    func describe(self) -> string {{ \"circle, ${{super.describe()}}\" }}
}}"
    )));
}

#[test]
fn defaults_of_generic_traits() {
    assert_no_errors(check(
        "trait Source<T> {
    func get(self) -> T;

    func get_twice(self) -> T[] { [self.get(), self.get()] }
}

struct Constant { value: i32 }

impl Source<i32> for Constant {
    func get(self) -> i32 { self.value }
}

let values: i32[] = Constant { value: 3 }.get_twice();",
    ));
}

#[test]
fn super_outside_trait_impls() {
    only_errors(
        check("struct Point { x: i32 }\n\nimpl Point {\n    func get(self) -> i32 { super.get() }\n}"),
        |error| matches!(error, TypeError::SuperOutsideTraitImpl),
    );
}

#[test]
fn super_of_functions_without_defaults() {
    only_errors(
        check(&format!(
            "{SHAPE}struct Circle {{ radius: i32 }}

impl Shape for Circle {{
    func area(self) -> i32 {{ super.area() }}
}}"
        )),
        |error| matches!(error, TypeError::NoDefault { .. }),
    );
}

#[test]
fn missing_functions_without_defaults() {
    only_errors(check(&format!("{SHAPE}struct Empty {{ x: i32 }}\n\nimpl Shape for Empty {{}}")), |error| {
        matches!(error, TypeError::MissingTraitFunc { .. })
    });
}
