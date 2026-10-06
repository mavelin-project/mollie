//! Untrusted programs can't crash the compiler: programs from a seeded
//! generator (random tokens, mutations of real programs, and well-typed
//! programs) are compiled and run with limits. They may be rejected or trap,
//! but the compiler must never fail internally (panic).
//!
//! The number of programs per generator is `MOLLIE_FUZZ_ITERS` (default 50).
//! A compiler is reused for [`PROGRAMS_PER_COMPILER`] programs (a new one
//! loads and checks `std`, which takes most of the time), like a game
//! reloading an addon many times. The tests don't take the lock of the other
//! tests: generated programs can't name the host functions it protects.

use std::{
    cell::RefCell,
    collections::HashMap,
    io::Write,
    slice::from_ref,
    sync::{Mutex, Once, PoisonError},
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

use mollie_compiler::{Compiler, error::CompileError, sandbox::Limits};
use mollie_typing::{TypeError, TypeRef};

use crate::compiler;

/// Programs compiled by one compiler before a new one is made.
const PROGRAMS_PER_COMPILER: usize = 200;

type RunningProgram = (Instant, String, bool);

/// Programs being checked, by thread: when they started, and whether the
/// watchdog reported them.
static RUNNING: Mutex<Option<HashMap<ThreadId, RunningProgram>>> = Mutex::new(None);

/// A program taking longer than this is reported by the watchdog.
const SLOW: Duration = Duration::from_secs(10);

/// Starts a thread reporting programs that take longer than [`SLOW`] (likely
/// hanging the compiler), on stderr directly: output of tests is only shown
/// when they fail, and a hanging one never does.
fn start_watchdog() {
    static STARTED: Once = Once::new();

    STARTED.call_once(|| {
        thread::spawn(|| {
            loop {
                thread::sleep(Duration::from_secs(1));

                let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);

                for (start, source, reported) in running.get_or_insert_with(HashMap::new).values_mut() {
                    if !*reported && start.elapsed() > SLOW {
                        *reported = true;

                        let _ = writeln!(
                            std::io::stderr(),
                            "\nfuzz: a program has been compiling or running for over {SLOW:?}:\n{source}\n"
                        );
                    }
                }
            }
        });
    });
}

/// Records that the current thread checks `source` until the guard is
/// dropped.
fn watch(source: &str) -> impl Drop {
    struct Watched;

    impl Drop for Watched {
        fn drop(&mut self) {
            let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);

            running.get_or_insert_with(HashMap::new).remove(&thread::current().id());
        }
    }

    start_watchdog();
    RUNNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(thread::current().id(), (Instant::now(), source.to_owned(), false));

    Watched
}

thread_local! {
    /// The compiler of the current batch of programs on this thread, and how
    /// many it compiled.
    static BATCH: RefCell<Option<(Compiler<()>, usize)>> = const { RefCell::new(None) };
}

/// A xorshift generator: the same seed gives the same programs.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n.max(1) as u64).unwrap_or(0)
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn iterations() -> usize {
    std::env::var("MOLLIE_FUZZ_ITERS").ok().and_then(|value| value.parse().ok()).unwrap_or(50)
}

/// The result of compiling a program.
enum Outcome {
    /// Rejected, with the displayed errors.
    Rejected(CompileError, String),
    Ran,
}

/// Compiles `source` (returning an `i32`) and runs it with tight limits.
/// Panics only if the compiler fails internally.
#[track_caller]
fn check(source: &str) -> Outcome {
    let _watched = watch(source);

    BATCH.with_borrow_mut(|batch| {
        if batch.as_ref().is_none_or(|&(_, compiled)| compiled >= PROGRAMS_PER_COMPILER) {
            *batch = Some((compiler(), 0));
        }

        let (compiler, compiled_count) = batch.as_mut().expect("made above");

        *compiled_count += 1;

        check_with(compiler, source)
    })
}

#[track_caller]
fn check_with(compiler: &mut Compiler<()>, source: &str) -> Outcome {
    let i32 = compiler.type_context.tcx.types.core_types.i32;
    let mut provider = compiler.start_compiling();

    match provider.compile("main", Vec::<(String, TypeRef)>::new(), Some(i32), source) {
        Err(CompileError::Internal(message)) => panic!("internal compiler error: {message}\n\nprogram:\n{source}"),
        Err(error) => {
            let text = error.display(&provider.type_context.tcx).to_string();

            return Outcome::Rejected(error, text);
        }
        Ok(_) => (),
    }

    let main = unsafe { provider.compiler.get_func::<extern "C" fn() -> i32>("main") }.expect("`main` must be compiled");
    let limits = Limits {
        fuel: Some(200_000),
        heap_bytes: Some(4 * 1024 * 1024),
        stack_bytes: Some(256 * 1024),
        ..Limits::default()
    };

    // Traps are fine: the program stops, the host doesn't.
    let _ = provider.compiler.run(limits, || main());

    Outcome::Ran
}

const WORDS: &[&str] = &[
    "let", "mut", "func", "struct", "enum", "value", "trait", "impl", "for", "in", "while", "loop", "break", "continue", "return", "if", "else", "match", "is",
    "as", "self", "super", "import", "from", "module", "const", "view", "x", "y", "Point", "Option", "Some", "None", "T", "i32", "f32", "bool", "string", "0",
    "1", "42", "1.5", "true", "\"text\"", "\"${x}\"", "(", ")", "{", "}", "[", "]", "<", ">", ",", ";", ":", "::", ".", "..", "..=", "=", "==", "+", "-", "*",
    "/", "%", "!", "&&", "||", "?", "=>", "->", "|", "_", "'a", "@[", "é", "😀",
];

const CORPUS: &[&str] = &[
    "struct Point { x: i32, y: i32 }\nlet p = Point { x: 1, y: 2 };\np.x + p.y",
    "enum Shape { Circle { r: i32 }, Square { s: i32 } }\nfunc area(s: Shape) -> i32 { match s { Circle { r } => r * r * 3, Square { s } => s * s } }\narea(Shape::Circle { r: 2 })",
    "let mut total = 0;\nfor i in 0..10 { if i % 2 == 0 { total += i; } }\ntotal",
    "func f<T>(x: T, g: func(T) -> i32) -> i32 { g(x) }\nf(3, |v| { v * 2 })",
    "trait Area { func area(self) -> i32; }\nstruct Sq { s: i32 }\nimpl Area for Sq { func area(self) -> i32 { self.s * self.s } }\nlet a: Area = Sq { s: 3 };\na.area()",
    "let values = [1, 2, 3];\nvalues.push(4);\nvalues[3] + values.len() as i32",
    "value struct V { a: i32, b: i32 }\nimpl V { func bump(mut self) { self.a += 1; } }\nlet mut v = V { a: 1, b: 2 };\nv.bump();\nv.a",
    "let s = \"héllo ${1 + 1}\";\ns.len() as i32",
    "func fib(n: i32) -> i32 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }\nfib(15)",
    "let found = 'outer: loop { for i in 0..5 { if i == 3 { break 'outer i; } } break 'outer -1; };\nfound",
];

#[test]
fn random_tokens() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    for _ in 0..iterations() {
        let length = 1 + rng.below(40);
        let source = (0..length).map(|_| *rng.pick(WORDS)).collect::<Vec<_>>().join(" ");

        check(&source);
    }
}

#[test]
fn mutated_programs() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);

    for _ in 0..iterations() {
        let mut words = rng.pick(CORPUS).split(' ').map(str::to_owned).collect::<Vec<_>>();

        for _ in 0..=rng.below(4) {
            let index = rng.below(words.len());

            match rng.below(4) {
                0 if words.len() > 1 => {
                    words.remove(index);
                }
                1 => {
                    let word = words[index].clone();

                    words.insert(index, word);
                }
                2 => words[index] = (*rng.pick(WORDS)).to_owned(),
                _ => {
                    let other = rng.below(words.len());

                    words.swap(index, other);
                }
            }
        }

        check(&words.join(" "));
    }
}

/// A well-typed `i32` expression of at most `depth` levels, using the
/// variables `vars`.
fn int_expr(rng: &mut Rng, depth: usize, vars: &[String]) -> String {
    if depth == 0 || rng.chance(25) {
        return match rng.below(3) {
            0 if !vars.is_empty() => rng.pick(vars).clone(),
            1 => format!("{}", rng.below(100)),
            _ => format!("-{}", rng.below(10)),
        };
    }

    let next = depth - 1;

    match rng.below(9) {
        0 => format!("({} {} {})", int_expr(rng, next, vars), rng.pick(&["+", "-", "*"]), int_expr(rng, next, vars)),
        // May divide by zero: a trap.
        1 => format!("({} {} {})", int_expr(rng, next, vars), rng.pick(&["/", "%"]), int_expr(rng, next, vars)),
        2 => format!(
            "if {} {{ {} }} else {{ {} }}",
            bool_expr(rng, next, vars),
            int_expr(rng, next, vars),
            int_expr(rng, next, vars)
        ),
        3 => {
            let name = format!("v{depth}");
            let value = int_expr(rng, next, vars);
            let body = int_expr(rng, next, &[vars, from_ref(&name)].concat());

            format!("{{ let {name} = {value}; {body} }}")
        }
        // May be out of bounds: a trap.
        4 => format!(
            "[{}, {}][({} % 3) as usize]",
            int_expr(rng, next, vars),
            int_expr(rng, next, vars),
            int_expr(rng, next, vars)
        ),
        5 => format!("twice(|x| {{ {} }}, {})", int_expr(rng, next, &[String::from("x")]), int_expr(rng, next, vars)),
        6 => format!("pick(Choice::{} {{ value: {} }})", rng.pick(&["Left", "Right"]), int_expr(rng, next, vars)),
        7 => format!("\"{}\".len() as i32", "a".repeat(rng.below(5))),
        _ => format!("sum_to({})", int_expr(rng, next, vars)),
    }
}

fn bool_expr(rng: &mut Rng, depth: usize, vars: &[String]) -> String {
    let next = depth.saturating_sub(1);

    match rng.below(3) {
        0 => format!(
            "({} {} {})",
            int_expr(rng, next, vars),
            rng.pick(&["<", "<=", "==", "!=", ">"]),
            int_expr(rng, next, vars)
        ),
        1 => format!("!({})", bool_expr(rng, next, vars)),
        _ => (*rng.pick(&["true", "false"])).to_owned(),
    }
}

const PRELUDE: &str = "enum Choice { Left { value: i32 }, Right { value: i32 } }
func pick(choice: Choice) -> i32 { match choice { Left { value } => value, Right { value } => -value } }
func twice(f: func(i32) -> i32, x: i32) -> i32 { f(f(x)) }
func sum_to(n: i32) -> i32 { let mut total = 0; let mut i = 0; while i < n { total += i; i += 1; } total }
";

#[test]
fn well_typed_programs() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);

    for _ in 0..iterations() {
        let source = format!("{PRELUDE}{}", int_expr(&mut rng, 6, &[]));

        if let Outcome::Rejected(_, errors) = check(&source) {
            panic!("a well-typed program was rejected:\n{errors}\n\nprogram:\n{source}");
        }
    }
}

#[test]
fn long_and_deep_code_is_rejected() {
    let chain = format!("{}1", "1 + ".repeat(100_000));
    let parens = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));
    let blocks = format!("{}1{}", "{ ".repeat(10_000), " }".repeat(10_000));
    let unary = format!("{}1", "-".repeat(10_000));
    let else_ifs = format!("if false {{ 0 }}{} else {{ 1 }}", " else if false { 0 }".repeat(10_000));
    let calls = format!("func f(x: i32) -> i32 {{ x }}\n{}1{}", "f(".repeat(10_000), ")".repeat(10_000));
    let interpolations = format!("{}x{}", "\"${".repeat(1000), "}\"".repeat(1000));
    let types = format!("let x: {}i32{} = 1;\n1", "Option<".repeat(5_000), ">".repeat(5_000));
    let huge = format!("let x = 1;\n{}1", "x + 0;\n".repeat(1_000_000));

    for source in [chain, parens, blocks, unary, else_ifs, calls, interpolations, types, huge] {
        assert!(
            matches!(check(&source), Outcome::Rejected(..)),
            "deep code must be rejected: {}...",
            &source[..80.min(source.len())]
        );
    }
}

#[test]
fn programs_that_hung_the_parser_are_rejected() {
    // Found by `random_tokens` and `mutated_programs`: a name after a node
    // property without a comma was never consumed.
    for source in [
        "Point { x: i32, y: i32 i32 enum p struct Point { x: 1, y: 2 };\np.x + p.y",
        "y { Option Some > || break ? _ @[ } else true from",
        "Point { x: 1 y: 2 }",
    ] {
        assert!(matches!(check(source), Outcome::Rejected(..)), "{source}");
    }
}

#[test]
fn nesting_within_the_limits_works() {
    let chain = format!("{}1", "1 + ".repeat(100));
    let parens = format!("{}1{}", "(".repeat(50), ")".repeat(50));
    let blocks = format!("let value = {}1{};\nvalue", "{ ".repeat(50), " }".repeat(50));

    for source in [chain, parens, blocks] {
        match check(&source) {
            Outcome::Ran => (),
            Outcome::Rejected(_, errors) => panic!("{errors}\n\nprogram:\n{source}"),
        }
    }
}

#[test]
fn growing_instances_are_rejected() {
    for source in [
        "func f<T>(x: T) -> i32 { f([x]) }\nf(1)",
        "struct Pair<T> { a: T, b: T }\nfunc f<T>(x: T) -> i32 { f(Pair { a: x, b: x }) }\nf(1)",
    ] {
        let Outcome::Rejected(CompileError::Type(errors), _) = check(source) else {
            panic!("`{source}` must be rejected");
        };

        assert!(errors.iter().any(|error| matches!(&*error.error, TypeError::InstantiationLimit)), "{errors:?}");
    }
}

#[test]
fn unicode_outside_of_strings() {
    check("let é = 1;\né");
    check("let x = \"😀 ${1}\";\nx.len() as i32 ⚠ 1");
    check("1 \u{a0}+ 2");
}
