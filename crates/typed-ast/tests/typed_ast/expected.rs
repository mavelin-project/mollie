//! Types expected of expressions: arrays of trait objects and variants
//! without the name of their enum.

use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors};

const SHAPES: &str = "trait Shape { func area(self) -> f32; }
struct Square { side: f32 }
impl Shape for Square { func area(self) -> f32 { self.side * self.side } }
struct Circle { radius: f32 }
impl Shape for Circle { func area(self) -> f32 { self.radius } }
";

#[test]
fn arrays_of_implementors_where_trait_objects_are_expected() {
    assert_no_errors(check(&format!(
        "{SHAPES}
struct Scene {{ shapes: Shape[] }}

func total(shapes: Shape[]) -> f32 {{ 0.0 }}
func make() -> Shape[] {{ [Square {{ side: 1.0 }}, Circle {{ radius: 1.0 }}] }}

let annotated: Shape[] = [Square {{ side: 1.0 }}, Circle {{ radius: 1.0 }}];
let scene = Scene {{ shapes: [Circle {{ radius: 1.0 }}, Square {{ side: 1.0 }}] }};
let sum = total([Square {{ side: 1.0 }}, Circle {{ radius: 2.0 }}]);
let mut assigned: Shape[] = [];
assigned = [Circle {{ radius: 1.0 }}, Square {{ side: 1.0 }}];"
    )));
}

#[test]
fn method_arguments_are_expected_of_parameter_types() {
    assert_no_errors(check(&format!(
        "{SHAPES}
struct Scene {{ shapes: Shape[] }}

impl Scene {{
    func replace(self, shapes: Shape[]) {{ self.shapes = shapes; }}
}}

let scene = Scene {{ shapes: [] }};
scene.replace([Square {{ side: 1.0 }}, Circle {{ radius: 1.0 }}]);"
    )));
}

#[test]
fn arrays_without_expected_type_have_one_element_type() {
    let errors = check(&format!("{SHAPES}\nlet shapes = [Square {{ side: 1.0 }}, Circle {{ radius: 1.0 }}];"));

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn elements_must_implement_the_expected_trait() {
    let errors = check(&format!(
        "{SHAPES}
struct Point {{ x: f32 }}

let shapes: Shape[] = [Square {{ side: 1.0 }}, Point {{ x: 1.0 }}];"
    ));

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn variants_without_their_enum_where_it_is_expected() {
    assert_no_errors(check(
        "func first(values: i32[]) -> Option<i32> {
    if values.len() == 0 { None } else { Some { value: values[0] } }
}

func describe(value: Option<i32>) -> i32 {
    match value {
        Some { value } => value,
        None => 0,
    }
}

struct Settings { limit: Option<i32> = Option::None }

let some: Option<i32> = Some { value: 1 };
let mut none: Option<i32> = None;
none = Some { value: 2 };
let described = describe(Some { value: 3 }) + describe(None);
let settings = Settings { limit: Some { value: 10 } };
let result: Result<i32, string> = Err { error: \"failed\" };",
    ));
}

#[test]
fn variants_are_checked_against_the_expected_enum() {
    let errors = check("let some: Option<i32> = Some { value: true };");

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn variant_without_enum_and_expected_type_is_not_found() {
    let errors = check("let some = Some { value: 1 };");

    assert!(
        errors.0.iter().any(|error| matches!(error, TypeError::NotFound { name, .. } if name == "Some")),
        "{:?}",
        errors.0
    );
}

#[test]
fn variables_shadow_variants() {
    assert_no_errors(check(
        "let None: i32 = 5;
let value: i32 = None;",
    ));
}
