use mollie_typing::TypeError;

use crate::{assert_no_errors, check, only_errors, single_error};

const SHAPES: &str = "trait Shape { func area(self) -> f32; }
struct Square { side: f32 }
impl Shape for Square { func area(self) -> f32 { self.side * self.side } }
struct Circle { radius: f32 }
impl Shape for Circle { func area(self) -> f32 { self.radius } }
";

#[test]
fn view_accepts_children_of_its_children_type() {
    assert_no_errors(check(&format!(
        "{SHAPES}
view Column {{ spacing: f32 = 0.0, children: Shape[] }}

let column = Column {{ spacing: 2.0, Square {{ side: 1.0 }} Circle {{ radius: 2.0 }} }};
let shapes: Shape[] = column.children;"
    )));
}

#[test]
fn view_with_a_single_child() {
    assert_no_errors(check(&format!(
        "{SHAPES}
view Frame {{ children: Shape }}

let frame = Frame {{ Square {{ side: 1.0 }} }};"
    )));
}

#[test]
fn view_with_a_single_child_rejects_more() {
    let error = single_error(check(&format!(
        "{SHAPES}
view Frame {{ children: Shape }}

let frame = Frame {{ Square {{ side: 1.0 }} Square {{ side: 2.0 }} }};"
    )));

    assert!(matches!(error, TypeError::TooManyChildren { found: 2 }), "{error:?}");
}

#[test]
fn view_rejects_children_of_other_types() {
    let errors = check(&format!(
        "{SHAPES}
view Squares {{ children: Square[] }}

let squares = Squares {{ Square {{ side: 1.0 }} Circle {{ radius: 2.0 }} }};"
    ));

    only_errors(errors, |error| matches!(error, TypeError::Unexpected { .. }));
}

#[test]
fn views_implement_container() {
    assert_no_errors(check(&format!(
        "{SHAPES}
view Column {{ children: Shape[] }}

func count(container: Container<Shape[]>) -> usize {{
    container.children().len()
}}

let column = Column {{ Square {{ side: 1.0 }} }};
let n: usize = count(column);
column.set_children([Circle {{ radius: 1.0 }}]);"
    )));
}

#[test]
fn view_named_like_the_trait() {
    // The generated impl names the trait by its full path.
    assert_no_errors(check(&format!(
        "{SHAPES}
view Container {{ children: Shape }}

let container = Container {{ Square {{ side: 1.0 }} }};"
    )));
}

#[test]
fn generic_view() {
    assert_no_errors(check(&format!(
        "{SHAPES}
view List<T> {{ children: T[] }}

let list: List<Square> = List {{ Square {{ side: 1.0 }} Square {{ side: 2.0 }} }};
let first: Square = list.children[0];"
    )));
}

#[test]
fn struct_implementing_container() {
    assert_no_errors(check(&format!(
        "{SHAPES}
struct Group {{ items: Shape[] }}

impl Container<Shape[]> for Group {{
    func children(self) -> Shape[] {{ self.items }}
    func set_children(self, children: Shape[]) {{ self.items = children; }}
}}

let group = Group {{ items: [Square {{ side: 0.0 }}], Circle {{ radius: 1.0 }} Square {{ side: 2.0 }} }};"
    )));
}

#[test]
fn struct_without_container_rejects_children() {
    let error = single_error(check(&format!(
        "{SHAPES}
struct Plain {{}}

let plain = Plain {{ Square {{ side: 1.0 }} }};"
    )));

    assert!(matches!(error, TypeError::NotContainer { .. }), "{error:?}");
}

#[test]
fn control_flow_in_children() {
    assert_no_errors(check(&format!(
        "{SHAPES}
view Column {{ children: Shape[] }}

let sides = [1.0, 2.0];
let round = true;
let column = Column {{
    Square {{ side: 1.0 }}
    if round {{ Circle {{ radius: 1.0 }} }} else if sides.len() > 1 {{ Square {{ side: 2.0 }} Square {{ side: 3.0 }} }}
    for side in sides {{ Square {{ side }} }}
    {{ Circle {{ radius: 4.0 }} }}
}};
let shapes: Shape[] = column.children;"
    )));
}

#[test]
fn single_child_under_a_condition() {
    assert_no_errors(check(&format!(
        "{SHAPES}
view Frame {{ children: Shape }}

let round = true;
let frame = Frame {{ if round {{ Circle {{ radius: 1.0 }} }} else {{ Square {{ side: 1.0 }} }} }};
let expression = Frame {{ {{ Square {{ side: 2.0 }} }} }};"
    )));
}

#[test]
fn single_child_rejects_loops_and_missing_else() {
    let error = single_error(check(&format!(
        "{SHAPES}
view Frame {{ children: Shape }}

let frame = Frame {{ for side in [1.0] {{ Square {{ side }} }} }};"
    )));

    assert!(matches!(error, TypeError::RepeatedChild), "{error:?}");

    let error = single_error(check(&format!(
        "{SHAPES}
view Frame {{ children: Shape }}

let frame = Frame {{ if true {{ Square {{ side: 1.0 }} }} }};"
    )));

    assert!(matches!(error, TypeError::ConditionalChild), "{error:?}");

    let error = single_error(check(&format!(
        "{SHAPES}
view Frame {{ children: Shape }}

let frame = Frame {{ if true {{ Square {{ side: 1.0 }} }} else {{ Square {{ side: 1.0 }} Square {{ side: 2.0 }} }} }};"
    )));

    assert!(matches!(error, TypeError::TooManyChildren { found: 2 }), "{error:?}");
}

#[test]
fn closure_fields() {
    assert_no_errors(check(
        "view Slider { value: f32, on_change: func(f32) -> f32, on_release: func() }

let scale = 2.0;
let slider = Slider {
    value: 0.5,
    on_change[factor = scale, scale](value) => { value * factor * scale },
    on_release => { },
};",
    ));
}
