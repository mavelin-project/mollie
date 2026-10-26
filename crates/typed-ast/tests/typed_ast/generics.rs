use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

#[test]
fn generic_field_keeps_its_parameter_type() {
    assert_no_errors(check(
        "struct Holder<T> { value: T }
impl<T> Holder<T> {
    func get(self) -> T { self.value }
}",
    ));
}

#[test]
fn generic_parameter_is_not_a_concrete_type() {
    only_errors(
        check(
            "struct Holder<T> { value: T }
impl<T> Holder<T> {
    func broken(self) -> i32 { self.value }
}",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn generic_method_is_instantiated_by_its_receiver() {
    assert_no_errors(check(
        "struct Holder<T> { value: T }
impl<T> Holder<T> {
    func get(self) -> T { self.value }
}
let holder = Holder { value: 5 };
let value: i32 = holder.get();",
    ));
}

#[test]
fn instantiated_method_returns_the_instantiated_type() {
    only_errors(
        check(
            "struct Holder<T> { value: T }
impl<T> Holder<T> {
    func get(self) -> T { self.value }
}
let holder = Holder { value: true };
let value: i32 = holder.get();",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn type_arguments_of_annotation_keep_their_order() {
    assert_no_errors(check(
        "struct Pair<A, B> { first: A, second: B }
let pair: Pair<i32, bool> = Pair { first: 1, second: true };",
    ));
}

#[test]
fn swapped_type_arguments_are_reported() {
    only_errors(
        check(
            "struct Pair<A, B> { first: A, second: B }
let pair: Pair<i32, bool> = Pair { first: true, second: 1 };",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn omitted_type_arguments_are_inferred() {
    assert_no_errors(check(
        "enum Option<T> { Some { value: T }, None }
let none: Option<i32> = Option::None;
let some: Option<i32> = Option::Some { value: 1 };",
    ));
}

#[test]
fn inferred_type_arguments_must_agree() {
    only_errors(
        check(
            "enum Option<T> { Some { value: T }, None }
let some: Option<bool> = Option::Some { value: 1 };",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn generic_function_is_instantiated_at_each_use() {
    assert_no_errors(check(
        "func id<T>(value: T) -> T { value }
let number: i32 = id(1);
let flag: bool = id(true);",
    ));
}

#[test]
fn generic_function_result_has_the_argument_type() {
    only_errors(
        check(
            "func id<T>(value: T) -> T { value }
let flag: i32 = id(true);",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn generic_parameter_of_function_is_rigid() {
    only_errors(check("func next<T>(value: T) -> T { value + 1 }"), |error| {
        matches!(error, TypeError::Unexpected { .. } | TypeError::InvalidOperator { .. })
    });
}

#[test]
fn arguments_of_one_generic_parameter_must_agree() {
    only_errors(
        check(
            "func first<T>(a: T, b: T) -> T { a }
let value = first(1, true);",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn generic_function_can_call_generic_function() {
    assert_no_errors(check(
        "struct Holder<T> { value: T }
func wrap<T>(value: T) -> Holder<T> { Holder { value } }
func unwrap<T>(holder: Holder<T>) -> T { holder.value }
func round_trip<T>(value: T) -> T { unwrap(wrap(value)) }
let value: i32 = round_trip(5);",
    ));
}

#[test]
fn methods_with_their_own_generics() {
    assert_no_errors(check(
        "struct Holder<T> { value: T }
impl<T> Holder<T> {
    func map<U>(self, f: func(T) -> U) -> Holder<U> { Holder { value: f(self.value) } }
    func pair<U>(self, other: U) -> bool { true }
}
let numbers = Holder { value: 2 };
let flags: Holder<bool> = numbers.map(|value| { value > 1 });
let strings: Holder<string> = flags.map(|flag| { \"${flag}\" });
let paired: bool = numbers.pair(\"x\") && numbers.pair(true);",
    ));
}

#[test]
fn type_arguments_of_methods_are_checked() {
    only_errors(
        check(
            "struct Holder<T> { value: T }
impl<T> Holder<T> {
    func map<U>(self, f: func(T) -> U) -> Holder<U> { Holder { value: f(self.value) } }
}
let numbers = Holder { value: 2 };
let flags: Holder<bool> = numbers.map(|value| { value + 1 });",
        ),
        |error| matches!(error, TypeError::Unexpected { .. }),
    );
}

#[test]
fn functions_of_trait_impls_cannot_have_their_own_generics() {
    let errors = check(
        "trait Named { func name(self) -> string; }
struct User {}
impl Named for User {
    func name<T>(self) -> string { \"user\" }
}",
    );

    assert!(errors.0.iter().any(|error| matches!(error, TypeError::GenericMethod)), "{:?}", errors.0);
}
