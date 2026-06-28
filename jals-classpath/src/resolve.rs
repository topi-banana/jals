//! Host-side resolution of a project's `[dependencies]` into local `.jar` paths.
//!
//! [`jals_build::Manifest::dependency_sources`] classifies each `[dependencies]` entry — purely, no
//! I/O — into a [`DependencySource`]: a local [`Path`](DependencySource::Path) (from a `file://` URL
//! or a bare path) or a remote [`Url`](DependencySource::Url). This module does the host I/O that the
//! pure manifest layer cannot: it confirms local jars exist and **downloads** remote ones into a
//! cache directory, yielding a list of local `.jar` paths the caller appends to the classpath before
//! handing it to [`crate::load_classpath`] (which actually unzips and parses them — keeping a single
//! read path for `.class` bytes).
//!
//! Like [`crate::load_classpath`], resolution is **error-resilient**: a failed download or a missing
//! local jar becomes a [`Warning`] and is skipped, never aborting — a project should still get
//! analysis from the dependencies that *did* resolve.
//!
//! The downloader uses `reqwest`'s **blocking** client, which must not run inside a Tokio runtime
//! (it would panic spinning up its own). `jals-cli` is synchronous and calls this directly; the
//! Tokio-based `jals-lsp` must call it from a dedicated `std::thread`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use jals_build::{DependencySource, Manifest};

use crate::Warning;

/// The outcome of resolving `[dependencies]`: the local `.jar` paths to add to the classpath, plus
/// any non-fatal [`Warning`]s for sources that could not be resolved.
#[derive(Debug, Default)]
pub struct ResolvedDependencies {
    /// Local `.jar` paths (downloaded remotes and confirmed local files), in dependency order. Feed
    /// these to [`crate::load_classpath`] alongside the manifest's own classpath entries.
    pub jars: Vec<PathBuf>,
    /// One per dependency that could not be resolved (a download failure, a missing local jar).
    pub warnings: Vec<Warning>,
}

/// Resolve classified dependency `sources` to local `.jar` paths, downloading remote ones into
/// `cache_dir`.
///
/// - A [`DependencySource::Path`] is confirmed to exist and pushed verbatim (it is *not* read here —
///   [`crate::load_classpath`] unzips/parses it later, the single `.class`-reading path); a missing
///   one is a [`Warning`].
/// - A [`DependencySource::Url`] is downloaded into `cache_dir` under a name derived from the
///   dependency name and a hash of the URL, **skipping the download when a non-empty cached file
///   already exists**. A request/IO failure is a [`Warning`].
///
/// Never panics and never aborts; the caller decides whether to surface the warnings.
pub fn resolve_dependencies(
    sources: &[(String, DependencySource)],
    cache_dir: &Path,
) -> ResolvedDependencies {
    let mut resolved = ResolvedDependencies::default();
    // A single blocking client, reused across downloads. Building it is cheap and infallible-by-
    // default; only the requests below can fail.
    let client = reqwest::blocking::Client::new();
    for (name, source) in sources {
        match source {
            DependencySource::Path(path) => {
                if path.is_file() {
                    resolved.jars.push(path.clone());
                } else {
                    resolved.warn(path, "dependency jar does not exist");
                }
            }
            DependencySource::Url(url) => {
                let dest = cached_jar_path(name, url, cache_dir);
                match download(&client, url, &dest) {
                    Ok(()) => resolved.jars.push(dest),
                    Err(message) => resolved.warn(Path::new(url), &message),
                }
            }
        }
    }
    resolved
}

/// Resolve a project's `[dependencies]` to local jar paths, the host-side end-to-end orchestration
/// both `jals-cli` and `jals-lsp` need.
///
/// Classifies each entry ([`Manifest::dependency_sources`]), then [`resolve_dependencies`] confirms
/// the local jars and downloads the remotes into `<root>/target/jals/deps` (a cache; `target/` is
/// already build output). Every classification error and resolution [`Warning`] is reported through
/// `warn` — the caller supplies the sink and message prefix (e.g. `jals-cli` prints to stderr,
/// `jals-lsp` prefixes `jals-lsp:`). Returns the local jars to append to the classpath.
///
/// Best-effort like [`load_classpath`](crate::load_classpath): a bad spec or a failed download is
/// warned and skipped, never fatal. Synchronous — it uses `reqwest`'s blocking client, which must not
/// run inside a Tokio runtime, so an async host (e.g. `jals-lsp`) must call this on a dedicated thread.
pub fn resolve_project_dependencies(
    manifest: &Manifest,
    root: &Path,
    mut warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let (sources, errors) = manifest.dependency_sources(root);
    // Classification errors are normally caught earlier by `Manifest::validate`; surface any that
    // reach here (e.g. a manifest parsed without validation) rather than dropping them.
    for error in errors {
        warn(error.to_string());
    }
    let cache_dir = root.join("target/jals/deps");
    let resolved = resolve_dependencies(&sources, &cache_dir);
    for warning in resolved.warnings {
        warn(format!("{}: {}", warning.path.display(), warning.message));
    }
    resolved.jars
}

/// The cache path a remote dependency downloads to: `<cache_dir>/<name>-<url-hash>.jar`.
///
/// Combining the human-readable dependency name with a hash of the URL keeps filenames legible while
/// disambiguating two URLs that share a name (and avoiding a stale cache silently serving the wrong
/// jar). The hash is [`DefaultHasher`] (fixed-keyed, so it is stable across runs) — collision
/// resistance is not security-critical here, only disambiguation. Public so tests can pre-seed the
/// cache and exercise the skip-if-exists path without touching the network.
pub fn cached_jar_path(name: &str, url: &str, cache_dir: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    cache_dir.join(format!("{name}-{:016x}.jar", hasher.finish()))
}

/// Download `url` to `dest`, returning a human-readable message on failure.
///
/// Skips the download when `dest` already exists and is non-empty (an immutable-URL cache). Writes to
/// a `.part` sibling first and renames into place, so an interrupted download never leaves a
/// truncated file that a later run would mistake for a valid cache hit.
fn download(client: &reqwest::blocking::Client, url: &str, dest: &Path) -> Result<(), String> {
    if dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(()); // cache hit
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating cache dir {}: {e}", parent.display()))?;
    }
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("downloading {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("downloading {url}: {e}"))?;
    let bytes = response
        .bytes()
        .map_err(|e| format!("reading response from {url}: {e}"))?;
    let tmp = dest.with_extension("jar.part");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("writing cache {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("finalizing cache {}: {e}", dest.display()))?;
    Ok(())
}

impl ResolvedDependencies {
    fn warn(&mut self, path: &Path, message: &str) {
        self.warnings.push(Warning::new(path, message));
    }
}
