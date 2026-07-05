//! The host (native) implementations of the [`Fetcher`] / [`Git`] capabilities, and the synchronous,
//! `PathBuf`-based facade over the wasm-compatible core.
//!
//! Behind the default `native` feature. The facade functions preserve `jals-classpath`'s historic
//! public API (`&Path`/`PathBuf` in, `PathBuf` out) so `jals-cli`/`jals-lsp` are unchanged: each wraps
//! an [`OsFileTree`] and, where a download can happen, a blocking [`ReqwestFetcher`] driven by
//! [`futures::executor::block_on`]. The blocking `reqwest` client must not run inside a Tokio runtime
//! (it would panic spinning up its own), so `jals-lsp` calls these from a dedicated `std::thread`;
//! `block_on` itself establishes no runtime, so the blocking client is safe under it.

use std::path::{Path, PathBuf};
use std::process::Command;

use futures::executor::block_on;
use jals_build::{DependencySource, ManifestExt};
use jals_classfile::ClassFile;
use jals_config::{GitRef, Manifest};
use jals_fs::OsFileTree;

use crate::Warning;
use crate::io::{Fetcher, Git};
use crate::load::{
    ClasspathLoad, extract_nested_jars_in, extract_sources_in, load_classpath_in,
    synthesize_classpath_sources_in,
};
use crate::project::{ProjectInputOptions, assemble_project_inputs_in};
use crate::resolve::{
    cached_jar_path_str, resolve_dependencies_in, resolve_project_dependencies_in,
    resolve_project_source_deps_in, resolve_project_sources_in, vpath,
};

// ---- Capability implementations -------------------------------------------------------------

/// A [`Fetcher`] backed by `reqwest`'s **blocking** client. Its `fetch` does blocking work and the
/// returned future resolves in a single poll, so a synchronous host drives it with `block_on` —
/// provided no Tokio runtime is active on the current thread (`reqwest::blocking` panics inside one).
pub struct ReqwestFetcher {
    client: reqwest::blocking::Client,
}

impl ReqwestFetcher {
    /// Build a fetcher with a fresh blocking client (cheap; reused across a resolution batch).
    pub fn new() -> Self {
        ReqwestFetcher {
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for ReqwestFetcher {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?;
        let bytes = response
            .bytes()
            .map_err(|e| format!("reading response: {e}"))?;
        Ok(bytes.to_vec())
    }
}

/// A [`Git`] that shells out to the `git` binary (`git clone` + `git checkout`), the host source-
/// dependency capability.
pub struct SubprocessGit;

impl Git for SubprocessGit {
    fn clone_checkout(&self, url: &str, reference: &GitRef, dest: &str) -> Result<(), String> {
        clone_git(url, reference, Path::new(dest))
    }
}

/// Clone `url` into `dest` and check out `reference`. Skips the work when `dest` already exists (a
/// pinned ref's checkout is immutable). Clones into a `.part` sibling and renames into place, so an
/// interrupted clone never leaves a partial checkout a later run mistakes for a cache hit.
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

// ---- PathBuf result structs (the historic public API) --------------------------------------

/// The outcome of resolving `[dependencies]`: local `.jar` paths + non-fatal [`Warning`]s.
#[derive(Debug, Default)]
pub struct ResolvedDependencies {
    /// Local `.jar` paths (downloaded remotes and confirmed local files), in dependency order.
    pub jars: Vec<PathBuf>,
    /// One per dependency that could not be resolved.
    pub warnings: Vec<Warning>,
}

/// The outcome of extracting dependency **sources** jars: the `.java` files written + [`Warning`]s.
#[derive(Debug, Default)]
pub struct SourcesExtraction {
    /// The extracted `.java` file paths on disk, in jar/archive order.
    pub java_files: Vec<PathBuf>,
    /// One per jar or member that could not be read/extracted.
    pub warnings: Vec<Warning>,
}

/// The outcome of recursively unpacking a jar's **bundled jars**: the nested `*.jar` files + warnings.
#[derive(Debug, Default)]
pub struct NestedJarsExtraction {
    /// The extracted nested jar paths on disk (at every depth).
    pub jars: Vec<PathBuf>,
    /// One per jar or member that could not be read/extracted.
    pub warnings: Vec<Warning>,
}

/// A project's assembled analysis / build inputs, on the real filesystem — the host `PathBuf`-based
/// form of [`ProjectInputsIn`](crate::ProjectInputsIn), with the manifest's source roots added.
/// Produced by [`assemble_project_inputs`]. Which fields are populated depends on the
/// [`ProjectInputOptions`] passed.
#[derive(Debug, Default)]
pub struct ProjectInputs {
    /// The project's `[build] source-dirs`, resolved against the manifest dir (from
    /// [`ManifestExt::source_roots`]). The `.java` roots the host walks.
    pub source_roots: Vec<PathBuf>,
    /// The resolved `[dependencies]` jar paths — `jals build`/`run`'s `javac -classpath` additions.
    pub dependency_jars: Vec<PathBuf>,
    /// The loaded classpath `.class` files, ready for `ProjectIndex::lower_classpath`. Empty unless
    /// classpath loading was requested.
    pub classpath_classes: Vec<ClassFile>,
    /// Navigation `.java`: extracted `-sources.jar` source then synthesized skeletons, in that order.
    pub library_sources: Vec<PathBuf>,
    /// The `git`/`path` source dependencies' `.java` — an index input and a `javac` source.
    pub source_dep_sources: Vec<PathBuf>,
    /// The project's target Java feature version from `[package] edition` (edition-rule gate).
    pub target_java_version: Option<u32>,
}

// ---- Path helpers ---------------------------------------------------------------------------

/// Every path in `ps` as a virtual path.
fn vpaths(ps: &[PathBuf]) -> Vec<String> {
    ps.iter().map(|p| vpath(p)).collect()
}

/// Virtual paths back into `PathBuf`s (on a host a virtual path *is* the OS path string).
fn to_pathbufs(ss: Vec<String>) -> Vec<PathBuf> {
    ss.into_iter().map(PathBuf::from).collect()
}

// ---- Facades (historic signatures) ----------------------------------------------------------

/// Load every `.class` file reachable from `entries` off the real filesystem. See
/// [`load_classpath_in`](crate::load_classpath_in).
pub fn load_classpath(entries: &[PathBuf]) -> ClasspathLoad {
    let fs = OsFileTree;
    load_classpath_in(&fs, &vpaths(entries))
}

/// The cache path a remote dependency downloads to: `<cache_dir>/<name>-<url-hash>.jar`. Public so
/// tests can pre-seed the cache and exercise the skip-if-exists path without the network.
pub fn cached_jar_path(name: &str, url: &str, cache_dir: &Path) -> PathBuf {
    PathBuf::from(cached_jar_path_str(name, url, &vpath(cache_dir)))
}

/// Resolve classified dependency `sources` to local `.jar` paths, downloading remote ones into
/// `cache_dir`. See [`resolve_dependencies_in`](crate::resolve_dependencies_in).
pub fn resolve_dependencies(
    sources: &[(String, DependencySource)],
    cache_dir: &Path,
) -> ResolvedDependencies {
    let mut fs = OsFileTree;
    let fetcher = ReqwestFetcher::new();
    let (jars, warnings) = block_on(resolve_dependencies_in(
        &fetcher,
        &mut fs,
        sources,
        &vpath(cache_dir),
    ));
    ResolvedDependencies {
        jars: to_pathbufs(jars),
        warnings,
    }
}

/// Extract every `*.java` member of each sources jar in `jars` into `dest_dir`. See
/// [`extract_sources_in`](crate::extract_sources_in).
pub fn extract_sources(jars: &[PathBuf], dest_dir: &Path) -> SourcesExtraction {
    let mut fs = OsFileTree;
    let (java_files, warnings) = extract_sources_in(&mut fs, &vpaths(jars), &vpath(dest_dir));
    SourcesExtraction {
        java_files: to_pathbufs(java_files),
        warnings,
    }
}

/// Recursively extract every **bundled jar** of `jar` into `dest_dir`. See
/// [`extract_nested_jars_in`](crate::extract_nested_jars_in).
pub fn extract_nested_jars(jar: &Path, dest_dir: &Path) -> NestedJarsExtraction {
    let mut fs = OsFileTree;
    let (jars, warnings) = extract_nested_jars_in(&mut fs, &vpath(jar), &vpath(dest_dir));
    NestedJarsExtraction {
        jars: to_pathbufs(jars),
        warnings,
    }
}

/// Resolve a project's `[dependencies]` to local jar paths (downloading remotes into
/// `<root>/target/jals/deps`). See [`resolve_project_dependencies_in`](crate::resolve_project_dependencies_in).
pub fn resolve_project_dependencies(
    manifest: &Manifest,
    root: &Path,
    warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let mut fs = OsFileTree;
    let fetcher = ReqwestFetcher::new();
    let jars = block_on(resolve_project_dependencies_in(
        &fetcher,
        &mut fs,
        manifest,
        &vpath(root),
        warn,
    ));
    to_pathbufs(jars)
}

/// Resolve a project's `[dependencies]` **sources** jars and extract their `.java`. See
/// [`resolve_project_sources_in`](crate::resolve_project_sources_in).
pub fn resolve_project_sources(
    manifest: &Manifest,
    root: &Path,
    warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let mut fs = OsFileTree;
    let fetcher = ReqwestFetcher::new();
    let files = block_on(resolve_project_sources_in(
        &fetcher,
        &mut fs,
        manifest,
        &vpath(root),
        warn,
    ));
    to_pathbufs(files)
}

/// Resolve a project's **source-form** `[dependencies]` (`git`/`path`) to `.java` files. See
/// [`resolve_project_source_deps_in`](crate::resolve_project_source_deps_in).
pub fn resolve_project_source_deps(
    manifest: &Manifest,
    root: &Path,
    warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let fs = OsFileTree;
    let git = SubprocessGit;
    let files = resolve_project_source_deps_in(&fs, Some(&git), manifest, &vpath(root), warn);
    to_pathbufs(files)
}

/// Synthesize signature-only `.java` skeletons for `classes` into `<root>/target/jals/deps/decompiled`.
/// See [`synthesize_classpath_sources_in`](crate::synthesize_classpath_sources_in).
pub fn synthesize_classpath_sources(
    classes: &[ClassFile],
    root: &Path,
    warn: impl FnMut(String),
) -> Vec<PathBuf> {
    let mut fs = OsFileTree;
    let files = synthesize_classpath_sources_in(&mut fs, classes, &vpath(root), warn);
    to_pathbufs(files)
}

/// Assemble a project's analysis / build inputs off the real filesystem: resolve `[dependencies]`
/// (downloading remotes with a blocking `reqwest` [`Fetcher`], cloning `git` deps with a subprocess
/// [`Git`]), load / synthesize per `options`, and add the manifest's source roots + edition. The
/// single seam `jals-cli` and `jals-lsp` build their `ProjectIndex` / compile inputs from. See
/// [`assemble_project_inputs_in`](crate::assemble_project_inputs_in) for the pure core.
///
/// Uses the blocking `reqwest` client via [`block_on`], which panics inside a Tokio runtime — so
/// `jals-lsp` calls this from a dedicated `std::thread` (the `git` subprocess and tree I/O are safe
/// under `block_on` regardless).
pub fn assemble_project_inputs(
    manifest: &Manifest,
    root: &Path,
    options: ProjectInputOptions,
    warn: impl FnMut(String),
) -> ProjectInputs {
    let mut fs = OsFileTree;
    let fetcher = ReqwestFetcher::new();
    let git = SubprocessGit;
    let inputs = block_on(assemble_project_inputs_in(
        &fetcher,
        Some(&git),
        &mut fs,
        manifest,
        &vpath(root),
        options,
        warn,
    ));
    ProjectInputs {
        source_roots: manifest.source_roots(root),
        dependency_jars: to_pathbufs(inputs.dependency_jars),
        classpath_classes: inputs.classpath_classes,
        library_sources: to_pathbufs(inputs.library_sources),
        source_dep_sources: to_pathbufs(inputs.source_dep_sources),
        target_java_version: inputs.target_java_version,
    }
}
