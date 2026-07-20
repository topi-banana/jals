//! Native recursive path/Git acquisition and root-project input projection.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classpath::{NativeProjectPlan, ProjectInputOptions, ProjectInputs, ReqwestFetcher};
use jals_config::{Dependency, GitDependency, Manifest, PathDependency};
use jals_exec::{Exec, LocalBoxFuture};
use jals_storage::{
    Diagnostic, DirKey, EntryRef, FileKey, MemoryCache, Name, NativeSource, NativeStorage,
    ProjectStorage, ProjectView, RelativePath,
};

use crate::assemble::{CompileClasspathEntry, CompileClasspathFile, ProjectAssemblyError};
use crate::graph::{
    BinaryInput, CapturedClasspathEntry, CapturedFile, CycleEdge, GraphEdge, GraphError,
    GraphMetadata, GraphWarning, NodeBody, NodeId, PreprocessedProjectGraph, ResolvedNode,
    ResolvedProjectGraph, SourceNode,
};

/// A `git` invocation that can never stop waiting for a human.
///
/// Dependency acquisition runs unattended — from `jals build`, but also from the language server
/// while someone is just editing. Git's default behaviour on a private or mistyped remote is to
/// prompt for credentials on the inherited terminal, which would hang the build (or the whole LSP
/// session) with no visible cause. Fail fast instead.
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

/// Native entry point for recursive dependency graph discovery.
pub struct NativeProjectGraph;

/// Fully projected native root plus its preprocessed dependency graph.
#[derive(Debug)]
pub struct NativeProjectAssembly {
    pub graph: GraphMetadata,
    pub plan: jals_classpath::ProjectInputPlan,
    pub inputs: ProjectInputs,
    pub source_roots: Vec<DirKey>,
    pub compile_classpath: Vec<CompileClasspathEntry>,
    pub warnings: Vec<GraphWarning>,
    pub errors: Vec<ProjectAssemblyError>,
    pub watch_paths: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub(crate) struct NativeGraphState {
    watch_paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct GitConfinement {
    checkout: PathBuf,
    stable_repository: String,
}

#[derive(Clone)]
struct DeclaringProject {
    root: PathBuf,
    view: ProjectView,
    confinement: Option<GitConfinement>,
}

struct AcquiredSource {
    id: NodeId,
    root: PathBuf,
    confinement: Option<GitConfinement>,
    watch: bool,
    checkout: Option<tempfile::TempDir>,
}

struct CapturedSnapshot {
    view: ProjectView,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Visiting,
    Complete,
}

struct StackEntry {
    id: NodeId,
    incoming: GraphEdge,
}

struct GraphBuilder {
    exec: Exec,
    nodes: Vec<ResolvedNode>,
    seen_nodes: BTreeSet<NodeId>,
    states: BTreeMap<NodeId, VisitState>,
    edges: Vec<GraphEdge>,
    stack: Vec<StackEntry>,
    order: Vec<usize>,
    warnings: Vec<GraphWarning>,
    watch_paths: BTreeSet<PathBuf>,
}

impl NativeProjectGraph {
    /// Discover all root path/Git dependencies recursively. The root manifest is never searched
    /// upward; every dependency probes exactly its selected root's `jals.toml`.
    pub async fn discover(
        root_manifest: &Manifest,
        root_directory: &Path,
        exec: &Exec,
    ) -> Result<ResolvedProjectGraph, GraphError> {
        root_manifest
            .validate()
            .map_err(|error| GraphError::InvalidRootManifest {
                message: error.to_string(),
            })?;
        let root = GraphBuilder::canonical_project_root(root_directory).await?;
        let snapshot = GraphBuilder::snapshot(&root, exec).await?;
        let declaring = DeclaringProject {
            root,
            view: snapshot.view,
            confinement: None,
        };
        let mut builder = GraphBuilder::new(exec.clone());
        builder.push_snapshot_warnings(None, snapshot.diagnostics);
        builder
            .visit_dependencies(None, &declaring, root_manifest)
            .await?;
        Ok(ResolvedProjectGraph {
            nodes: builder.nodes,
            edges: builder.edges,
            order: builder.order,
            warnings: builder.warnings,
            native: NativeGraphState {
                watch_paths: builder.watch_paths.into_iter().collect(),
            },
        })
    }
}

impl PreprocessedProjectGraph {
    /// Assemble the mode-independent graph plan, then apply `mode` only while projecting root and
    /// graph plans through the existing classpath pipeline.
    pub async fn assemble_native(
        &self,
        root_manifest: &Manifest,
        root_directory: &Path,
        storage: &mut NativeStorage,
        mode: ProjectInputOptions,
    ) -> NativeProjectAssembly {
        let graph_assembly = self.assemble(storage.artifacts_mut()).await;
        let mut root_only = root_manifest.clone();
        root_only.dependencies.clear();
        let (mut inputs, source_roots) =
            NativeProjectPlan::assemble_native(&root_only, root_directory, storage, mode).await;
        let fetcher = ReqwestFetcher::for_project(root_directory.to_path_buf());
        let graph_inputs =
            ProjectInputs::assemble(&fetcher, storage, &graph_assembly.plan, mode).await;

        let binary_nodes: BTreeSet<_> = self
            .nodes
            .iter()
            .filter(|node| matches!(node.body, NodeBody::Binary(_)))
            .map(|node| node.id.clone())
            .collect();
        let mut compile_classpath = graph_assembly.compile_classpath;
        compile_classpath
            .retain(|entry| entry.node().is_none_or(|node| !binary_nodes.contains(node)));
        for key in &graph_inputs.dependency_jars {
            let path = RelativePath::new([
                Name::new("dependencies").expect("constant is portable"),
                Name::new("resolved").expect("constant is portable"),
                Name::new(format!("{}.jar", key.content().to_hex()))
                    .expect("digest-derived file name is portable"),
            ]);
            compile_classpath.push(CompileClasspathEntry::File(CompileClasspathFile {
                node: None,
                path,
                key: key.clone(),
            }));
        }

        inputs.dependency_jars.extend(graph_inputs.dependency_jars);
        inputs
            .classpath_classes
            .extend(graph_inputs.classpath_classes);
        inputs.library_sources.extend(graph_inputs.library_sources);
        inputs
            .source_dep_sources
            .extend(graph_inputs.source_dep_sources);
        inputs.warnings.extend(graph_inputs.warnings);

        NativeProjectAssembly {
            graph: graph_assembly.graph,
            plan: graph_assembly.plan,
            inputs,
            source_roots,
            compile_classpath,
            warnings: graph_assembly.warnings,
            errors: graph_assembly.errors,
            watch_paths: self.native.watch_paths.clone(),
        }
    }
}

impl GraphBuilder {
    const fn new(exec: Exec) -> Self {
        Self {
            exec,
            nodes: Vec::new(),
            seen_nodes: BTreeSet::new(),
            states: BTreeMap::new(),
            edges: Vec::new(),
            stack: Vec::new(),
            order: Vec::new(),
            warnings: Vec::new(),
            watch_paths: BTreeSet::new(),
        }
    }

    fn visit_dependencies<'a>(
        &'a mut self,
        parent: Option<NodeId>,
        declaring: &'a DeclaringProject,
        manifest: &'a Manifest,
    ) -> LocalBoxFuture<'a, Result<(), GraphError>> {
        Box::pin(async move {
            for (name, dependency) in &manifest.dependencies {
                match dependency {
                    Dependency::Jar(jar) => {
                        if let Err(message) = self
                            .visit_binary(
                                parent.clone(),
                                declaring,
                                name,
                                &jar.jar,
                                jar.recursive.unwrap_or(false),
                                false,
                            )
                            .await
                        {
                            self.warnings.push(GraphWarning::dependency(name, message));
                        }
                        if let Some(sources) = &jar.sources
                            && let Err(message) = self
                                .visit_binary(parent.clone(), declaring, name, sources, false, true)
                                .await
                        {
                            self.warnings.push(GraphWarning::dependency(name, message));
                        }
                    }
                    Dependency::Path(path) => {
                        let acquired = match self.acquire_path(declaring, path).await {
                            Ok(acquired) => acquired,
                            Err(message) => {
                                self.warnings.push(GraphWarning::dependency(name, message));
                                continue;
                            }
                        };
                        self.visit_source(parent.clone(), name, acquired).await?;
                    }
                    Dependency::Git(git) => {
                        let acquired = match self.acquire_git(declaring, name, git).await {
                            Ok(acquired) => acquired,
                            Err(message) => {
                                self.warnings.push(GraphWarning::dependency(name, message));
                                continue;
                            }
                        };
                        self.visit_source(parent.clone(), name, acquired).await?;
                    }
                }
            }
            Ok(())
        })
    }

    async fn visit_source(
        &mut self,
        parent: Option<NodeId>,
        dependency: &str,
        mut acquired: AcquiredSource,
    ) -> Result<(), GraphError> {
        let checkout = acquired.checkout.take();
        let result = self.visit_source_inner(parent, dependency, acquired).await;
        let cleanup = if let Some(checkout) = checkout {
            jals_exec::tokio_rt::on_blocking_pool(move || {
                checkout
                    .close()
                    .map_err(|error| format!("removing temporary Git checkout: {error}"))
            })
            .await
        } else {
            Ok(())
        };
        result?;
        // Removing the scratch checkout is housekeeping, not part of resolving the graph. On
        // Windows an antivirus or indexer holding a handle makes this fail routinely, and failing
        // the whole build over a leftover temp directory leaves the user no way forward. Report it
        // and move on; the directory is under the OS temp root either way.
        if let Err(message) = cleanup {
            self.warnings.push(GraphWarning::dependency(
                dependency,
                format!("could not remove the temporary Git checkout: {message}"),
            ));
        }
        Ok(())
    }

    async fn visit_source_inner(
        &mut self,
        parent: Option<NodeId>,
        dependency: &str,
        acquired: AcquiredSource,
    ) -> Result<(), GraphError> {
        let incoming = GraphEdge {
            from: parent,
            dependency: dependency.to_owned(),
            to: acquired.id.clone(),
            recursive: false,
        };
        self.edges.push(incoming.clone());
        match self.states.get(&acquired.id) {
            Some(VisitState::Complete) => return Ok(()),
            Some(VisitState::Visiting) => return Err(self.cycle(&incoming)),
            None => {}
        }

        let snapshot = Self::snapshot(&acquired.root, &self.exec).await?;
        let manifest = Self::probe_manifest(&acquired, &snapshot)?;
        self.push_snapshot_warnings(Some(&acquired.id), snapshot.diagnostics);
        let view = snapshot.view;
        let declaring = DeclaringProject {
            root: acquired.root.clone(),
            view: view.clone(),
            confinement: acquired.confinement.clone(),
        };
        let (body, child_manifest) = if let Some(manifest) = manifest {
            let authored_sources = self.capture_manifest_sources(&declaring, &manifest).await;
            let classpath = self.capture_manifest_classpath(&declaring, &manifest).await;
            (
                NodeBody::JalsSource {
                    source: SourceNode {
                        view,
                        authored_sources,
                        classpath,
                    },
                    manifest: Box::new(manifest.clone()),
                },
                Some(manifest),
            )
        } else {
            (
                NodeBody::PlainSource(SourceNode {
                    authored_sources: Self::capture_plain_sources(&view),
                    view,
                    classpath: Vec::new(),
                }),
                None,
            )
        };
        let index = self.nodes.len();
        self.seen_nodes.insert(acquired.id.clone());
        self.nodes.push(ResolvedNode {
            id: acquired.id.clone(),
            body,
        });
        self.states
            .insert(acquired.id.clone(), VisitState::Visiting);
        self.stack.push(StackEntry {
            id: acquired.id.clone(),
            incoming,
        });
        if acquired.watch {
            self.watch_paths.insert(acquired.root.clone());
        }
        if let Some(manifest) = child_manifest.as_ref() {
            self.visit_dependencies(Some(acquired.id.clone()), &declaring, manifest)
                .await?;
        }
        self.stack.pop();
        self.states
            .insert(acquired.id.clone(), VisitState::Complete);
        self.order.push(index);
        Ok(())
    }

    async fn visit_binary(
        &mut self,
        parent: Option<NodeId>,
        declaring: &DeclaringProject,
        dependency: &str,
        locator: &str,
        recursive: bool,
        source_archive: bool,
    ) -> Result<(), String> {
        let role = if source_archive { "source" } else { "binary" };
        let (id, input) = if locator.starts_with("http://") || locator.starts_with("https://") {
            let input = if source_archive {
                BinaryInput::ExternalSource {
                    locator: locator.to_owned(),
                }
            } else {
                BinaryInput::External {
                    locator: locator.to_owned(),
                }
            };
            (
                NodeId::from_identity(format!("{role}-external\0{locator}").as_bytes()),
                input,
            )
        } else {
            let raw = locator.strip_prefix("file://").unwrap_or(locator);
            let unresolved = Self::resolve_path(&declaring.root, raw);
            let canonical = Self::canonical_file(&unresolved).await?;
            Self::require_confinement(declaring, &canonical)?;
            let identity = Self::stable_local_identity(declaring, &canonical, role)?;
            let bytes = Self::read_declared_file(declaring, &canonical).await?;
            let logical = Self::logical_file_path(declaring, &canonical)?;
            let captured = CapturedFile {
                path: logical,
                bytes,
            };
            let input = if source_archive {
                BinaryInput::CapturedSource(captured)
            } else {
                BinaryInput::Captured(captured)
            };
            (NodeId::from_identity(identity.as_bytes()), input)
        };
        self.edges.push(GraphEdge {
            from: parent,
            dependency: dependency.to_owned(),
            to: id.clone(),
            recursive,
        });
        if !self.seen_nodes.insert(id.clone()) {
            return Ok(());
        }
        let index = self.nodes.len();
        self.nodes.push(ResolvedNode {
            id,
            body: NodeBody::Binary(input),
        });
        self.order.push(index);
        Ok(())
    }

    async fn acquire_path(
        &self,
        declaring: &DeclaringProject,
        dependency: &PathDependency,
    ) -> Result<AcquiredSource, String> {
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
        Ok(AcquiredSource {
            id: NodeId::from_identity(identity.as_bytes()),
            root,
            confinement,
            watch,
            checkout: None,
        })
    }

    async fn acquire_git(
        &self,
        declaring: &DeclaringProject,
        dependency_name: &str,
        dependency: &GitDependency,
    ) -> Result<AcquiredSource, String> {
        let reference = dependency
            .git_ref(dependency_name)
            .map_err(|error| error.to_string())?;
        let (clone_argument, stable_locator) =
            self.resolve_git_locator(declaring, &dependency.git).await?;
        let checkout_arg = reference.checkout_arg().map(ToOwned::to_owned);
        let selected_dir = dependency.dir.clone();
        let current_directory = declaring.root.clone();
        let (temporary, checkout, selected, commit) =
            jals_exec::tokio_rt::on_blocking_pool(move || {
                let temporary = tempfile::tempdir()
                    .map_err(|error| format!("creating temporary Git checkout: {error}"))?;
                let checkout = temporary.path().join("checkout");
                let clone = git_command()
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
                    let output = git_command()
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
                let head = git_command()
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
                let checkout = fs::canonicalize(&checkout)
                    .map_err(|error| format!("canonicalizing Git checkout: {error}"))?;
                let selected = selected_dir
                    .as_deref()
                    .map_or_else(|| checkout.clone(), |dir| checkout.join(dir));
                let selected = fs::canonicalize(&selected)
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
        Ok(AcquiredSource {
            id: NodeId::from_identity(identity.as_bytes()),
            root: if selected.is_empty() {
                checkout.clone()
            } else {
                Self::resolve_path(&checkout, &selected)
            },
            confinement: Some(GitConfinement {
                checkout,
                stable_repository,
            }),
            watch: false,
            checkout: Some(temporary),
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

    fn probe_manifest(
        acquired: &AcquiredSource,
        snapshot: &CapturedSnapshot,
    ) -> Result<Option<Manifest>, GraphError> {
        let key = FileKey::parse("jals.toml").expect("constant is a portable file key");
        let file = match snapshot.view.tree().lookup_file(&key) {
            Some(EntryRef::File(file)) => file,
            Some(EntryRef::Directory(_)) => {
                return Err(GraphError::Acquisition {
                    operation: format!("reading dependency manifest for {}", acquired.id),
                    message: "`jals.toml` is not a file".to_owned(),
                });
            }
            None => {
                let path = acquired.root.join("jals.toml");
                if let Some(message) =
                    snapshot
                        .diagnostics
                        .iter()
                        .find_map(|diagnostic| match diagnostic {
                            Diagnostic::UnreadableEntry(message)
                                if message
                                    .strip_prefix(path.to_string_lossy().as_ref())
                                    .is_some_and(|suffix| suffix.starts_with(':')) =>
                            {
                                Some(message.clone())
                            }
                            Diagnostic::SymlinkEscapesRoot(logical)
                            | Diagnostic::SymlinkCycle(logical)
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
                {
                    return Err(GraphError::Acquisition {
                        operation: format!("reading dependency manifest for {}", acquired.id),
                        message,
                    });
                }
                return Ok(None);
            }
        };
        let text = file.text().map_err(|error| GraphError::MalformedManifest {
            node: acquired.id.clone(),
            location: format!("dependencies/{}/jals.toml", acquired.id.token()),
            message: error.to_string(),
        })?;
        text.parse::<Manifest>()
            .map(Some)
            .map_err(|error| GraphError::MalformedManifest {
                node: acquired.id.clone(),
                location: format!("dependencies/{}/jals.toml", acquired.id.token()),
                message: error.to_string(),
            })
    }

    fn push_snapshot_warnings(&mut self, node: Option<&NodeId>, diagnostics: Vec<Diagnostic>) {
        self.warnings
            .extend(diagnostics.into_iter().map(|diagnostic| GraphWarning {
                node: node.cloned(),
                dependency: None,
                message: format!("dependency snapshot: {diagnostic:?}"),
            }));
    }

    async fn capture_manifest_sources(
        &mut self,
        declaring: &DeclaringProject,
        manifest: &Manifest,
    ) -> Vec<CapturedFile> {
        let mut files = BTreeMap::new();
        for (index, source) in manifest.build.source_dirs.iter().enumerate() {
            let physical = Self::resolve_path(&declaring.root, source);
            let canonical = match Self::canonical_directory(&physical).await {
                Ok(path) => path,
                Err(message) => {
                    self.warnings.push(GraphWarning::dependency(
                        source,
                        format!("source directory is unavailable: {message}"),
                    ));
                    continue;
                }
            };
            if let Err(message) = Self::require_confinement(declaring, &canonical) {
                self.warnings
                    .push(GraphWarning::dependency(source, message));
                continue;
            }
            if let Some(relative) = RelativePath::from_host_path(&declaring.root, &canonical) {
                let root = DirKey::new(relative);
                if declaring.view.directory(&root).is_ok() {
                    for file in declaring
                        .view
                        .tree()
                        .files_under(&root)
                        .filter(|file| file.key().has_extension("java"))
                    {
                        files.insert(file.key().path().clone(), file.bytes().to_vec());
                    }
                    continue;
                }
            }
            match Self::snapshot(&canonical, &self.exec).await {
                Ok(snapshot) => {
                    for diagnostic in snapshot.diagnostics {
                        self.warnings.push(GraphWarning::dependency(
                            source,
                            format!("source snapshot: {diagnostic:?}"),
                        ));
                    }
                    let prefix = RelativePath::new([Name::new(format!("external-source-{index}"))
                        .expect("generated prefix is portable")]);
                    for file in snapshot
                        .view
                        .tree()
                        .files_under(&DirKey::ROOT)
                        .filter(|file| file.key().has_extension("java"))
                    {
                        files.insert(prefix.concat(file.key().path()), file.bytes().to_vec());
                    }
                }
                Err(error) => self.warnings.push(GraphWarning::dependency(
                    source,
                    format!("source snapshot failed: {error}"),
                )),
            }
        }
        files
            .into_iter()
            .map(|(path, bytes)| CapturedFile { path, bytes })
            .collect()
    }

    fn capture_plain_sources(view: &ProjectView) -> Vec<CapturedFile> {
        let root = ["src/main/java", "src"]
            .into_iter()
            .filter_map(|path| DirKey::parse(path).ok())
            .find(|path| view.directory(path).is_ok())
            .unwrap_or(DirKey::ROOT);
        view.tree()
            .files_under(&root)
            .filter(|file| file.key().has_extension("java"))
            .map(|file| CapturedFile {
                path: file.key().path().clone(),
                bytes: file.bytes().to_vec(),
            })
            .collect()
    }

    async fn capture_manifest_classpath(
        &mut self,
        declaring: &DeclaringProject,
        manifest: &Manifest,
    ) -> Vec<CapturedClasspathEntry> {
        let mut entries = Vec::new();
        for (index, entry) in manifest.build.classpath.iter().enumerate() {
            let unresolved = Self::resolve_path(&declaring.root, entry);
            let canonical = match Self::canonical_existing(&unresolved).await {
                Ok(path) => path,
                Err(message) => {
                    self.warnings.push(GraphWarning::dependency(
                        entry,
                        format!("classpath entry is unavailable: {message}"),
                    ));
                    continue;
                }
            };
            if let Err(message) = Self::require_confinement(declaring, &canonical) {
                self.warnings.push(GraphWarning::dependency(entry, message));
                continue;
            }
            let metadata = match Self::is_file(&canonical).await {
                Ok(is_file) => is_file,
                Err(message) => {
                    self.warnings.push(GraphWarning::dependency(entry, message));
                    continue;
                }
            };
            if metadata {
                match Self::read_declared_file(declaring, &canonical).await {
                    Ok(bytes) => {
                        let logical = RelativePath::from_host_path(&declaring.root, &canonical)
                            .filter(|path| !path.is_root())
                            .map_or_else(|| Self::external_classpath_file(index, &canonical), Ok);
                        match logical {
                            Ok(path) => entries
                                .push(CapturedClasspathEntry::File(CapturedFile { path, bytes })),
                            Err(message) => {
                                self.warnings.push(GraphWarning::dependency(entry, message));
                            }
                        }
                    }
                    Err(message) => self.warnings.push(GraphWarning::dependency(entry, message)),
                }
                continue;
            }
            let relative = RelativePath::from_host_path(&declaring.root, &canonical);
            let (view, path, root, diagnostics) = if let Some(relative) = relative {
                (
                    declaring.view.clone(),
                    relative.clone(),
                    DirKey::new(relative),
                    Vec::new(),
                )
            } else {
                match Self::snapshot(&canonical, &self.exec).await {
                    Ok(snapshot) => (
                        snapshot.view,
                        RelativePath::new([Name::new(format!("external-classpath-{index}"))
                            .expect("generated prefix is portable")]),
                        DirKey::ROOT,
                        snapshot.diagnostics,
                    ),
                    Err(error) => {
                        self.warnings.push(GraphWarning::dependency(
                            entry,
                            format!("classpath snapshot failed: {error}"),
                        ));
                        continue;
                    }
                }
            };
            for diagnostic in diagnostics {
                self.warnings.push(GraphWarning::dependency(
                    entry,
                    format!("classpath snapshot: {diagnostic:?}"),
                ));
            }
            let prefix_len = root.path().segments().len();
            let members = view
                .tree()
                .files_under(&root)
                .filter(|file| file.key().has_extension("class"))
                .map(|file| CapturedFile {
                    path: RelativePath::new(file.key().path().segments().skip(prefix_len).cloned()),
                    bytes: file.bytes().to_vec(),
                })
                .collect();
            entries.push(CapturedClasspathEntry::Tree { path, members });
        }
        entries
    }

    fn external_classpath_file(index: usize, canonical: &Path) -> Result<RelativePath, String> {
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "dependency file name is not portable UTF-8".to_owned())?;
        Ok(RelativePath::new([
            Name::new(format!("external-classpath-{index}")).expect("generated prefix is portable"),
            Name::new(name)
                .map_err(|error| format!("dependency file name is not portable: {error:?}"))?,
        ]))
    }

    fn cycle(&self, closing: &GraphEdge) -> GraphError {
        let position = self
            .stack
            .iter()
            .position(|entry| entry.id == closing.to)
            .expect("visiting node is on the DFS stack");
        let mut chain: Vec<_> = self.stack[position + 1..]
            .iter()
            .map(|entry| CycleEdge {
                from: entry
                    .incoming
                    .from
                    .clone()
                    .expect("cycle edges are between dependency nodes"),
                dependency: entry.incoming.dependency.clone(),
                to: entry.id.clone(),
            })
            .collect();
        chain.push(CycleEdge {
            from: closing
                .from
                .clone()
                .expect("cycle closing edge has a dependency parent"),
            dependency: closing.dependency.clone(),
            to: closing.to.clone(),
        });
        GraphError::Cycle { chain }
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

    fn logical_file_path(
        declaring: &DeclaringProject,
        canonical: &Path,
    ) -> Result<RelativePath, String> {
        if let Some(relative) = RelativePath::from_host_path(&declaring.root, canonical)
            && !relative.is_root()
        {
            return Ok(relative);
        }
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "dependency file name is not portable UTF-8".to_owned())?;
        Ok(RelativePath::new([Name::new(name).map_err(|error| {
            format!("dependency file name is not portable: {error:?}")
        })?]))
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

    async fn canonical_project_root(path: &Path) -> Result<PathBuf, GraphError> {
        Self::canonical_directory(path)
            .await
            .map_err(|message| GraphError::Acquisition {
                operation: "resolving project root".to_owned(),
                message,
            })
    }

    async fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
        let path = path.to_path_buf();
        jals_exec::tokio_rt::on_blocking_pool(move || {
            let canonical = fs::canonicalize(&path)
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
            fs::canonicalize(&path)
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

#[cfg(test)]
mod tests {
    use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
    use jals_storage::{CodeTree, MemoryStorage};

    use super::*;

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
                    body: NodeBody::Binary(BinaryInput::External {
                        locator: "https://example.invalid/dependency.jar".to_owned(),
                    }),
                },
                ResolvedNode {
                    id: NodeId::from_identity(b"plain"),
                    body: NodeBody::PlainSource(source()),
                },
                ResolvedNode {
                    id: NodeId::from_identity(b"jals"),
                    body: NodeBody::JalsSource {
                        source: source(),
                        manifest: Box::new(Manifest::default()),
                    },
                },
            ];
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
                &BuildScriptEnvironment::new(),
                &BuildScriptLimits::default(),
            )
            .await
            .unwrap();
            assert_eq!(graph.exports.len(), 3);
        });
    }
}
