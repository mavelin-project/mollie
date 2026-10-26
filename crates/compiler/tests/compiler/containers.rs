use crate::{run_i32, run_i32_stressed, run_usize};

const SHAPES: &str = "trait Shape { func area(self) -> i32; }
struct Square { side: i32 }
impl Shape for Square { func area(self) -> i32 { self.side * self.side } }
struct Circle { radius: i32 }
impl Shape for Circle { func area(self) -> i32 { self.radius * 3 } }

func total(shapes: Shape[]) -> i32 {
    let mut sum = 0;

    for shape in shapes {
        sum += shape.area();
    }

    sum
}
";

#[test]
fn view_children() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}
view Column {{ spacing: i32 = 1, children: Shape[] }}

let column = Column {{ Square {{ side: 1 }} Circle {{ radius: 2 }} Square {{ side: 3 }} }};
total(column.children) * 10 + column.spacing"
        )),
        (1 + 6 + 9) * 10 + 1
    );
}

#[test]
fn view_with_a_single_child() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}
view Frame {{ children: Shape }}

let frame = Frame {{ Circle {{ radius: 5 }} }};
frame.children.area()"
        )),
        15
    );
}

#[test]
fn containers_as_trait_objects() {
    assert_eq!(
        run_usize(&format!(
            "{SHAPES}
view Column {{ children: Shape[] }}

func count(container: Container<Shape[]>) -> usize {{
    container.children().len()
}}

let column = Column {{ Square {{ side: 1 }} Square {{ side: 2 }} }};
let before = count(column);

column.set_children([Square {{ side: 1 }}, Square {{ side: 1 }}, Square {{ side: 1 }}]);

before * 10 + count(column)"
        )),
        23
    );
}

#[test]
fn struct_implementing_container() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}
struct Group {{ items: Shape[], updates: i32 = 0 }}

impl Container<Shape[]> for Group {{
    func children(self) -> Shape[] {{ self.items }}

    func set_children(self, children: Shape[]) {{
        self.items = children;
        self.updates += 1;
    }}
}}

let group = Group {{ items: [Square {{ side: 9 }}], Square {{ side: 1 }} Circle {{ radius: 1 }} }};
group.updates * 100 + total(group.children())"
        )),
        100 + 4
    );
}

#[test]
fn nested_views_survive_collections() {
    assert_eq!(
        run_i32_stressed(&format!(
            "{SHAPES}
view Column {{ children: Shape[] }}

impl Shape for Column {{
    func area(self) -> i32 {{ total(self.children) }}
}}

let root = Column {{
    Column {{ Square {{ side: 1 }} Square {{ side: 2 }} }}
    Circle {{ radius: 1 }}
    Column {{ Column {{ Square {{ side: 3 }} }} }}
}};

root.area()"
        )),
        1 + 4 + 3 + 9
    );
}

#[test]
fn arrays_of_different_implementors() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}
view Column {{ children: Shape[] }}

let shapes: Shape[] = [Square {{ side: 2 }}, Circle {{ radius: 1 }}];
let column = Column {{ Square {{ side: 1 }} }};

column.set_children([Circle {{ radius: 2 }}, Square {{ side: 3 }}]);
total(shapes) * 100 + total(column.children)"
        )),
        (4 + 3) * 100 + 6 + 9
    );
}

#[test]
fn control_flow_in_children() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}
view Column {{ children: Shape[] }}

func build(round: bool, sides: i32[]) -> Column {{
    Column {{
        Square {{ side: 1 }}
        if round {{ Circle {{ radius: 1 }} }} else if sides.len() > 2 {{ Square {{ side: 10 }} }} else {{ Square {{ side: 2 }} Square {{ side: 2 }} }}
        for side in sides {{
            if side > 1 {{ Square {{ side }} }}
        }}
        {{ Circle {{ radius: 100 }} }}
    }}
}}

total(build(true, [1, 2, 3]).children) * 1000 + total(build(false, [1]).children)"
        )),
        (1 + 3 + 4 + 9 + 300) * 1000 + (1 + 4 + 4 + 300)
    );
}

#[test]
fn single_child_under_a_condition() {
    assert_eq!(
        run_i32(&format!(
            "{SHAPES}
view Frame {{ children: Shape }}

func frame(round: bool) -> Frame {{
    Frame {{ if round {{ Circle {{ radius: 5 }} }} else {{ Square {{ side: 5 }} }} }}
}}

frame(true).children.area() * 100 + frame(false).children.area()"
        )),
        15 * 100 + 25
    );
}

#[test]
fn closure_field_bindings_are_evaluated_once() {
    assert_eq!(
        run_i32(
            "view Button { on_click: func(i32) -> i32, on_hover: func() -> i32 }

let mut count = 1;
let button = Button {
    on_click[before = count * 10](by) => { before + by },
    on_hover => { 7 },
};

count = 5;
button.on_click(2) * 10 + button.on_hover()"
        ),
        (10 + 2) * 10 + 7
    );
}
