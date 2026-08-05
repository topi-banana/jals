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
use jals_build::task::{TaskPlan, TaskPublishIntent};
use jals_classpath::{
    ClasspathCoverage, ExternalLocator, Fetcher, LibrarySource, MappingSpec, NetworkPolicy,
    WarningOrigin,
};
use jals_config::{AmbiguousMapping, Dependency, Manifest, ResolvedBuildFeatures};
use jals_exec::Exec;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, DirKey, FileKey,
    ProjectView, ProvenanceFold, RelativePath,
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
    /// The mapping set this entry's `remap` names, still ungated. `None` when the entry declares
    /// none, or when the name it declares is not a `[mappings]` entry the declaring manifest has.
    pub(crate) remap: Option<EdgeRemap>,
}

/// A `remap` reference resolved against the declaring manifest, with its gate still to apply.
///
/// The two halves travel together because the gate is evaluated somewhere the lowering cannot be:
/// `required-features` is answered by the *declaring project's* resolved selection, which discovery
/// has not computed when it builds this edge and preprocessing settles afterwards. Lowering early
/// and gating late is what lets one lowering serve both, instead of a second one growing in the
/// graph.
///
/// Both halves are private to this module: the pair is only ever read together, and
/// [`active`](EdgeRemap::active) is that reading. An assembler reaching past it for `spec` would be
/// taking the lowering without the gate, which is the one combination this type exists to refuse.
///
/// *Every* alternative of the referenced entry is lowered, not just the one that will turn out to
/// apply, because the selection that decides which is not known here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeRemap {
    /// The entry's alternatives, lowered in declaration order, each with its own gate.
    alternatives: Vec<EdgeAlternative>,
    /// The `[mappings]` key, for the ambiguity message.
    reference: String,
}

/// One lowered alternative and the gate that selects it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EdgeAlternative {
    spec: MappingSpec,
    /// The alternative's `required-features`, verbatim. Conjunctive: every one must be enabled.
    required_features: BTreeSet<String>,
}

impl EdgeRemap {
    /// Read one `[dependencies]` entry's `remap` against the manifest that declared it.
    ///
    /// # Errors
    /// A message naming why the referenced entry could not be lowered, for the builder to attribute
    /// to the declaring project through its own `warn_declared`. `Ok(None)` is an entry with no
    /// `remap` at all.
    pub(crate) fn of(manifest: &Manifest, dependency: &Dependency) -> Result<Option<Self>, String> {
        let Some(reference) = dependency.remap() else {
            return Ok(None);
        };
        let Some(entry) = manifest.mappings.get(reference) else {
            return Err(format!("`remap` names no `[mappings]` entry `{reference}`"));
        };
        let mut alternatives = Vec::with_capacity(entry.alternatives().len());
        for source in entry.alternatives() {
            let mut warnings = Vec::new();
            let Some(spec) = MappingSpec::lower(reference, source, &mut warnings) else {
                // Rendered whole rather than by `message`: several of these name their subject only
                // in the origin, so the message alone would drop the one part a user can act on.
                return Err(warnings.first().map_or_else(
                    || format!("mapping `{reference}` is malformed"),
                    ToString::to_string,
                ));
            };
            alternatives.push(EdgeAlternative {
                spec,
                required_features: source.required_features().iter().cloned().collect(),
            });
        }
        Ok(Some(Self {
            alternatives,
            reference: reference.to_owned(),
        }))
    }

    /// The spec, when `features` satisfies exactly one alternative's gate.
    ///
    /// An unmet gate is not a diagnostic: it is how a manifest says "this selection ships no
    /// mappings", which is the whole reason the key exists.
    ///
    /// # Errors
    /// A message when more than one alternative is active. `Manifest::validate` rejects every table
    /// where that is provable, so reaching this means an unvalidated manifest — and the jar is left
    /// unremapped rather than remapped by whichever alternative came first.
    pub(crate) fn active(
        &self,
        features: &BTreeSet<String>,
    ) -> Result<Option<&MappingSpec>, String> {
        let mut active = self
            .alternatives
            .iter()
            .enumerate()
            .filter(|(_, alternative)| alternative.required_features.is_subset(features));
        let Some((first, alternative)) = active.next() else {
            return Ok(None);
        };
        match active.next() {
            // The same sentence `MappingEntry::active` reports, rendered from the same type: this
            // gate is a second *evaluation* of the manifest's rule, not a second statement of it.
            Some((second, _)) => Err(AmbiguousMapping {
                name: self.reference.clone(),
                first: first + 1,
                second: second + 1,
            }
            .to_string()),
            None => Ok(Some(&alternative.spec)),
        }
    }
}

/// What one `jar` `[dependencies]` entry declares about the edge itself, kept together for the
/// reason [`DeclaredEdgeFeatures`] is: a builder threading these one by one is a builder that can
/// carry three of them across a boundary and forget the fourth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredBinaryEdge {
    /// Whether the entry requests recursive nested-jar extraction.
    pub(crate) recursive: bool,
    /// Whether this edge is the entry's companion `sources` archive rather than its classes. One
    /// entry emits both under one dependency name.
    pub(crate) source_archive: bool,
    /// The entry's `remap`, still ungated. Always `None` on the `sources` edge: the manifest rejects
    /// declaring both keys on one entry.
    pub(crate) remap: Option<EdgeRemap>,
}

impl DeclaredBinaryEdge {
    /// The classes edge of a `jar` entry.
    pub(crate) const fn classes(recursive: bool, remap: Option<EdgeRemap>) -> Self {
        Self {
            recursive,
            source_archive: false,
            remap,
        }
    }

    /// The companion `sources` edge of a `jar` entry: never recursive, never remapped.
    pub(crate) const fn sources() -> Self {
        Self {
            recursive: false,
            source_archive: true,
            remap: None,
        }
    }
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
///
/// The attribution is not readable apart from the message: a host reports one of these by
/// rendering the whole thing through its [`Display`](fmt::Display), which is why the fields are
/// sealed. Several messages name no subject at all, so a host that read `message` alone dropped
/// the half a user can act on, and the three that reassembled a subject themselves each stated it
/// differently. The same rule holds for [`jals_classpath::Warning`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphWarning {
    /// The attributed node's [`location`](ResolvedNode::location), never its [`NodeId`]: a digest
    /// names nothing a reader can go and look at, which is why [`GraphError::BuildScript`] carries
    /// one too. Two nodes may describe themselves identically — identity is the digest, and it is
    /// deliberately not what a diagnostic shows — so two warnings can read the same.
    ///
    /// *Which* node it is depends on `dependency`. Alone, it is the node the warning is **about**;
    /// beside a `dependency`, it is the project that **declared** that entry. `Display` spells the
    /// two arms differently, so a producer setting both is saying the second.
    pub(crate) node: Option<String>,
    /// The manifest entry this is about, as the manifest spells it — which is not necessarily a
    /// [`Name`](jals_storage::Name), since one warning is that it isn't.
    ///
    /// Usually a `[dependencies]` key, which is what `Display` calls it. A `[build] source-dirs` or
    /// `[build] classpath` entry also arrives here and is rendered the same way, so `dependency
    /// `src/main/java` of project `../lib`: source directory is unavailable` is a thing a user can
    /// be shown. Telling the two apart is a change to what a warning *is* — a third kind of subject
    /// — not to how one is written, so it does not belong in the rendering. Naming the project that
    /// declared it is not that change: it is the same `node` half every other warning carries, and
    /// `src/main/java` needs it more than `lib` does.
    pub(crate) dependency: Option<String>,
    pub(crate) message: String,
}

impl GraphWarning {
    pub(crate) fn node(location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            node: Some(location.into()),
            dependency: None,
            message: message.into(),
        }
    }

    /// A manifest entry gone wrong, attributed to the project whose manifest spells it.
    ///
    /// `declaring` is `None` for the root, which has no node and needs none: its `jals.toml` is the
    /// one the reader is already in, so naming it would put a host path on every line without
    /// narrowing anything. A transitive project's entry is the case that needs it — `lib` alone
    /// does not say which of several `jals.toml` files declares `lib`.
    pub(crate) fn declared(
        declaring: Option<String>,
        name: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            node: declaring,
            dependency: Some(name.to_owned()),
            message: message.into(),
        }
    }
}

/// `<subject>: <message>` — the whole of what a host can say about one of these, which is why the
/// node and the entry a warning is attributed to are not readable separately.
///
/// Several messages name no subject at all — a snapshot diagnostic and `source directory is
/// unavailable` carry theirs only in the attribution — so a host that rendered the message alone
/// would drop the half a user can act on. Every host reports these through this, exactly as one is
/// reported for [`jals_classpath::Warning`]; the attribution a producer chose is the attribution a
/// user sees.
///
/// The node and the entry are independent, so all four combinations are spelled out. A warning that
/// names both is a declaring project's entry gone wrong, and both halves are reported: the entry
/// alone does not say which project wrote it.
impl fmt::Display for GraphWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.dependency, &self.node) {
            (Some(dependency), Some(node)) => {
                write!(f, "dependency `{dependency}` of project `{node}`: ")
            }
            (Some(dependency), None) => write!(f, "dependency `{dependency}`: "),
            (None, Some(node)) => write!(f, "dependency project `{node}`: "),
            (None, None) => f.write_str("project graph: "),
        }?;
        f.write_str(&self.message)
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

/// Everything one node puts on its own classpath, in the three shapes it arrives in.
///
/// Kept together so the scan and the cache key it is recorded under can never see different sets:
/// keying on a subset is how a stale answer gets served for a classpath that changed.
#[derive(Clone, Copy)]
struct NodeClasspath<'a> {
    /// The manifest's own `[build] classpath`, as discovery captured it — read from the capture
    /// rather than from the view because a native node may have taken it from a host path that is
    /// in no project revision.
    captured: &'a [CapturedClasspathEntry],
    /// What the build script registered, already read out of the view into `NodeExports`.
    registered: &'a [CapturedFile],
    /// What a build *task* put there, as keys in the verified cache. The expensive half to read,
    /// and free to name.
    tasks: &'a [CacheKey],
}

/// Wire version of a recorded coverage scan. Bump it whenever the record's meaning changes for
/// unchanged bytes; a mismatch is a miss, never a misread.
const PUBLICATION_COVERAGE_VERSION: u32 = 1;

/// One classpath scan's answer, recorded so an editor reload does not re-digest a game JAR to
/// re-derive it.
///
/// Only the *covered* half is stored. The roots, their intents and the `[dependencies]` caveat are
/// rebuilt from live data on every hit, so a record holds exactly what cost something to learn.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageRecord {
    version: u32,
    covered: Vec<String>,
}

/// One published root nothing on the declaring project's own classpath defines a class under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnbackedPublication {
    pub(crate) owner: String,
    pub(crate) destination: DirKey,
    pub(crate) prefix: RelativePath,
    pub(crate) intent: TaskPublishIntent,
}

/// Everything one node's coverage check found, as **one** report.
///
/// One report rather than one per root, because two of the three things it says are properties of
/// the *project* — what the check could not see, and what it could not read — and stating those
/// once is then structural rather than a matter of appending them to whichever message happens to
/// come first.
///
/// Crate-internal, and rendered into the `String` a [`NodeExports::warnings`] entry carries.
/// [`GraphWarning`] is the one diagnostic a host sees, and it attributes this to the node at
/// assembly; a second host-facing type would be a second thing for four hosts to learn to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicationDiagnosis {
    pub(crate) roots: Vec<UnbackedPublication>,
    /// Classpath entries that could not be inspected, rendered whole through
    /// [`jals_classpath::Warning`]'s `Display` — several of its messages name no location at all,
    /// so the message alone would drop the half a user can act on.
    pub(crate) unread: Vec<jals_classpath::Warning>,
    pub(crate) dependencies_unseen: bool,
}

impl fmt::Display for PublicationDiagnosis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("nothing this project puts on its own classpath defines a class under what ")?;
        f.write_str("its build task publishes —")?;
        for (index, root) in self.roots.iter().enumerate() {
            let separator = if index == 0 { " " } else { ", " };
            let intent = match root.intent {
                TaskPublishIntent::Compile => "compile",
                TaskPublishIntent::Navigation => "navigation",
            };
            write!(
                f,
                "{separator}`{}` at `{}` (`{intent}`, package `{}`)",
                root.owner, root.destination, root.prefix
            )?;
        }
        f.write_str(".")?;
        if self.mentions(TaskPublishIntent::Navigation) {
            f.write_str(
                " A `navigation` publication is a view of types the classpath defines, and never a \
                 compile input, so a consumer has nothing there to compile against.",
            )?;
        }
        if self.mentions(TaskPublishIntent::Compile) {
            f.write_str(
                " A `compile` publication does reach a consumer, but only as source it recompiles \
                 itself: nothing carries those types for anything that does not build this tree.",
            )?;
        }
        if self.dependencies_unseen {
            f.write_str(
                " This project also declares `[dependencies]`, whose contribution is settled after \
                 this check and is invisible to it — so if one of them is what backs these types, \
                 this warning is the check's blind spot rather than a finding.",
            )?;
        }
        if !self.unread.is_empty() {
            f.write_str(
                " Part of the classpath could not be read, so this is what was reachable rather \
                 than the whole of it:",
            )?;
            for (index, warning) in self.unread.iter().enumerate() {
                let separator = if index == 0 { " " } else { "; " };
                write!(f, "{separator}{warning}")?;
            }
            f.write_str(".")?;
        }
        // Last, so the sentence a reader can act on is the one they end on however many caveats
        // came before it.
        //
        // The two are not offered as equals, because only one of them is one this check can see.
        // Declaring the library as a `[dependencies]` jar does carry the types, but the jar becomes
        // a graph node rather than an entry on this project's own classpath, so it settles the
        // consumer's build and leaves this warning exactly where it was — offering it flatly beside
        // `add_classpath` would send a reader after a fix and let them find the same sentence.
        f.write_str(
            " Put the library's own jar on the classpath with `tasks.add_classpath`, which is what \
             this check reads. Declaring it as a `[dependencies]` jar carries the types too, but \
             this check cannot see one and will keep reporting.",
        )
    }
}

impl PublicationDiagnosis {
    fn mentions(&self, intent: TaskPublishIntent) -> bool {
        self.roots.iter().any(|root| root.intent == intent)
    }
}

/// One captured `[build] classpath` entry, beside what the manifest spelled to reach it.
#[derive(Debug)]
pub(crate) struct CapturedClasspathEntry {
    /// The `[build] classpath` string, verbatim.
    ///
    /// Not the same string as where the bytes ended up, and only one of the two is a location a
    /// user can act on: the captured path is where discovery *put* them, which is
    /// declaring-relative for an entry inside the project but a synthesized
    /// `external-classpath-<n>/<name>` for one outside it. Naming that in a diagnostic sends a
    /// reader looking for a file nobody wrote and nothing has.
    pub(crate) declared: String,
    pub(crate) kind: CapturedClasspathKind,
}

#[derive(Debug)]
pub(crate) enum CapturedClasspathKind {
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

    /// The location of the node `id` names, which is how a warning about an entry that project's
    /// manifest declared is attributed. `None` is the root: discovery gives it no node.
    ///
    /// A scan rather than an index because it runs once per *failing* entry, and a graph with
    /// enough nodes for the difference to show would have to fail on most of them to reach it.
    pub(crate) fn location_of(nodes: &[Self], id: Option<&NodeId>) -> Option<String> {
        let id = id?;
        nodes
            .iter()
            .find(|node| &node.id == id)
            .map(|node| node.location.clone())
    }

    /// [`location_of`](Self::location_of) where the caller already holds an id, so an absent node
    /// is not the root but a node this graph does not carry — unreachable, and a digest is still a
    /// worse diagnostic than a location rather than no diagnostic at all.
    pub(crate) fn location_or_digest(nodes: &[Self], id: &NodeId) -> String {
        Self::location_of(nodes, Some(id)).unwrap_or_else(|| id.to_string())
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
            self.publication_exports(manifest, &execution.publications, &mut exports)?;
            exports.unbacked_publications = self
                .diagnose_unbacked_publications(
                    cache,
                    manifest,
                    &output.task_plan,
                    NodeClasspath {
                        captured: &source.classpath,
                        registered: &exports.classpath,
                        tasks: &exports.task_classpath,
                    },
                    &execution.publications,
                )
                .await;
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

    /// Published trees readdressed for the channel their declared intent routes them to.
    ///
    /// A destination is written project-relative (`src/main/java/net/minecraft`) because that is
    /// where a *root* project would physically publish it, and the two channels want opposite
    /// halves of that:
    ///
    /// - **Navigation** is addressed by package. A consumer never sees the dependency's directory
    ///   layout, only its types, so the source root is stripped and what remains is the package
    ///   path — exactly how extracted `sources` jars and synthesized skeletons are addressed, so
    ///   all three agree on where a class lives and one type resolves to one artifact.
    /// - **Compile** keeps the whole project-relative path, because it joins this node's authored
    ///   sources on the way through its own frontend and gets a node token in front of it at
    ///   assembly. Reusing the package address there would make two dependencies publishing the
    ///   same package collide.
    ///
    /// `package_prefix` runs for both, and its result is discarded on the compile side: a
    /// destination outside every declared source root is a mistake regardless of who reads the
    /// tree, and skipping the call for one intent would leave that check to the root host alone.
    fn publication_exports(
        &self,
        manifest: &Manifest,
        publications: &[BuildTaskPublication],
        exports: &mut NodeExports,
    ) -> Result<(), GraphError> {
        for publication in publications {
            let prefix = self.package_prefix(manifest, &publication.destination)?;
            exports
                .publication_roots
                .push(publication.destination.path().clone());
            let (channel, base) = match publication.intent {
                TaskPublishIntent::Navigation => (&mut exports.library_sources, prefix),
                TaskPublishIntent::Compile => (
                    &mut exports.compile_sources,
                    publication.destination.path().clone(),
                ),
            };
            channel.extend(publication.tree.files.iter().map(|file| LibrarySource {
                path: base.concat(&file.path),
                key: file.key.clone(),
            }));
        }
        Ok(())
    }

    /// Warn about the published roots this node's own classpath does not stand behind.
    ///
    /// Routing a `navigation` publication away from the compiler is right for the shape it was
    /// written for — a task that puts a JAR on the classpath *and* publishes readable sources for
    /// the same types, where handing `javac` both would only duplicate them. Nothing enforces that
    /// shape, though, so the check is here: a publication can be the only carrier of a package, and
    /// there the same routing deletes the package outright. The consumer then fails on types this
    /// project believes it exports, several layers away from the declaration that caused it — so
    /// the declaration says so itself, here.
    ///
    /// A `compile` publication is reported too, and says something different: those types *do* reach
    /// a consumer, but only as source, so nothing carries them for anyone who does not build this
    /// tree.
    ///
    /// This is a *consumer-side* check by construction. Discovery gives the root project no node, so
    /// a library's author never sees it building their own repository; it fires in the build of
    /// whoever declares them as a `path` or `git` dependency.
    ///
    /// Only *this* node's classpath is inspected. Its `[dependencies]` are not late but out of
    /// reach: discovery resolved them into graph nodes before any preprocessing ran, yet a
    /// [`ResolvedNode`] holds no handle to the graph it sits in, and what a dependency finally
    /// contributes is decided at assembly. Declaring one is therefore not a reason to stay silent —
    /// that would lose the warning for every root a project publishes as soon as it gains a single
    /// dependency — but it is a reason to say the check could not see them, for every dependency
    /// kind: a `git`/`path` dependency's sources reach a consumer's compiler too, so narrowing the
    /// caveat to `jar` would be the same overreach in the other direction.
    async fn diagnose_unbacked_publications<C: CacheBackend>(
        &self,
        cache: &mut ArtifactCache<C>,
        manifest: &Manifest,
        plan: &TaskPlan,
        classpath: NodeClasspath<'_>,
        publications: &[BuildTaskPublication],
    ) -> Option<PublicationDiagnosis> {
        // Terminal order, which is the order a report lists them in and the order the cache key
        // folds them in. Both are properties of the plan, so neither depends on how a classpath
        // happened to be walked.
        let mut roots = Vec::new();
        for publication in publications {
            // A destination outside every source root is already a hard error from
            // `publication_exports`, which ran first, so this cannot be reached with one.
            let Ok(prefix) = self.package_prefix(manifest, &publication.destination) else {
                continue;
            };
            roots.push(UnbackedPublication {
                owner: publication.owner.clone(),
                destination: publication.destination.clone(),
                prefix,
                intent: publication.intent,
            });
        }
        if roots.is_empty() {
            // `ClasspathCoverage::seeking` with nothing to seek is already complete, and folding a
            // classpath into it would be work for an answer nobody asked for.
            return None;
        }

        // `run_task_plan` already serialized this plan to key its own record, so `None` is
        // unreachable rather than a second policy — and if it ever happened, the answer would still
        // be computed, just never recorded.
        let provenance = BuildTaskExecutor::plan_fingerprint(plan)
            .ok()
            .map(|plan| Self::coverage_provenance(plan, &roots, classpath));
        if let Some(provenance) = provenance
            && let Some(covered) = Self::cached_coverage(cache, provenance).await
        {
            roots.retain(|root| !covered.contains(&root.prefix));
            return (!roots.is_empty()).then(|| PublicationDiagnosis {
                roots,
                // Empty by construction rather than by optimism, which takes both halves of the
                // rule. A record is written only from a scan whose `warnings()` were empty; and the
                // key folds every entry that scan could have read — captured and registered bytes
                // by digest, a task artifact by its key, whose content half `open_verified` checks
                // on the way in. A hit therefore names byte-identical inputs walked in the same
                // deterministic order, so re-scanning could only find the same nothing. The one
                // input a key cannot pin — whether a task artifact is still *present* — was settled
                // before this ran: `run_task_plan`'s own memo re-verifies each one and re-executes
                // if any is gone.
                //
                // That also stands in for `ClasspathCoverage::warnings()` being consulted, which a
                // hit never builds one to consult.
                unread: Vec::new(),
                dependencies_unseen: !manifest.dependencies.is_empty(),
            });
        }

        let mut coverage = ClasspathCoverage::seeking(roots.iter().map(|root| root.prefix.clone()));
        // Held bytes first, cache keys last, because the scan stops as soon as every prefix is
        // covered and the cheap half of a classpath can settle a question the expensive half is
        // then never opened for. What discovery captured is already in memory; a `CacheKey` costs
        // `open_verified`'s whole SHA-256 pass. Nothing observable depends on the order: an entry
        // reached only after the answer was settled could not have changed it, and one skipped for
        // that reason produces no warning that would have been reported anyway.
        //
        // The manifest's own `[build] classpath` is read from what discovery captured, not from the
        // view: a native node may have taken it from a host path that is in no project revision.
        let mut yielder = jals_exec::Yielder::new();
        for entry in classpath.captured {
            if coverage.is_complete() {
                break;
            }
            let origin = WarningOrigin::External(ExternalLocator::new(entry.declared.clone()));
            match &entry.kind {
                CapturedClasspathKind::File(file) => {
                    coverage.add_resident(origin, &file.path, &file.bytes).await;
                }
                // A classpath directory *is* a package root, so a captured member's path already
                // spells its binary name. A built tree holds as many of them as a jar holds
                // members and settles just as early, so this stops mid-walk and yields on the way
                // rather than holding the thread for a directory answered by its first entry.
                CapturedClasspathKind::Tree { members, .. } => {
                    for member in members {
                        if coverage.is_complete() {
                            break;
                        }
                        yielder.tick().await;
                        coverage.add_class(&member.path);
                    }
                }
            }
        }
        // The build script's registered classpath was already read out of the view above, into
        // `NodeExports::classpath`. Asking the same revision the same question would copy the
        // answer a second time.
        for file in classpath.registered {
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
        for key in classpath.tasks {
            if coverage.is_complete() {
                break;
            }
            coverage.add_cached_artifact(cache, key).await;
        }

        // Recorded only when the whole classpath was readable. An unreadable entry is a transient
        // state of the host, not a property of the plan, and a recorded "could not tell" would keep
        // answering that after the jar was fixed.
        if coverage.warnings().is_empty()
            && let Some(provenance) = provenance
        {
            let covered: Vec<_> = roots
                .iter()
                .filter(|root| coverage.covers(&root.prefix))
                .map(|root| root.prefix.clone())
                .collect();
            Self::record_coverage(cache, provenance, &covered).await;
        }

        roots.retain(|root| !coverage.covers(&root.prefix));
        if roots.is_empty() {
            return None;
        }
        Some(PublicationDiagnosis {
            roots,
            // An entry that could not be read is not an entry that defines nothing. Reported beside
            // the roots rather than instead of them: a broken jar makes the finding less certain,
            // not less actionable, and dropping five specific findings because a sixth entry was
            // unreadable would trade what a reader can do something about for what they cannot.
            //
            // Collected once for the whole report, however many entries went unread, since each of
            // them qualifies the same claim about the same roots.
            unread: coverage.warnings().to_vec(),
            dependencies_unseen: !manifest.dependencies.is_empty(),
        })
    }

    /// Identity of one coverage answer: the question, and every classpath entry that could have
    /// changed it.
    ///
    /// Deliberately *not* folded into the task execution's own key. The declaring project's
    /// `[build] classpath` and its build script's registered classpath are inputs here and are no
    /// part of a task execution's identity; adding them there would make editing one
    /// `[build] classpath` line re-fetch, re-remap and re-decompile a whole plan for an answer
    /// about package names.
    ///
    /// Nothing is opened to compute this. Captured bytes are already in memory, and a `CacheKey`
    /// already carries its own content digest — so the half of the classpath that costs an
    /// `open_verified` to *read* costs nothing to *name*.
    fn coverage_provenance(
        plan: ContentDigest,
        roots: &[UnbackedPublication],
        classpath: NodeClasspath<'_>,
    ) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"jals.project.publication-coverage\0");
        fold.version(PUBLICATION_COVERAGE_VERSION).digest(plan);
        // Each section is preceded by its own length. `ProvenanceFold::bytes` frames one *item*,
        // which is not the same as framing the run of them: without this, a prefix and a captured
        // entry's declared spelling are the same shape, so a root list one longer than it should be
        // folds identically to a classpath entry one shorter. Both would then read one recorded
        // answer for two different questions.
        fold.bytes(&(roots.len() as u64).to_be_bytes());
        // The question, not only its inputs: a record answers about the prefixes it was asked
        // about, and the plan digest alone would not distinguish two source-root layouts that put
        // one destination under different packages.
        for root in roots {
            fold.bytes(root.prefix.to_string().as_bytes());
        }
        fold.bytes(&(classpath.captured.len() as u64).to_be_bytes());
        for entry in classpath.captured {
            fold.bytes(entry.declared.as_bytes());
            match &entry.kind {
                CapturedClasspathKind::File(file) => {
                    fold.bytes(file.path.to_string().as_bytes())
                        .digest(ContentDigest::of(&file.bytes));
                }
                // Only member *names* are read from a captured tree, so only they can change the
                // answer — but a member appearing or disappearing changes the set, so every name is
                // folded and not only how many there were.
                CapturedClasspathKind::Tree { path, members } => {
                    fold.bytes(path.to_string().as_bytes());
                    fold.bytes(&(members.len() as u64).to_be_bytes());
                    for member in members {
                        fold.bytes(member.path.to_string().as_bytes());
                    }
                }
            }
        }
        fold.bytes(&(classpath.registered.len() as u64).to_be_bytes());
        for file in classpath.registered {
            fold.bytes(file.path.to_string().as_bytes())
                .digest(ContentDigest::of(&file.bytes));
        }
        // Fixed-width and self-delimiting, so the count buys nothing a `parent` does not already
        // frame — folded anyway, because the rule "every section states its length" is one a reader
        // can check and "every section except the last one" is one they have to reason about.
        fold.bytes(&(classpath.tasks.len() as u64).to_be_bytes());
        for key in classpath.tasks {
            fold.parent(key);
        }
        fold.finish()
    }

    /// The prefixes a recorded scan found covered, or `None` for a miss.
    ///
    /// A record that cannot be read, decoded, or that was written by another version is a miss and
    /// never an error: re-running the scan reproduces it.
    async fn cached_coverage<C: CacheBackend>(
        cache: &ArtifactCache<C>,
        provenance: ContentDigest,
    ) -> Option<BTreeSet<RelativePath>> {
        let key = cache
            .indexed_key(CacheNamespace::PublicationCoverage, provenance)
            .await
            .ok()
            .flatten()?;
        let bytes = cache.lookup(&key).await.ok().flatten()?;
        let record: CoverageRecord = serde_json::from_slice(&bytes).ok()?;
        if record.version != PUBLICATION_COVERAGE_VERSION {
            return None;
        }
        record
            .covered
            .iter()
            .map(|prefix| RelativePath::parse(prefix).ok())
            .collect()
    }

    /// Record one scan's answer. A failure to write it is not a failure to answer, so it is
    /// dropped rather than reported: the next preprocess simply scans again.
    async fn record_coverage<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        provenance: ContentDigest,
        covered: &[RelativePath],
    ) {
        let record = CoverageRecord {
            version: PUBLICATION_COVERAGE_VERSION,
            covered: covered.iter().map(ToString::to_string).collect(),
        };
        let Ok(bytes) = serde_json::to_vec(&record) else {
            return;
        };
        let key = CacheKey::new(
            CacheNamespace::PublicationCoverage,
            provenance,
            ContentDigest::of(&bytes),
        );
        if cache.publish(&key, &bytes).await.is_ok() {
            let _ = cache.record_index(&key).await;
        }
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
    /// Sources a build task published as `navigation`, addressed package-relative like every other
    /// library source. Never a compile input.
    ///
    /// That routing is a *contract*, not a shortcut: a dependency exports its types through the
    /// classpath, and a navigation publication is a view of types defined there. Handing `javac`
    /// both a decompiled tree and the JAR it came from is how a working build acquires duplicates.
    /// A script whose tree is the only carrier of its package says so, and lands in
    /// [`compile_sources`](Self::compile_sources) instead.
    pub(crate) library_sources: Vec<LibrarySource>,
    /// Sources a build task published as `compile`, addressed project-relative — they join this
    /// node's authored sources on the way through its own frontend, and a node token separates
    /// them from another dependency's at assembly.
    pub(crate) compile_sources: Vec<LibrarySource>,
    /// Every publication destination, project-relative and whatever the intent.
    ///
    /// A `replace-root` publication owns its destination completely, so a source captured under one
    /// is what a previous run of this same plan left on disk — not an authored input. Assembly
    /// drops those, which is what keeps a dependency's compile set from depending on whether
    /// somebody once ran a build in its directory.
    pub(crate) publication_roots: Vec<RelativePath>,
    /// What the coverage check found, kept structured up to the point a host is told about it.
    /// Assembly renders it into a [`GraphWarning`] beside `warnings`, which is where every other
    /// node diagnostic already goes.
    pub(crate) unbacked_publications: Option<PublicationDiagnosis>,
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

    /// Every node's diagnostic [`location`](ResolvedNode::location), in discovery order. Nothing
    /// in production reads these as a set — a diagnostic names the one node it is about — so this
    /// exists to let the crate's own tests pin what a reader would be shown.
    #[cfg(test)]
    pub(crate) fn locations(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .map(|node| node.location.as_str())
            .collect()
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
            root_features: options.root_features.features().clone(),
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
    /// The root project's own resolved selection.
    ///
    /// Kept beside the per-node map because the root has no node — discovery gives it none — and an
    /// edge the root declared still has to be gated against *something*. Without this, a root
    /// `[dependencies] remap` would be evaluated against an empty set and silently never apply.
    pub(crate) root_features: BTreeSet<String>,
    #[cfg(feature = "native")]
    pub(crate) native: crate::native::NativeGraphState,
}

impl PreprocessedProjectGraph {
    pub(crate) fn metadata(&self) -> GraphMetadata {
        GraphMetadata::from_graph(&self.nodes, &self.edges)
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::string::ToString;

    use super::GraphWarning;

    /// Every arm names its subject, and the node arms name it by where the node came from. The one
    /// a host used to reach by reading `message` directly — neither field set, which the root
    /// project's own snapshot diagnostics produce — is the reason this is a `Display` and not three
    /// copies in three hosts.
    #[test]
    fn warning_display_names_its_subject() {
        assert_eq!(
            GraphWarning::declared(None, "lib", "source directory is unavailable").to_string(),
            "dependency `lib`: source directory is unavailable"
        );
        assert_eq!(
            GraphWarning::node("../shared", "build script wrote nothing").to_string(),
            "dependency project `../shared`: build script wrote nothing"
        );
        assert_eq!(
            GraphWarning {
                node: None,
                dependency: None,
                message: "snapshot: unreadable".to_owned(),
            }
            .to_string(),
            "project graph: snapshot: unreadable"
        );
    }

    /// A warning that names both keeps both: the declaring project is not recoverable from the
    /// entry, and dropping it is what every host's precedence chain used to do.
    #[test]
    fn warning_display_keeps_both_halves() {
        let warning = GraphWarning {
            node: Some("https://example.invalid/declaring.git".to_owned()),
            dependency: Some("lib".to_owned()),
            message: "dependency name is not a portable name".to_owned(),
        };
        assert_eq!(
            warning.to_string(),
            "dependency `lib` of project `https://example.invalid/declaring.git`: dependency name \
             is not a portable name"
        );
    }
}
