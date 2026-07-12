//! Classpath loading and `[dependencies]` resolution: turn the entries a `jals.toml` lists (jars,
//! directories of `.class` files, and remote/`git`/`path` dependencies) into the parsed
//! [`ClassFile`](jals_classfile::ClassFile)s and navigation `.java` that `jals-hir`'s classpath bridge
//! and the editors consume.
//!
//! The crate is split so the analysis logic runs anywhere:
//!
//! - The **core** ([`io`], [`load`], [`resolve`], `skeleton`) is pure and `wasm32`-compatible. It does
//!   all its filesystem access through a [`jals_fs::FileTree`] (reads/writes `/`-separated virtual
//!   paths; jars are unzipped from an in-memory `Cursor`, never a `std::fs::File`), and abstracts the
//!   two host-only capabilities — HTTP download and `git` — behind the [`Fetcher`] and [`Git`] traits.
//!   The only asynchronous step is [`Fetcher::fetch`], so the download orchestration is `async`.
//! - The **native facade** ([`native`], behind the default `native` feature) supplies the host
//!   implementations — a blocking `reqwest` [`Fetcher`], a subprocess [`Git`], and an
//!   [`OsFileTree`](jals_fs::OsFileTree) — and re-exports synchronous, `PathBuf`-based functions with
//!   the crate's historic signatures, so `jals-cli`/`jals-lsp` are unchanged. The browser playground
//!   uses the crate with `default-features = false` and drives the core with an
//!   [`InMemoryFileTree`](jals_fs::InMemoryFileTree) cache + a `fetch`-backed [`Fetcher`].
//!
//! `target/jals/deps` is used only as the native cache (downloads, git clones, extracted sources,
//! unpacked nested jars) and sits under `target/`, already build output; the browser holds the same
//! layout in an in-memory tree. Loading and resolution are **error-resilient**: an unreadable jar, a
//! corrupt `.class`, a failed download, or a missing entry is recorded as a [`Warning`] (or a `warn`
//! call) and skipped, never aborting.

mod io;
mod load;
mod project;
mod resolve;
mod skeleton;

#[cfg(feature = "native")]
mod native;

pub use io::{Fetcher, Git};
pub use load::{ClasspathLoad, JarExtraction};
pub use project::{ProjectInputOptions, ProjectInputsIn};
pub use resolve::DepsCache;
pub use skeleton::SkeletonGroup;

#[cfg(feature = "native")]
pub use native::{
    NestedJarsExtraction, ProjectInputs, ReqwestFetcher, ResolvedDependencies, SourcesExtraction,
    SubprocessGit,
};

/// A single classpath entry or member file that could not be loaded, or a dependency that could not be
/// resolved. Advisory only — the rest of the classpath still loads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// The path that failed: a classpath entry, a `.class`/`.java` member inside a jar, or a cache
    /// path — a `/`-separated virtual path (on a host, the real OS path string).
    pub path: String,
    /// A human-readable reason, suitable for a CLI/LSP diagnostic.
    pub message: String,
}

impl Warning {
    /// Build a [`Warning`] for `path` with `message`, owning both. The single construction site shared
    /// by the load (`load.rs`) and resolve (`resolve.rs`) halves of this crate.
    pub(crate) fn new(path: &str, message: &str) -> Self {
        Self {
            path: path.to_string(),
            message: message.to_string(),
        }
    }
}
