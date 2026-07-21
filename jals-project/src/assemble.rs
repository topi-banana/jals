//! Portable publication of preprocessed graph products into classpath inputs.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classpath::{
    ClasspathEntry, DependencyLocation, DependencySpec, ExternalLocator, LibrarySource,
    ProjectInputPlan,
};
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, FileKey, Name,
    RelativePath,
};

use crate::graph::{
    BinaryInput, CapturedClasspathEntry, CapturedFile, GraphMetadata, GraphWarning, NodeBody,
    NodeId, PreprocessedProjectGraph,
};

/// One verified file entry on the compile classpath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileClasspathFile {
    pub node: Option<NodeId>,
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
    pub node: NodeId,
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
    pub const fn node(&self) -> Option<&NodeId> {
        match self {
            Self::File(file) => file.node.as_ref(),
            Self::Tree(tree) => Some(&tree.node),
        }
    }
}

/// Structured non-script assembly failure. Other nodes continue to assemble deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAssemblyError {
    pub node: NodeId,
    pub path: Option<RelativePath>,
    pub message: String,
}

/// Mode-independent graph projection. `ProjectInputOptions` is applied only when this plan is
/// subsequently executed by `ProjectInputs`.
#[derive(Debug)]
pub struct ProjectGraphAssembly {
    pub graph: GraphMetadata,
    pub plan: ProjectInputPlan,
    pub compile_classpath: Vec<CompileClasspathEntry>,
    pub warnings: Vec<GraphWarning>,
    pub errors: Vec<ProjectAssemblyError>,
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
    pub async fn assemble<C: CacheBackend>(
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
                        .push(GraphWarning::node(node.id.clone(), warning.clone()));
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
        for file in &source.authored_sources {
            self.publish_source_file(&node.id, b"source", file).await;
        }
        for entry in &source.classpath {
            self.publish_classpath_entry(&node.id, entry).await;
        }
        if let Some(exports) = self.graph.exports.get(&node.id) {
            for file in &exports.sources {
                self.publish_source_file(&node.id, b"generated-source", file)
                    .await;
            }
            for file in &exports.classpath {
                self.publish_classpath_file(&node.id, file).await;
            }
        }
    }

    async fn publish_source_file(&mut self, node: &NodeId, category: &[u8], file: &CapturedFile) {
        if !self
            .published_sources
            .insert((node.clone(), file.path.clone()))
        {
            return;
        }
        if let Some((path, key)) = self
            .publish_file(node, CacheNamespace::PathSource, category, file)
            .await
        {
            self.plan
                .source_dependency_artifacts
                .push(LibrarySource { path, key });
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
            let Ok(member_path) = FileKey::new(member.path.clone()) else {
                self.errors.push(ProjectAssemblyError {
                    node: node.clone(),
                    path: Some(member.path.clone()),
                    message: "classpath tree member is not a file path".to_owned(),
                });
                return;
            };
            published.push(CompileClasspathTreeMember {
                path: member_path,
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
        let rendered_path = file_path.to_string();
        let mut provenance = Vec::with_capacity(category.len() + 32 + rendered_path.len());
        provenance.extend_from_slice(category);
        provenance.extend_from_slice(node.digest().as_bytes());
        provenance.extend_from_slice(rendered_path.as_bytes());
        let key = CacheKey::new(
            namespace,
            ContentDigest::of(&provenance),
            ContentDigest::of(bytes),
        );
        if let Err(error) = self.cache.publish(&key, bytes).await {
            self.errors.push(ProjectAssemblyError {
                node: node.clone(),
                path: Some(path),
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
                self.warnings.push(GraphWarning {
                    node: projected.from,
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
