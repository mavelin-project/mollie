//! Bounds of generic parameters: `func f<T: Shape>(value: T)`.

use mollie_typing::{Type, TypeError};

use crate::{assert_no_errors, check, only_errors};

const SHAPES: &str = "trait Shape { func area(self) -> i32; }
trait Named { func name(self) -> string; }

struct Square { side: i32 }

impl Shape for Square {
    func area(self) -> i32 { self.side * self.side }
}

impl Named for Square {
    func name(self) -> string { \"square\" }
}
";

#[test]
fn functions_of_bounds_are_called_on_generics() {
    assert_no_errors(check(&format!(
        "{SHAPES}func describe<T: Shape + Named>(value: T) -> string {{
    let area: i32 = value.area();

    \"${{value.name()}}: ${{area}}\"
}}

let text = describe(Square {{ side: 2 }});"
    )));
}

#[test]
fn impls_with_bounds() {
    assert_no_errors(check(&format!(
        "{SHAPES}struct Twice<T> {{ inner: T }}

impl<T: Shape> Shape for Twice<T> {{
    func area(self) -> i32 {{ self.inner.area() * 2 }}
}}

impl<T> Twice<T> {{
    func larger<U: Shape>(self, other: U) -> i32 {{ other.area() }}
}}

let twice = Twice {{ inner: Square {{ side: 1 }} }};
let area = twice.area() + twice.larger(Square {{ side: 3 }});"
    )));
}

#[test]
fn inherent_impls_with_different_bounds() {
    let (errors, tcx) = check(&format!(
        "{SHAPES}struct Twice<T> {{ inner: T }}

impl<T: Shape> Twice<T> {{
    func area(self) -> i32 {{ self.inner.area() }}
}}

impl<T> Twice<T> {{
    func larger<U: Shape>(self, other: U) -> i32 {{ other.area() }}
}}

let twice = Twice {{ inner: Square {{ side: 1 }} }};
let area = twice.area() + twice.larger(Square {{ side: 3 }});"
    ));

    let twice = tcx
        .def_registry
        .adt_types
        .iter()
        .find(|(_, adt)| adt.name.as_deref() == Some("Twice"))
        .map(|(key, _)| key)
        .expect("registered Twice<T> struct");

    assert_eq!(
        tcx.impl_registry
            .impls
            .values()
            .filter(|generator| matches!(tcx.types[generator.ty], Type::Adt(ty, _) if ty == twice))
            .count(),
        2,
    );

    assert_no_errors((errors, tcx));
}

#[test]
fn type_arguments_must_satisfy_bounds() {
    only_errors(
        check(&format!("{SHAPES}func area<T: Shape>(value: T) -> i32 {{ value.area() }}\n\nlet x = area(5);")),
        |error| matches!(error, TypeError::UnsatisfiedBound { .. }),
    );
}

#[test]
fn methods_not_in_bounds_are_errors() {
    let (errors, _) = check(&format!("{SHAPES}func name<T: Shape>(value: T) -> string {{ value.name() }}"));

    assert!(!errors.is_empty());
}

#[test]
fn bounds_must_be_traits() {
    only_errors(check(&format!("{SHAPES}func area<T: Square>(value: T) -> i32 {{ 0 }}")), |error| {
        matches!(error, TypeError::Unexpected { .. })
    });
}

const SOURCES: &str = "trait Source<T> { func get(self) -> T; }

struct Both { number: i32, text: string }

impl Source<i32> for Both {
    func get(self) -> i32 { self.number }
}

impl Source<string> for Both {
    func get(self) -> string { self.text }
}

struct Numbers { value: i32 }

impl Source<i32> for Numbers {
    func get(self) -> i32 { self.value }
}
";

#[test]
fn arguments_of_bounds_are_checked() {
    only_errors(
        check(&format!(
            "{SOURCES}struct Texts {{ value: string }}

impl Source<string> for Texts {{
    func get(self) -> string {{ self.value }}
}}

func number<T: Source<i32>>(source: T) -> i32 {{ 0 }}

let x = number(Texts {{ value: \"a\" }});"
        )),
        |error| matches!(error, TypeError::UnsatisfiedBound { .. }),
    );
}

#[test]
fn several_impls_of_one_trait() {
    assert_no_errors(check(&format!(
        "{SOURCES}func number<T: Source<i32>>(source: T) -> i32 {{ 0 }}
func text<T: Source<string>>(source: T) -> i32 {{ 0 }}

let both = Both {{ number: 1, text: \"a\" }};
let a = number(both);
let b = text(both);
let numbers: Source<i32> = both;
let texts: Source<string> = both;"
    )));
}

#[test]
fn generic_arguments_need_the_bound() {
    only_errors(
        check(&format!(
            "{SHAPES}func area<T: Shape>(shape: T) -> i32 {{ shape.area() }}
func forward<T>(shape: T) -> i32 {{ area(shape) }}"
        )),
        |error| matches!(error, TypeError::UnsatisfiedBound { .. }),
    );

    assert_no_errors(check(&format!(
        "{SHAPES}func area<T: Shape>(shape: T) -> i32 {{ shape.area() }}
func forward<T: Shape + Named>(shape: T) -> i32 {{ area(shape) }}"
    )));
}

#[test]
fn bounds_with_generic_arguments() {
    assert_no_errors(check(&format!(
        "{SOURCES}func first<T, S: Source<T>>(source: S, fallback: T) -> T {{ fallback }}

let x: i32 = first(Numbers {{ value: 1 }}, 2);"
    )));
}

#[test]
fn trait_objects_satisfy_bounds_of_their_trait() {
    assert_no_errors(check(&format!(
        "{SHAPES}func area<T: Shape>(shape: T) -> i32 {{ shape.area() }}

let shape: Shape = Square {{ side: 2 }};
struct Holder {{ shape: Shape }}
let holder = Holder {{ shape: Square {{ side: 2 }} }};
let x = area(holder.shape);"
    )));
}
