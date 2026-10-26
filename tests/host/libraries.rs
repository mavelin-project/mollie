use std::path::Path;

use mollie::{
    compiler::{Compiler, sandbox::Limits},
    host::{CompilerExt, LibraryOptions, ScriptOptions},
};

use crate::lock;

const SHAPES: &str = "pub struct Square { pub side: i32 }

pub func area<T>(value: T, side: i32) -> i32 { side * side }

func secret() -> i32 { 7 }";

#[track_caller]
fn compiler_with_shapes(package: Option<&str>) -> Compiler<()> {
    let mut compiler = Compiler::with_symbols((), []).unwrap_or_else(|error| panic!("can't create the compiler: {error}"));

    compiler
        .load_library("shapes", SHAPES, Path::new("."), &LibraryOptions { package, dependencies: &[] })
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    compiler
}

#[track_caller]
fn run(compiler: &mut Compiler<()>, name: &str, source: &str, options: &ScriptOptions<'_>) -> i32 {
    compiler
        .compile_script_in::<(), i32>(name, &[], source, options)
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    compiler.script_fn::<(), i32>(name).expect("compiled").call((), Limits::default()).expect("it runs")
}

#[test]
fn programs_of_a_compiler_share_a_library() {
    let _guard = lock();
    let mut compiler = compiler_with_shapes(None);
    let options = ScriptOptions {
        libraries: &["shapes"],
        ..ScriptOptions::default()
    };

    assert_eq!(run(&mut compiler, "a", "import { area } from shapes;\narea(1, 3)", &options), 9);
    // The generic function again (another instance), and the struct.
    assert_eq!(run(&mut compiler, "b", "let square = shapes::Square { side: 2 };\nshapes::area(\"x\", 4) + square.side", &options), 18);
    // Both still run.
    assert_eq!(compiler.script_fn::<(), i32>("a").expect("compiled").call((), Limits::default()), Ok(9));
}

#[test]
fn libraries_are_used_when_granted() {
    let _guard = lock();
    let mut compiler = compiler_with_shapes(None);

    assert!(compiler.compile_script::<(), i32>("plain", &[], "shapes::area(1, 1)").is_err());
    assert!(
        compiler
            .compile_script_in::<(), i32>("missing", &[], "1", &ScriptOptions {
                libraries: &["missing"],
                ..ScriptOptions::default()
            })
            .is_err()
    );
    // A library is loaded once.
    assert!(compiler.load_library("shapes", SHAPES, Path::new("."), &LibraryOptions::default()).is_err());
}

#[test]
fn private_items_are_for_the_package() {
    let _guard = lock();
    let mut compiler = compiler_with_shapes(Some("game"));
    let outside = ScriptOptions {
        libraries: &["shapes"],
        ..ScriptOptions::default()
    };
    let inside = ScriptOptions {
        libraries: &["shapes"],
        package: Some("game"),
        ..ScriptOptions::default()
    };

    assert!(compiler.compile_script_in::<(), i32>("outside", &[], "shapes::secret()", &outside).is_err());
    assert_eq!(run(&mut compiler, "inside", "shapes::secret()", &inside), 7);
}

#[test]
fn libraries_use_their_dependencies() {
    let _guard = lock();
    let mut compiler = compiler_with_shapes(None);

    compiler
        .load_library("scenes", "pub func total() -> i32 { shapes::area(0, 2) + 1 }", Path::new("."), &LibraryOptions {
            package: None,
            dependencies: &["shapes"],
        })
        .unwrap_or_else(|error| panic!("{}", error.display(&compiler.type_context.tcx)));

    // Programs only need what they use.
    let options = ScriptOptions {
        libraries: &["scenes"],
        ..ScriptOptions::default()
    };

    assert_eq!(run(&mut compiler, "scene", "scenes::total()", &options), 5);
    // A dependency must be loaded before.
    assert!(compiler.load_library("broken", "pub func f() -> i32 { 1 }", Path::new("."), &LibraryOptions {
        package: None,
        dependencies: &["nowhere"],
    })
    .is_err());
}
