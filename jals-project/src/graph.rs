//! Portable project graph and its resolved-to-preprocessed phase transition.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use jals_build::build_script::{
    BuildScriptCacheScope, BuildScriptDiagnostic, BuildScriptEnvironment, BuildScriptLimits,
    prepare_build_script,
};
use jals_build::task::TaskPlan;
use jals_classpath::{
    ClasspathCoverage, ClasspathEntry, ExternalLocator, Fetcher, LibrarySource, NetworkPolicy,
    WarningOrigin,
};
use jals_config::{Dependency, Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, ContentDigest, DirKey, FileKey, ProjectView,
    RelativePath,
};

use crate::task::{
    BuildTaskExecution, BuildTaskExecutor, BuildTaskPublication, SnapshotTaskOptions, TaskRuntime,
};

/// Stable opaque identity of a resolved dependency node.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(ContentDigest);

impl NodeId {
    pub(crate) fn from_identity(identity: &[u8]) -> Self {
        Self(ContentDigest::of(identity))
    }

    pub(crate) const fn digest(&self) -> ContentDigest {
        self.0
    }

    /// Stable token suitable for collision-free logical artifact paths.
    pub(crate) fn token(&self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.token())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.token())
    }
}

/// Classification of one graph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeKind {
    Binary,
    PlainSource,
    JalsSource,
}

/// One dependency-name-labeled edge. The label is deliberately not part of node identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    /// `None` denotes the root project, which is not itself a dependency node.
    pub(crate) from: Option<NodeId>,
    pub(crate) dependency: String,
    pub(crate) to: NodeId,
    /// Whether a binary dependency requests recursive nested-jar extraction.
    pub(crate) recursive: bool,
    /// The build features this edge's `[dependencies]` entry enables in the target project. Empty
    /// for a binary node, which has no build script. Purely what the manifest declared: the entry's
    /// `features` list and nothing else — what the declaring project's own `[features]` forwards
    /// through a `<dependency>/<feature>` entry depends on its resolved selection, so it is applied
    /// by [`ResolvedProjectGraph::resolve_node_features`] rather than baked in here.
    pub(crate) features: BTreeSet<String>,
    /// Whether this edge lets the target resolve its own `[features] default` list
    /// (`default-features`, `true` unless the entry says otherwise; always `true` for a binary
    /// node, which receives no features at all).
    pub(crate) default_features: bool,
}

/// What one `[dependencies]` entry declares about its target's build features, kept together so a
/// builder cannot carry one half of the pair across a boundary and forget the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredEdgeFeatures {
    pub(crate) features: BTreeSet<String>,
    pub(crate) default_features: bool,
}

impl DeclaredEdgeFeatures {
    /// Read one `[dependencies]` entry.
    ///
    /// Every name is already known good: both builders reach a dependency's manifest through
    /// `probe_manifest`, whose `parse` validates (and the root is validated by `discover`), so
    /// [`Dependency::validate_features`](jals_config::Dependency::validate_features) has rejected an
    /// empty, reserved, or cross-package name before anything reaches here. The set is unordered on
    /// purpose — the declaration order of a feature list means nothing, and dropping it keeps a
    /// node's union independent of which parent was traversed first.
    pub(crate) fn of(dependency: &Dependency) -> Self {
        Self {
            features: dependency.features().iter().cloned().collect(),
            default_features: dependency.default_features(),
        }
    }

    /// What a binary edge declares: nothing. A jar contributes compiled classes and runs no build
    /// script, so the `jar` form carries neither key at all (writing one is a parse error).
    pub(crate) const fn binary() -> Self {
        Self {
            features: BTreeSet::new(),
            default_features: true,
        }
    }
}

/// One edge in a deterministic cycle diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleEdge {
    pub(crate) from: NodeId,
    pub(crate) dependency: String,
    pub(crate) to: NodeId,
}

/// Stable read-only node metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNodeMetadata {
    pub(crate) id: NodeId,
    pub(crate) kind: NodeKind,
}

/// Read-only graph projection retained by assembly products.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphMetadata {
    nodes: Vec<GraphNodeMetadata>,
    edges: Vec<GraphEdge>,
}

impl GraphMetadata {
    fn from_graph(nodes: &[ResolvedNode], edges: &[GraphEdge]) -> Self {
        Self {
            nodes: nodes
                .iter()
                .map(|node| GraphNodeMetadata {
                    id: node.id.clone(),
                    kind: node.kind(),
                })
                .collect(),
            edges: edges.to_vec(),
        }
    }

    /// Nodes in deterministic parent/discovery order.
    #[cfg(test)]
    pub(crate) fn nodes(&self) -> &[GraphNodeMetadata] {
        &self.nodes
    }

    /// Edges in deterministic manifest traversal order.
    #[cfg(test)]
    pub(crate) fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }
}

/// Non-fatal graph discovery or preprocessing diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphWarning {
    pub node: Option<NodeId>,
    pub dependency: Option<String>,
    pub message: String,
}

impl GraphWarning {
    pub(crate) fn dependency(name: &str, message: impl Into<String>) -> Self {
        Self {
            node: None,
            dependency: Some(name.to_owned()),
            message: message.into(),
        }
    }

    pub(crate) fn node(node: NodeId, message: impl Into<String>) -> Self {
        Self {
            node: Some(node),
            dependency: None,
            message: message.into(),
        }
    }
}

/// Structured hard failure from graph discovery or preprocessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    InvalidRootManifest {
        message: String,
    },
    InvalidDependency {
        declaring: Option<NodeId>,
        dependency: String,
        message: String,
    },
    MalformedManifest {
        node: NodeId,
        location: String,
        message: String,
    },
    Cycle {
        chain: Vec<CycleEdge>,
    },
    BuildScript {
        node: NodeId,
        /// The node's [`location`](ResolvedNode::location) — a digest alone tells a reader nothing
        /// about which of their dependencies to go and look at.
        location: String,
        message: String,
    },
    Acquisition {
        operation: String,
        message: String,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRootManifest { message } => write!(f, "invalid root manifest: {message}"),
            Self::InvalidDependency {
                dependency,
                message,
                ..
            } => write!(f, "invalid dependency `{dependency}`: {message}"),
            Self::MalformedManifest {
                location, message, ..
            } => write!(f, "malformed dependency manifest `{location}`: {message}"),
            Self::Cycle { chain } => {
                f.write_str("dependency cycle")?;
                for edge in chain {
                    write!(f, " {} -[{}]-> {}", edge.from, edge.dependency, edge.to)?;
                }
                Ok(())
            }
            Self::BuildScript {
                location, message, ..
            } => {
                write!(f, "dependency build script `{location}` failed: {message}")
            }
            Self::Acquisition { operation, message } => write!(f, "{operation}: {message}"),
        }
    }
}

impl core::error::Error for GraphError {}

#[derive(Debug, Clone)]
pub(crate) struct CapturedFile {
    pub(crate) path: RelativePath,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum CapturedClasspathEntry {
    /// One captured file, beside what `[build] classpath` spelled to reach it.
    ///
    /// The two are not the same string, and only one of them is a location a user can act on: the
    /// captured path is where discovery *put* the bytes, which is declaring-relative for an entry
    /// inside the project but a synthesized `external-classpath-<n>/<name>` for one outside it.
    /// Naming that in a diagnostic sends a reader looking for a file nobody wrote and nothing has.
    File {
        declared: String,
        file: CapturedFile,
    },
    Tree {
        path: RelativePath,
        members: Vec<CapturedFile>,
    },
}

#[derive(Debug)]
pub(crate) enum BinaryInput {
    Captured(CapturedFile),
    External { locator: String },
    CapturedSource(CapturedFile),
    ExternalSource { locator: String },
}

#[derive(Debug)]
pub(crate) struct SourceNode {
    pub(crate) view: ProjectView,
    pub(crate) authored_sources: Vec<CapturedFile>,
    pub(crate) classpath: Vec<CapturedClasspathEntry>,
}

#[derive(Debug)]
pub(crate) enum NodeBody {
    Binary(BinaryInput),
    PlainSource(SourceNode),
    JalsSource {
        source: SourceNode,
        manifest: Box<Manifest>,
    },
}

#[derive(Debug)]
pub(crate) struct ResolvedNode {
    pub(crate) id: NodeId,
    /// Where this node came from, in whatever terms its host used to acquire it: a declaring-
    /// relative path, a host directory, a clone URL. Diagnostics only — node identity is
    /// [`id`](Self::id), and two hosts may well describe one node differently.
    pub(crate) location: String,
    pub(crate) body: NodeBody,
}

impl ResolvedNode {
    const fn kind(&self) -> NodeKind {
        match &self.body {
            NodeBody::Binary(_) => NodeKind::Binary,
            NodeBody::PlainSource(_) => NodeKind::PlainSource,
            NodeBody::JalsSource { .. } => NodeKind::JalsSource,
        }
    }

    pub(crate) const fn source(&self) -> Option<&SourceNode> {
        match &self.body {
            NodeBody::PlainSource(source) | NodeBody::JalsSource { source, .. } => Some(source),
            NodeBody::Binary(_) => None,
        }
    }

    /// The scheduler calls this method uniformly for every node kind. Binary and legacy source
    /// nodes intentionally do nothing; only a manifest-backed source node prepares a script.
    ///
    /// `features` is this node's own build-feature set (see
    /// [`ResolvedProjectGraph::node_features`]), which replaces whatever the declaring project
    /// selected — features never cross a project boundary.
    async fn preprocess<F: Fetcher, C: CacheBackend>(
        &self,
        cache: &mut ArtifactCache<C>,
        features: BTreeSet<String>,
        options: &GraphPreprocess<'_, F>,
    ) -> Result<NodeExports, GraphError> {
        let NodeBody::JalsSource { source, manifest } = &self.body else {
            return Ok(NodeExports::default());
        };
        let environment = options.environment.for_project(manifest, features.clone());
        let prepared = prepare_build_script(
            &source.view,
            cache,
            BuildScriptCacheScope::new(self.id.digest()),
            manifest,
            &environment,
            options.limits,
        )
        .await
        .map_err(|error| self.script_error(error.to_string()))?;
        let Some(prepared) = prepared else {
            return Ok(NodeExports::default());
        };
        let output = prepared.output(source.view.revision());
        let mut exports = NodeExports::default();
        for path in &output.generated_sources {
            exports.sources.push(CapturedFile {
                path: path.path().clone(),
                bytes: prepared
                    .file_bytes(&source.view, path)
                    .map_err(|error| {
                        self.script_error(format!(
                            "registered source `{path}` cannot be read: {error}"
                        ))
                    })?
                    .to_vec(),
            });
        }
        for path in &output.additional_classpath {
            exports.classpath.push(CapturedFile {
                path: path.path().clone(),
                bytes: prepared
                    .file_bytes(&source.view, path)
                    .map_err(|error| {
                        self.script_error(format!(
                            "registered classpath `{path}` cannot be read: {error}"
                        ))
                    })?
                    .to_vec(),
            });
        }
        if !output.task_plan.is_empty() {
            let execution = self
                .run_task_plan(cache, &output.task_plan, &features, options, &source.view)
                .await?;
            exports.task_classpath = execution.classpath;
            exports.library_sources = self.navigation_sources(manifest, &execution.publications)?;
            exports.warnings.extend(
                self.diagnose_unbacked_publications(
                    cache,
                    source,
                    manifest,
                    &exports.classpath,
                    &exports.task_classpath,
                    &execution.publications,
                )
                .await,
            );
        }
        exports
            .warnings
            .extend(
                output
                    .diagnostics
                    .iter()
                    .filter_map(|diagnostic| match diagnostic {
                        BuildScriptDiagnostic::Warning(message) => Some(message.clone()),
                        BuildScriptDiagnostic::Error(_) => None,
                    }),
            );
        if let Err(message) = prepared.persist(cache).await {
            exports.warnings.push(format!(
                "could not persist prepared build-script artifacts: {message}"
            ));
        }
        Ok(exports)
    }

    /// Run this node's declarative task plan against its own immutable snapshot.
    ///
    /// Nothing here writes to the dependency: the executor runs under
    /// [`BuildTaskHost::Snapshot`](crate::BuildTaskHost::Snapshot), so the JARs it produces stay in
    /// the *consumer's* verified cache and the source trees it declares come back as values rather
    /// than as edits to a project the consumer does not own. That is the whole reason a dependency
    /// may declare tasks at all — the snapshot it was captured from is byte-identical afterwards.
    async fn run_task_plan<F: Fetcher, C: CacheBackend>(
        &self,
        cache: &mut ArtifactCache<C>,
        plan: &TaskPlan,
        features: &BTreeSet<String>,
        options: &GraphPreprocess<'_, F>,
        view: &ProjectView,
    ) -> Result<BuildTaskExecution, GraphError> {
        BuildTaskExecutor::execute_snapshot(
            options.exec,
            options.fetcher,
            view,
            cache,
            plan,
            SnapshotTaskOptions {
                identity: self.id.digest(),
                features,
                runtime: TaskRuntime {
                    network: options.network,
                    max_fetch_bytes: options.limits.max_fetch_bytes,
                },
            },
        )
        .await
        .map_err(|error| self.script_error(error.to_string()))
    }

    /// Published trees readdressed the way a consumer sees library sources: by package.
    ///
    /// A destination is written project-relative (`src/main/java/net/minecraft`) because that is
    /// where a *root* project would physically publish it. A consumer never sees the dependency's
    /// directory layout, only its types, so the source root is stripped and what remains is the
    /// package path — which is exactly how extracted `sources` jars and synthesized skeletons are
    /// addressed, so all three agree on where a class lives.
    fn navigation_sources(
        &self,
        manifest: &Manifest,
        publications: &[BuildTaskPublication],
    ) -> Result<Vec<LibrarySource>, GraphError> {
        let mut sources = Vec::new();
        for publication in publications {
            let prefix = self.package_prefix(manifest, &publication.destination)?;
            sources.extend(publication.tree.files.iter().map(|file| LibrarySource {
                path: prefix.concat(&file.path),
                key: file.key.clone(),
            }));
        }
        Ok(sources)
    }

    /// Warn about each published tree this node's classpath does not stand behind.
    ///
    /// Routing a dependency's publications away from the compiler is right for the shape it was
    /// written for — a task that puts a JAR on the classpath *and* publishes readable sources for
    /// the same types, where handing both to `javac` would only duplicate them. Nothing enforces
    /// that shape, though: a publication can equally be the only carrier of a package, and there the
    /// same rule deletes the package outright. The consumer then fails on types this project
    /// believes it exports, several layers away from the declaration that caused it — so the
    /// declaration says so itself, here.
    ///
    /// One case escapes that, and it is *decided* rather than hedged: a project built as a root of
    /// its own leaves its publications on disk, where discovery captured them as ordinary authored
    /// sources that do reach a consumer's compiler. Whether that happened is visible from here — an
    /// authored source under the publication's own package prefix is exactly the trace it leaves —
    /// so the message states which of the two situations this is instead of carrying an escape
    /// clause every reader has to evaluate against a tree they are not looking at. It still warns
    /// either way: an export that exists only because the dependency happens to have been built in
    /// place is a property of its build state and not of its manifest, and `jals clean` there takes
    /// it back.
    ///
    /// Only *this* node's classpath is inspected, and its `[dependencies]` are not late but out of
    /// reach: discovery resolved them into graph nodes before any preprocessing ran, yet a
    /// [`ResolvedNode`] holds no handle to the graph it sits in, and what a dependency finally
    /// contributes is decided at assembly, once every node has been preprocessed. Declaring one is
    /// therefore not a reason to stay silent — that would lose the warning for every root a project
    /// publishes as soon as it gains a single dependency — but it is a reason to say the check could
    /// not see them.
    ///
    /// That hedge covers every dependency kind on purpose. A `git`/`path` dependency's sources reach
    /// the consumer's compiler as
    /// [`source_dependency_artifacts`](jals_classpath::ProjectInputPlan::source_dependency_artifacts),
    /// so narrowing it to `jar` would claim these types cannot come from anywhere else, which is the
    /// same overreach in the other direction.
    async fn diagnose_unbacked_publications<C: CacheBackend>(
        &self,
        cache: &ArtifactCache<C>,
        source: &SourceNode,
        manifest: &Manifest,
        script_classpath: &[CapturedFile],
        task_classpath: &[CacheKey],
        publications: &[BuildTaskPublication],
    ) -> Vec<String> {
        let mut roots = Vec::new();
        for publication in publications {
            // A destination outside every source root is already a hard error from
            // `navigation_sources`, which ran first, so this cannot be reached with one.
            let Ok(prefix) = self.package_prefix(manifest, &publication.destination) else {
                continue;
            };
            roots.push((publication, prefix));
        }
        if roots.is_empty() {
            return Vec::new();
        }

        let mut coverage =
            ClasspathCoverage::seeking(roots.iter().map(|(_, prefix)| prefix.clone()));
        // Held bytes first, cache keys last, because the scan stops as soon as every prefix is
        // covered and the cheap half of a classpath can settle the question the expensive half is
        // never then opened for. What discovery captured is already in memory; a `CacheKey` costs
        // `open_verified`'s whole SHA-256 pass, and the memoized execution just made that same pass
        // over these same artifacts, so folding one here is a second digest rather than a first
        // read. Nothing observable depends on the order: an entry reached only after the answer was
        // settled could not have changed it, and one skipped for the same reason produces no
        // warning that would have been reported anyway.
        //
        // The manifest's own `[build] classpath` is read from what discovery captured, not from the
        // view: a native node may have taken it from a host path that is in no project revision.
        for entry in &source.classpath {
            if coverage.is_complete() {
                break;
            }
            match entry {
                CapturedClasspathEntry::File { declared, file } => {
                    coverage
                        .add_resident(
                            WarningOrigin::External(ExternalLocator::new(declared.clone())),
                            &file.path,
                            &file.bytes,
                        )
                        .await;
                }
                CapturedClasspathEntry::Tree { members, .. } => {
                    for member in members {
                        coverage.add_class(&member.path);
                    }
                }
            }
        }
        // A build script's registered classpath was already read out of the view above, into
        // `NodeExports::classpath`. Reading it a second time as a `ClasspathEntry::ProjectFile`
        // would ask the same revision the same question and copy the answer again.
        for file in script_classpath {
            if coverage.is_complete() {
                break;
            }
            // Every one of these came from a `FileKey` in the view, so the fallback is unreachable
            // rather than a second way of naming the same file.
            let origin = FileKey::new(file.path.clone()).map_or_else(
                |_| WarningOrigin::External(ExternalLocator::new(file.path.to_string())),
                WarningOrigin::ProjectFile,
            );
            coverage.add_resident(origin, &file.path, &file.bytes).await;
        }
        for key in task_classpath {
            if coverage.is_complete() {
                break;
            }
            coverage
                .add_entry(&source.view, cache, &ClasspathEntry::Artifact(key.clone()))
                .await;
        }

        let uncovered: Vec<_> = roots
            .into_iter()
            .filter(|(_, prefix)| !coverage.covers(prefix))
            .collect();
        if uncovered.is_empty() {
            return Vec::new();
        }
        // An entry that could not be read is not an entry that defines nothing. Once something went
        // unread, report *that* rather than turning an I/O failure into a claim about packages —
        // but only here, past the point where the rest of the classpath already answered the
        // question and the unread entry could not have changed it.
        if !coverage.warnings().is_empty() {
            // One report, not one per unread entry. Every one of them withholds the same claim
            // about the same roots, so a message each would restate that root list verbatim as
            // many times as the classpath happened to be unreadable.
            //
            // Withholding the claim is not the same as saying nothing: a reader still has to know
            // which roots were left unanswered, or the report names a broken jar and no reason to
            // care about it.
            let withheld = uncovered
                .iter()
                .map(|(publication, prefix)| format!("`{}` (`{prefix}`)", publication.owner))
                .collect::<Vec<_>>()
                .join(", ");
            let unread = coverage
                .warnings()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return Vec::from([format!(
                "cannot tell whether this project's classpath backs what its build task \
                 publishes — {withheld}: {unread}"
            )]);
        }

        let mut messages: Vec<String> = uncovered
            .iter()
            .map(|(publication, prefix)| {
                // Not a caveat to state unconditionally: a publication left on disk by a root build
                // of this project was captured by discovery as an authored source, inside the very
                // destination the publication names. Captured sources are addressed the way the
                // destination is — relative to the declaring project, not to a source root — so
                // this is the destination and not the package prefix. The one exception to "a
                // consumer has nothing here" is therefore decidable, and saying which case this is
                // beats making every reader check a tree they are not looking at.
                let on_disk = source
                    .authored_sources
                    .iter()
                    .any(|file| file.path.starts_with(publication.destination.path()));
                let state = if on_disk {
                    " These types do reach a consumer today, but only because this project has \
                     itself been built as a root and left the publication on disk as ordinary \
                     source files — `jals clean` here takes them away again, and a consumer that \
                     never built this project directly never had them."
                } else {
                    ""
                };
                format!(
                    "build task publishes `{}` at `{}`, but nothing this project puts on its own \
                     classpath defines a class under `{prefix}`. A dependency's publications are \
                     navigation sources for a reader and never compile inputs, so a consumer has \
                     nothing here to compile against.{state} Put the library's own jar on the \
                     classpath with `tasks.add_classpath`, or declare it as a `[dependencies]` jar.",
                    publication.owner, publication.destination,
                )
            })
            .collect();
        // Carried by the first message rather than by every one of them: the caveat is about what
        // the check could not see, which is one property of the project and not a fact about each
        // root. Repeating it per root is the same sentence as many times as the project publishes.
        if !manifest.dependencies.is_empty()
            && let Some(first) = messages.first_mut()
        {
            first.push_str(
                " This project also declares `[dependencies]`, whose contribution is settled after \
                 this check and is invisible to it — disregard this project's publication warnings \
                 if one of them already carries the types they name.",
            );
        }
        messages
    }

    /// The package prefix a publication destination lies at, or an error if it lies outside every
    /// declared source root — where a consumer has no way to address it.
    fn package_prefix(
        &self,
        manifest: &Manifest,
        destination: &DirKey,
    ) -> Result<RelativePath, GraphError> {
        manifest
            .build
            .source_dirs
            .iter()
            .filter_map(|root| RelativePath::parse(root).ok())
            .filter_map(|root| destination.path().strip_prefix(&root))
            .find(|relative| !relative.is_root())
            .ok_or_else(|| {
                self.script_error(format!(
                    "publication destination `{destination}` must be a strict descendant of a \
                     `[build] source-dirs` entry"
                ))
            })
    }

    fn script_error(&self, message: String) -> GraphError {
        GraphError::BuildScript {
            node: self.id.clone(),
            location: self.location.clone(),
            message,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct NodeExports {
    pub(crate) sources: Vec<CapturedFile>,
    pub(crate) classpath: Vec<CapturedFile>,
    /// JARs a build *task* put on the classpath (`tasks.add_classpath` / `add_nested_classpath`).
    ///
    /// Kept as cache keys rather than bytes: the executor already published them into the same
    /// verified cache assembly reads from, so materializing a remapped game JAR back into memory to
    /// re-publish it under a second key would double the work and the storage for no gain.
    pub(crate) task_classpath: Vec<CacheKey>,
    /// Navigation-only sources a build task published (`tasks.publish_tree`), addressed
    /// package-relative like every other library source. Never a compile input.
    ///
    /// A publication therefore means two different things by position: as the *root* it is written
    /// into the project's source tree and compiles like an authored file, and as a *dependency* it
    /// reaches a reader and stops there. The asymmetry is deliberate — a dependency exports types
    /// through its classpath and publishes a view of them — and
    /// [`diagnose_unbacked_publications`](ResolvedNode::diagnose_unbacked_publications) is what
    /// keeps a project that meant the other thing from finding out in its consumer's `javac`.
    pub(crate) library_sources: Vec<LibrarySource>,
    pub(crate) warnings: Vec<String>,
}

/// Fully discovered graph. Its internals cannot be assembled until [`preprocess`](Self::preprocess)
/// consumes it and returns [`PreprocessedProjectGraph`].
#[derive(Debug)]
pub struct ResolvedProjectGraph {
    pub(crate) nodes: Vec<ResolvedNode>,
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) order: Vec<usize>,
    pub(crate) warnings: Vec<GraphWarning>,
    #[cfg(feature = "native")]
    pub(crate) native: crate::native::NativeGraphState,
}

/// Everything [`ResolvedProjectGraph::preprocess`] needs beyond the cache it writes to.
///
/// A dependency's build script may declare a task plan, and running one needs a fetch capability,
/// an execution context, and a network policy — which is why they travel with the script inputs
/// rather than being reachable from the graph itself. A host that cannot fetch still passes its own
/// `Fetcher`; `network` is what actually decides whether one is used.
pub struct GraphPreprocess<'a, F: Fetcher> {
    pub exec: &'a Exec,
    pub fetcher: &'a F,
    pub environment: &'a BuildScriptEnvironment,
    pub root_features: &'a ResolvedBuildFeatures,
    pub limits: &'a BuildScriptLimits,
    pub network: NetworkPolicy,
}

/// A direct `[dependencies] features` name that its target's `[features]` table does not declare.
struct UndeclaredEdgeFeature {
    /// The node that declared the edge, or `None` for a root edge.
    declaring: Option<NodeId>,
    /// The dependency name the edge points at.
    dependency: String,
    /// The name that appears in no `[features]` key of the target.
    feature: String,
}

impl ResolvedProjectGraph {
    /// The discovered shape, before preprocessing. Production only ever projects the *preprocessed*
    /// graph ([`PreprocessedProjectGraph::metadata`]), so this exists for the crate's own tests.
    #[cfg(test)]
    pub(crate) fn metadata(&self) -> GraphMetadata {
        GraphMetadata::from_graph(&self.nodes, &self.edges)
    }

    /// Discovery warnings. The procedure reads the field directly when it has to carry them past
    /// a failed phase; this borrow is for the crate's own tests.
    #[cfg(test)]
    pub(crate) fn warnings(&self) -> &[GraphWarning] {
        &self.warnings
    }

    /// Every direct `[dependencies] features` name that its target dependency does not declare in
    /// `[features]`.
    ///
    /// Only `edge.features` — the names written directly on a `[dependencies]` entry — are checked.
    /// A `<dependency>/<feature>` forward never reaches here: it arrives through
    /// [`resolve_node_features`](Self::resolve_node_features)'s routing, not on the edge, and stays
    /// deliberately permissive (a project may know a feature its dependency's own table does not).
    /// A manifest-less target is skipped — it has no `[features]` table to check against, matching
    /// the existing rule that a plain-source node keeps what it was sent, inert.
    ///
    /// Every edge is walked, including a second edge to an already-visited node, so a diamond whose
    /// two entries disagree is fully covered — the same reason the per-node union reads the edges
    /// rather than tracking a set during traversal. The scan is over `edges` (discovery order) and
    /// each edge's `features` (`BTreeSet`), so the order is deterministic.
    fn undeclared_edge_features(&self) -> Vec<UndeclaredEdgeFeature> {
        let manifests: BTreeMap<&NodeId, &Manifest> = self
            .nodes
            .iter()
            .filter_map(|node| match &node.body {
                NodeBody::JalsSource { manifest, .. } => Some((&node.id, manifest.as_ref())),
                NodeBody::Binary(_) | NodeBody::PlainSource(_) => None,
            })
            .collect();

        let mut undeclared = Vec::new();
        for edge in &self.edges {
            let Some(manifest) = manifests.get(&edge.to) else {
                continue;
            };
            for feature in &edge.features {
                // The `[features]` keys are the target's complete valid feature namespace; there is
                // no optional-dependency-implies-feature mechanism to widen it.
                if !manifest.features.contains_key(feature) {
                    undeclared.push(UndeclaredEdgeFeature {
                        declaring: edge.from.clone(),
                        dependency: edge.dependency.clone(),
                        feature: feature.clone(),
                    });
                }
            }
        }
        undeclared
    }

    /// The build features every node resolves to, given what the root project selected.
    ///
    /// Two inputs reach a node, and both are written by whoever declares it: the `features` on its
    /// incoming edges, and the `<dependency>/<feature>` entries a declaring project's own
    /// `[features]` forwards once *its* selection is resolved. Their union is closed over the node's
    /// own `[features]` — enables map plus, when any incoming edge allows it, its `default` list —
    /// which is what makes the routing transitive: a mid-graph project forwards to its own
    /// dependencies from features it merely received.
    ///
    /// Cargo's feature unification, over graph nodes: two `[dependencies]` entries reaching the same
    /// project give it one set and one build script run, rather than splitting it into two nodes
    /// whose classes would both land on the classpath. `default-features` unifies the same
    /// (additive) way — one entry asking for the defaults turns them on for the shared node. A node
    /// nobody sends anything to (every binary node, and any source dependency declared bare) is
    /// absent from the map and gets the empty set.
    ///
    /// One pass in reverse [`order`](Self::order) suffices, with no fixpoint iteration: routing only
    /// ever points from a project to its dependency, `order` is the discovery DFS's post-order, and
    /// cycles are already rejected — so its reverse visits every node after every project that can
    /// send to it. `BTreeMap`/`BTreeSet` keep the result independent of traversal order.
    fn resolve_node_features(
        &self,
        root: &ResolvedBuildFeatures,
    ) -> BTreeMap<NodeId, BTreeSet<String>> {
        debug_assert_eq!(
            self.order.iter().copied().collect::<BTreeSet<_>>().len(),
            self.nodes.len(),
            "`order` must be a permutation of the nodes for the reverse pass to be topological"
        );
        // Where each project's `<dependency>/<feature>` entries land. A `jar` emits *two* edges under
        // one dependency name (the jar and its companion `sources` archive), so this index is only
        // unambiguous because `Manifest::validate` rejects routing to a `jar` name — hence source
        // edges only, rather than trusting that the two agree.
        let sources: BTreeSet<&NodeId> = self
            .nodes
            .iter()
            .filter(|node| node.source().is_some())
            .map(|node| &node.id)
            .collect();
        let mut targets: BTreeMap<(Option<&NodeId>, &str), &NodeId> = BTreeMap::new();
        let mut arrived: BTreeMap<NodeId, BTreeSet<String>> = BTreeMap::new();
        let mut defaults: BTreeMap<NodeId, bool> = BTreeMap::new();
        for edge in &self.edges {
            if sources.contains(&edge.to) {
                targets.insert((edge.from.as_ref(), edge.dependency.as_str()), &edge.to);
            }
            arrived
                .entry(edge.to.clone())
                .or_default()
                .extend(edge.features.iter().cloned());
            *defaults.entry(edge.to.clone()).or_default() |= edge.default_features;
        }

        let route = |from: Option<&NodeId>,
                     resolved: &ResolvedBuildFeatures,
                     arrived: &mut BTreeMap<NodeId, BTreeSet<String>>| {
            for (dependency, features) in resolved.dependencies() {
                // A dependency whose acquisition failed has no edge, only a warning; a `path` that
                // resolved to a directory without `jals.toml` has an edge but no manifest to read
                // them. Both are already reported, so routing simply stops here.
                if let Some(to) = targets.get(&(from, dependency)) {
                    arrived
                        .entry((*to).clone())
                        .or_default()
                        .extend(features.iter().cloned());
                }
            }
        };
        route(None, root, &mut arrived);

        let mut features: BTreeMap<NodeId, BTreeSet<String>> = BTreeMap::new();
        for index in self.order.iter().rev() {
            let node = &self.nodes[*index];
            let seed = arrived.remove(&node.id).unwrap_or_default();
            let NodeBody::JalsSource { manifest, .. } = &node.body else {
                // No manifest: nothing to close over and no outgoing edge to forward to. A plain
                // source node keeps what it was sent, inert until it grows a `jals.toml`.
                if !seed.is_empty() {
                    features.insert(node.id.clone(), seed);
                }
                continue;
            };
            let resolved = manifest
                .expand_build_features(seed, defaults.get(&node.id).copied().unwrap_or(false));
            route(Some(&node.id), &resolved, &mut arrived);
            if !resolved.features().is_empty() {
                features.insert(node.id.clone(), resolved.into_features());
            }
        }
        // Every node takes its seed out, so anything left was routed to a node already past — the
        // one way the single pass could quietly drop a feature if `order` stopped being topological.
        debug_assert!(
            arrived.is_empty(),
            "a forwarded feature reached an already-resolved node"
        );
        features
    }

    /// Preprocess every resolved node exactly once in dependency-first order.
    ///
    /// `options.root_features` is the root project's own resolved selection: its queryable half
    /// belongs to the root's script (which the host runs, not this graph), and its
    /// [`dependencies`](ResolvedBuildFeatures::dependencies) half is what the root's `[features]`
    /// forwards into this graph.
    pub(crate) async fn preprocess<F: Fetcher, C: CacheBackend>(
        self,
        cache: &mut ArtifactCache<C>,
        options: GraphPreprocess<'_, F>,
    ) -> Result<PreprocessedProjectGraph, GraphError> {
        // A `[dependencies] features` name the target does not declare is a mistake, not an empty
        // selection: reject it before any build script runs, the way Cargo does, rather than letting
        // it expand to nothing and silently build the default. Fail on the first, in deterministic
        // order.
        if let Some(bad) = self.undeclared_edge_features().into_iter().next() {
            return Err(GraphError::InvalidDependency {
                declaring: bad.declaring,
                dependency: bad.dependency,
                message: format!(
                    "requests feature `{}`, which it does not declare in `[features]`",
                    bad.feature
                ),
            });
        }

        let features_by_node = self.resolve_node_features(options.root_features);
        let mut exports = BTreeMap::new();
        for index in &self.order {
            let node = &self.nodes[*index];
            let features = features_by_node.get(&node.id).cloned().unwrap_or_default();
            let output = node.preprocess(cache, features, &options).await?;
            exports.insert(node.id.clone(), output);
        }
        Ok(PreprocessedProjectGraph {
            nodes: self.nodes,
            edges: self.edges,
            warnings: self.warnings,
            exports,
            features: features_by_node,
            #[cfg(feature = "native")]
            native: self.native,
        })
    }
}

/// Graph whose every node has passed preprocessing. Assembly exists only on this state.
#[derive(Debug)]
pub struct PreprocessedProjectGraph {
    pub(crate) nodes: Vec<ResolvedNode>,
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) warnings: Vec<GraphWarning>,
    pub(crate) exports: BTreeMap<NodeId, NodeExports>,
    /// Each node's unified build-feature selection (from
    /// [`resolve_node_features`](ResolvedProjectGraph::resolve_node_features)), kept so assembly
    /// can hand a node's own features to its dialect frontend (`#[cfg(feature = "…")]`).
    pub(crate) features: BTreeMap<NodeId, BTreeSet<String>>,
    #[cfg(feature = "native")]
    pub(crate) native: crate::native::NativeGraphState,
}

impl PreprocessedProjectGraph {
    pub(crate) fn metadata(&self) -> GraphMetadata {
        GraphMetadata::from_graph(&self.nodes, &self.edges)
    }
}
