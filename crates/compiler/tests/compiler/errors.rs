use mollie_compiler::{error::CompileError, sandbox::Limits};
use mollie_typing::TypeRef;

use crate::{compile_error, compile_error_text, compiler, lock};

#[test]
fn type_errors_are_reported() {
    let error = compile_error("let x: i32 = true;");

    assert!(matches!(error, CompileError::Type(ref diagnostics) if !diagnostics.is_empty()), "{error:?}");
}

#[test]
fn compiler_is_usable_after_a_type_error() {
    let _guard = lock();
    let mut compiler = compiler();
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let mut provider = compiler.start_compiling();

    assert!(provider.compile("broken", Vec::<(String, TypeRef)>::new(), Some(i32), "true").is_err());

    provider
        .compile("fixed", Vec::<(String, TypeRef)>::new(), Some(i32), "40 + 2")
        .unwrap_or_else(|error| panic!("{error}"));

    let fixed = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("fixed") }.expect("`fixed` must be compiled");

    assert_eq!(provider.compiler.run(Limits::default(), || fixed()), Ok(42));
}

#[test]
fn errors_are_displayed_with_locations() {
    let text = compile_error_text("let x: i32 = 1;\nconst y: i32 = true;");

    assert!(text.starts_with("error: "), "{text}");
    assert!(text.contains("`i32`") && text.contains("`bool`"), "{text}");
    assert!(text.contains("--> <root>:2:"), "{text}");
}
