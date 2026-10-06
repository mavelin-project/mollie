use crate::{run_bool, run_f32, run_i32, run_usize};

#[test]
fn elements_smaller_than_a_pointer() {
    // Elements must be stored with their own size, not pointer size.
    assert_eq!(run_i32("let a = [10, 20, 30, 40];\na[0] + a[3] * 2"), 90);
    assert!(run_bool("let flags = [true, false, true];\nflags[2] && flags[1] == false"));
    assert!((run_f32("[0.5, 1.5, 2.5][2]") - 2.5).abs() < f32::EPSILON);
}

#[test]
fn element_assignment() {
    assert_eq!(run_i32("let a = [1, 2, 3];\na[1] = 20;\na[2] += 10;\na[0] + a[1] + a[2]"), 34);
}

#[test]
fn push_grows_the_array() {
    assert_eq!(run_i32("let a = [1, 2];\na.push(3);\na.push(4);\na[3] + a[2]"), 7);
}

#[test]
fn arrays_of_structs() {
    assert_eq!(
        run_i32(
            "struct Point { x: i32, y: i32 }

let points = [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }];
points[1].x + points[0].y"
        ),
        5
    );
}

#[test]
fn arrays_of_strings() {
    assert!(run_bool("let names = [\"a\", \"bb\"];\nnames[1] == \"bb\" && names[0] != \"bb\""));
}

#[test]
fn length_of_an_array() {
    assert_eq!(run_usize("get_size([1, 2, 3])"), 3);
}
