//! Cargo-style build orchestration for Java projects.
//!
//! A [`Manifest`] is a `jals.toml` project manifest, the Java analogue of `Cargo.toml`: it says
//! where the sources live, where compiled classes go, which Java release to target, and what is on
//! the classpath. [`build_invocation`] and [`run_invocation`] turn a manifest plus already-resolved
//! inputs into an [`Invocation`] — a program name and an argument vector for `javac`/`java`.
//! [`clean_paths`] resolves the build artifacts to delete (for `jals clean`). [`scaffold`] goes the
//! other way: it produces the files a brand-new project needs (for `jals init`).
//! [`resolve_run_target`] picks which `main-class` `jals run` should execute, from a manifest's
//! `[[bin]]` entries, `[package] default-run`, and `[run] main-class`.
//!
//! Everything here is pure: it never spawns a process or touches the filesystem, mirroring
//! `jals-fmt`/`jals-lint`. `jals-cli` owns the process and directory-walking I/O and feeds the
//! discovered source list back in (and writes the scaffold files, and removes the clean paths).
//! Keeping this logic pure makes it deterministic and unit-testable with no JDK installed, and keeps
//! the crate `wasm32`-compatible.

mod clean;
mod init;
mod invocation;
mod manifest;
mod target;

pub use clean::clean_paths;
pub use init::{InitOptions, ScaffoldFile, scaffold};
pub use invocation::{Invocation, build_invocation, run_invocation};
pub use manifest::{
    Bin, Build, Dependency, DependencyError, DependencySource, GitDependency, GitRef, GitSource,
    JarDependency, Manifest, ManifestError, Package, PathDependency, PathSource, Run,
    SourceDependency, ValidationError,
};
pub use target::{ResolveTargetError, resolve_run_target};
