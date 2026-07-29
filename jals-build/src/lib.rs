//! Cargo-style build orchestration for Java projects.
//!
//! A [`Manifest`](jals_config::Manifest) is a `jals.toml` project manifest, the Java analogue of `Cargo.toml`: it says
//! where the sources live, where compiled classes go, which Java release to target, and what is on
//! the classpath. `Invocation::build` and `Invocation::run` turn a manifest plus already-resolved
//! inputs into an `Invocation` — a program name and an argument vector for `javac`/`java`.
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
//! discover installed JDKs — plus the `<dyn Runtime>::select` factory that matches a manifest's
//! `[toolchain] runtime` enum to the right boxed backend for the run step.
//!
//! **What compiles a project is [`Backend`], in the ungated [`backend`] module.** It takes a
//! frontend's lowered tree — `(path, CacheKey)` pairs — rather than host paths, so the contract
//! carries no `std::path` and compiles for `wasm32` unconditionally. Two adapters implement it:
//! the portable [`JalsBackend`] and, under `native`, [`JavacBackend`]. A host picks one by calling
//! [`BackendSelection`] once — [`in_process`](BackendSelection::in_process) in a browser,
//! [`for_host`](BackendSelection::for_host) on a machine with a JDK — and never matches on
//! `[build] backend` itself.
//!
//! [`Runtime`] / [`RunRequest`] / [`BuiltinToolchain`] and the crate-internal `Compiler` /
//! `CompileRequest` / `ToolResolver` are **not** part of that portable core: every one of them is
//! `native`-gated and built on `std::path::PathBuf`. They are the `javac`/`java` invocation layer,
//! which [`JavacBackend`] drives *beneath* [`Backend`] once the lowered tree is materialized. Build
//! with `--no-default-features` to see exactly what is portable.

#![cfg_attr(not(feature = "native"), no_std)]

extern crate alloc;

pub mod backend;
#[cfg(feature = "rhai")]
pub mod build_script;
#[cfg(feature = "native")]
mod builtin;
mod clean;
mod init;
#[cfg(feature = "native")]
mod invocation;
mod jals_backend;
#[cfg(feature = "native")]
mod javac_backend;
#[cfg(feature = "native")]
mod manifest_ext;
#[cfg(feature = "native")]
mod request;
#[cfg(feature = "native")]
mod staging;
mod target;
#[cfg(feature = "rhai")]
pub mod task;
#[cfg(feature = "native")]
mod toolchain;

#[cfg(feature = "native")]
mod native;

pub use backend::{
    Backend, BackendAbsence, BackendError, BackendFuture, BackendOptions, BackendOutcome,
    BackendRequest, BackendSelection, BackendSource,
};
#[cfg(feature = "native")]
pub use builtin::BuiltinToolchain;
pub use clean::CleanTargets;
pub use init::{InitOptions, ScaffoldFile};
pub use jals_backend::JalsBackend;
#[cfg(feature = "native")]
pub use javac_backend::{HostCompileInputs, JavacBackend};
#[cfg(feature = "native")]
pub use manifest_ext::{ManifestError, ManifestExt};
// Only `RunRequest` is exported. `CompileRequest` is crate-internal along with the `Compiler` trait
// it feeds — hosts assemble a portable `BackendRequest` plus `HostCompileInputs` now, and the
// `javac` adapter turns those into the compile request.
#[cfg(feature = "native")]
pub use request::RunRequest;
#[cfg(feature = "native")]
pub use staging::{FRONTEND_OUT_DIR, StagedTree};
pub use target::{ResolveTargetError, RunTarget};
#[cfg(feature = "native")]
pub(crate) use toolchain::Candidates;
#[cfg(feature = "native")]
pub use toolchain::{BuildOutcome, JdkInstall, Runtime, ToolchainError, ToolchainFuture};

#[cfg(feature = "native")]
pub use native::SubprocessToolchain;
