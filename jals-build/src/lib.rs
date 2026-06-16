//! Cargo-style build orchestration for Java projects.
//!
//! A [`Manifest`] is a `jals.toml` project manifest, the Java analogue of `Cargo.toml`: it says
//! where the sources live, where compiled classes go, which Java release to target, and what is on
//! the classpath. [`build_invocation`] and [`run_invocation`] turn a manifest plus already-resolved
//! inputs into an [`Invocation`] — a program name and an argument vector for `javac`/`java`.
//!
//! Everything here is pure: it never spawns a process, mirroring `jals-fmt`/`jals-lint`. `jals-cli`
//! owns the process and directory-walking I/O and feeds the discovered source list back in. Keeping
//! the command-building pure makes it deterministic and unit-testable with no JDK installed, and
//! keeps the crate `wasm32`-compatible.

mod invocation;
mod manifest;

pub use invocation::{Invocation, build_invocation, run_invocation};
pub use manifest::{Build, Manifest, ManifestError, Package, Run};
