//! Project input assembly: the one place that turns a parsed [`Manifest`] plus injected capabilities
//! into the ready-to-use analysis / build inputs every host adapter needs.
//!
//! CLI, LSP, and the browser playground all need the *same* pipeline — resolve `[dependencies]` jars,
//! optionally each dependency's `-sources.jar` `.java` and `git`/`path` source deps, load the classpath
//! `.class` files, synthesize skeleton `.java` for jars that ship no source, and read the project's
//! `[package] edition`. Rather than re-sequence those primitives (and re-invent the warning-formatting,
//! skeleton-append-order, and classpath-fold conventions) in each adapter, this module composes them
//! once behind one call. Adapters supply the capabilities ([`Fetcher`] / [`Git`] / [`FileTree`]) and a
//! single `warn` sink, and receive a [`ProjectInputsIn`] with every resolved input.
//!
//! This is the pure, `wasm32`-compatible core (all I/O through the [`FileTree`] abstraction, the two
//! host capabilities behind traits, the only async step the download). The [`native`](crate::native)
//! facade wraps it with `OsFileTree` + a blocking `reqwest` [`Fetcher`] + a subprocess [`Git`] and
//! returns the `PathBuf`-based [`ProjectInputs`](crate::native::ProjectInputs), adding the manifest's
//! source roots on top.
//!
//! Which optional inputs to assemble is chosen by [`ProjectInputOptions`] — `Analysis` (load the
//! classpath for a `ProjectIndex`), `Compile` (resolve dependency jars + source deps for `javac`,
//! without loading), or `Editor` (everything, for the LSP's full navigation surface).

use std::path::Path;

use jals_build::ManifestExt;
use jals_classfile::ClassFile;
use jals_config::Manifest;
use jals_fs::FileTree;

use crate::io::{Fetcher, Git};
use crate::load::ClasspathLoad;
use crate::resolve::{DepsCache, PathExt};
use crate::skeleton::SkeletonGroup;

/// Which optional project inputs [`ProjectInputsIn::assemble_project_inputs_in`] should assemble. Each
/// variant is exactly one host adapter's need — the only three combinations any caller uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectInputOptions {
    /// For linting / analysis: load the classpath `.class` files (the `[build] classpath` entries plus
    /// the resolved `[dependencies]` jars) into [`ProjectInputsIn::classpath_classes`] for a
    /// `ProjectIndex`. No source deps, sources jars, or skeletons (a `jals lint` must never clone a
    /// `git` dependency or extract source).
    Analysis,
    /// For `jals build`/`run`: resolve the `[dependencies]` jar *paths* (for `javac -classpath`, into
    /// [`ProjectInputsIn::dependency_jars`]) and the `git`/`path` source deps' `.java`
    /// ([`ProjectInputsIn::source_dep_sources`], compiled alongside the project). No classpath
    /// *loading* (the compiler reads the jars itself), sources jars, or skeletons.
    Compile,
    /// For the LSP: everything — the loaded classpath, the `git`/`path` source deps folded into the
    /// index, and both real (`-sources.jar`) and synthesized (skeleton) navigation sources appended to
    /// [`ProjectInputsIn::library_sources`].
    Editor,
}

/// The assembled project inputs, as `/`-separated virtual paths (the core representation). See
/// [`ProjectInputs`](crate::native::ProjectInputs) for the host `PathBuf`-based form.
#[derive(Debug, Default)]
pub struct ProjectInputsIn {
    /// The resolved `[dependencies]` jar paths (downloaded remotes / confirmed local jars, plus any
    /// unpacked bundled jars). What `jals build`/`run` puts on `javac`'s classpath.
    pub dependency_jars: Vec<String>,
    /// The loaded classpath `.class` files, ready for `ProjectIndex::lower_classpath`. Empty unless
    /// the [`ProjectInputOptions`] loaded the classpath ([`Analysis`](ProjectInputOptions::Analysis)
    /// or [`Editor`](ProjectInputOptions::Editor)).
    pub classpath_classes: Vec<ClassFile>,
    /// Navigation `.java`: each dependency's extracted `-sources.jar` source (when `sources`), then
    /// the synthesized skeletons (when `skeletons`), in that order so a first-declaration-wins overlay
    /// keeps real source authoritative.
    pub library_sources: Vec<String>,
    /// The `git`/`path` source dependencies' `.java` (when `source_deps`) — an index input and a
    /// `javac` source.
    pub source_dep_sources: Vec<String>,
    /// The project's target Java feature version from `[package] edition`, gating the edition-only
    /// lint rules. `None` when the manifest sets no edition.
    pub target_java_version: Option<u32>,
}

impl ProjectInputsIn {
    /// Assemble a project's analysis / build inputs from its parsed `manifest` (rooted at `root`, a
    /// `/`-separated virtual path), driving all I/O through `fs` and the injected `fetcher` / `git`
    /// capabilities.
    ///
    /// The one place the resolve → load → synthesize pipeline lives; adapters call it and
    /// consume the fields they need.
    ///
    /// Every non-fatal problem — a failed download, a missing local jar, an unreadable `.class`, a
    /// failed clone — is reported through `warn` with a category prefix (`dependency: …`, `sources: …`,
    /// `source dependency: …`, `classpath: <path>: …`, `decompile: …`) and skipped; the caller's `warn`
    /// sink owns only where the message goes (its own tool prefix, a status line, a marker).
    // The injected capabilities (`&mut dyn FileTree`, non-`Sync` `F`/`Git`, `impl FnMut` sink) are
    // deliberately not `Send` — the wasm core drives this single-threaded and the `native` facade
    // `block_on`s it on a dedicated thread, so `future_not_send` does not apply.
    #[allow(clippy::future_not_send)]
    pub async fn assemble_project_inputs_in<F: Fetcher>(
        fetcher: &F,
        git: Option<&dyn Git>,
        fs: &mut dyn FileTree,
        manifest: &Manifest,
        root: &str,
        options: ProjectInputOptions,
        mut warn: impl FnMut(String),
    ) -> Self {
        use ProjectInputOptions::{Analysis, Compile, Editor};

        // Expand the host-role preset into the four capability flags that actually gate the pipeline.
        // Skeletons render from the loaded classes, so any preset wanting them also loads the classpath.
        let (want_sources, want_source_deps, want_classes, want_skeletons) = match options {
            Analysis => (false, false, true, false),
            Compile => (false, true, false, false),
            Editor => (true, true, true, true),
        };

        // 1. Resolve the `[dependencies]` jars (download remotes / confirm locals, unpack bundled jars).
        let dependency_jars =
            DepsCache::resolve_project_dependencies_in(fetcher, &mut *fs, manifest, root, |m| {
                warn(format!("dependency: {m}"));
            })
            .await;

        // 2. Optional: each dependency's `-sources.jar` `.java`, the first (authoritative) navigation
        //    layer — extended with the skeletons below.
        let mut library_sources = if want_sources {
            DepsCache::resolve_project_sources_in(fetcher, &mut *fs, manifest, root, |m| {
                warn(format!("sources: {m}"));
            })
            .await
        } else {
            Vec::new()
        };

        // 3. Optional: the `git`/`path` source dependencies' `.java` (a shared borrow — this step
        //    writes only via the injected `git`, not the tree).
        let source_dep_sources = if want_source_deps {
            DepsCache::resolve_project_source_deps_in(&*fs, git, manifest, root, |m| {
                warn(format!("source dependency: {m}"));
            })
        } else {
            Vec::new()
        };

        // 4-5. Load the classpath `.class` (for analysis, and for the skeleton rendering that reads
        //      them): the manifest's `[build] classpath` entries folded together with the resolved
        //      dependency jars, exactly as the adapters did by hand.
        let classpath_classes = if want_classes {
            let mut entries: Vec<String> = manifest
                .classpath_entries(Path::new(root))
                .iter()
                .map(|p| p.vpath())
                .collect();
            entries.extend(dependency_jars.iter().cloned());
            let load = ClasspathLoad::load_classpath_in(&*fs, &entries);
            for warning in &load.warnings {
                warn(format!("classpath: {}: {}", warning.path, warning.message));
            }
            load.classes
        } else {
            Vec::new()
        };

        // 6. Optional: signature-only skeletons, appended **after** the real sources so the overlay
        //    keeps real source authoritative (a skeleton fills the gap only for a class shipping no
        //    source).
        if want_skeletons {
            library_sources.extend(SkeletonGroup::synthesize_classpath_sources_in(
                &mut *fs,
                &classpath_classes,
                root,
                |m| warn(format!("decompile: {m}")),
            ));
        }

        Self {
            dependency_jars,
            classpath_classes,
            library_sources,
            source_dep_sources,
            target_java_version: manifest.target_java_version(),
        }
    }
}
