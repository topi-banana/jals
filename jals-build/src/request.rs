//! Tool-agnostic build/run inputs.
//!
//! A [`CompileRequest`] / [`RunRequest`] bundles the already-resolved inputs a build or run needs —
//! the sources, the classpath, the release target, the entry point — independent of *how* those get
//! realized. A backend ([`Compiler`](crate::Compiler) / [`Runtime`](crate::Runtime)) consumes a
//! request and either plans a subprocess command ([`Invocation`](crate::Invocation), the native
//! path) or, in a future backend, drives an in-process compiler with the same inputs. Keeping the
//! inputs in one struct is what lets the same request feed a `javac` subprocess today and a wasm
//! compiler later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jals_config::Manifest;

/// The resolved inputs for compiling a project.
///
/// The host assembles this from the discovered manifest, source list, and resolved dependencies; a
/// [`Compiler`](crate::Compiler) turns it into an actual compilation. All paths are already
/// resolved (absolute) by the host.
pub struct CompileRequest<'a> {
    /// The project manifest (supplies `[build]` release/classpath/source-dirs/flags).
    pub manifest: &'a Manifest,
    /// The project root, against which the manifest's relative paths are resolved.
    pub project_root: &'a Path,
    /// The project's own `.java` sources to compile, in order.
    pub sources: &'a [PathBuf],
    /// Extra already-resolved `.java` sources compiled alongside `sources` (the `git`/`path` source
    /// dependencies), appended after them.
    pub extra_sources: &'a [PathBuf],
    /// Extra already-resolved classpath entries (the downloaded/local dependency jars), appended
    /// after the manifest's `[build] classpath`.
    pub extra_classpath: &'a [PathBuf],
    /// Extra `javac` arguments, appended after the manifest's `javac_flags` and before sources.
    pub extra_javac_args: &'a [String],
    /// Explicit environment entries for the compiler subprocess.
    pub compile_env: &'a BTreeMap<String, String>,
}

/// The resolved inputs for running a project's main class.
pub struct RunRequest<'a> {
    /// The project manifest (supplies `[build] classes-dir` and `classpath`).
    pub manifest: &'a Manifest,
    /// The project root, against which the manifest's relative paths are resolved.
    pub project_root: &'a Path,
    /// Extra JVM arguments passed before the classpath option.
    pub jvm_args: &'a [String],
    /// The fully-qualified main class to run.
    pub main_class: &'a str,
    /// Arguments passed to the program after the main class.
    pub program_args: &'a [String],
    /// Extra already-resolved classpath entries (the dependency jars), appended after the manifest's
    /// classpath.
    pub extra_classpath: &'a [PathBuf],
    /// Explicit environment entries for the runtime subprocess.
    pub run_env: &'a BTreeMap<String, String>,
}
