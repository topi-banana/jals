//! Workspace automation tasks (the `cargo xtask` pattern), run as
//! `cargo run -p xtask -- <task>`.
//!
//! Tasks:
//! - `codegen [--check]`: regenerate `jals-syntax/src/ast/generated.rs` from
//!   `jals-syntax/java.ungram` (`--check` verifies the committed file instead).
//! - `examples [--network <include|skip|only>] [--only <name>]... [--release]`: build and run
//!   every project under `examples/` the way its README documents.

mod codegen;
mod examples;

use std::process::ExitCode;

use examples::{Network, Options};

fn main() -> ExitCode {
    fn usage(error: &str) -> ExitCode {
        eprintln!("error: {error}");
        eprintln!("usage: cargo run -p xtask -- codegen [--check]");
        eprintln!(
            "       cargo run -p xtask -- examples [--network <include|skip|only>] \
             [--only <name>]... [--release]"
        );
        ExitCode::FAILURE
    }

    fn report(result: anyhow::Result<()>) -> ExitCode {
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err:#}");
                ExitCode::FAILURE
            }
        }
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
            report(codegen::Codegen::run(check))
        }
        Some((task, rest)) if task == "examples" => {
            let mut options = Options {
                network: Network::Include,
                only: Vec::new(),
                release: false,
            };
            let mut rest = rest.iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--release" => options.release = true,
                    "--network" => match rest.next().map(String::as_str) {
                        Some("include") => options.network = Network::Include,
                        Some("skip") => options.network = Network::Skip,
                        Some("only") => options.network = Network::Only,
                        Some(other) => {
                            return usage(&format!("unknown network policy `{other}`"));
                        }
                        None => return usage("`--network` needs include, skip, or only"),
                    },
                    "--only" => match rest.next() {
                        Some(name) => options.only.push(name.clone()),
                        None => return usage("`--only` needs an example name"),
                    },
                    other => return usage(&format!("unknown argument `{other}`")),
                }
            }
            report(examples::Examples::run(&options))
        }
        Some((task, _)) => usage(&format!("unknown task `{task}`")),
        None => usage("missing task"),
    }
}
