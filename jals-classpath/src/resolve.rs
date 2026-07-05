//! Host-agnostic resolution of a project's `[dependencies]`, routed through a [`jals_fs::FileTree`]
//! cache and the [`Fetcher`] / [`Git`] capability traits.
//!
//! [`jals_build::Manifest`] classifies each `[dependencies]` entry — purely, no I/O — into a
//! [`DependencySource`] (a local [`Path`](DependencySource::Path) or a remote
//! [`Url`](DependencySource::Url)) or a [`SourceDependency`] (`git`/`path`). This module does the
//! resolution the pure manifest layer cannot, but still without binding to a concrete host: it
//! confirms local jars exist and **downloads** remote ones (via an injected [`Fetcher`]) into a cache
//! directory on the [`FileTree`], and clones **`git`** source deps (via an injected [`Git`]). The
//! result is a list of `/`-separated virtual paths the caller feeds to [`load_classpath_in`] (jars) or
//! registers as navigation `.java` (sources / source deps).
//!
//! The only asynchronous step is [`Fetcher::fetch`]; everything else (unzip, tree writes, git) is
//! synchronous, so the three functions that download are `async` and the git/`path` resolver is not.
//! Resolution is **error-resilient**: a failed download / missing local jar / failed clone becomes a
//! [`Warning`] (or a `warn` call) and is skipped, never aborting.
//!
//! The [`native`](crate::native) module wraps these with `OsFileTree` + a blocking `reqwest`
//! [`Fetcher`] + a subprocess [`Git`], preserving the crate's historic `PathBuf` API; the browser
//! playground drives them with an `InMemoryFileTree` + a `fetch`-backed [`Fetcher`] and no `Git`.

use std::path::Path;

use jals_build::{DependencySource, GitSource, ManifestExt, PathSource, SourceDependency};
use jals_config::{GitRef, Manifest};
use jals_fs::{FileTree, path};

use crate::io::{Fetcher, Git};
use crate::load::{extract_nested_jars_in, extract_sources_in};
use crate::{Warning, hash_hex};

/// The project's dependency cache root, `<root>/target/jals/deps` — downloads, git clones, extracted
/// sources, and unpacked nested jars all live under it (`target/` is already build output). The single
/// definition of the cache layout.
pub(crate) fn deps_cache_dir(root: &str) -> String {
    path::join(root, "target/jals/deps")
}

/// The cache path a remote dependency downloads to: `<cache_dir>/<name>-<url-hash>.jar`.
///
/// Combining the human-readable dependency name with a hash of the URL keeps filenames legible while
/// disambiguating two URLs that share a name (and avoiding a stale cache silently serving the wrong
/// jar).
pub(crate) fn cached_jar_path_str(name: &str, url: &str, cache_dir: &str) -> String {
    path::join(cache_dir, &format!("{name}-{}.jar", hash_hex(url)))
}

/// A `PathBuf` rendered as a virtual `&str` path. On a host a virtual path *is* the OS path string,
/// so this is lossless there. Shared with the [`native`](crate::native) facade, which converts the
/// crate's historic `PathBuf` API to and from the core's virtual `&str` paths.
pub(crate) fn vpath(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The virtual cache path a classified dependency `source` resolves to: a
/// [`Path`](DependencySource::Path) verbatim, a [`Url`](DependencySource::Url) at its
/// [`cached_jar_path_str`] location. The single place mapping a source to its cache path, shared by
/// [`resolve_dependencies_in`] and the recursive bundled-jar pass.
fn source_jar_path(name: &str, source: &DependencySource, cache_dir: &str) -> String {
    match source {
        DependencySource::Path(p) => vpath(p),
        DependencySource::Url(url) => cached_jar_path_str(name, url, cache_dir),
    }
}

/// Download `url` to virtual path `dest` unless already cached (a non-existent `dest`; writes are
/// atomic, so "exists" implies "complete"). `fs` is borrowed only *after* the await, so no `&mut`
/// borrow is held across the fetch.
async fn ensure_cached_in<F: Fetcher>(
    fetcher: &F,
    fs: &mut dyn FileTree,
    url: &str,
    dest: &str,
) -> Result<(), String> {
    if fs.is_file(dest) {
        return Ok(()); // cache hit
    }
    let bytes = fetcher
        .fetch(url)
        .await
        .map_err(|e| format!("downloading {url}: {e}"))?;
    fs.write(dest, &bytes)
        .map_err(|e| format!("writing cache {dest}: {e}"))
}

/// Resolve classified dependency `sources` to virtual jar paths, downloading remote ones into
/// `cache_dir` via `fetcher`.
///
/// - A [`DependencySource::Path`] is confirmed to exist and pushed verbatim (it is not read here —
///   [`load_classpath_in`](crate::load_classpath_in) unzips/parses it later); a missing one is a
///   [`Warning`].
/// - A [`DependencySource::Url`] is downloaded into `cache_dir` (skip-if-exists). A fetch/write
///   failure is a [`Warning`].
pub async fn resolve_dependencies_in<F: Fetcher>(
    fetcher: &F,
    fs: &mut dyn FileTree,
    sources: &[(String, DependencySource)],
    cache_dir: &str,
) -> (Vec<String>, Vec<Warning>) {
    let mut jars = Vec::new();
    let mut warnings = Vec::new();
    for (name, source) in sources {
        let dest = source_jar_path(name, source, cache_dir);
        match source {
            DependencySource::Path(_) => {
                if fs.is_file(&dest) {
                    jars.push(dest);
                } else {
                    warnings.push(Warning::new(&dest, "dependency jar does not exist"));
                }
            }
            DependencySource::Url(url) => match ensure_cached_in(fetcher, fs, url, &dest).await {
                Ok(()) => jars.push(dest),
                Err(message) => warnings.push(Warning::new(url, &message)),
            },
        }
    }
    (jars, warnings)
}

/// Resolve a project's `[dependencies]` to virtual jar paths (the classpath jars to load), the
/// host-agnostic end-to-end orchestration `jals-cli` / `jals-lsp` / the playground need.
///
/// Classifies each entry ([`Manifest::dependency_sources`]), [`resolve_dependencies_in`] confirms the
/// local jars and downloads the remotes into `<root>/target/jals/deps`, then a second pass unpacks the
/// bundled jars of any `recursive = true` jar. Every classification error and resolution [`Warning`]
/// is reported through `warn`.
pub async fn resolve_project_dependencies_in<F: Fetcher>(
    fetcher: &F,
    fs: &mut dyn FileTree,
    manifest: &Manifest,
    root: &str,
    mut warn: impl FnMut(String),
) -> Vec<String> {
    let (sources, errors) = manifest.dependency_sources(Path::new(root));
    for error in errors {
        warn(error.to_string());
    }
    let cache_dir = deps_cache_dir(root);
    let (mut jars, warnings) = resolve_dependencies_in(fetcher, fs, &sources, &cache_dir).await;
    for warning in warnings {
        warn(format!("{}: {}", warning.path, warning.message));
    }

    // Second pass: for `recursive = true` jar deps, unpack their bundled (nested) jars and add them
    // too. The top-level jar is already resolved above; ask `source_jar_path` where it landed.
    let recursive = manifest.recursive_jar_dependencies();
    if !recursive.is_empty() {
        let nested_dir = path::join(&cache_dir, "nested");
        for (name, source) in &sources {
            if !recursive.contains(name.as_str()) {
                continue;
            }
            let jar_path = source_jar_path(name, source, &cache_dir);
            if !fs.is_file(&jar_path) {
                continue;
            }
            let (nested, warnings) = extract_nested_jars_in(fs, &jar_path, &nested_dir);
            for warning in warnings {
                warn(format!("{}: {}", warning.path, warning.message));
            }
            jars.extend(nested);
        }
    }
    jars
}

/// Resolve a project's `[dependencies]` **sources** jars and extract their `.java`, returning the
/// extracted virtual `.java` paths the host registers for go-to-definition into library source.
///
/// Classifies each `sources` spec ([`Manifest::dependency_source_jars`]), resolves the local/remote
/// jars into `<root>/target/jals/deps` (reusing [`resolve_dependencies_in`]), then
/// [`extract_sources_in`](crate::extract_sources_in) inflates their `.java` into
/// `<root>/target/jals/deps/sources`. A project with no `sources` (the common case) does no work.
pub async fn resolve_project_sources_in<F: Fetcher>(
    fetcher: &F,
    fs: &mut dyn FileTree,
    manifest: &Manifest,
    root: &str,
    mut warn: impl FnMut(String),
) -> Vec<String> {
    let (sources, errors) = manifest.dependency_source_jars(Path::new(root));
    for error in errors {
        warn(error.to_string());
    }
    if sources.is_empty() {
        return Vec::new();
    }
    let cache_dir = deps_cache_dir(root);
    let (jars, warnings) = resolve_dependencies_in(fetcher, fs, &sources, &cache_dir).await;
    for warning in warnings {
        warn(format!("{}: {}", warning.path, warning.message));
    }
    let sources_dir = path::join(&cache_dir, "sources");
    let (java_files, warnings) = extract_sources_in(fs, &jars, &sources_dir);
    for warning in warnings {
        warn(format!("{}: {}", warning.path, warning.message));
    }
    java_files
}

/// Resolve a project's **source-form** `[dependencies]` (`git` / `path`) to the virtual `.java` paths
/// the host indexes for analysis and go-to-definition (and, for the CLI, compiles alongside the
/// project's own sources).
///
/// Classifies each ([`Manifest::dependency_source_dirs`]), then for each locates a directory of
/// `.java`:
/// - a [`path`](SourceDependency::Path) dependency is read in place;
/// - a [`git`](SourceDependency::Git) dependency is cloned into `<root>/target/jals/deps/git` (via
///   `git`) and its `branch`/`tag`/`rev` checked out — when `git` is `None` (the browser), a `git`
///   dependency is warned and skipped.
///
/// Within each, the source root is the explicit `dir` or the first conventional layout that exists
/// (`src/main/java` → `src` → the dependency root); every `*.java` under it is returned.
pub fn resolve_project_source_deps_in(
    fs: &dyn FileTree,
    git: Option<&dyn Git>,
    manifest: &Manifest,
    root: &str,
    mut warn: impl FnMut(String),
) -> Vec<String> {
    let (specs, errors) = manifest.dependency_source_dirs(Path::new(root));
    for error in errors {
        warn(error.to_string());
    }
    if specs.is_empty() {
        return Vec::new();
    }
    let git_cache = path::join(&deps_cache_dir(root), "git");
    let mut java_files = Vec::new();
    for (name, spec) in specs {
        let base = match spec {
            SourceDependency::Path(PathSource {
                root: dep_root,
                dir,
            }) => {
                let dep_root = vpath(&dep_root);
                if !fs.is_dir(&dep_root) {
                    warn(format!(
                        "{dep_root}: path dependency `{name}` directory does not exist"
                    ));
                    continue;
                }
                source_root(fs, &dep_root, dir.as_deref())
            }
            SourceDependency::Git(GitSource {
                url,
                reference,
                dir,
            }) => {
                let Some(git) = git else {
                    warn(format!(
                        "git dependency `{name}` is not supported in this environment"
                    ));
                    continue;
                };
                let dest = path::join(&git_cache, &git_subdir(&name, &url, &reference));
                if let Err(message) = git.clone_checkout(&url, &reference, &dest) {
                    warn(format!("{url}: git dependency `{name}`: {message}"));
                    continue;
                }
                source_root(fs, &dest, dir.as_deref())
            }
        };
        if !fs.is_dir(&base) {
            warn(format!(
                "{base}: source dependency `{name}` has no source directory"
            ));
            continue;
        }
        // Every `*.java` under the source root, sorted by `walk_ext` so the index is deterministic.
        java_files.extend(fs.walk_ext(&base, "java").unwrap_or_default());
    }
    java_files
}

/// The per-dependency git checkout subdir: `<name>-<hash of (url, ref)>`. Hashing the ref alongside
/// the URL gives each `branch`/`tag`/`rev` its own immutable checkout.
fn git_subdir(name: &str, url: &str, reference: &GitRef) -> String {
    format!("{name}-{}", hash_hex((url, reference)))
}

/// The `.java` source root within a dependency `base`: the explicit `dir` when given, else the first
/// conventional layout that exists (`src/main/java`, then `src`), else `base` itself.
fn source_root(fs: &dyn FileTree, base: &str, dir: Option<&str>) -> String {
    if let Some(dir) = dir {
        return path::join(base, dir);
    }
    for candidate in ["src/main/java", "src"] {
        let candidate = path::join(base, candidate);
        if fs.is_dir(&candidate) {
            return candidate;
        }
    }
    base.to_string()
}
