//! The engine rungs: hand every emitted module to the specification's validator and to a real
//! WebAssembly engine.
//!
//! Nothing upstream of a validator can tell a well-formed module from a plausible one. The encoder
//! writes its own type indices, block types and local counts, and `Module::finish` encodes whatever
//! they say — so a body whose stack does not balance is bytes that round-trip perfectly and are
//! still a module no engine will load. `wasm-tools validate` is the specification's own answer, and
//! it is the authority.
//!
//! # Why the modules are instantiated as well
//!
//! Validation is a static judgement. Instantiating runs the **start function**, which is where the
//! lowering puts a Java `static` initialiser — so it is the cheapest way to execute the lowering of
//! every case that has one, with no entry point to choose and no arguments to invent.
//!
//! `wasmtime run --invoke <absent name>` is what asks for exactly that: the engine compiles the
//! module and instantiates it (running the start function) *before* it looks the export up, so a
//! `no func export named` reply is the proof that instantiation succeeded, and anything else is
//! the trap that stopped it. A wall-clock `-W timeout=` bounds it, because a corpus of compiler
//! tests contains initialisers that do not terminate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rayon::prelude::*;

use super::{CaseResult, Outcome, WASM_TOOLS_PIN, WASMTIME_PIN};

/// What one invocation of an exported function produced.
#[derive(Debug, Clone)]
pub(super) enum Invocation {
    /// The call returned. The string is what the engine printed, empty for a `void` result.
    Returned(String),
    /// The call trapped, or the engine refused to make it.
    Trapped(String),
}

/// Validates and runs emitted modules through the pinned external tools.
pub struct Engine {
    /// Scratch directory holding one `.wasm` per case, kept until the report is built so the
    /// agreement rung can invoke the same bytes the engine already accepted.
    staging: tempfile::TempDir,
    /// How long any one engine invocation may run.
    timeout: Duration,
}

impl Engine {
    /// A name whose absence is the signal that instantiation got all the way through.
    ///
    /// Deliberately not a name any Java method could have: the export lookup must fail for the
    /// reason this harness intends and never because a case happened to define it.
    const ABSENT_EXPORT: &'static str = "__jals_corpus_absent_export__";

    /// An engine staging into a fresh scratch directory.
    ///
    /// # Errors
    /// If the scratch directory cannot be created.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            staging: tempfile::tempdir().map_err(|e| format!("scratch directory: {e}"))?,
            timeout,
        })
    }

    /// Whether both pinned tools are on this host, saying so out loud when they are not.
    ///
    /// A quiet stand-down would let a run of nothing read as a clean sheet, and these two are the
    /// only authority on whether an emitted module is a module at all.
    pub fn available() -> bool {
        let tools = Self::tool("wasm-tools", WASM_TOOLS_PIN);
        // Not `&&`: a host missing both should hear about both.
        Self::tool("wasmtime", WASMTIME_PIN) && tools
    }

    /// Whether `name` runs here, and whether it is the release the report is defined against.
    fn tool(name: &str, pin: &str) -> bool {
        let Ok(output) = Command::new(name).arg("--version").output() else {
            eprintln!(
                "warning: no `{name}` on this host — the modules will be emitted and never judged,\n\
                 warning: and this is the only rung that proves one is a module at all."
            );
            return false;
        };
        // `wasm-tools 1.258.0 (5c6d31c78 2026-08-24)` — the release is the second field.
        let text = String::from_utf8_lossy(&output.stdout);
        let found = text.split_whitespace().nth(1).unwrap_or("?");
        if found != pin {
            eprintln!(
                "warning: {name} {found} is on PATH, but the report is defined against {pin};\n\
                 warning: this run's failure buckets are keyed on a different tool's wording"
            );
        }
        true
    }

    /// Where one case's module is staged, keyed by the same relative path the result carries.
    fn module_path(&self, rel: &Path) -> PathBuf {
        // One flat directory: a case path contains separators, so it becomes one file name.
        let slug: String = rel
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        self.staging.path().join(format!("{slug}.wasm"))
    }

    /// Stage, validate and instantiate every case that lowered.
    pub(super) fn judge_all(&self, results: &mut [CaseResult]) {
        results.par_iter_mut().for_each(|result| {
            if result.outcome != Outcome::Unvalidated {
                return;
            }
            let Some(bytes) = result.module.as_deref() else {
                return;
            };
            let path = self.module_path(&result.rel);
            if let Err(error) = std::fs::write(&path, bytes) {
                result.outcome = Outcome::Rejected(format!("could not stage the module: {error}"));
                return;
            }
            if let Some(message) = self.validate(&path) {
                result.outcome = Outcome::Rejected(message);
                return;
            }
            if let Some(message) = self.instantiate(&path) {
                result.outcome = Outcome::Trapped(message);
                return;
            }
            result.outcome = Outcome::NotRun;
        });
    }

    /// `Some(message)` when the specification's validator refuses the module.
    fn validate(&self, path: &Path) -> Option<String> {
        let output = Command::new("wasm-tools")
            .arg("validate")
            .arg(path)
            .output()
            .ok()?;
        if output.status.success() {
            return None;
        }
        Some(Self::flatten(&output.stderr))
    }

    /// `Some(message)` when the engine could not instantiate the module — the start function
    /// trapped, since a module that reaches the export lookup has already run it.
    fn instantiate(&self, path: &Path) -> Option<String> {
        let output = Command::new("wasmtime")
            .arg("run")
            .arg("-W")
            .arg(format!("timeout={}s", self.timeout.as_secs()))
            .args(["--invoke", Self::ABSENT_EXPORT])
            .arg(path)
            .output()
            .ok()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() || stderr.contains("no func export named") {
            return None;
        }
        Some(Self::flatten(&output.stderr))
    }

    /// Call one exported function on the staged module and report what came back.
    pub(super) fn invoke(&self, rel: &Path, name: &str, args: &[String]) -> Invocation {
        let output = Command::new("wasmtime")
            .arg("run")
            .arg("-W")
            .arg(format!("timeout={}s", self.timeout.as_secs()))
            .args(["--invoke", name])
            .arg(self.module_path(rel))
            .args(args)
            .output();
        match output {
            Err(error) => Invocation::Trapped(format!("could not run wasmtime: {error}")),
            Ok(output) if output.status.success() => {
                Invocation::Returned(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            }
            Ok(output) => Invocation::Trapped(Self::flatten(&output.stderr)),
        }
    }

    /// A tool's multi-line report as one line: the outermost message and the innermost cause.
    ///
    /// Both halves are needed and neither alone is enough — `error: func 1 failed to validate`
    /// says where and `type mismatch: …` says what — while the frames between them are a debugger's
    /// material rather than a table row's.
    fn flatten(stderr: &[u8]) -> String {
        let text = String::from_utf8_lossy(stderr);
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != "Caused by:");
        let head = lines.next().unwrap_or("(no message)").to_owned();
        let tail = lines.next_back().unwrap_or_default().to_owned();
        let joined = if tail.is_empty() || tail == head {
            head
        } else {
            format!("{head} — {tail}")
        };
        let joined = joined.replace(['\t', '\r'], " ");
        if joined.chars().count() > 300 {
            joined.chars().take(300).collect::<String>() + "…"
        } else {
            joined
        }
    }
}
