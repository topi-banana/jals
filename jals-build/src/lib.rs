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
//! that match a manifest's `[toolchain]` enums to the right boxed backend, one per step.
//!
//! **The seam a future embedded or wasm compiler implements is [`Backend`], in the ungated
//! [`backend`] module.** It takes a frontend's lowered tree — `(path, CacheKey)` pairs — rather
//! than host paths, so it carries no `std::path` and compiles for `wasm32` unconditionally.
//!
//! [`Compiler`] / [`Runtime`] / [`CompileRequest`] / [`RunRequest`] / [`ToolResolver`] /
//! [`BuiltinToolchain`] are **not** part of that portable core, despite what this doc used to
//! claim: every one of them is `native`-gated and built on `std::path::PathBuf`. They remain the
//! `javac`/`java` invocation layer, which the host backend adapter drives *beneath* [`Backend`]
//! after materializing the lowered tree. Build with `--no-default-features` to see exactly what
//! is portable.

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
    BackendRequest, BackendSelection,
};
#[cfg(feature = "native")]
pub use builtin::BuiltinToolchain;
pub use clean::CleanTargets;
pub use init::{InitOptions, ScaffoldFile};
#[cfg(feature = "native")]
pub use invocation::{Invocation, MAX_COMMAND_LINE_BYTES};
#[cfg(feature = "native")]
pub use manifest_ext::{ManifestError, ManifestExt};
#[cfg(feature = "native")]
pub use request::{CompileRequest, RunRequest};
#[cfg(feature = "native")]
pub use staging::{FRONTEND_OUT_DIR, StagedTree};
pub use target::{ResolveTargetError, RunTarget};
#[cfg(feature = "native")]
pub use toolchain::{
    BuildOutcome, Candidates, Compiler, JdkInstall, Runtime, Tool, ToolResolver, ToolchainError,
    ToolchainFuture,
};

#[cfg(feature = "native")]
pub use native::SubprocessToolchain;
