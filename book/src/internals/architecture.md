# Architecture

## Crates

The compiler is split into crates along its passes. Each crate only depends on the ones before it in this list.

The support crates come first. `mollie-shared` has what every pass needs: spans, positions, operators and the limits of the compiler. `mollie-index` has `IndexVec`, a vector indexed by a typed id, which most passes use instead of pointers. `mollie-const` holds values known at compile time (`ConstantValue`), for constants and defaults of fields.

The front end turns text into a syntax tree. `mollie-lexer` makes tokens, and `mollie-parser` makes `Stmt`s and `Expr`s out of them. The parser uses the primitive types of `mollie-typing`, and nothing else from it.

The middle is where most of the language lives. `mollie-typing` has the types themselves and the registry of everything a program declares: modules, structs and enums, traits, impls and functions. It also has the diagnostics. `mollie-typed-ast` uses it to resolve names and check types, which turns a syntax tree into a typed AST. The standard library (`std/*.mol`) and the module loaders are here too.

The back end makes machine code. `mollie-ir` decides how values are represented in code (`MollieType`, chunks) and how vtables are laid out. `mollie-compiler` generates Cranelift IR, compiles it with the JIT, and has the runtime that compiled code calls: the allocator and garbage collector, strings, math, and the sandbox that enforces limits.

The root crate, `mollie`, is what hosts use. It has the host API (`mollie::host`), handles to values of programs (`GcPtr`, `MolStr`), the builders of types, and stubs (`mollie::stub`).

Two tools are built on the same crates. `mollie-lsp` is the language server, and only goes as far as the typed AST. `mollie-fmt` is the formatter, and only needs the parser.

## From source to running code

```text
  source text
      │  mollie-lexer
      ▼
  tokens
      │  mollie-parser
      ▼
  syntax tree           one per module
      │  mollie-typed-ast
      ▼
  typed AST             plus the used items: function instances, vtables, layouts
      │  mollie-compiler
      ▼
  machine code          called by the host through an entry
```

Each step owns its output and hands it to the next, and nothing goes back. Errors in the program are found by the time it's type checked. Code generation can still fail, but only on something the type checker should have caught (`CompileError::Unsupported`) or on a bug.

### Lexing and parsing

The lexer reads the whole source at once into a `Vec` of tokens with positions. A string with `${...}` in it becomes one token holding the tokens of its expressions, so the parser never has to look inside strings.

The parser works on a mutable slice of that `Vec`. Consuming a token takes it out of the slice, so tokens are never cloned. Code between brackets is handed to a new parser over just that part of the slice (`Parser::split`).

Since the compiler runs code from strangers, it refuses source that's too big (when a module is loaded), too deeply nested (in the parser), or with strings interpolated in each other too deeply (in the lexer). The exact numbers are in [Runtime](runtime.md#limits). Passes that recurse over the tree grow the stack when they get close to its end (`mollie_shared::limits::grow_stack`), so even an allowed depth can't overflow a small thread's stack.

### Modules

A program is made of its root module, its submodules, the standard library and the host's modules. `module ui;` asks the `ModuleLoader` for `ui`. `FileModuleLoader` reads it from `ui.mol` next to the root, and `()` is a loader with no submodules at all. A file that's missing is reported as a missing module, and one that doesn't parse as a syntax error in that file (`LoadFailure::Syntax`).

`ModuleMap` registers the items of every module before checking any function body. That's why items can be used before they're declared.

Names are looked up in this order: the module's own items and imports, then libraries (`std` is one), then the host's root module, and last the prelude (`std::prelude`).

### Type checking

Types are inferred by unification (`TypeSolver`). Every expression starts with a type variable, and constraints between expressions unify them. Generic functions get fresh variables at every use.

After inference, `typed-ast` checks everything else the language requires. Trait bounds are checked, and calls of trait functions are resolved to impls (`find_vtable`). Places where a concrete value becomes a trait object are recorded, because code generation has to box them. `match` must be exhaustive, and changes must go through `let mut` places or `mut self`. Every error becomes a `Diagnostic` with a span.

Type checking also collects the *used items*: every function instance (a function with its type arguments), vtable and layout the program needs. Generic code is compiled once per instance. Polymorphic recursion would make instances forever, so their number and size are limited, and going over the limit is a compile error.

Maps that are iterated are `IndexMap`s, so the same program always gets its diagnostics in the same order.

### Code generation

`mollie-compiler` turns the typed AST into Cranelift IR, one used item at a time. `FuncCompiler::compile` makes three things out of a program:

- its top-level code, as a function without a name. Compiling a program again under the same name replaces it, which is how reloading works;
- every function instance and vtable it uses, unless another program of the same compiler already compiled them;
- an *entry*, a wrapper with a C signature that the host calls (see [Runtime](runtime.md#calling-conventions)).

A panic in the compiler is caught (`catch_unwind`) and returned as `CompileError::Internal`. The compiler's state may be broken after that, so it refuses to compile anything else.

### The runtime

Compiled code calls into a small runtime for allocation, strings, math that must be the same on every platform, panics, traps and fuel. Calls that can collect garbage first record the caller's frame, so the collector can find references on the stack. [Runtime](runtime.md) has the details.
