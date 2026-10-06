//! The type checker reports errors in malformed programs instead of
//! panicking: compilers of scripting languages run inside their hosts.

use std::panic::{self, AssertUnwindSafe};

use crate::check;

/// Type-checks every source, returning those that made it panic (their
/// panics are printed too).
fn panicking<'a>(sources: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    sources
        .into_iter()
        .filter(|source| panic::catch_unwind(AssertUnwindSafe(|| check(source))).is_err())
        .collect()
}

#[test]
fn prefixes_of_a_real_program() {
    let source = include_str!("../../../../examples/ui.mol");
    let prefixes = source
        .char_indices()
        .map(|(index, _)| index)
        .step_by(97)
        .map(|end| &source[..end])
        .collect::<Vec<_>>();

    let failed = panicking(prefixes);

    assert!(
        failed.is_empty(),
        "panicked on {} prefixes, the shortest ends with:\n{:?}",
        failed.len(),
        failed
            .first()
            .map(|source| source.chars().rev().take(200).collect::<String>().chars().rev().collect::<String>())
    );
}

#[test]
fn malformed_programs() {
    let failed = panicking([
        "()",
        "let x = ();",
        "1..2",
        "1.len()",
        "0x",
        "99999999999999999999",
        "struct Point {}\nconst p = Point::X {};",
        "struct Point { x: i32 = 1 + }",
        "struct Point { x: i32 = undefined }",
        "struct Point { x: i32 = 200 * 200 * 200 * 200 * 200 }",
        "struct Point { x: i8 = -(0 - 128) }",
        "struct Point { x: i32 = 1 / 0 }",
        "struct Point { x: i32 = { let mut i = 0; while true { i += 1; } i } }",
        "struct Point { x: bool = 1 < 2 }",
        "trait Make { func make() -> i32; }\nstruct A {}\nimpl Make for A { func make() -> i32 { 1 } }\nconst a = A {};\na.make();",
        "module missing;",
        "import { x } from nowhere::deeper;",
        "match 1 {}",
        "match {",
        "let x = match 1 { _ => };",
        "if x is { }",
        "\"${\"",
        "\"${}\"",
        "\"${1 +}\"",
        "func f<T>(x: T) -> T { x }\nf();",
        "view V { children: i32[] }\nconst v = V { V {} };",
        "struct S {}\nconst s = S { S {} };",
        "enum E { A }\nconst e = E { };",
        "x.y.z()",
        "[][0]",
        "let a: i32[] = [];",
        // Loops, labels and ranges.
        "break;",
        "continue 'nowhere;",
        "'a: 'b: loop {}",
        "loop { break 'missing 1; }",
        "'x:",
        "let r = ..;",
        "let r = 1..=;",
        "for i in 0..\"a\" {}",
        "for i in true..false {}",
        // Bounds, defaults and `super`.
        "func f<T: Missing>(x: T) {}",
        "func f<T: i32>(x: T) {}",
        "func f<T:>(x: T) {}",
        "trait A { func a(self) -> i32 { self.b() } }",
        "trait A { func a(self) -> i32 { super.a() } }",
        "super.x();",
        "super;",
        "trait A { func a(self) -> i32 { 1 } func b(self) -> i32 { 2 } }\ntrait A { func a(self) -> i32; }",
        "trait A { func a(self) -> i32 { 1 } }\nstruct S {}\nimpl A for S { func a(self) -> i32 { super.a(1, 2) } }",
        // Named and default arguments.
        "func f(a: i32 = b) {}\nf();",
        "func f(a: i32 = a) {}\nf();",
        "func f(a: i32, b: i32 = a) {}\nf(b: 1);",
        "func f(a: i32) {}\nf(a: 1, a: 2, c: 3, 4);",
        "func f(a: i32 = ) {}",
        "let g = f(x:);",
        // Value types.
        "value struct",
        "value enum E",
        "value struct A { a: A }",
        "value struct A<T> { a: T }\nvalue struct B { a: A<B> }",
        "value value struct S {}",
        "struct S {}\nimpl S { func f(mut self) {} }",
        "value struct S { x: i32 }\nimpl S { func f(mut x: i32) {} }",
        "value struct S { x: i32 }\nlet s = S { x: 1 };\ns.x.y = 2;",
        "value struct S { x: i32 }\nS { x: 1 }.x = 2;",
        "value enum E { A { x: i32 } }\nlet same = E::A { x: 1 } == E::A { x: 1 };",
        // Format specifiers.
        "\"${1:}\"",
        "\"${1:..}\"",
        "\"${1:99999999}\"",
        "\"${:4}\"",
        "\"${x:4}\"",
    ]);

    assert!(failed.is_empty(), "panicked on: {failed:#?}");
}
