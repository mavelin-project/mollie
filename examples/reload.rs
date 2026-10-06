//! Hot reloading: the script `examples/reload.mol` is compiled again whenever
//! it changes, while the state kept by the host survives.
//!
//! ```sh
//! cargo run --example reload
//! ```
//!
//! Every new version is a new program, with its own items: it can redeclare
//! structs and functions of the previous one. The host keeps its state in its
//! own values (an `i32` here), so new versions start from it.

use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, SystemTime},
};

use mollie::{
    MolStr,
    compiler::{Compiler, sandbox::Limits},
    host::{CompilerExt, Host},
    typed_ast::FileModuleLoader,
};

fn main() {
    let examples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let path = examples_dir.join("reload.mol");
    let mut compiler = Compiler::with_symbols(
        FileModuleLoader {
            current_dir: examples_dir.clone(),
        },
        [],
    )
    .unwrap_or_else(|error| panic!("can't create the compiler: {error}"));

    Host::new(&mut compiler).function_named("log", &["message"], |message: MolStr| println!("[script] {message}"));

    let mut modified: Option<SystemTime> = None;
    let mut loaded = false;
    let mut state = 0;

    println!("edit {} to change the script, Ctrl+C to stop", path.display());

    for frame in 0.. {
        let current = fs::metadata(&path).and_then(|metadata| metadata.modified()).ok();

        if current != modified {
            modified = current;

            let source = fs::read_to_string(&path).unwrap_or_default();

            match compiler.compile_script::<(i32, i32), i32>("update", &["state", "frame"], &source) {
                Ok(()) => {
                    loaded = true;

                    println!("loaded a new version");
                }
                // The previous version keeps running until the script is fixed.
                Err(error) => eprintln!("{}", error.display(&compiler.type_context.tcx)),
            }
        }

        if loaded {
            // A script stuck in a loop can't freeze the host.
            let limits = Limits {
                fuel: Some(1_000_000),
                ..Limits::default()
            };
            let update = compiler
                .script_fn::<(i32, i32), i32>("update")
                .unwrap_or_else(|error| panic!("`update` must be compiled: {error}"));

            match update.call((state, frame), limits) {
                Ok(new_state) => state = new_state,
                Err(trap) => eprintln!("frame {frame}: the script stopped: {trap}"),
            }
        }

        thread::sleep(Duration::from_millis(500));
    }
}
