#![cfg_attr(not(test), no_std)]
//! The configuration data models for a `jals` project, in one place.
//!
//! `jals` reads three TOML configuration files, each historically owned by a different crate:
//!
//! - `jals.toml` — the project [`Manifest`] (the Java analogue of `Cargo.toml`).
//! - `jalsfmt.toml` — the formatter [`fmt::Config`].
//! - `jalslint.toml` — the linter [`lint::Config`].
//!
//! This crate owns the **schema, parsing, discovery, and validation** for all three: the serde data
//! models, the `from_file` / `discover` loaders (read through a [`jals_fs::FileTree`]), the
//! `FromStr` / `validate` entry points, and the shared [`ConfigError`]. It is a single dependency a
//! future configuration-file language server can build on. Everything here is pure and `no_std`
//! (`alloc` only), so it stays `wasm32`-compatible for the browser playground.
//!
//! The *behavior* that consumes a config stays in the owning crate: `jals-fmt` formats with an
//! [`fmt::Config`], `jals-lint` lints with a [`lint::Config`], and `jals-build`'s host-only
//! `ManifestExt` (`std::path`-based classpath / invocation / scaffold resolution) extends
//! [`Manifest`].

extern crate alloc;

mod loader;

pub mod fmt;
pub mod lint;
pub mod manifest;
pub mod toolchain;

pub use lint::Severity;
pub use loader::{ConfigError, DiscoverableConfig};
pub use manifest::{
    Bin, Build, Dependency, DependencyError, Feature, FeatureSet, GitDependency, GitRef,
    JarDependency, Manifest, ManifestParseError, Package, PathDependency, Run, ValidationError,
};
pub use toolchain::{ToolSpec, Toolchain};
