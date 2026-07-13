//! The host (native) implementations of the [`Fetcher`] / [`Git`] capabilities, and the synchronous,
//! `PathBuf`-based facade over the wasm-compatible core.
//!
//! Behind the default `native` feature. The facade functions preserve `jals-classpath`'s historic
//! public API (`&Path`/`PathBuf` in, `PathBuf` out) so `jals-cli`/`jals-lsp` are unchanged: each wraps
//! an [`OsFileTree`] and, where a download can happen, a blocking [`ReqwestFetcher`] driven by
//! [`futures::executor::block_on`]. The blocking `reqwest` client must not run inside a Tokio runtime
//! (it would panic spinning up its own), so `jals-lsp` calls these from a dedicated `std::thread`;
//! `block_on` itself establishes no runtime, so the blocking client is safe under it.
//!
//! Each facade lives on the same type as the core operation it wraps: [`ClasspathLoad::load_classpath`]
//! next to [`ClasspathLoad::load_classpath_in`], the resolution facades on [`DepsCache`], the
//! extraction facades on [`SourcesExtraction`] / [`NestedJarsExtraction`], the skeleton facade on
//! [`SkeletonGroup`], and the assembly facade on [`ProjectInputs`].

use std::path::{Path, PathBuf};
use std::process::Command;

use futures::executor::block_on;
use jals_build::{DependencySource, ManifestExt};
use jals_classfile::ClassFile;
use jals_config::{FeatureSet, GitRef, JavaVersion, Manifest};
use jals_fs::OsFileTree;

use crate::Warning;
use crate::io::{Fetcher, Git};
use crate::load::{ClasspathLoad, JarExtraction};
use crate::project::{ProjectInputOptions, ProjectInputsIn};
use crate::resolve::{DepsCache, PathExt};
use crate::skeleton::SkeletonGroup;

// ---- Capability implementations -------------------------------------------------------------

/// A [`Fetcher`] backed by `reqwest`'s **blocking** client.
///
/// Its `fetch` does blocking work and the returned future resolves in a single poll, so a synchronous
/// host drives it with `block_on` — provided no Tokio runtime is active on the current thread
/// (`reqwest::blocking` panics inside one).
pub struct ReqwestFetcher {
    client: reqwest::blocking::Client,
}

impl ReqwestFetcher {
    /// Build a fetcher with a fresh blocking client (cheap; reused across a resolution batch).
    pub fn new() -> Self {
        Self {
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
        Self::clone_git(url, reference, Path::new(dest))
    }
}

impl SubprocessGit {
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
        Self::run_git(&mut clone)?;

        if let Some(name) = reference.checkout_arg() {
            let mut co = Command::new("git");
            co.arg("-C")
                .arg(&tmp)
                .arg("checkout")
                .arg("--quiet")
                .arg(name);
            Self::run_git(&mut co).inspect_err(|_| {
                // Don't leave a clone parked at the wrong ref for a later cache hit.
                let _ = std::fs::remove_dir_all(&tmp);
            })?;
        }
        std::fs::rename(&tmp, dest)
            .map_err(|e| format!("finalizing git clone {}: {e}", dest.display()))
    }

    /// Run a configured `git` command, mapping a non-zero exit (or a missing `git` binary) to a
    /// message.
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
///
/// Produced by [`ProjectInputs::assemble_project_inputs`]. Which fields are populated depends on the
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
    /// The project's resolved language feature set from `[package] features` (feature-rule gate).
    pub feature_set: FeatureSet,
    /// The project's declared Java language system from `[package] java-version` (reserved).
    pub java_version: Option<JavaVersion>,
}

// ---- Path helpers ---------------------------------------------------------------------------

/// Conversions between the native `PathBuf` world and the core's `/`-separated virtual `&str` paths
/// (on a host a virtual path *is* the OS path string, so both directions are lossless).
struct VPaths;

impl VPaths {
    /// Every path in `ps` as a virtual path.
    fn strings(ps: &[PathBuf]) -> Vec<String> {
        ps.iter().map(|p| p.vpath()).collect()
    }

    /// Virtual paths back into `PathBuf`s.
    fn pathbufs(ss: Vec<String>) -> Vec<PathBuf> {
        ss.into_iter().map(PathBuf::from).collect()
    }
}

// ---- Facades (historic signatures) ----------------------------------------------------------

impl ClasspathLoad {
    /// Load every `.class` file reachable from `entries` off the real filesystem. See
    /// [`ClasspathLoad::load_classpath_in`].
    pub fn load_classpath(entries: &[PathBuf]) -> Self {
        let fs = OsFileTree;
        Self::load_classpath_in(&fs, &VPaths::strings(entries))
    }
}

impl DepsCache {
    /// The cache path a remote dependency downloads to: `<cache_dir>/<name>-<url-hash>.jar`. Public so
    /// tests can pre-seed the cache and exercise the skip-if-exists path without the network.
    pub fn cached_jar_path(name: &str, url: &str, cache_dir: &Path) -> PathBuf {
        PathBuf::from(Self::jar_path_str(name, url, &cache_dir.vpath()))
    }

    /// Resolve classified dependency `sources` to local `.jar` paths, downloading remote ones into
    /// `cache_dir`. See [`DepsCache::resolve_dependencies_in`].
    pub fn resolve_dependencies(
        sources: &[(String, DependencySource)],
        cache_dir: &Path,
    ) -> ResolvedDependencies {
        let mut fs = OsFileTree;
        let fetcher = ReqwestFetcher::new();
        let (jars, warnings) = block_on(Self::resolve_dependencies_in(
            &fetcher,
            &mut fs,
            sources,
            &cache_dir.vpath(),
        ));
        ResolvedDependencies {
            jars: VPaths::pathbufs(jars),
            warnings,
        }
    }

    /// Resolve a project's `[dependencies]` to local jar paths (downloading remotes into
    /// `<root>/target/jals/deps`). See [`DepsCache::resolve_project_dependencies_in`].
    pub fn resolve_project_dependencies(
        manifest: &Manifest,
        root: &Path,
        warn: impl FnMut(String),
    ) -> Vec<PathBuf> {
        let mut fs = OsFileTree;
        let fetcher = ReqwestFetcher::new();
        let jars = block_on(Self::resolve_project_dependencies_in(
            &fetcher,
            &mut fs,
            manifest,
            &root.vpath(),
            warn,
        ));
        VPaths::pathbufs(jars)
    }

    /// Resolve a project's `[dependencies]` **sources** jars and extract their `.java`. See
    /// [`DepsCache::resolve_project_sources_in`].
    pub fn resolve_project_sources(
        manifest: &Manifest,
        root: &Path,
        warn: impl FnMut(String),
    ) -> Vec<PathBuf> {
        let mut fs = OsFileTree;
        let fetcher = ReqwestFetcher::new();
        let files = block_on(Self::resolve_project_sources_in(
            &fetcher,
            &mut fs,
            manifest,
            &root.vpath(),
            warn,
        ));
        VPaths::pathbufs(files)
    }

    /// Resolve a project's **source-form** `[dependencies]` (`git`/`path`) to `.java` files. See
    /// [`DepsCache::resolve_project_source_deps_in`].
    pub fn resolve_project_source_deps(
        manifest: &Manifest,
        root: &Path,
        warn: impl FnMut(String),
    ) -> Vec<PathBuf> {
        let fs = OsFileTree;
        let git = SubprocessGit;
        let files =
            Self::resolve_project_source_deps_in(&fs, Some(&git), manifest, &root.vpath(), warn);
        VPaths::pathbufs(files)
    }
}

impl SourcesExtraction {
    /// Extract every `*.java` member of each sources jar in `jars` into `dest_dir`. See
    /// [`JarExtraction::extract_sources_in`].
    pub fn extract_sources(jars: &[PathBuf], dest_dir: &Path) -> Self {
        let mut fs = OsFileTree;
        let (java_files, warnings) =
            JarExtraction::extract_sources_in(&mut fs, &VPaths::strings(jars), &dest_dir.vpath());
        Self {
            java_files: VPaths::pathbufs(java_files),
            warnings,
        }
    }
}

impl NestedJarsExtraction {
    /// Recursively extract every **bundled jar** of `jar` into `dest_dir`. See
    /// [`JarExtraction::extract_nested_jars_in`].
    pub fn extract_nested_jars(jar: &Path, dest_dir: &Path) -> Self {
        let mut fs = OsFileTree;
        let (jars, warnings) =
            JarExtraction::extract_nested_jars_in(&mut fs, &jar.vpath(), &dest_dir.vpath());
        Self {
            jars: VPaths::pathbufs(jars),
            warnings,
        }
    }
}

impl SkeletonGroup<'_> {
    /// Synthesize signature-only `.java` skeletons for `classes` into
    /// `<root>/target/jals/deps/decompiled`. See [`SkeletonGroup::synthesize_classpath_sources_in`].
    pub fn synthesize_classpath_sources(
        classes: &[ClassFile],
        root: &Path,
        warn: impl FnMut(String),
    ) -> Vec<PathBuf> {
        let mut fs = OsFileTree;
        let files = Self::synthesize_classpath_sources_in(&mut fs, classes, &root.vpath(), warn);
        VPaths::pathbufs(files)
    }
}

impl ProjectInputs {
    /// Assemble a project's analysis / build inputs off the real filesystem.
    ///
    /// Resolves `[dependencies]` (downloading remotes with a blocking `reqwest` [`Fetcher`], cloning
    /// `git` deps with a subprocess [`Git`]), loads / synthesizes per `options`, and adds the
    /// manifest's source roots + feature set. The single seam `jals-cli` and `jals-lsp` build their
    /// `ProjectIndex` / compile inputs from. See
    /// [`ProjectInputsIn::assemble_project_inputs_in`] for the pure core.
    ///
    /// Uses the blocking `reqwest` client via [`block_on`], which panics inside a Tokio runtime — so
    /// `jals-lsp` calls this from a dedicated `std::thread` (the `git` subprocess and tree I/O are safe
    /// under `block_on` regardless).
    pub fn assemble_project_inputs(
        manifest: &Manifest,
        root: &Path,
        options: ProjectInputOptions,
        warn: impl FnMut(String),
    ) -> Self {
        let mut fs = OsFileTree;
        let fetcher = ReqwestFetcher::new();
        let git = SubprocessGit;
        let inputs = block_on(ProjectInputsIn::assemble_project_inputs_in(
            &fetcher,
            Some(&git),
            &mut fs,
            manifest,
            &root.vpath(),
            options,
            warn,
        ));
        Self {
            source_roots: manifest.source_roots(root),
            dependency_jars: VPaths::pathbufs(inputs.dependency_jars),
            classpath_classes: inputs.classpath_classes,
            library_sources: VPaths::pathbufs(inputs.library_sources),
            source_dep_sources: VPaths::pathbufs(inputs.source_dep_sources),
            feature_set: inputs.feature_set,
            java_version: inputs.java_version,
        }
    }
}
