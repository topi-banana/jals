//! Cargo-style build orchestration for Java projects.
//!
//! A [`Manifest`] is a `jals.toml` project manifest, the Java analogue of `Cargo.toml`: it says
//! where the sources live, where compiled classes go, which Java release to target, and what is on
//! the classpath. [`build_invocation`] and [`run_invocation`] turn a manifest plus already-resolved
//! inputs into an [`Invocation`] — a program name and an argument vector for `javac`/`java`.
//! [`scaffold`] goes the other way: it produces the files a brand-new project needs (for
//! `jals init`).
//!
//! Everything here is pure: it never spawns a process or touches the filesystem, mirroring
//! `jals-fmt`/`jals-lint`. `jals-cli` owns the process and directory-walking I/O and feeds the
//! discovered source list back in (and writes the scaffold files). Keeping this logic pure makes it
//! deterministic and unit-testable with no JDK installed, and keeps the crate `wasm32`-compatible.

mod init;
mod invocation;
mod manifest;

pub use init::{InitOptions, ScaffoldFile, scaffold};
pub use invocation::{Invocation, build_invocation, run_invocation};
pub use manifest::{Build, Manifest, ManifestError, Package, Run};
