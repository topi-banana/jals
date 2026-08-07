//! Native path/Git acquisition and root-project input projection.
//!
//! The walk is [`crate::walk`]'s; this is the half a host owns. Acquiring means canonicalizing a
//! path or cloning a repository, and *opening* means snapshotting what was acquired into a
//! [`ProjectView`] — after which the walk reads exactly as it does over a captured tree.

use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};
use std::process::Command;

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classpath::{
    Fetcher, NativeProjectPlan, NetworkPolicy, ProjectInputOptions, ProjectInputs,
};
use jals_config::{GitDependency, Manifest, PathDependency};
use jals_exec::Exec;
use jals_storage::{
    Diagnostic, DirKey, FileKey, MemoryCache, Name, NativeSource, NativeStorage, ProjectStorage,
    ProjectView, RelativePath,
};

use crate::assemble::{CompileClasspathEntry, ProjectAssemblyError};
use crate::assembly::{GraphResolveError, ProjectScript, RootProjection};
use crate::diagnostics::ProjectReport;
use crate::graph::{
    GraphError, GraphMetadata, GraphPreprocess, GraphWarning, NodeId, PreprocessedProjectGraph,
    ResolvedProjectGraph,
};
use crate::walk::{
    Acquired, DeclaredEntry, DeclaredFile, DeclaredTree, GraphHost, GraphWalk, Opened, Placement,
};

/// Native entry point for recursive dependency graph discovery.
pub struct NativeProjectGraph;

impl NativeProjectGraph {
    /// A `git` invocation that can never stop waiting for a human.
    ///
    /// Dependency acquisition runs unattended — from `jals build`, but also from the language
    /// server while someone is just editing. Git's default behaviour on a private or mistyped
    /// remote is to prompt for credentials on the inherited terminal, which would hang the build
    /// (or the whole LSP session) with no visible cause. Fail fast instead.
    fn git_command() -> Command {
        let mut command = Command::new("git");
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
            .stdin(std::process::Stdio::null());
        command
    }
}

/// Fully projected native root plus its preprocessed dependency graph.
#[derive(Debug)]
pub struct NativeProjectAssembly {
    #[allow(dead_code)]
    graph: GraphMetadata,
    /// The graph's plan, already executed into [`inputs`](Self::inputs). Nothing downstream
    /// re-runs it, so only this crate's projection tests read it — they assert that a
    /// [`ProjectInputOptions`] applies to the plan and not merely to the resolved inputs.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) plan: jals_classpath::ProjectInputPlan,
    pub inputs: ProjectInputs,
    pub source_roots: Vec<DirKey>,
    pub compile_classpath: Vec<CompileClasspathEntry>,
    pub(crate) warnings: Vec<GraphWarning>,
    pub(crate) errors: Vec<ProjectAssemblyError>,
    pub watch_paths: Vec<PathBuf>,
}

impl NativeProjectAssembly {
    /// Everything this assembly reported, in one value.
    ///
    /// The native half of [`MemoryProjectAssembly::report`], and for the same reason: the graph's
    /// channels and the classpath's are one report, and a host reading either alone reports half of
    /// what the procedure said.
    pub fn report(&self) -> ProjectReport<'_> {
        ProjectReport::new(&self.warnings, &self.errors, &self.inputs.warnings)
    }
}

#[derive(Debug, Default)]
pub(crate) struct NativeGraphState {
    watch_paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct GitConfinement {
    checkout: PathBuf,
    /// Identity framing, folded into every node id acquired inside this checkout. NUL-delimited
    /// and nested — `git-local\0git\0<url>\0<commit>\0<relative>` is a shape this takes — so it is
    /// a digest input, never a string to show anyone.
    stable_repository: String,
    /// How a diagnostic names this repository: the argument `git clone` was given, which is what
    /// the reader wrote in their manifest or a canonical host path they can open.
    location: String,
}

#[derive(Clone)]
struct DeclaringProject {
    root: PathBuf,
    view: ProjectView,
    confinement: Option<GitConfinement>,
}

struct CapturedSnapshot {
    view: ProjectView,
    diagnostics: Vec<Diagnostic>,
}

/// A dependency source located on this host but not yet snapshotted.
struct NativeSite {
    root: PathBuf,
    confinement: Option<GitConfinement>,
    /// Whether a host watching this project should watch this dependency too. A path dependency
    /// outside a Git checkout is editable in place; a temporary clone is not.
    watch: bool,
}

/// The host half of discovery: what it takes to reach a path or a repository, and what it collected
/// on the way.
struct NativeHost {
    exec: Exec,
    network: NetworkPolicy,
    watch_paths: BTreeSet<PathBuf>,
}

impl NativeProjectGraph {
    /// Discover all root path/Git dependencies recursively. The root manifest is never searched
    /// upward; every dependency probes exactly its selected root's `jals.toml`.
    pub(crate) async fn discover(
        root_manifest: &Manifest,
        root_directory: &Path,
        exec: &Exec,
        network: NetworkPolicy,
    ) -> Result<ResolvedProjectGraph, GraphError> {
        root_manifest
            .validate()
            .map_err(|error| GraphError::InvalidRootManifest {
                message: error.to_string(),
            })?;
        let root = NativeHost::canonical_project_root(root_directory).await?;
        let snapshot = NativeHost::snapshot(&root, exec).await?;
        // The root project is no node, so its own snapshot diagnostics are attributed to nothing.
        // Seeded ahead of the walk so they read before anything it finds.
        let warnings = NativeHost::snapshot_notes("snapshot", &snapshot.diagnostics)
            .into_iter()
            .map(|message| GraphWarning {
                node: None,
                dependency: None,
                message,
            })
            .collect();
        let declaring = DeclaringProject {
            root,
            view: snapshot.view,
            confinement: None,
        };
        let mut host = NativeHost {
            exec: exec.clone(),
            network,
            watch_paths: BTreeSet::new(),
        };
        let output = GraphWalk::run(&mut host, &declaring, root_manifest, warnings).await?;
        Ok(ResolvedProjectGraph {
            nodes: output.nodes,
            edges: output.edges,
            order: output.order,
            warnings: output.warnings,
            native: NativeGraphState {
                watch_paths: host.watch_paths.into_iter().collect(),
            },
        })
    }
}

impl ProjectScript {
    /// The graph phase over a native project root: discover, preprocess, project, and resolve the
    /// root's and the graph's inputs against `storage`.
    ///
    /// `preprocess.fetcher`'s [`NetworkPolicy`] governs the whole phase, discovery and input
    /// resolution included — a host cannot ask the graph to be discovered online and resolved
    /// offline, because there is one capability and every step is handed the same one.
    /// `preprocess.exec` likewise drives all of it.
    pub async fn resolve_native<F: Fetcher>(
        &self,
        manifest: &Manifest,
        root: &Path,
        storage: &mut NativeStorage,
        preprocess: GraphPreprocess<'_, F>,
        options: ProjectInputOptions,
    ) -> Result<NativeProjectAssembly, GraphResolveError> {
        // `preprocess` is consumed by the phase it names, but the graph plan needs the same fetch
        // capability again when it resolves. The field is a shared reference, so copy it out first
        // — exactly as `resolve_memory` does. Rebuilding one here instead is what used to fetch
        // under `--offline`.
        let fetcher = preprocess.fetcher;
        let graph =
            NativeProjectGraph::discover(manifest, root, preprocess.exec, fetcher.network())
                .await
                .map_err(GraphResolveError::unreported)?;
        let discovered = graph.warnings.clone();
        let graph = graph
            .preprocess(storage.artifacts_mut(), preprocess)
            .await
            .map_err(|error| GraphResolveError::reporting(error, discovered))?;
        Ok(self
            .project_native(&graph, manifest, root, storage, fetcher, options)
            .await)
    }

    /// The root manifest with its `[dependencies]` removed.
    ///
    /// Unlike the portable sibling, [`NativeProjectPlan::assemble_native`] *does* lower a
    /// `[dependencies]` jar entry — it has to, because a host path or URL is exactly what it exists
    /// to classify. Every declared dependency is already a graph node, so leaving the table in place
    /// would resolve each jar a second time and double-count it on the classpath. That makes this
    /// stripping the native path's own precondition, not a rule about root plans in general, which
    /// is why it lives here and `resolve_memory` hands its manifest over whole.
    fn root_only(manifest: &Manifest) -> Manifest {
        let mut root_only = manifest.clone();
        root_only.dependencies.clear();
        root_only
    }

    /// The native half of the projection: lower the root plan through the host path pipeline, then
    /// hand both plans to the shared merge.
    ///
    /// The projection step on its own, reachable inside the crate so a test can project one
    /// preprocessed graph under more than one [`ProjectInputOptions`] without rediscovering it.
    /// A host has no such need and reaches it through
    /// [`resolve_native`](Self::resolve_native), which owns the order of the phases before it.
    pub(crate) async fn project_native<F: Fetcher>(
        &self,
        graph: &PreprocessedProjectGraph,
        root_manifest: &Manifest,
        root_directory: &Path,
        storage: &mut NativeStorage,
        fetcher: &F,
        mode: ProjectInputOptions,
    ) -> NativeProjectAssembly {
        let graph_assembly = graph.assemble(storage.artifacts_mut()).await;
        let (inputs, source_roots) = NativeProjectPlan::assemble_native(
            &Self::root_only(root_manifest),
            // `root_only` cleared `[dependencies]`, so nothing this call lowers reads a feature.
            // The graph resolved the real selection per node before this point.
            &jals_config::ResolvedBuildFeatures::default(),
            root_directory,
            storage,
            fetcher,
            mode,
        )
        .await;
        let projected = self
            .project(
                graph,
                graph_assembly,
                RootProjection {
                    inputs,
                    source_roots,
                },
                fetcher,
                storage,
                mode,
            )
            .await;
        NativeProjectAssembly {
            graph: projected.graph,
            plan: projected.plan,
            inputs: projected.inputs,
            source_roots: projected.source_roots,
            compile_classpath: projected.compile_classpath,
            warnings: projected.warnings,
            errors: projected.errors,
            watch_paths: graph.native.watch_paths.clone(),
        }
    }
}

impl NativeHost {
    /// How a diagnostic names a node. A Git dependency lives in a temporary checkout whose path
    /// means nothing to a reader, so it is named by the repository it was cloned from instead —
    /// by that repository's [`location`](GitConfinement::location), never by the identity framing
    /// beside it.
    fn node_location(root: &Path, confinement: Option<&GitConfinement>) -> String {
        confinement.map_or_else(
            || root.display().to_string(),
            |confinement| confinement.location.clone(),
        )
    }

    /// One declared file's own name, portable enough to address it by.
    fn host_file_name(canonical: &Path) -> Result<Name, String> {
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "dependency file name is not portable UTF-8".to_owned())?;
        Name::new(name).map_err(|error| format!("dependency file name is not portable: {error:?}"))
    }

    /// Where a canonical host path sits relative to the project that declared it.
    fn placement(declaring: &DeclaringProject, canonical: &Path) -> Placement {
        RelativePath::from_host_path(&declaring.root, canonical)
            .filter(|relative| !relative.is_root())
            .map_or(Placement::External, Placement::Local)
    }

    /// Why a `jals.toml` the snapshot did not produce could not be read, if that is what happened.
    ///
    /// A permission failure is not missing data, and the two are indistinguishable from the view
    /// alone — the entry is simply absent either way. The snapshot's own diagnostics are what tell
    /// them apart, so they are scanned here, once, while they are still in hand.
    fn manifest_unreadable(root: &Path, diagnostics: &[Diagnostic]) -> Option<String> {
        let path = root.join("jals.toml");
        diagnostics.iter().find_map(|diagnostic| match diagnostic {
            Diagnostic::UnreadableEntry(message)
                if message
                    .strip_prefix(path.to_string_lossy().as_ref())
                    .is_some_and(|suffix| suffix.starts_with(':')) =>
            {
                Some(message.clone())
            }
            Diagnostic::SymlinkEscapesRoot(logical) | Diagnostic::SymlinkCycle(logical)
                if logical == "jals.toml" =>
            {
                Some(format!("`jals.toml` is unreadable: {diagnostic:?}"))
            }
            Diagnostic::ExternalChangeShadowed(_)
            | Diagnostic::NonUtf8Entry(_)
            | Diagnostic::SymlinkEscapesRoot(_)
            | Diagnostic::SymlinkCycle(_)
            | Diagnostic::UnreadableEntry(_) => None,
        })
    }

    async fn resolve_git_locator(
        &self,
        declaring: &DeclaringProject,
        locator: &str,
    ) -> Result<(String, String), String> {
        let local = locator
            .strip_prefix("file://")
            .map(PathBuf::from)
            .or_else(|| {
                (!locator.contains("://") && !locator.starts_with("git@"))
                    .then(|| PathBuf::from(locator))
            });
        let Some(local) = local else {
            return Ok((locator.to_owned(), locator.to_owned()));
        };
        let unresolved = if local.is_absolute() {
            local
        } else {
            declaring.root.join(local)
        };
        let canonical = Self::canonical_directory(&unresolved).await?;
        Self::require_confinement(declaring, &canonical)?;
        let canonical_path = Self::stable_path(&canonical)?;
        let stable = if let Some(confinement) = &declaring.confinement {
            format!(
                "git-local\0{}\0{}",
                confinement.stable_repository,
                Self::stable_relative(&confinement.checkout, &canonical)?
            )
        } else {
            format!("file\0{canonical_path}")
        };
        Ok((canonical_path, stable))
    }

    fn require_confinement(declaring: &DeclaringProject, selected: &Path) -> Result<(), String> {
        if declaring
            .confinement
            .as_ref()
            .is_some_and(|confinement| !selected.starts_with(&confinement.checkout))
        {
            return Err("Git-origin local dependency leaves its checkout".to_owned());
        }
        Ok(())
    }

    fn stable_local_identity(
        declaring: &DeclaringProject,
        selected: &Path,
        kind: &str,
    ) -> Result<String, String> {
        if let Some(confinement) = &declaring.confinement {
            return Ok(format!(
                "{kind}-in-git\0{}\0{}",
                confinement.stable_repository,
                Self::stable_relative(&confinement.checkout, selected)?
            ));
        }
        Ok(format!("{kind}-path\0{}", Self::stable_path(selected)?))
    }

    async fn read_declared_file(
        declaring: &DeclaringProject,
        canonical: &Path,
    ) -> Result<Vec<u8>, String> {
        if let Some(relative) = RelativePath::from_host_path(&declaring.root, canonical)
            && let Ok(key) = FileKey::new(relative)
            && let Ok(file) = declaring.view.file(&key)
        {
            return Ok(file.bytes().to_vec());
        }
        let path = canonical.to_path_buf();
        jals_exec::tokio_rt::on_blocking_pool(move || {
            fs::read(&path).map_err(|error| format!("reading dependency file: {error}"))
        })
        .await
    }

    async fn snapshot(root: &Path, exec: &Exec) -> Result<CapturedSnapshot, GraphError> {
        let root = root.to_path_buf();
        let source = jals_exec::tokio_rt::on_blocking_pool(move || {
            NativeSource::new(root).map(|source| {
                source
                    .excluding(RelativePath::parse(".git").expect("constant is portable"))
                    .excluding(
                        RelativePath::parse(NativeStorage::PROJECT_CACHE_DIR)
                            .expect("constant is portable"),
                    )
            })
        })
        .await
        .map_err(|error| GraphError::Acquisition {
            operation: "opening dependency snapshot".to_owned(),
            message: error.to_string(),
        })?;
        let storage = ProjectStorage::open(source, MemoryCache::default(), exec.clone())
            .await
            .map_err(|error| GraphError::Acquisition {
                operation: "capturing dependency snapshot".to_owned(),
                message: error.to_string(),
            })?;
        Ok(CapturedSnapshot {
            view: storage.view(),
            diagnostics: storage.diagnostics().to_vec(),
        })
    }

    /// What the host has to say about a tree it captured, attributed to whatever is being captured.
    ///
    /// `label` names the capture rather than the diagnostic, because the reader is told what was
    /// being read at the time: a dependency's own tree, or the `[build]` entry that named one.
    fn snapshot_notes(label: &str, diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics
            .iter()
            .map(|diagnostic| format!("{label}: {diagnostic:?}"))
            .collect()
    }

    /// A declared directory nothing already holds a view of, captured on its own.
    ///
    /// The caller decides *whether* a declared directory is external, because the two `[build]`
    /// entries do not agree on how to tell — `source-dirs` requires the snapshot to have kept it,
    /// where a `classpath` directory is taken at its host path. Once outside, they are the same
    /// capture, and `label` is the one word both a remark and a failure are spelled with.
    async fn external_tree(
        &self,
        canonical: &Path,
        label: &str,
        notes: &mut Vec<String>,
    ) -> Result<DeclaredTree, String> {
        let snapshot = Self::snapshot(canonical, &self.exec)
            .await
            .map_err(|error| format!("{label} failed: {error}"))?;
        notes.extend(Self::snapshot_notes(label, &snapshot.diagnostics));
        Ok(DeclaredTree {
            view: snapshot.view,
            root: DirKey::ROOT,
            placement: Placement::External,
        })
    }

    async fn canonical_project_root(path: &Path) -> Result<PathBuf, GraphError> {
        Self::canonical_directory(path)
            .await
            .map_err(|message| GraphError::Acquisition {
                operation: "resolving project root".to_owned(),
                message,
            })
    }

    /// `fs::canonicalize`, minus the Windows *verbatim* spelling.
    ///
    /// Windows canonicalization answers with an extended-length path (`\\?\C:\…`). Two things
    /// here cannot live with that: Git for Windows does not recognise the spelling as a
    /// repository, so a local `git = "…"` dependency fails to clone; and a node identity built
    /// from the text would record how a path was spelled rather than where it points. Every
    /// canonicalization in this adapter goes through this function, so both sides of every prefix
    /// comparison — and every watch path handed to a host — agree on one spelling.
    ///
    /// Only a verbatim *disk* prefix is rewritten. `\\?\UNC\…` and the device namespace mean
    /// something the plain form cannot express, so they are left exactly as the OS gave them.
    fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
        let canonical = fs::canonicalize(path)?;
        let mut components = canonical.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return Ok(canonical);
        };
        let Prefix::VerbatimDisk(drive) = prefix.kind() else {
            return Ok(canonical);
        };
        let mut plain = PathBuf::from(format!("{}:\\", char::from(drive)));
        plain.extend(components.filter(|component| !matches!(component, Component::RootDir)));
        Ok(plain)
    }

    async fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
        let path = path.to_path_buf();
        jals_exec::tokio_rt::on_blocking_pool(move || {
            let canonical = Self::canonicalize(&path)
                .map_err(|error| format!("canonicalizing directory: {error}"))?;
            if !canonical.is_dir() {
                return Err("selected dependency root is not a directory".to_owned());
            }
            Ok(canonical)
        })
        .await
    }

    async fn canonical_file(path: &Path) -> Result<PathBuf, String> {
        let canonical = Self::canonical_existing(path).await?;
        if !Self::is_file(&canonical).await? {
            return Err("selected dependency is not a file".to_owned());
        }
        Ok(canonical)
    }

    async fn canonical_existing(path: &Path) -> Result<PathBuf, String> {
        let path = path.to_path_buf();
        jals_exec::tokio_rt::on_blocking_pool(move || {
            Self::canonicalize(&path)
                .map_err(|error| format!("canonicalizing dependency path: {error}"))
        })
        .await
    }

    async fn is_file(path: &Path) -> Result<bool, String> {
        let path = path.to_path_buf();
        jals_exec::tokio_rt::on_blocking_pool(move || {
            fs::metadata(&path)
                .map(|metadata| metadata.is_file())
                .map_err(|error| format!("reading dependency metadata: {error}"))
        })
        .await
    }

    fn resolve_path(root: &Path, raw: &str) -> PathBuf {
        let raw = Path::new(raw);
        if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            root.join(raw)
        }
    }

    fn stable_path(path: &Path) -> Result<String, String> {
        path.to_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| "dependency path is not UTF-8".to_owned())
    }

    fn stable_relative(root: &Path, selected: &Path) -> Result<String, String> {
        let relative = selected
            .strip_prefix(root)
            .map_err(|_| "selected path leaves its Git checkout".to_owned())?;
        let mut segments = Vec::new();
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(segment) => segments.push(
                    segment
                        .to_str()
                        .ok_or_else(|| "dependency path is not UTF-8".to_owned())?,
                ),
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return Err("dependency path is not a confined relative path".to_owned());
                }
            }
        }
        Ok(segments.join("/"))
    }
}

impl GraphHost for NativeHost {
    type Site = NativeSite;
    type Project = DeclaringProject;
    /// A Git dependency is cloned into a scratch directory that must outlive its whole subtree.
    type Guard = Option<tempfile::TempDir>;

    const SCOPE: &'static str = "native";

    /// A dependency's manifest is addressed by the node's own token: a temporary checkout's path is
    /// no help, and two dependencies can sit at the same relative place under different roots.
    fn manifest_location(&self, id: &NodeId, _acquired: &Acquired<Self>) -> String {
        format!("dependencies/{}/jals.toml", id.token())
    }

    async fn acquire_path(
        &mut self,
        declaring: &Self::Project,
        dependency: &PathDependency,
    ) -> Result<Acquired<Self>, String> {
        let base = Self::resolve_path(&declaring.root, &dependency.path);
        let selected = dependency
            .dir
            .as_deref()
            .map_or_else(|| base.clone(), |dir| base.join(dir));
        let root = Self::canonical_directory(&selected).await?;
        Self::require_confinement(declaring, &root)?;
        let (identity, confinement, watch) = if let Some(confinement) = &declaring.confinement {
            let relative = Self::stable_relative(&confinement.checkout, &root)?;
            (
                format!("path-in-git\0{}\0{relative}", confinement.stable_repository),
                Some(confinement.clone()),
                false,
            )
        } else {
            let rendered = Self::stable_path(&root)?;
            (format!("path\0{rendered}"), None, true)
        };
        Ok(Acquired {
            identity,
            location: Self::node_location(&root, confinement.as_ref()),
            site: NativeSite {
                root,
                confinement,
                watch,
            },
            guard: None,
        })
    }

    async fn acquire_git(
        &mut self,
        declaring: &Self::Project,
        dependency_name: &str,
        dependency: &GitDependency,
    ) -> Result<Acquired<Self>, String> {
        // Cloning is a network operation, and it is not gated anywhere downstream. `--offline`
        // has to stop it here, and the language server runs offline unconditionally — opening a
        // folder in an editor must not reach out to a remote the user never asked about.
        if self.network == NetworkPolicy::Offline {
            return Err(format!(
                "git dependency `{dependency_name}` cannot be acquired offline"
            ));
        }
        let reference = dependency
            .git_ref(dependency_name)
            .map_err(|error| error.to_string())?;
        let (clone_argument, stable_locator) =
            self.resolve_git_locator(declaring, &dependency.git).await?;
        // Kept out of the clone below, which moves `clone_argument` onto the blocking pool. This
        // is the readable half of the pair: a URL as the manifest wrote it, or a canonical host
        // path — where `stable_locator` is NUL-framed identity input.
        let location = clone_argument.clone();
        let checkout_arg = reference.checkout_arg().map(ToOwned::to_owned);
        let selected_dir = dependency.dir.clone();
        let current_directory = declaring.root.clone();
        let (temporary, checkout, selected, commit) =
            jals_exec::tokio_rt::on_blocking_pool(move || {
                let temporary = tempfile::tempdir()
                    .map_err(|error| format!("creating temporary Git checkout: {error}"))?;
                let checkout = temporary.path().join("checkout");
                let clone = NativeProjectGraph::git_command()
                    .current_dir(&current_directory)
                    .arg("clone")
                    .arg("--quiet")
                    // `--` ends option parsing: without it a URL or path that happens to look
                    // like a flag would be read as one.
                    .arg("--")
                    .arg(&clone_argument)
                    .arg(&checkout)
                    .output()
                    .map_err(|error| format!("running git clone: {error}"))?;
                if !clone.status.success() {
                    return Err(format!(
                        "git clone failed: {}",
                        String::from_utf8_lossy(&clone.stderr).trim()
                    ));
                }
                if let Some(target) = checkout_arg {
                    let output = NativeProjectGraph::git_command()
                        .arg("-C")
                        .arg(&checkout)
                        .arg("checkout")
                        .arg("--quiet")
                        .arg(target)
                        // No pathspecs follow, so a ref sharing a name with a file is still
                        // resolved as a ref.
                        .arg("--")
                        .output()
                        .map_err(|error| format!("running git checkout: {error}"))?;
                    if !output.status.success() {
                        return Err(format!(
                            "git checkout failed: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                }
                let head = NativeProjectGraph::git_command()
                    .arg("-C")
                    .arg(&checkout)
                    .arg("rev-parse")
                    .arg("HEAD")
                    .output()
                    .map_err(|error| format!("reading Git HEAD: {error}"))?;
                if !head.status.success() {
                    return Err("could not resolve checked-out Git HEAD".to_owned());
                }
                let commit = String::from_utf8(head.stdout)
                    .map_err(|_| "Git HEAD is not UTF-8".to_owned())?
                    .trim()
                    .to_owned();
                let checkout = Self::canonicalize(&checkout)
                    .map_err(|error| format!("canonicalizing Git checkout: {error}"))?;
                let selected = selected_dir
                    .as_deref()
                    .map_or_else(|| checkout.clone(), |dir| checkout.join(dir));
                let selected = Self::canonicalize(&selected)
                    .map_err(|error| format!("selecting Git dependency root: {error}"))?;
                if !selected.is_dir() || !selected.starts_with(&checkout) {
                    return Err("selected Git dependency root leaves the checkout".to_owned());
                }
                let selected = Self::stable_relative(&checkout, &selected)?;
                Ok((temporary, checkout, selected, commit))
            })
            .await?;
        let identity = format!("git\0{stable_locator}\0{commit}\0{selected}");
        let stable_repository = format!("git\0{stable_locator}\0{commit}");
        let root = if selected.is_empty() {
            checkout.clone()
        } else {
            Self::resolve_path(&checkout, &selected)
        };
        Ok(Acquired {
            identity,
            // The repository as the manifest spelled it. A temporary checkout path names nothing
            // a reader could open.
            location: location.clone(),
            site: NativeSite {
                root,
                confinement: Some(GitConfinement {
                    checkout,
                    stable_repository,
                    location,
                }),
                // Never: the clone is scratch, and it is removed as soon as the subtree is walked.
                watch: false,
            },
            guard: Some(temporary),
        })
    }

    async fn open(&mut self, acquired: &Acquired<Self>) -> Result<Opened<Self>, GraphError> {
        let snapshot = Self::snapshot(&acquired.site.root, &self.exec).await?;
        let manifest_unreadable =
            Self::manifest_unreadable(&acquired.site.root, &snapshot.diagnostics);
        let notes = Self::snapshot_notes("snapshot", &snapshot.diagnostics);
        Ok(Opened {
            project: DeclaringProject {
                root: acquired.site.root.clone(),
                view: snapshot.view.clone(),
                confinement: acquired.site.confinement.clone(),
            },
            view: snapshot.view,
            notes,
            manifest_unreadable,
        })
    }

    fn admitted(&mut self, acquired: &Acquired<Self>) {
        if acquired.site.watch {
            self.watch_paths.insert(acquired.site.root.clone());
        }
    }

    /// Removing the scratch checkout is housekeeping, not part of resolving the graph. On Windows
    /// an antivirus or indexer holding a handle makes this fail routinely, and failing the whole
    /// build over a leftover temp directory leaves the user no way forward. Report it and move on;
    /// the directory is under the OS temp root either way.
    async fn release(&mut self, guard: Self::Guard) -> Result<(), String> {
        let Some(checkout) = guard else {
            return Ok(());
        };
        jals_exec::tokio_rt::on_blocking_pool(move || {
            checkout
                .close()
                .map_err(|error| format!("removing temporary Git checkout: {error}"))
        })
        .await
        .map_err(|message| format!("could not remove the temporary Git checkout: {message}"))
    }

    async fn resolve_declared_file(
        &mut self,
        declaring: &Self::Project,
        raw: &str,
        role: &str,
    ) -> Result<DeclaredFile, String> {
        let unresolved = Self::resolve_path(&declaring.root, raw);
        let canonical = Self::canonical_file(&unresolved).await?;
        Self::require_confinement(declaring, &canonical)?;
        let identity = Self::stable_local_identity(declaring, &canonical, role)?;
        Ok(DeclaredFile {
            bytes: Self::read_declared_file(declaring, &canonical).await?,
            name: Self::host_file_name(&canonical)?,
            identity,
            placement: Self::placement(declaring, &canonical),
        })
    }

    async fn resolve_source_dir(
        &mut self,
        declaring: &Self::Project,
        raw: &str,
        notes: &mut Vec<String>,
    ) -> Result<DeclaredTree, String> {
        let physical = Self::resolve_path(&declaring.root, raw);
        let canonical = Self::canonical_directory(&physical)
            .await
            .map_err(|message| format!("source directory is unavailable: {message}"))?;
        Self::require_confinement(declaring, &canonical)?;
        // Inside the project the snapshot already holds it, so no second scan of the same bytes.
        if let Some(relative) = RelativePath::from_host_path(&declaring.root, &canonical) {
            let root = DirKey::new(relative.clone());
            if declaring.view.directory(&root).is_ok() {
                return Ok(DeclaredTree {
                    view: declaring.view.clone(),
                    root,
                    placement: Placement::Local(relative),
                });
            }
        }
        self.external_tree(&canonical, "source snapshot", notes)
            .await
    }

    async fn resolve_classpath_entry(
        &mut self,
        declaring: &Self::Project,
        raw: &str,
        notes: &mut Vec<String>,
    ) -> Result<DeclaredEntry, String> {
        let unresolved = Self::resolve_path(&declaring.root, raw);
        let canonical = Self::canonical_existing(&unresolved)
            .await
            .map_err(|message| format!("classpath entry is unavailable: {message}"))?;
        Self::require_confinement(declaring, &canonical)?;
        if Self::is_file(&canonical).await? {
            return Ok(DeclaredEntry::File(DeclaredFile {
                bytes: Self::read_declared_file(declaring, &canonical).await?,
                name: Self::host_file_name(&canonical)?,
                // A `[build] classpath` entry becomes no node, so it needs no identity.
                identity: String::new(),
                placement: Self::placement(declaring, &canonical),
            }));
        }
        if let Some(relative) = RelativePath::from_host_path(&declaring.root, &canonical) {
            return Ok(DeclaredEntry::Tree(DeclaredTree {
                view: declaring.view.clone(),
                root: DirKey::new(relative.clone()),
                placement: Placement::Local(relative),
            }));
        }
        let tree = self
            .external_tree(&canonical, "classpath snapshot", notes)
            .await?;
        Ok(DeclaredEntry::Tree(tree))
    }
}

#[cfg(test)]
mod tests {
    use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
    use jals_config::ResolvedBuildFeatures;
    use jals_storage::{CodeTree, MemoryStorage};

    use super::*;
    use crate::graph::{BinaryInput, NodeBody, ResolvedNode, SourceNode};

    /// A fetch capability for graphs that declare no task plan. Reaching it is the failure.
    struct UnreachableFetcher;

    impl jals_classpath::Fetcher for UnreachableFetcher {
        // `Online`: the panic is the assertion — `Offline` would refuse first and pass blind.
        fn network(&self) -> jals_classpath::NetworkPolicy {
            jals_classpath::NetworkPolicy::Online
        }

        async fn fetch_admitted(&self, locator: &str) -> Result<Vec<u8>, String> {
            panic!("this graph must not fetch, but asked for `{locator}`")
        }
    }

    #[test]
    fn scheduler_invokes_every_node_kind_once() {
        jals_exec::block_on_inline(async {
            let mut storage = MemoryStorage::memory(CodeTree::default());
            let view = storage.view();
            let source = || SourceNode {
                view: view.clone(),
                authored_sources: Vec::new(),
                classpath: Vec::new(),
            };
            let nodes = vec![
                ResolvedNode {
                    id: NodeId::from_identity(b"binary"),
                    location: "https://example.invalid/dependency.jar".to_owned(),
                    body: NodeBody::Binary(BinaryInput::External {
                        locator: "https://example.invalid/dependency.jar".to_owned(),
                    }),
                },
                ResolvedNode {
                    id: NodeId::from_identity(b"plain"),
                    location: "plain".to_owned(),
                    body: NodeBody::PlainSource(source()),
                },
                ResolvedNode {
                    id: NodeId::from_identity(b"jals"),
                    location: "jals".to_owned(),
                    body: NodeBody::JalsSource {
                        source: source(),
                        manifest: Box::new(Manifest::default()),
                    },
                },
            ];
            let exec = Exec::inline();
            let graph = ResolvedProjectGraph {
                nodes,
                edges: Vec::new(),
                order: vec![0, 1, 2],
                warnings: Vec::new(),
                native: NativeGraphState {
                    watch_paths: Vec::new(),
                },
            }
            .preprocess(
                storage.artifacts_mut(),
                crate::graph::GraphPreprocess {
                    exec: &exec,
                    fetcher: &UnreachableFetcher,
                    environment: &BuildScriptEnvironment::new(),
                    root_features: &ResolvedBuildFeatures::default(),
                    limits: &BuildScriptLimits::default(),
                },
            )
            .await
            .unwrap();
            assert_eq!(graph.exports.len(), 3);
        });
    }

    #[test]
    fn a_path_dependency_edge_features_reach_its_build_script() {
        // The per-node union lives in `graph.rs` and is shared, but reading `features` off a
        // `[dependencies]` entry and putting it on the edge is written once here and once in
        // `memory.rs`. Covering only the memory builder would let an omission on this side ship
        // silently, since nothing else observes the difference.
        jals_exec::tokio_rt::run(|exec| async move {
            let project = tempfile::TempDir::new().unwrap();
            let write = |path: &str, contents: &str| {
                let path = project.path().join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, contents).unwrap();
            };
            write(
                "dep/jals.toml",
                "[features]\nhello = []\n\
                 [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
            );
            write(
                "dep/build.rhai",
                r#"
                    for name in ["hello", "root-only"] {
                        if build.feature(name) {
                            let source = output.write_text(name + ".java", "class X {}");
                            build.add_source(source);
                        }
                    }
                "#,
            );
            let root: Manifest =
                "[dependencies]\ndep = { path = \"dep\", features = [\"hello\"] }\n"
                    .parse()
                    .unwrap();

            let mut storage = MemoryStorage::memory(CodeTree::default());
            let graph =
                NativeProjectGraph::discover(&root, project.path(), &exec, NetworkPolicy::Offline)
                    .await
                    .unwrap()
                    .preprocess(
                        storage.artifacts_mut(),
                        crate::graph::GraphPreprocess {
                            exec: &exec,
                            fetcher: &UnreachableFetcher,
                            // A root selection the dependency must not inherit.
                            environment: &BuildScriptEnvironment::new()
                                .with_features(BTreeSet::from(["root-only".to_owned()])),
                            root_features: &ResolvedBuildFeatures::default(),
                            limits: &BuildScriptLimits::default(),
                        },
                    )
                    .await
                    .unwrap();

            let generated: Vec<String> = graph
                .exports
                .values()
                .flat_map(|exports| exports.sources.iter())
                .filter_map(|file| file.path.name().map(ToString::to_string))
                .collect();
            assert_eq!(generated, ["hello.java"]);
        })
        .unwrap();
    }
}
