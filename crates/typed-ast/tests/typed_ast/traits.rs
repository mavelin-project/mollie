use mollie_typing::{TypeError, TypeErrorValue};

use crate::{assert_no_errors, check, only_errors, single_error};

const SHAPE: &str = "trait Shape { func area(self) -> f32; }
struct Square { side: f32 }
impl Shape for Square { func area(self) -> f32 { self.side * self.side } }
struct Circle { radius: f32 }
";

#[test]
fn trait_method_can_be_called_on_implementor() {
    let errors = check(&format!(
        "{SHAPE}
let square = Square {{ side: 2.0 }};
let area: f32 = square.area();"
    ));

    assert_no_errors(errors);
}

#[test]
fn implementor_can_be_used_as_trait_object() {
    let errors = check(&format!("{SHAPE}\nlet shape: Shape = Square {{ side: 2.0 }};"));

    assert_no_errors(errors);
}

#[test]
fn trait_object_field_accepts_implementor() {
    let errors = check(&format!(
        "{SHAPE}\nstruct Holder {{ shape: Shape }}\nlet holder = Holder {{ shape: Square {{ side: 2.0 }} }};"
    ));

    assert_no_errors(errors);
}

#[test]
fn unsized_trait_array_field_accepts_array_literal() {
    let errors = check(&format!(
        "{SHAPE}\nstruct Scene {{ shapes: Shape[] }}\nlet scene = Scene {{ shapes: [Square {{ side: 1.0 }}, Square {{ side: 2.0 }}] }};"
    ));

    assert_no_errors(errors);
}

#[test]
fn trait_object_field_rejects_non_implementor() {
    let errors = check(&format!(
        "{SHAPE}\nstruct Holder {{ shape: Shape }}\nlet holder = Holder {{ shape: Circle {{ radius: 1.0 }} }};"
    ));

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn non_implementor_cannot_be_used_as_trait_object() {
    let error = single_error(check(&format!("{SHAPE}\nlet shape: Shape = Circle {{ radius: 1.0 }};")));

    assert!(matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn inherent_methods() {
    let errors = check(
        "struct Counter { value: i32 }
impl Counter {
    func get(self) -> i32 { self.value }
    func increment(self) { self.value += 1; }
}
let counter = Counter { value: 0 };
counter.increment();
let value: i32 = counter.get();",
    );

    assert_no_errors(errors);
}

#[test]
fn missing_trait_function_is_reported() {
    let error = single_error(check(
        "trait Named { func name(self) -> string; func id(self) -> i32; }
struct User {}
impl Named for User { func name(self) -> string { \"user\" } }",
    ));

    assert!(matches!(error, TypeError::MissingTraitFunc { ref name, .. } if name == "id"));
}

#[test]
fn function_not_in_trait_is_reported() {
    let error = single_error(check(
        "trait Named { func name(self) -> string; }
struct User {}
impl Named for User {
    func name(self) -> string { \"user\" }
    func extra(self) {}
}",
    ));

    assert!(matches!(error, TypeError::NotTraitMember { ref name, .. } if name == "extra"));
}

#[test]
fn unknown_trait_is_reported() {
    let error = single_error(check(
        "struct User {}
impl Unknown for User {}",
    ));

    assert!(matches!(error, TypeError::NotFound { ref name, .. } if name == "Unknown"));
}

#[test]
fn path_through_non_module_is_reported() {
    let error = single_error(check(
        "struct Outer {}
struct User {}
impl Outer::Named for User {}",
    ));

    assert!(matches!(error, TypeError::Unexpected {
        expected: TypeErrorValue::Module,
        ..
    }));
}

#[test]
fn implementing_non_trait_is_reported() {
    let error = single_error(check(
        "struct NotATrait {}
struct User {}
impl NotATrait for User {}",
    ));

    assert!(matches!(error, TypeError::Unexpected {
        expected: TypeErrorValue::Trait,
        ..
    }));
}

#[test]
fn generic_trait_arguments_are_checked() {
    let errors = check(
        "trait Source<T> { func get(self) -> T; }
struct Number { value: i32 }
impl Source<i32> for Number { func get(self) -> i32 { self.value } }
let ints: Source<i32> = Number { value: 1 };",
    );

    assert_no_errors(errors);

    let errors = check(
        "trait Source<T> { func get(self) -> T; }
struct Number { value: i32 }
impl Source<i32> for Number { func get(self) -> i32 { self.value } }
let bools: Source<bool> = Number { value: 1 };",
    );

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn method_of_generic_trait_object_returns_its_type_argument() {
    let errors = check(
        "trait Source<T> { func get(self) -> T; }
func read(source: Source<i32>) -> i32 { source.get() }",
    );

    assert_no_errors(errors);
}
