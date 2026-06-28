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

use std::path::{Path, PathBuf};
use std::process::Command;

use jals_build::{DependencySource, GitRef, GitSource, Manifest, PathSource, SourceDependency};
use walkdir::WalkDir;

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

/// Resolve a project's `[dependencies]` **sources** jars (the optional `sources = "..."` of each entry)
/// and extract their `.java` files, returning the extracted file paths the host registers for
/// go-to-definition into library source. The sources counterpart of
/// [`resolve_project_dependencies`].
///
/// Classifies each `sources` spec ([`Manifest::dependency_source_jars`]), resolves the local/remote
/// jars into `<root>/target/jals/deps` (reusing [`resolve_dependencies`]), then
/// [`extract_sources`](crate::extract_sources) inflates their `.java` into
/// `<root>/target/jals/deps/sources`. Every classification error and resolution/extraction [`Warning`]
/// is reported through `warn`. Sources are an editor-navigation aid only — never a compile or analysis
/// input — so a project with no `sources` (the common case) does no work and no network I/O.
///
/// Best-effort and synchronous, exactly like [`resolve_project_dependencies`]: a failed download or a
/// corrupt jar is warned and skipped, and because it uses `reqwest`'s blocking client an async host
/// (e.g. `jals-lsp`) must call this on a dedicated thread.
pub fn resolve_project_sources(
    manifest: &Manifest,
    root: &Path,
    mut warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let (sources, errors) = manifest.dependency_source_jars(root);
    for error in errors {
        warn(error.to_string());
    }
    if sources.is_empty() {
        return Vec::new();
    }
    let cache_dir = root.join("target/jals/deps");
    let resolved = resolve_dependencies(&sources, &cache_dir);
    for warning in resolved.warnings {
        warn(format!("{}: {}", warning.path.display(), warning.message));
    }
    let extraction = crate::extract_sources(&resolved.jars, &cache_dir.join("sources"));
    for warning in extraction.warnings {
        warn(format!("{}: {}", warning.path.display(), warning.message));
    }
    extraction.java_files
}

/// Resolve a project's **source-form** `[dependencies]` (`git` / `path`) to the `.java` files the host
/// indexes for analysis and go-to-definition. The source-tree counterpart of
/// [`resolve_project_sources`] (which handles `-sources.jar`s) and of
/// [`resolve_project_dependencies`] (which handles binary jars).
///
/// Classifies each source dependency ([`Manifest::dependency_source_dirs`]), then for each one locates a
/// directory of `.java`:
/// - a [`path`](SourceDependency::Path) dependency is read in place;
/// - a [`git`](SourceDependency::Git) dependency is cloned into `<root>/target/jals/deps/git` (a cache;
///   a clone is reused when its directory already exists, since a pinned ref is immutable) and the
///   requested `branch`/`tag`/`rev` is checked out.
///
/// Within each, the source root is the dependency's explicit `dir`, or (when absent) auto-detected
/// (`src/main/java` → `src` → the dependency root); every `*.java` under it is returned. These sources
/// are an editor analysis + navigation input only — never a compile input — so a project with no
/// `git`/`path` dependency (the common case) does no work.
///
/// Best-effort and synchronous: a missing path, a failed `git` clone/checkout (including `git` not
/// being installed), or a missing source directory is reported through `warn` and skipped, never
/// aborting. The `git` invocations are subprocesses (not `reqwest`), so unlike
/// [`resolve_project_dependencies`] this does not itself require a dedicated thread; an async host that
/// also resolves jars/sources alongside it should still keep the whole batch off the Tokio runtime.
pub fn resolve_project_source_deps(
    manifest: &Manifest,
    root: &Path,
    mut warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let (specs, errors) = manifest.dependency_source_dirs(root);
    for error in errors {
        warn(error.to_string());
    }
    if specs.is_empty() {
        return Vec::new();
    }
    let git_cache = root.join("target/jals/deps/git");
    let mut java_files = Vec::new();
    for (name, spec) in specs {
        let base = match spec {
            SourceDependency::Path(PathSource {
                root: dep_root,
                dir,
            }) => {
                if !dep_root.is_dir() {
                    warn(format!(
                        "{}: path dependency `{name}` directory does not exist",
                        dep_root.display()
                    ));
                    continue;
                }
                source_root(&dep_root, dir.as_deref())
            }
            SourceDependency::Git(GitSource {
                url,
                reference,
                dir,
            }) => {
                let dest = git_cache.join(git_subdir(&name, &url, &reference));
                if let Err(message) = clone_git(&url, &reference, &dest) {
                    warn(format!("{url}: git dependency `{name}`: {message}"));
                    continue;
                }
                source_root(&dest, dir.as_deref())
            }
        };
        if !base.is_dir() {
            warn(format!(
                "{}: source dependency `{name}` has no source directory",
                base.display()
            ));
            continue;
        }
        collect_java_files(&base, &mut java_files);
    }
    java_files
}

/// The per-dependency git checkout subdir: `<name>-<hash of (url, ref)>`. Hashing the ref alongside the
/// URL gives each `branch`/`tag`/`rev` its own immutable checkout, so switching a dependency's ref does
/// not collide with the previous one.
fn git_subdir(name: &str, url: &str, reference: &GitRef) -> String {
    format!("{name}-{}", crate::hash_hex((url, reference)))
}

/// Clone `url` into `dest` and check out `reference`, returning a human-readable message on failure.
///
/// Skips the work when `dest` already exists (a pinned ref's checkout is immutable, so the cache is
/// reused). Clones into a `.part` sibling first and renames into place, so an interrupted clone never
/// leaves a partial checkout a later run would mistake for a complete cache hit.
fn clone_git(url: &str, reference: &GitRef, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        return Ok(()); // cache hit
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating git cache dir {}: {e}", parent.display()))?;
    }
    let tmp = dest.with_extension("part");
    // A leftover `.part` from an interrupted run would make `git clone` refuse a non-empty target.
    let _ = std::fs::remove_dir_all(&tmp);

    let mut clone = Command::new("git");
    clone.arg("clone").arg("--quiet").arg(url).arg(&tmp);
    run_git(&mut clone)?;

    if let Some(name) = reference.checkout_arg() {
        let mut co = Command::new("git");
        co.arg("-C")
            .arg(&tmp)
            .arg("checkout")
            .arg("--quiet")
            .arg(name);
        run_git(&mut co).inspect_err(|_| {
            // Don't leave a clone parked at the wrong ref for a later cache hit.
            let _ = std::fs::remove_dir_all(&tmp);
        })?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("finalizing git clone {}: {e}", dest.display()))
}

/// Run a configured `git` command, mapping a non-zero exit (or a missing `git` binary) to a message.
fn run_git(cmd: &mut Command) -> Result<(), String> {
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git (is it installed?): {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("git failed: {}", stderr.trim()))
}

/// The `.java` source root within a dependency `base`: the explicit `dir` when given, else the first of
/// the conventional layouts that exists (`src/main/java`, then `src`), else `base` itself.
fn source_root(base: &Path, dir: Option<&str>) -> PathBuf {
    if let Some(dir) = dir {
        return base.join(dir);
    }
    for candidate in ["src/main/java", "src"] {
        let path = base.join(candidate);
        if path.is_dir() {
            return path;
        }
    }
    base.to_path_buf()
}

/// Append every `*.java` file under `root` (walked in sorted order, so the index is deterministic) to
/// `out`.
fn collect_java_files(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if entry.file_type().is_file() && crate::has_ext(path, "java") {
            out.push(path.to_path_buf());
        }
    }
}

/// The cache path a remote dependency downloads to: `<cache_dir>/<name>-<url-hash>.jar`.
///
/// Combining the human-readable dependency name with a hash of the URL keeps filenames legible while
/// disambiguating two URLs that share a name (and avoiding a stale cache silently serving the wrong
/// jar). Public so tests can pre-seed the cache and exercise the skip-if-exists path without touching
/// the network.
pub fn cached_jar_path(name: &str, url: &str, cache_dir: &Path) -> PathBuf {
    cache_dir.join(format!("{name}-{}.jar", crate::hash_hex(url)))
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
