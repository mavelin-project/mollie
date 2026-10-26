# Tooling

## Stubs of the host's API

Editors don't run the host, so they can't know what it registers. A host tells them by writing a *stub*: Mollie source that declares everything it registered, with `extern` functions without bodies and `extern` types without implementations.

```rust,ignore
use mollie::host::CompilerExt;

// After registering the API: writes `project/.mollie/host` if it changed.
compiler.write_host_stub(Path::new("project"))?;
```

`mollie::stub::host_stub(&compiler)` builds the stub as a `HostStub`. `write_to(dir)` writes it, and `update(dir)` writes only what changed and removes modules that are gone. `read_from(dir)` reads it back.

A stub has a `lib.mol` for the host's root module, and a file for each other module:

```mollie
// lib.mol
module graphics;

extern func log(message: string);
extern func tr(key: string, ..args: string[..]) -> string;

extern trait Drawable {
    func measure(self, size: graphics::Size, ctx: DrawContext) -> graphics::Size;
}

// graphics.mol
extern value struct Size { width: f32, height: f32 }
extern struct Block { id: string, collidable: bool = true }
```

Properties are declared as `extern get func fov(self) -> f32;` and `extern set func fov(self, value: f32);`, and defaults of traits are written out with their bodies.

The language server also needs to know the parameters of programs. `compiler.write_host_stub_for::<(Opaque<Data>, i32)>(project, &["data", "limit"])` declares them as variables of the host, adding `extern let mut data: Data;` and `extern let mut limit: i32;` to `lib.mol`. `host_stub_with` does the same for `host_stub`. A program's parameters hide the host's variables with the same names.

`extern` is only allowed in stubs (`ParsedModule::parse_stub`). Stubs include every capability: tools see the whole API, while each program sees only what it's granted.

## Language server

`mollie-lsp` (`crates/lsp`) is a language server that talks over stdio. It shows diagnostics as you type, types on hover, and completions, including methods of bounded generics. Go to definition works into `std` too, which opens as read-only files.

It reads its configuration from `mollie.toml` in the project's folder:

```toml
# Programs: other files are their submodules.
entries = ["addons/shop/main.mol", "addons/quests/main.mol"]
# The stub of the host's API (this is the default).
host = ".mollie/host"
# Libraries programs use: folders with a `lib.mol`, in the order they load.
libraries = ["ui=../resources/ui"]
```

Hosts can also list libraries in `.mollie/libraries` next to the stub, with lines like `ui = "/path/to/ui"`. Every listed library is visible to the program being checked. A program inside a library's folder, like an addon's `main.mol` next to its `lib.mol`, is in that library's package.

A file opened on its own is checked as part of the entry whose program contains it. Without `mollie.toml`, the server looks for the stub in `.mollie/host` of the file's folder and of every folder above it.

## Formatter

`mollie-fmt` (`crates/fmt`) prints syntax trees back as formatted source.

## Logging

The compiler logs through `tracing`, only at the `trace` level, so nothing shows up with the usual `info` filter. There are two targets:

- `mollie-compiler/adt_types` logs every type compiled, with its size and alignment;
- `mollie-compiler/virtual_tables` logs every trait impl compiled into a vtable.

To see them, enable them in the host's subscriber, for example `RUST_LOG=mollie-compiler=trace` with an `EnvFilter`.

## Tests

Most crates have unit tests. Type checking has its own tests in `crates/typed-ast/tests`, and code generation and the runtime in `crates/compiler/tests`. A few of them are worth knowing about:

- `crates/compiler/tests/compiler/fuzz.rs` compiles and runs random and mutated programs with tight limits. The compiler must never panic or hang on them.
- `crates/compiler/tests/compiler/gc.rs` runs programs with a collection at every allocation, which catches references missing from stack maps.
- `crates/compiler/tests/compiler/determinism.rs` checks that results, traps and fuel use are the same across runs.
- `tests/host` tests the host API: isolation between programs, value types, callbacks, trait objects, threads and stubs.

The fuzz tests run 50 programs per generator. Set `MOLLIE_FUZZ_ITERS` to run more, such as 100 000 before a release. If a program runs for over 10 seconds, a watchdog prints it, since that's probably a hang.

## Examples

`examples/ui.rs` draws a UI from a script through `Drawable`. `ui run stress` runs the GC and performance test, and `ui stub` writes the stub.

`examples/reload.rs` reloads a program while keeping the host's state.
