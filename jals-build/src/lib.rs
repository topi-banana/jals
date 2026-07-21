//! Cargo-style build orchestration for Java projects.
//!
//! A [`Manifest`](jals_config::Manifest) is a `jals.toml` project manifest, the Java analogue of `Cargo.toml`: it says
//! where the sources live, where compiled classes go, which Java release to target, and what is on
//! the classpath. [`Invocation::build`] and [`Invocation::run`] turn a manifest plus already-resolved
//! inputs into an [`Invocation`] — a program name and an argument vector for `javac`/`java`.
//! [`CleanTargets::keys`] resolves the build artifacts to delete (for `jals clean`).
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
//! implementing both traits — today a dummy that copies sources through [`jals_storage::ProjectStorage`]
//! instead of compiling, the seam a real embedded compiler fills) is what a future wasm compiler
//! would implement instead; build the crate with `--no-default-features` for that `wasm32`-only
//! core.

#![cfg_attr(not(feature = "native"), no_std)]
#[cfg(feature = "rhai")]
pub mod build_script;
#[cfg(feature = "native")]
mod builtin;
mod clean;
mod init;
#[cfg(feature = "native")]
mod invocation;
#[cfg(feature = "native")]
mod manifest_ext;
#[cfg(feature = "native")]
mod request;
mod target;
#[cfg(feature = "rhai")]
pub mod task;
#[cfg(feature = "native")]
mod toolchain;

#[cfg(feature = "native")]
mod native;

#[cfg(feature = "native")]
pub use builtin::BuiltinToolchain;
pub use clean::CleanTargets;
pub use init::{InitOptions, ScaffoldFile};
#[cfg(feature = "native")]
pub use invocation::Invocation;
#[cfg(feature = "native")]
pub use manifest_ext::{ManifestError, ManifestExt};
#[cfg(feature = "native")]
pub use request::{CompileRequest, RunRequest};
pub use target::{ResolveTargetError, RunTarget};
#[cfg(feature = "native")]
pub use toolchain::{
    BuildOutcome, Candidates, Compiler, JdkInstall, Runtime, Tool, ToolResolver, ToolchainError,
    ToolchainFuture,
};

#[cfg(feature = "native")]
pub use native::SubprocessToolchain;
