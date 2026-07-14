//! Cargo-style build orchestration for Java projects.
//!
//! A [`Manifest`](jals_config::Manifest) is a `jals.toml` project manifest, the Java analogue of `Cargo.toml`: it says
//! where the sources live, where compiled classes go, which Java release to target, and what is on
//! the classpath. [`Invocation::build`] and [`Invocation::run`] turn a manifest plus already-resolved
//! inputs into an [`Invocation`] — a program name and an argument vector for `javac`/`java`.
//! [`CleanTargets::paths`] resolves the build artifacts to delete (for `jals clean`).
//! [`InitOptions::scaffold`] goes the other way: it produces the files a brand-new project needs (for
//! `jals init`). [`RunTarget::resolve`] picks which `main-class` `jals run` should execute, from a
//! manifest's `[[bin]]` entries, `[package] default-run`, and `[run] main-class`.
//!
//! The core is pure: it never spawns a process or touches the filesystem, mirroring
//! `jals-fmt`/`jals-lint`. `jals-cli` owns the process and directory-walking I/O and feeds the
//! discovered source list back in (and writes the scaffold files, and removes the clean paths).
//! Keeping this logic pure makes it deterministic and unit-testable with no JDK installed, and keeps
//! the crate `wasm32`-compatible.
//!
//! The one exception is the default-on **`native` feature**, which supplies the host
//! `SubprocessToolchain` — the only piece that spawns `javac`/`java` and probes the filesystem to
//! discover installed JDKs — plus the `<dyn Compiler>::select` / `<dyn Runtime>::select` factories
//! that match a manifest's `[toolchain]` enums to the right boxed backend, one per step. The pure
//! core (the [`Compiler`] / [`Runtime`] traits plus the [`CompileRequest`] / [`RunRequest`] inputs,
//! the filesystem-free [`ToolResolver`] policy, and the [`BuiltinToolchain`] in-process backend
//! implementing both traits — today a dummy that copies sources through a `jals_fs::FileTree`
//! instead of compiling, the seam a real embedded compiler fills) is what a future wasm compiler
//! would implement instead; build the crate with `--no-default-features` for that `wasm32`-only
//! core.

mod builtin;
mod clean;
mod init;
mod invocation;
mod manifest_ext;
mod request;
mod target;
mod toolchain;

#[cfg(feature = "native")]
mod native;

pub use builtin::BuiltinToolchain;
pub use clean::CleanTargets;
pub use init::{InitOptions, ScaffoldFile};
pub use invocation::Invocation;
pub use manifest_ext::{
    DependencySource, GitSource, ManifestError, ManifestExt, PathSource, SourceDependency,
};
pub use request::{CompileRequest, RunRequest};
pub use target::{ResolveTargetError, RunTarget};
pub use toolchain::{
    BuildOutcome, Candidates, Compiler, JdkInstall, Runtime, Tool, ToolResolver, ToolchainError,
};

#[cfg(feature = "native")]
pub use native::SubprocessToolchain;
