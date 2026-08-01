//! Portable publication of preprocessed graph products into classpath inputs.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use jals_classpath::{
    ClasspathEntry, DependencyLocation, DependencySpec, ExternalLocator, LibrarySource,
    ProjectInputPlan,
};
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, FileKey, Name,
    ProvenanceFold, RelativePath,
};

use crate::graph::{
    BinaryInput, CapturedClasspathEntry, CapturedFile, GraphMetadata, GraphWarning, NodeBody,
    NodeId, PreprocessedProjectGraph, ResolvedNode,
};

/// One verified file entry on the compile classpath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileClasspathFile {
    pub(crate) node: Option<NodeId>,
    pub path: RelativePath,
    pub key: CacheKey,
}

/// One verified member of a compile classpath directory, addressed relative to the directory root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileClasspathTreeMember {
    pub path: FileKey,
    pub key: CacheKey,
}

/// A declared classpath directory whose member artifacts remain individually available for
/// portable analysis but must be materialized as one directory for `javac`/`java`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileClasspathTree {
    node: NodeId,
    pub path: RelativePath,
    pub members: Vec<CompileClasspathTreeMember>,
}

/// Typed compile classpath input. Directory boundaries are never flattened into member files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileClasspathEntry {
    File(CompileClasspathFile),
    Tree(CompileClasspathTree),
}

impl CompileClasspathEntry {
    pub(crate) const fn node(&self) -> Option<&NodeId> {
        match self {
            Self::File(file) => file.node.as_ref(),
            Self::Tree(tree) => Some(&tree.node),
        }
    }
}

/// Structured non-script assembly failure. Other nodes continue to assemble deterministically.
///
/// The fields are sealed, as [`GraphWarning`]'s are: a host reports one of these by rendering the
/// whole thing through its [`Display`](fmt::Display). Both halves name the failing node's own side
/// of the graph, never a file in the consumer's tree, so there is nothing here for a host to attach
/// a diagnostic to that it could not attach to the manifest already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAssemblyError {
    /// The failing node's [`location`](crate::graph::ResolvedNode::location), for the reason
    /// [`GraphWarning`] holds one: a [`NodeId`] renders as a digest, and a digest names nothing a
    /// reader can go and look at. The identity is still what `logical_path` derives artifact paths
    /// from — it is just not what a diagnostic says.
    node: String,
    /// The file inside that node, addressed the way the node itself addresses it. Never the
    /// `logical_path` an artifact is published under: that begins with the node's hex token, which
    /// is the same digest `node` avoids and says as little in the middle of a sentence as it does
    /// at the start of one.
    path: Option<RelativePath>,
    message: String,
}

/// `dependency project <node> could not assemble[ <path>]: <message>` — what a host reports.
///
/// One rendering for every host, for the same reason [`GraphWarning`] has one: a message names its
/// file at most, never the node, and a host that restates the node in its own words states it
/// differently from the next host. It reads as a whole sentence because it is also this type's
/// [`Error`](core::error::Error) rendering, which nothing wraps.
impl fmt::Display for ProjectAssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dependency project `{}` could not assemble", self.node)?;
        if let Some(path) = &self.path {
            write!(f, " `{path}`")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl core::error::Error for ProjectAssemblyError {}

/// Mode-independent graph projection. `ProjectInputOptions` is applied only when this plan is
/// subsequently executed by `ProjectInputs`.
#[derive(Debug)]
pub struct ProjectGraphAssembly {
    pub(crate) graph: GraphMetadata,
    pub(crate) plan: ProjectInputPlan,
    pub(crate) compile_classpath: Vec<CompileClasspathEntry>,
    /// Crate-internal: the projection folds these into the assembly a host receives, so a host that
    /// read them here would be reporting the graph's warnings without the root's.
    pub(crate) warnings: Vec<GraphWarning>,
    pub(crate) errors: Vec<ProjectAssemblyError>,
}

struct Assembler<'a, C: CacheBackend> {
    graph: &'a PreprocessedProjectGraph,
    cache: &'a mut ArtifactCache<C>,
    plan: ProjectInputPlan,
    binary_locations: BTreeMap<NodeId, DependencyLocation>,
    binary_compile: BTreeMap<NodeId, CompileClasspathFile>,
    source_archive_locations: BTreeMap<NodeId, DependencyLocation>,
    published_sources: BTreeSet<(NodeId, RelativePath)>,
    published_classpath: BTreeSet<(NodeId, RelativePath)>,
    compile_classpath: Vec<CompileClasspathEntry>,
    warnings: Vec<GraphWarning>,
    errors: Vec<ProjectAssemblyError>,
}

impl PreprocessedProjectGraph {
    /// Publish captured source/classpath bytes and project a complete transitive classpath plan.
    /// This operation is mode-independent and never mutates a dependency source backend.
    pub(crate) async fn assemble<C: CacheBackend>(
        &self,
        cache: &mut ArtifactCache<C>,
    ) -> ProjectGraphAssembly {
        Assembler::new(self, cache).assemble().await
    }
}

impl<'a, C: CacheBackend> Assembler<'a, C> {
    fn new(graph: &'a PreprocessedProjectGraph, cache: &'a mut ArtifactCache<C>) -> Self {
        Self {
            graph,
            cache,
            plan: ProjectInputPlan::default(),
            binary_locations: BTreeMap::new(),
            binary_compile: BTreeMap::new(),
            source_archive_locations: BTreeMap::new(),
            published_sources: BTreeSet::new(),
            published_classpath: BTreeSet::new(),
            compile_classpath: Vec::new(),
            warnings: graph.warnings.clone(),
            errors: Vec::new(),
        }
    }

    async fn assemble(mut self) -> ProjectGraphAssembly {
        for node in &self.graph.nodes {
            match &node.body {
                NodeBody::Binary(input) => self.publish_binary(&node.id, input).await,
                NodeBody::PlainSource(_) | NodeBody::JalsSource { .. } => {
                    self.publish_source_node(node).await;
                }
            }
            if let Some(exports) = self.graph.exports.get(&node.id) {
                for warning in &exports.warnings {
                    self.warnings
                        .push(GraphWarning::node(node.location.clone(), warning.clone()));
                }
            }
        }
        self.project_binary_edges();
        ProjectGraphAssembly {
            graph: self.graph.metadata(),
            plan: self.plan,
            compile_classpath: self.compile_classpath,
            warnings: self.warnings,
            errors: self.errors,
        }
    }

    async fn publish_binary(&mut self, node: &NodeId, input: &BinaryInput) {
        match input {
            BinaryInput::External { locator } => {
                self.binary_locations.insert(
                    node.clone(),
                    DependencyLocation::External {
                        locator: ExternalLocator::new(locator.clone()),
                        expected: None,
                    },
                );
            }
            BinaryInput::ExternalSource { locator } => {
                self.source_archive_locations.insert(
                    node.clone(),
                    DependencyLocation::External {
                        locator: ExternalLocator::new(locator.clone()),
                        expected: None,
                    },
                );
            }
            BinaryInput::Captured(file) => {
                let Some((logical, key)) = self
                    .publish_file(node, CacheNamespace::DependencyJar, b"binary", file)
                    .await
                else {
                    return;
                };
                self.binary_locations
                    .insert(node.clone(), DependencyLocation::Artifact(key.clone()));
                self.binary_compile.insert(
                    node.clone(),
                    CompileClasspathFile {
                        node: Some(node.clone()),
                        path: logical,
                        key,
                    },
                );
            }
            BinaryInput::CapturedSource(file) => {
                let Some((_, key)) = self
                    .publish_file(node, CacheNamespace::DependencyJar, b"source-archive", file)
                    .await
                else {
                    return;
                };
                self.source_archive_locations
                    .insert(node.clone(), DependencyLocation::Artifact(key));
            }
        }
    }

    async fn publish_source_node(&mut self, node: &crate::graph::ResolvedNode) {
        let source = node.source().expect("source node has a source payload");

        // Run this dependency's own frontend over its authored and generated sources before
        // publishing them, so a consumer's backend sees only lowered output — the same guarantee
        // the CLI gives the root project. The frontend comes from *this node's* manifest, never
        // the root's: a dependency is lowered under its own authority and must not be re-expanded
        // by whoever depends on it.
        self.publish_lowered_sources(node, source).await;

        for entry in &source.classpath {
            self.publish_classpath_entry(&node.id, entry).await;
        }
        if let Some(exports) = self.graph.exports.get(&node.id) {
            for file in &exports.classpath {
                self.publish_classpath_file(&node.id, file).await;
            }
            self.project_task_exports(&node.id, exports);
        }
    }

    /// Project a dependency's build-task output, which is already in this cache under its own keys.
    ///
    /// Unlike every other publication here there are no bytes to write: the task executor produced
    /// these artifacts against the same verified cache, so the plan only has to name them.
    fn project_task_exports(&mut self, node: &NodeId, exports: &crate::graph::NodeExports) {
        for (index, key) in exports.task_classpath.iter().enumerate() {
            // Named after the root's own build-task materialization so the two read alike in a
            // classpath dump; the node token keeps two dependencies' JARs apart.
            let path = Self::logical_path(node, &Self::build_task_jar(index), b"classpath");
            self.plan.classpath.push(ClasspathEntry::ArtifactFile {
                path: path.clone(),
                key: key.clone(),
            });
            self.compile_classpath
                .push(CompileClasspathEntry::File(CompileClasspathFile {
                    node: Some(node.clone()),
                    path,
                    key: key.clone(),
                }));
        }
        // Navigation sources keep the package-relative path the task gave them, with no node token
        // in front: that is the address every other library source uses, and sharing it is what
        // lets one type resolve to one artifact when a jar's sources and a skeleton also offer it.
        //
        // A `compile` publication is deliberately absent. It went through `publish_lowered_sources`
        // with the node's authored files and is already in `source_dependency_artifacts`; adding it
        // here as well would mount the same type twice in an editor — once under `.jals/library`
        // and once under `.jals/source-dependency` — and the library-source deduplication below
        // only ever compares library sources with each other. This is a routing, not a fan-out.
        self.plan
            .library_source_artifacts
            .extend(exports.library_sources.iter().cloned());
    }

    fn build_task_jar(index: usize) -> RelativePath {
        RelativePath::new([
            Name::new("build-task").expect("constant is a portable name"),
            Name::new(format!("{index}.jar")).expect("index-derived file name is portable"),
        ])
    }

    /// Lower a source node's authored + generated `.java` and publish the frontend output.
    async fn publish_lowered_sources(
        &mut self,
        node: &crate::graph::ResolvedNode,
        source: &crate::graph::SourceNode,
    ) {
        // A `compile` publication is the one input here that arrives as a cache key rather than as
        // bytes, because the task executor published it into this very cache and copying it back
        // out at capture time would have bought nothing. Read before the loop below rather than
        // inside it: the borrow of `self.graph` that reaches the authored set and the read from
        // `self.cache` do not have to overlap, and this way neither does.
        let publications: Vec<LibrarySource> = self
            .graph
            .exports
            .get(&node.id)
            .map(|exports| exports.compile_sources.clone())
            .unwrap_or_default();
        let mut published = Vec::with_capacity(publications.len());
        for file in &publications {
            match self.cache.lookup(&file.key).await {
                Ok(Some(bytes)) => published.push((file.path.clone(), bytes)),
                Ok(None) => self.errors.push(ProjectAssemblyError {
                    node: node.location.clone(),
                    path: Some(file.path.clone()),
                    message: "published compile source is not cached".to_owned(),
                }),
                Err(error) => self.errors.push(ProjectAssemblyError {
                    node: node.location.clone(),
                    path: Some(file.path.clone()),
                    message: format!("published compile source is invalid: {error:?}"),
                }),
            }
        }

        // Publications, then authored, then generated; `preprocess` never sees the authored set, so
        // this is the only place all three merge. A build script may register a path that is also
        // an authored file, so the union is deduplicated here — `LoweredTree` rejects a duplicate
        // path, correctly. Publications go first because a `replace-root` destination belongs to
        // the publication: anything found there is what a previous run of this same plan left
        // behind, and the fresh tree is the one that is current.
        let generated = self
            .graph
            .exports
            .get(&node.id)
            .map(|exports| &exports.sources);
        let mut seen = BTreeSet::new();
        let mut files = Vec::new();
        for (path, bytes) in &published {
            if seen.insert(path.clone()) {
                files.push(jals_frontend::IrFile::new(
                    path.clone(),
                    bytes.as_slice().into(),
                ));
            }
        }
        for file in source
            .authored_sources
            .iter()
            .chain(generated.into_iter().flatten())
        {
            if seen.insert(file.path.clone()) {
                files.push(jals_frontend::IrFile::new(
                    file.path.clone(),
                    file.bytes.as_slice().into(),
                ));
            }
        }
        if files.is_empty() {
            return;
        }

        // The frontend comes from this node's own manifest — a JALS node's `[build.frontend]`, or
        // the identity for a legacy source node with no manifest. Not a rule this crate states:
        // it asks `jals-frontend` the same question the CLI and the playground ask, at every depth
        // of the graph. The build features are the node's own unified selection, so a dependency
        // is lowered under its own authority, against what its consumers routed to it.
        let empty = BTreeSet::new();
        let frontend = match &node.body {
            NodeBody::JalsSource { manifest, .. } => {
                jals_frontend::FrontendSelection::for_manifest(
                    manifest,
                    self.graph.features.get(&node.id).unwrap_or(&empty),
                )
            }
            NodeBody::PlainSource(_) | NodeBody::Binary(_) => {
                jals_frontend::FrontendSelection::vanilla()
            }
        };
        let lowered = match frontend.lower(self.cache, files).await {
            Ok(lowered) => lowered,
            Err(error) => {
                self.errors.push(ProjectAssemblyError {
                    node: node.location.clone(),
                    path: None,
                    message: format!("frontend `{}` failed: {error}", frontend.id()),
                });
                return;
            }
        };

        for file in lowered.tree.files() {
            if !self
                .published_sources
                .insert((node.id.clone(), file.path.clone()))
            {
                continue;
            }
            // Keep the existing `dependencies/<node-hex>/sources/<path>` logical layout so a
            // consumer materializes lowered output exactly where it materialized authored source
            // before. Only the bytes' origin changed, not their address.
            let path = Self::logical_path(&node.id, &file.path, b"generated-source");
            self.plan.source_dependency_artifacts.push(LibrarySource {
                path,
                key: file.key.clone(),
            });
        }
    }

    async fn publish_classpath_entry(&mut self, node: &NodeId, entry: &CapturedClasspathEntry) {
        match entry {
            CapturedClasspathEntry::File(file) => self.publish_classpath_file(node, file).await,
            CapturedClasspathEntry::Tree { path, members } => {
                self.publish_classpath_tree(node, path, members).await;
            }
        }
    }

    async fn publish_classpath_file(&mut self, node: &NodeId, file: &CapturedFile) {
        if !self
            .published_classpath
            .insert((node.clone(), file.path.clone()))
        {
            return;
        }
        let Some((path, key)) = self
            .publish_file(node, CacheNamespace::ExternalClasspath, b"classpath", file)
            .await
        else {
            return;
        };
        self.plan.classpath.push(ClasspathEntry::ArtifactFile {
            path: path.clone(),
            key: key.clone(),
        });
        self.compile_classpath
            .push(CompileClasspathEntry::File(CompileClasspathFile {
                node: Some(node.clone()),
                path,
                key,
            }));
    }

    async fn publish_classpath_tree(
        &mut self,
        node: &NodeId,
        path: &RelativePath,
        members: &[CapturedFile],
    ) {
        let mut published = Vec::with_capacity(members.len());
        for member in members {
            let member_path = path.concat(&member.path);
            let first_publication = self
                .published_classpath
                .insert((node.clone(), member_path.clone()));
            let Some((logical, key)) = self
                .publish_bytes(
                    node,
                    CacheNamespace::ExternalClasspath,
                    b"classpath",
                    &member_path,
                    &member.bytes,
                )
                .await
            else {
                return;
            };
            if first_publication {
                self.plan.classpath.push(ClasspathEntry::ArtifactFile {
                    path: logical,
                    key: key.clone(),
                });
            }
            let Ok(member_key) = FileKey::new(member.path.clone()) else {
                let location = ResolvedNode::location_or_digest(&self.graph.nodes, node);
                self.errors.push(ProjectAssemblyError {
                    node: location,
                    // `member_path`, as the publication above reports it: both failures are about
                    // one member of one tree, and a reader given the entry-relative path by one
                    // and the member-relative path by the other has to work out which is which.
                    path: Some(member_path.clone()),
                    message: "classpath tree member is not a file path".to_owned(),
                });
                return;
            };
            published.push(CompileClasspathTreeMember {
                path: member_key,
                key,
            });
        }
        self.compile_classpath
            .push(CompileClasspathEntry::Tree(CompileClasspathTree {
                node: node.clone(),
                path: path.clone(),
                members: published,
            }));
    }

    async fn publish_file(
        &mut self,
        node: &NodeId,
        namespace: CacheNamespace,
        category: &[u8],
        file: &CapturedFile,
    ) -> Option<(RelativePath, CacheKey)> {
        self.publish_bytes(node, namespace, category, &file.path, &file.bytes)
            .await
    }

    async fn publish_bytes(
        &mut self,
        node: &NodeId,
        namespace: CacheNamespace,
        category: &[u8],
        file_path: &RelativePath,
        bytes: &[u8],
    ) -> Option<(RelativePath, CacheKey)> {
        let path = Self::logical_path(node, file_path, category);
        // The category folds as framed bytes, not as part of the kind tag: the same constants
        // also select the logical-path group, so they must stay NUL-free.
        let mut fold = ProvenanceFold::new(b"jals.project.assembly\0");
        fold.bytes(category)
            .digest(node.digest())
            .bytes(file_path.to_string().as_bytes());
        let key = CacheKey::new(namespace, fold.finish(), ContentDigest::of(bytes));
        if let Err(error) = self.cache.publish(&key, bytes).await {
            let location = ResolvedNode::location_or_digest(&self.graph.nodes, node);
            self.errors.push(ProjectAssemblyError {
                node: location,
                // The file as its own node spells it, not `path`: the reader owns the former and
                // has never seen the latter, which is a cache address this run failed to write.
                path: Some(file_path.clone()),
                message: format!("artifact publication failed: {error:?}"),
            });
            return None;
        }
        Some((path, key))
    }

    fn logical_path(node: &NodeId, path: &RelativePath, category: &[u8]) -> RelativePath {
        let dependencies = Name::new("dependencies").expect("constant is a portable name");
        let token = Name::new(node.token()).expect("hex digest is a portable name");
        let group = match category {
            b"binary" => "binary",
            b"classpath" => "classpath",
            b"source" | b"generated-source" => "sources",
            _ => "artifacts",
        };
        let group = Name::new(group).expect("logical group constants are portable");
        RelativePath::new([dependencies, token, group]).concat(path)
    }

    fn project_binary_edges(&mut self) {
        struct ProjectedBinary {
            node: NodeId,
            dependency: String,
            /// The project that declared this edge, as an identity: only the warning below names
            /// it, and resolving a location for every edge to drop all but the failing one is work
            /// with no reader. `None` is the root declaring its own edge, which reads as the entry
            /// alone.
            from: Option<NodeId>,
            location: DependencyLocation,
            source_archive: bool,
            recursive: bool,
        }

        let mut projected = Vec::<ProjectedBinary>::new();
        let mut indices = BTreeMap::<NodeId, usize>::new();
        for edge in &self.graph.edges {
            let (location, source_archive) = if let Some(location) =
                self.binary_locations.get(&edge.to).cloned()
            {
                (location, false)
            } else if let Some(location) = self.source_archive_locations.get(&edge.to).cloned() {
                (location, true)
            } else {
                continue;
            };
            if let Some(index) = indices.get(&edge.to).copied() {
                projected[index].recursive |= edge.recursive;
                continue;
            }
            indices.insert(edge.to.clone(), projected.len());
            projected.push(ProjectedBinary {
                node: edge.to.clone(),
                dependency: edge.dependency.clone(),
                from: edge.from.clone(),
                location,
                source_archive,
                recursive: edge.recursive,
            });
        }

        for projected in projected {
            let Ok(name) = Name::new(&projected.dependency) else {
                let declared_by = projected
                    .from
                    .as_ref()
                    .map(|from| ResolvedNode::location_or_digest(&self.graph.nodes, from));
                self.warnings.push(GraphWarning {
                    node: declared_by,
                    dependency: Some(projected.dependency),
                    message: "dependency name is not a portable name".to_owned(),
                });
                continue;
            };
            let dependency = DependencySpec {
                name,
                location: projected.location,
                recursive: projected.recursive,
            };
            if projected.source_archive {
                self.plan.source_archives.push(dependency);
            } else {
                self.plan.dependencies.push(dependency);
                if let Some(file) = self.binary_compile.remove(&projected.node) {
                    self.compile_classpath
                        .push(CompileClasspathEntry::File(file));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::string::ToString;

    use jals_storage::RelativePath;

    use super::ProjectAssemblyError;

    /// The file is optional, the node never is: a host that reported only the message would say
    /// which file failed for one node and nothing at all for the other. The node is named by where
    /// it came from — a digest would say as little as no node at all.
    #[test]
    fn assembly_error_display_names_node_and_file() {
        assert_eq!(
            ProjectAssemblyError {
                node: "../lib".to_owned(),
                path: None,
                message: "classpath entry is not cached".to_owned(),
            }
            .to_string(),
            "dependency project `../lib` could not assemble: classpath entry is not cached"
        );
        assert_eq!(
            ProjectAssemblyError {
                node: "../lib".to_owned(),
                path: Some(RelativePath::parse("src/Main.java").expect("a portable relative path")),
                message: "publishing failed".to_owned(),
            }
            .to_string(),
            "dependency project `../lib` could not assemble `src/Main.java`: publishing failed"
        );
    }
}
