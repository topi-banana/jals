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
use jals_config::{Dependency, Manifest};
use jals_storage::{ArtifactCache, CacheBackend, ContentDigest, ProjectView, RelativePath};

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
    pub fn token(&self) -> String {
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
    pub from: Option<NodeId>,
    pub dependency: String,
    pub to: NodeId,
    /// Whether a binary dependency requests recursive nested-jar extraction.
    pub recursive: bool,
    /// The build features this edge's `[dependencies]` entry enables in the target project. Empty
    /// for a binary node, which has no build script. One edge is not the whole story: the set a
    /// node's script actually sees is the union over its incoming edges, computed by
    /// [`ResolvedProjectGraph::preprocess`] (Cargo's feature unification).
    pub features: BTreeSet<String>,
}

impl GraphEdge {
    /// One `[dependencies]` entry's declared `features` as the set its edge carries.
    ///
    /// Every name is already known good: both builders reach a dependency's manifest through
    /// `probe_manifest`, whose `parse` validates (and the root is validated by `discover`), so
    /// [`Dependency::validate_features`](jals_config::Dependency::validate_features) has rejected an
    /// empty or reserved name before anything reaches here. The set is unordered on purpose — the
    /// declaration order of a feature list means nothing, and dropping it keeps a node's union
    /// independent of which parent was traversed first.
    pub(crate) fn declared_features(dependency: &Dependency) -> BTreeSet<String> {
        dependency.features().iter().cloned().collect()
    }
}

/// One edge in a deterministic cycle diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleEdge {
    pub from: NodeId,
    pub dependency: String,
    pub to: NodeId,
}

/// Stable read-only node metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNodeMetadata {
    pub id: NodeId,
    pub kind: NodeKind,
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
    pub fn nodes(&self) -> &[GraphNodeMetadata] {
        &self.nodes
    }

    /// Edges in deterministic manifest traversal order.
    pub fn edges(&self) -> &[GraphEdge] {
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
            Self::BuildScript { node, message } => {
                write!(f, "dependency build script for {node} failed: {message}")
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
    File(CapturedFile),
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
    pub(crate) body: NodeBody,
}

impl ResolvedNode {
    pub(crate) const fn kind(&self) -> NodeKind {
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
    async fn preprocess<C: CacheBackend>(
        &self,
        cache: &mut ArtifactCache<C>,
        environment: &BuildScriptEnvironment,
        features: BTreeSet<String>,
        limits: &BuildScriptLimits,
    ) -> Result<NodeExports, GraphError> {
        let NodeBody::JalsSource { source, manifest } = &self.body else {
            return Ok(NodeExports::default());
        };
        let environment = environment.for_project(manifest, features);
        let prepared = prepare_build_script(
            &source.view,
            cache,
            BuildScriptCacheScope::new(self.id.digest()),
            manifest,
            &environment,
            limits,
        )
        .await
        .map_err(|error| GraphError::BuildScript {
            node: self.id.clone(),
            message: error.to_string(),
        })?;
        let Some(prepared) = prepared else {
            return Ok(NodeExports::default());
        };
        let output = prepared.output(source.view.revision());
        if !output.task_plan.is_empty() {
            return Err(GraphError::BuildScript {
                node: self.id.clone(),
                message:
                    "declarative build tasks are not supported for immutable dependency projects"
                        .to_owned(),
            });
        }
        let mut exports = NodeExports::default();
        for path in &output.generated_sources {
            exports.sources.push(CapturedFile {
                path: path.path().clone(),
                bytes: prepared
                    .file_bytes(&source.view, path)
                    .map_err(|error| GraphError::BuildScript {
                        node: self.id.clone(),
                        message: format!("registered source `{path}` cannot be read: {error}"),
                    })?
                    .to_vec(),
            });
        }
        for path in &output.additional_classpath {
            exports.classpath.push(CapturedFile {
                path: path.path().clone(),
                bytes: prepared
                    .file_bytes(&source.view, path)
                    .map_err(|error| GraphError::BuildScript {
                        node: self.id.clone(),
                        message: format!("registered classpath `{path}` cannot be read: {error}"),
                    })?
                    .to_vec(),
            });
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
}

#[derive(Debug, Default)]
pub(crate) struct NodeExports {
    pub(crate) sources: Vec<CapturedFile>,
    pub(crate) classpath: Vec<CapturedFile>,
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

impl ResolvedProjectGraph {
    pub fn metadata(&self) -> GraphMetadata {
        GraphMetadata::from_graph(&self.nodes, &self.edges)
    }

    pub fn warnings(&self) -> &[GraphWarning] {
        &self.warnings
    }

    /// The build features every node receives: the union of the `features` on its incoming edges.
    ///
    /// Cargo's feature unification, over graph nodes: two `[dependencies]` entries reaching the same
    /// project with different lists give it one set and one build script run, rather than splitting
    /// it into two nodes whose classes would both land on the classpath. A node with no incoming
    /// edge carrying features (every binary node, and any source dependency declared without the
    /// key) is absent from the map and gets the empty set.
    ///
    /// Reading this off the edges rather than tracking it during traversal is what makes it
    /// complete: a node revisited after it is `Complete` still pushes its edge, so a second parent's
    /// features are recorded even though the node itself is never re-entered. `BTreeSet` keeps the
    /// result independent of traversal order.
    fn node_features(&self) -> BTreeMap<NodeId, BTreeSet<String>> {
        let mut features: BTreeMap<NodeId, BTreeSet<String>> = BTreeMap::new();
        for edge in &self.edges {
            if edge.features.is_empty() {
                continue;
            }
            features
                .entry(edge.to.clone())
                .or_default()
                .extend(edge.features.iter().cloned());
        }
        features
    }

    /// Preprocess every resolved node exactly once in dependency-first order.
    pub async fn preprocess<C: CacheBackend>(
        self,
        cache: &mut ArtifactCache<C>,
        environment: &BuildScriptEnvironment,
        limits: &BuildScriptLimits,
    ) -> Result<PreprocessedProjectGraph, GraphError> {
        let features_by_node = self.node_features();
        let mut exports = BTreeMap::new();
        for index in &self.order {
            let node = &self.nodes[*index];
            let features = features_by_node.get(&node.id).cloned().unwrap_or_default();
            let output = node
                .preprocess(cache, environment, features, limits)
                .await?;
            exports.insert(node.id.clone(), output);
        }
        Ok(PreprocessedProjectGraph {
            nodes: self.nodes,
            edges: self.edges,
            warnings: self.warnings,
            exports,
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
    #[cfg(feature = "native")]
    pub(crate) native: crate::native::NativeGraphState,
}

impl PreprocessedProjectGraph {
    pub fn metadata(&self) -> GraphMetadata {
        GraphMetadata::from_graph(&self.nodes, &self.edges)
    }

    pub fn warnings(&self) -> &[GraphWarning] {
        &self.warnings
    }
}
