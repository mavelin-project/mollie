# Embedding and Internals

This part is for people embedding Mollie in a host, like a game or an application, and for anyone working on Mollie itself. The language itself is described in [the first part](../language/index.md).

Mollie is a statically typed scripting language, compiled to machine code with [Cranelift](https://cranelift.dev). It was made for running code nobody has reviewed, like addons that players download, inside a host. Every program gets its own memory, its own limits and only the parts of the host's API it's allowed to use. And it does the same thing on every machine, so peers in a multiplayer game can run the same addons and agree on the result.

## In this part

- [Architecture](architecture.md): the crates, and how source code becomes running code.
- [Embedding](embedding.md): the host API. Registering functions and types, compiling and calling programs, limits, capabilities, libraries, reloading and threads.
- [Runtime](runtime.md): how values look in compiled code, calling conventions, traps and limits, the garbage collector, determinism, and the exact rules the first part leaves out.
- [Tooling](tooling.md): stubs of the host's API, the language server, the formatter and the tests.

## A first program

```rust,ignore
use mollie::{
    compiler::{Compiler, sandbox::Limits},
    host::{CompilerExt, Host},
};

let mut compiler = Compiler::with_symbols((), [])?;

Host::new(&mut compiler).function_named("log", &["message"], |message: mollie::MolStr| println!("{message}"));

compiler.compile_script::<(i32,), i32>("main", &["x"], "log(\"hi\");\nx * 2")?;

let main = compiler.script_fn::<(i32,), i32>("main")?;

assert_eq!(main.call((21,), Limits::default()), Ok(42));
```

The host registers a `log` function, compiles a program taking `x`, and calls it. The program's last expression, `x * 2`, is its result.

## What a host can rely on

Programs are isolated. Each `Compiler` owns its heap, its code and its host functions, and values of one program can't reach another.

Programs can't take the host down. Running out of fuel, call depth, stack or memory stops the program with a trap, and the host carries on.

Hostile source is rejected, not crashed on. Size, nesting and generic instances are limited. If the compiler itself panics, the panic becomes an error, and that compiler refuses any further work.

Programs are deterministic. The same program with the same inputs and limits gives the same results, and traps at the same point, on every platform (see [Runtime](runtime.md#determinism)).

Programs only see what they're granted. Parts of the host's API can be put behind capabilities, and libraries are only visible to programs that ask for them.
