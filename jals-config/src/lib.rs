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
//! models, the `load` / `discover` operations over an immutable [`jals_storage::ProjectView`], the
//! `FromStr` / `validate` entry points, and the shared [`ConfigError`]. It is a single dependency a
//! future configuration-file language server can build on. Everything here is pure and `no_std`
//! (`alloc` only), so it stays `wasm32`-compatible for the browser playground.
//!
//! The *behavior* that consumes a config stays in the owning crate: `jals-fmt` formats with an
//! [`fmt::Config`], `jals-lint` lints with a [`lint::Config`], and `jals-build`'s host-only
//! `ManifestExt` (`std::path`-based classpath / invocation / scaffold resolution) extends
//! [`Manifest`].
//!
//! One thing here is not a file schema: the **severity vocabulary**. The configured
//! [`LintLevel`](lint::LintLevel) has always lived here, and the presented [`DiagnosticSeverity`]
//! joins it, so that a crate which produces diagnostics can state how they present without
//! depending on an editor. `jals-editor` and `jals-project` both produce diagnostics and neither
//! depends on the other; this is the only crate they share.

extern crate alloc;

mod diagnostic;
mod loader;

pub mod fmt;
pub mod lint;
pub mod manifest;
pub mod toolchain;

pub use diagnostic::DiagnosticSeverity;
pub use lint::{Category, LintLevel};
pub use loader::{ConfigError, DiscoverableConfig};
pub use manifest::MANAGED_REMAP_ROOT;
pub use manifest::{
    AmbiguousMapping, BackendKind, Bin, Build, BuildFeatureError, BuildRemap, BuildResources,
    BuildScript, Dependency, DependencyError, Feature, FeatureRefError, FeatureSet, FileMappings,
    FrontendKind, GitDependency, GitRef, JarDependency, Manifest, ManifestParseError,
    MappingDigest, MappingEntry, MappingError, MappingFormatKind, MappingSource, Package,
    PathDependency, RemapSite, ResolvedBuildFeatures, ResourcePattern, ResourcePatternError, Run,
    UrlMappings, ValidationError,
};
pub use toolchain::{Compiler, Distribution, Runtime, ToolSpec, Toolchain};
