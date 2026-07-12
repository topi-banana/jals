//! Workspace automation tasks (the `cargo xtask` pattern), run as
//! `cargo run -p xtask -- <task>`.
//!
//! Tasks:
//! - `codegen [--check]`: regenerate `jals-syntax/src/ast/generated.rs` from
//!   `jals-syntax/java.ungram` (`--check` verifies the committed file instead).

mod codegen;

use std::process::ExitCode;

fn main() -> ExitCode {
    fn usage(error: &str) -> ExitCode {
        eprintln!("error: {error}");
        eprintln!("usage: cargo run -p xtask -- codegen [--check]");
        ExitCode::FAILURE
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((task, rest)) if task == "codegen" => {
            let mut check = false;
            for arg in rest {
                match arg.as_str() {
                    "--check" => check = true,
                    other => return usage(&format!("unknown argument `{other}`")),
                }
            }
            match codegen::Codegen::run(check) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("error: {err:#}");
                    ExitCode::FAILURE
                }
            }
        }
        Some((task, _)) => usage(&format!("unknown task `{task}`")),
        None => usage("missing task"),
    }
}
