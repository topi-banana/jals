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
//! Everything here is pure: it never spawns a process or touches the filesystem, mirroring
//! `jals-fmt`/`jals-lint`. `jals-cli` owns the process and directory-walking I/O and feeds the
//! discovered source list back in (and writes the scaffold files, and removes the clean paths).
//! Keeping this logic pure makes it deterministic and unit-testable with no JDK installed, and keeps
//! the crate `wasm32`-compatible.

mod clean;
mod init;
mod invocation;
mod manifest_ext;
mod target;

pub use clean::CleanTargets;
pub use init::{InitOptions, ScaffoldFile};
pub use invocation::Invocation;
pub use manifest_ext::{
    DependencySource, GitSource, ManifestError, ManifestExt, PathSource, SourceDependency,
};
pub use target::{ResolveTargetError, RunTarget};
