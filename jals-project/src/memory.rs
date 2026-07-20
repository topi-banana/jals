//! Portable recursive project-graph discovery over one immutable in-memory tree.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_config::{Dependency, Manifest, PathDependency};
use jals_exec::LocalBoxFuture;
use jals_storage::{
    CodeTree, DirKey, Entry, EntryRef, FileKey, MemoryStorage, Name, ProjectView, RelativePath,
};

use crate::graph::{
    BinaryInput, CapturedClasspathEntry, CapturedFile, CycleEdge, GraphEdge, GraphError,
    GraphWarning, NodeBody, NodeId, ResolvedNode, ResolvedProjectGraph, SourceNode,
};

/// Portable entry point for recursive dependency discovery inside one captured [`CodeTree`].
pub struct MemoryProjectGraph;

struct AcquiredSource {
    id: NodeId,
    root: RelativePath,
    view: ProjectView,
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
    root_view: ProjectView,
    nodes: Vec<ResolvedNode>,
    seen_nodes: BTreeSet<NodeId>,
    states: BTreeMap<NodeId, VisitState>,
    edges: Vec<GraphEdge>,
    stack: Vec<StackEntry>,
    order: Vec<usize>,
    warnings: Vec<GraphWarning>,
}

impl MemoryProjectGraph {
    /// Discover all path and jar dependencies from one immutable root snapshot.
    ///
    /// Path dependencies select subtrees of `root_view`. Their manifests and scripts see a view
    /// rooted at that selected subtree, so every key remains project-relative.
    pub async fn discover(
        root_manifest: &Manifest,
        root_view: &ProjectView,
    ) -> Result<ResolvedProjectGraph, GraphError> {
        root_manifest
            .validate()
            .map_err(|error| GraphError::InvalidRootManifest {
                message: error.to_string(),
            })?;
        let mut builder = GraphBuilder::new(root_view.clone());
        builder
            .visit_dependencies(None, &RelativePath::ROOT, root_manifest)
            .await?;
        Ok(ResolvedProjectGraph {
            nodes: builder.nodes,
            edges: builder.edges,
            order: builder.order,
            warnings: builder.warnings,
            #[cfg(feature = "native")]
            native: crate::native::NativeGraphState::default(),
        })
    }
}

impl GraphBuilder {
    const fn new(root_view: ProjectView) -> Self {
        Self {
            root_view,
            nodes: Vec::new(),
            seen_nodes: BTreeSet::new(),
            states: BTreeMap::new(),
            edges: Vec::new(),
            stack: Vec::new(),
            order: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn visit_dependencies<'a>(
        &'a mut self,
        parent: Option<NodeId>,
        declaring: &'a RelativePath,
        manifest: &'a Manifest,
    ) -> LocalBoxFuture<'a, Result<(), GraphError>> {
        Box::pin(async move {
            for (name, dependency) in &manifest.dependencies {
                match dependency {
                    Dependency::Jar(jar) => {
                        if let Err(message) = self.visit_binary(
                            parent.clone(),
                            declaring,
                            name,
                            &jar.jar,
                            jar.recursive.unwrap_or(false),
                            false,
                        ) {
                            self.warnings.push(GraphWarning::dependency(name, message));
                        }
                        if let Some(sources) = &jar.sources
                            && let Err(message) = self.visit_binary(
                                parent.clone(),
                                declaring,
                                name,
                                sources,
                                false,
                                true,
                            )
                        {
                            self.warnings.push(GraphWarning::dependency(name, message));
                        }
                    }
                    Dependency::Path(path) => {
                        let acquired = match self.acquire_path(declaring, path) {
                            Ok(acquired) => acquired,
                            Err(message) => {
                                self.warnings.push(GraphWarning::dependency(name, message));
                                continue;
                            }
                        };
                        self.visit_source(parent.clone(), name, acquired).await?;
                    }
                    Dependency::Git(_) => self.warnings.push(GraphWarning::dependency(
                        name,
                        "Git dependencies cannot be acquired from a portable memory graph",
                    )),
                }
            }
            Ok(())
        })
    }

    async fn visit_source(
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

        let manifest = Self::probe_manifest(&acquired)?;
        let declaring = acquired.root;
        let (body, child_manifest) = if let Some(manifest) = manifest {
            let authored_sources = self.capture_manifest_sources(&declaring, &manifest);
            let classpath = self.capture_manifest_classpath(&declaring, &manifest);
            (
                NodeBody::JalsSource {
                    source: SourceNode {
                        view: acquired.view,
                        authored_sources,
                        classpath,
                    },
                    manifest: Box::new(manifest.clone()),
                },
                Some(manifest),
            )
        } else {
            let authored_sources = Self::capture_plain_sources(&acquired.view);
            (
                NodeBody::PlainSource(SourceNode {
                    view: acquired.view,
                    authored_sources,
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

    fn visit_binary(
        &mut self,
        parent: Option<NodeId>,
        declaring: &RelativePath,
        dependency: &str,
        locator: &str,
        recursive: bool,
        source_archive: bool,
    ) -> Result<(), String> {
        let role = if source_archive { "source" } else { "binary" };
        let (id, input) = if locator.starts_with("http://") || locator.starts_with("https://") {
            let id = NodeId::from_identity(format!("memory-{role}-external\0{locator}").as_bytes());
            let input = if source_archive {
                BinaryInput::ExternalSource {
                    locator: locator.to_owned(),
                }
            } else {
                BinaryInput::External {
                    locator: locator.to_owned(),
                }
            };
            (id, input)
        } else {
            let raw = locator.strip_prefix("file://").unwrap_or(locator);
            let path = Self::normalize(declaring, raw)?;
            let key = FileKey::new(path.clone())
                .map_err(|error| format!("dependency file path is invalid: {error:?}"))?;
            let file = self
                .root_view
                .file(&key)
                .map_err(|error| format!("dependency file `{path}` is unavailable: {error}"))?;
            let identity = format!("memory-{role}\0{path}");
            let captured = CapturedFile {
                path: Self::rebase_file_path(declaring, &path, role)?,
                bytes: file.bytes().to_vec(),
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

    fn acquire_path(
        &self,
        declaring: &RelativePath,
        dependency: &PathDependency,
    ) -> Result<AcquiredSource, String> {
        let base = Self::normalize(declaring, &dependency.path)?;
        let selected = if let Some(dir) = dependency.dir.as_deref() {
            Self::normalize(&base, dir)?
        } else {
            base
        };
        let selected_dir = DirKey::new(selected.clone());
        self.root_view.directory(&selected_dir).map_err(|error| {
            format!("selected dependency root `{selected}` is unavailable: {error}")
        })?;
        let view = Self::subtree(&self.root_view, &selected)?;
        let identity = format!("memory-path\0{selected}");
        Ok(AcquiredSource {
            id: NodeId::from_identity(identity.as_bytes()),
            root: selected,
            view,
        })
    }

    fn probe_manifest(acquired: &AcquiredSource) -> Result<Option<Manifest>, GraphError> {
        let key = FileKey::parse("jals.toml").expect("constant is a portable file key");
        let file = match acquired.view.tree().lookup_file(&key) {
            Some(EntryRef::File(file)) => file,
            Some(EntryRef::Directory(_)) => {
                return Err(GraphError::Acquisition {
                    operation: format!("reading dependency manifest for {}", acquired.id),
                    message: "`jals.toml` is not a file".to_owned(),
                });
            }
            None => return Ok(None),
        };
        let text = file.text().map_err(|error| GraphError::MalformedManifest {
            node: acquired.id.clone(),
            location: Self::manifest_location(&acquired.root),
            message: error.to_string(),
        })?;
        text.parse::<Manifest>()
            .map(Some)
            .map_err(|error| GraphError::MalformedManifest {
                node: acquired.id.clone(),
                location: Self::manifest_location(&acquired.root),
                message: error.to_string(),
            })
    }

    fn manifest_location(root: &RelativePath) -> String {
        if root.is_root() {
            "jals.toml".to_owned()
        } else {
            format!("{root}/jals.toml")
        }
    }

    fn capture_manifest_sources(
        &mut self,
        declaring: &RelativePath,
        manifest: &Manifest,
    ) -> Vec<CapturedFile> {
        let mut files = BTreeMap::new();
        for (index, source) in manifest.build.source_dirs.iter().enumerate() {
            let path = match Self::normalize(declaring, source) {
                Ok(path) => path,
                Err(message) => {
                    self.warnings.push(GraphWarning::dependency(
                        source,
                        format!("source directory is unavailable: {message}"),
                    ));
                    continue;
                }
            };
            let root = DirKey::new(path.clone());
            if let Err(error) = self.root_view.directory(&root) {
                self.warnings.push(GraphWarning::dependency(
                    source,
                    format!("source directory is unavailable: {error}"),
                ));
                continue;
            }
            let local = path.starts_with(declaring);
            let prefix = if local {
                Self::strip_prefix(declaring, &path)
                    .expect("a path tested with starts_with has the prefix")
            } else {
                RelativePath::new([Name::new(format!("external-source-{index}"))
                    .expect("generated prefix is portable")])
            };
            let prefix_len = path.segments().len();
            for file in self
                .root_view
                .tree()
                .files_under(&root)
                .filter(|file| file.key().has_extension("java"))
            {
                let member =
                    RelativePath::new(file.key().path().segments().skip(prefix_len).cloned());
                files.insert(prefix.concat(&member), file.bytes().to_vec());
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

    fn capture_manifest_classpath(
        &mut self,
        declaring: &RelativePath,
        manifest: &Manifest,
    ) -> Vec<CapturedClasspathEntry> {
        let mut entries = Vec::new();
        for (index, entry) in manifest.build.classpath.iter().enumerate() {
            let path = match Self::normalize(declaring, entry) {
                Ok(path) => path,
                Err(message) => {
                    self.warnings.push(GraphWarning::dependency(
                        entry,
                        format!("classpath entry is unavailable: {message}"),
                    ));
                    continue;
                }
            };
            let found = self.root_view.tree().lookup_dir(&DirKey::new(path.clone()));
            match found {
                Some(EntryRef::File(file)) => {
                    let logical = if path.starts_with(declaring) {
                        Self::strip_prefix(declaring, &path)
                            .expect("a path tested with starts_with has the prefix")
                    } else {
                        Self::external_file_path(index, &path)
                    };
                    entries.push(CapturedClasspathEntry::File(CapturedFile {
                        path: logical,
                        bytes: file.bytes().to_vec(),
                    }));
                }
                Some(EntryRef::Directory(directory)) => {
                    let logical = if path.starts_with(declaring) {
                        Self::strip_prefix(declaring, &path)
                            .expect("a path tested with starts_with has the prefix")
                    } else {
                        RelativePath::new([Name::new(format!("external-classpath-{index}"))
                            .expect("generated prefix is portable")])
                    };
                    let prefix_len = path.segments().len();
                    let members = self
                        .root_view
                        .tree()
                        .files_under(directory)
                        .filter(|file| file.key().has_extension("class"))
                        .map(|file| CapturedFile {
                            path: RelativePath::new(
                                file.key().path().segments().skip(prefix_len).cloned(),
                            ),
                            bytes: file.bytes().to_vec(),
                        })
                        .collect();
                    entries.push(CapturedClasspathEntry::Tree {
                        path: logical,
                        members,
                    });
                }
                None => self.warnings.push(GraphWarning::dependency(
                    entry,
                    "classpath entry is unavailable",
                )),
            }
        }
        entries
    }

    fn strip_prefix(root: &RelativePath, path: &RelativePath) -> Option<RelativePath> {
        path.starts_with(root)
            .then(|| RelativePath::new(path.segments().skip(root.segments().len()).cloned()))
    }

    fn rebase_file_path(
        root: &RelativePath,
        path: &RelativePath,
        role: &str,
    ) -> Result<RelativePath, String> {
        if let Some(relative) = Self::strip_prefix(root, path)
            && !relative.is_root()
        {
            return Ok(relative);
        }
        let name = path
            .segments()
            .last()
            .cloned()
            .ok_or_else(|| "dependency file path is the project root".to_owned())?;
        Ok(RelativePath::new([
            Name::new(format!("external-{role}")).expect("generated dependency prefix is portable"),
            name,
        ]))
    }

    fn external_file_path(index: usize, path: &RelativePath) -> RelativePath {
        let name = path
            .segments()
            .last()
            .cloned()
            .expect("a file path is not root");
        RelativePath::new([
            Name::new(format!("external-classpath-{index}"))
                .expect("generated classpath prefix is portable"),
            name,
        ])
    }

    fn subtree(root: &ProjectView, selected: &RelativePath) -> Result<ProjectView, String> {
        let prefix_len = selected.segments().len();
        let selected = DirKey::new(selected.clone());
        let entries = root.tree().files_under(&selected).map(|file| {
            let path = RelativePath::new(file.key().path().segments().skip(prefix_len).cloned());
            Entry::File(
                FileKey::new(path).expect("a descendant file remains non-root"),
                file.bytes().to_vec(),
            )
        });
        let tree = CodeTree::new(entries)
            .map_err(|error| format!("capturing selected dependency subtree failed: {error:?}"))?;
        Ok(MemoryStorage::memory(tree).view())
    }

    fn normalize(base: &RelativePath, raw: &str) -> Result<RelativePath, String> {
        if raw.starts_with('/')
            || raw.starts_with('\\')
            || (raw.as_bytes().get(1) == Some(&b':') && raw.as_bytes()[0].is_ascii_alphabetic())
        {
            return Err("path must be relative to the declaring project".to_owned());
        }
        if raw.contains('\\') {
            return Err("path must use portable `/` separators".to_owned());
        }
        let mut segments: Vec<Name> = base.segments().cloned().collect();
        for part in raw.split('/') {
            match part {
                "." | "" => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err("path leaves the root project tree".to_owned());
                    }
                }
                part => segments.push(
                    Name::new(part)
                        .map_err(|error| format!("path contains an invalid segment: {error:?}"))?,
                ),
            }
        }
        Ok(RelativePath::new(segments))
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
}

#[cfg(test)]
mod tests {
    use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::*;
    use crate::NodeKind;

    fn manifest(text: &str) -> Manifest {
        text.parse().expect("test manifest is valid")
    }

    fn view(files: &[(&str, &[u8])]) -> ProjectView {
        MemoryStorage::memory(
            CodeTree::new(
                files.iter().map(|(path, bytes)| {
                    Entry::File(FileKey::parse(path).unwrap(), bytes.to_vec())
                }),
            )
            .unwrap(),
        )
        .view()
    }

    #[test]
    fn discovers_transitive_subtrees_and_preprocesses_dependency_scripts() {
        jals_exec::block_on_inline(async {
            let root_view = view(&[
                (
                    "deps/parent/jals.toml",
                    b"[build]\nsource-dirs = [\"src\"]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n[dependencies]\nchild = { path = \"../child\" }\n",
                ),
                (
                    "deps/parent/build.rhai",
                    br#"let source = output.write_text("Generated.java", "class Generated {}"); build.add_source(source);"#,
                ),
                ("deps/parent/src/Parent.java", b"class Parent {}"),
                (
                    "deps/child/jals.toml",
                    b"[build]\nsource-dirs = [\"src\"]\nclasspath = [\".\"]\n",
                ),
                ("deps/child/src/Child.java", b"class Child {}"),
                ("deps/child/lib/Child.class", b"class bytes"),
            ]);
            let root = manifest("[dependencies]\nparent = { path = \"deps/parent\" }\n");
            let mut storage = MemoryStorage::memory(CodeTree::default());
            let graph = MemoryProjectGraph::discover(&root, &root_view)
                .await
                .unwrap();
            assert_eq!(
                graph
                    .metadata()
                    .nodes()
                    .iter()
                    .map(|node| node.kind)
                    .collect::<Vec<_>>(),
                [NodeKind::JalsSource, NodeKind::JalsSource]
            );
            let graph = graph
                .preprocess(
                    storage.artifacts_mut(),
                    &BuildScriptEnvironment::new(),
                    &BuildScriptLimits::default(),
                )
                .await
                .unwrap();
            let assembly = graph.assemble(storage.artifacts_mut()).await;
            assert!(assembly.errors.is_empty(), "{:?}", assembly.errors);
            assert!(
                assembly
                    .plan
                    .source_dependency_artifacts
                    .iter()
                    .any(|source| source.path.to_string().ends_with("Generated.java"))
            );
            assert!(assembly.compile_classpath.iter().any(|entry| {
                match entry {
                    crate::CompileClasspathEntry::File(file) => {
                        file.path.to_string().ends_with("Child.class")
                    }
                    crate::CompileClasspathEntry::Tree(tree) => tree
                        .members
                        .iter()
                        .any(|member| member.path.to_string().ends_with("Child.class")),
                }
            }));
        });
    }

    #[test]
    fn manifest_probe_is_exact_and_malformed_is_hard() {
        jals_exec::block_on_inline(async {
            let root =
                manifest("[dependencies]\nselected = { path = \"base\", dir = \"./selected\" }\n");
            let absent = view(&[
                ("base/jals.toml", b"[build\n"),
                ("base/selected/src/S.java", b"class S {}"),
            ]);
            let graph = MemoryProjectGraph::discover(&root, &absent).await.unwrap();
            assert_eq!(graph.metadata().nodes()[0].kind, NodeKind::PlainSource);

            let malformed = view(&[("base/selected/jals.toml", b"[build\n")]);
            assert!(matches!(
                MemoryProjectGraph::discover(&root, &malformed).await,
                Err(GraphError::MalformedManifest { .. })
            ));
        });
    }

    #[test]
    fn cycles_and_root_escape_are_deterministic() {
        jals_exec::block_on_inline(async {
            let diamond = manifest(
                "[dependencies]\nleft = { path = \"left\" }\nright = { path = \"right\" }\n",
            );
            let diamond_view = view(&[
                (
                    "left/jals.toml",
                    b"[dependencies]\nshared = { path = \"../shared\" }\n",
                ),
                (
                    "right/jals.toml",
                    b"[dependencies]\nshared = { path = \"../shared\" }\n",
                ),
                ("shared/src/Shared.java", b"class Shared {}"),
            ]);
            let graph = MemoryProjectGraph::discover(&diamond, &diamond_view)
                .await
                .unwrap();
            assert_eq!(graph.metadata().nodes().len(), 3);
            assert_eq!(graph.metadata().edges().len(), 4);

            let root = manifest("[dependencies]\na = { path = \"a\" }\n");
            let root_view = view(&[
                ("a/jals.toml", b"[dependencies]\nb = { path = \"../b\" }\n"),
                (
                    "b/jals.toml",
                    b"[dependencies]\na-again = { path = \"../a\" }\n",
                ),
            ]);
            let GraphError::Cycle { chain } = MemoryProjectGraph::discover(&root, &root_view)
                .await
                .unwrap_err()
            else {
                panic!("expected a cycle");
            };
            assert_eq!(
                chain
                    .iter()
                    .map(|edge| edge.dependency.as_str())
                    .collect::<Vec<_>>(),
                ["b", "a-again"]
            );

            let escaped = manifest("[dependencies]\nx = { path = \"../x\" }\n");
            let graph = MemoryProjectGraph::discover(&escaped, &root_view)
                .await
                .unwrap();
            assert!(graph.metadata().nodes().is_empty());
            assert_eq!(graph.warnings().len(), 1);
        });
    }

    #[test]
    fn companion_source_archives_remain_separate_from_binary_dependencies() {
        jals_exec::block_on_inline(async {
            let root = manifest(
                "[dependencies]\nlib = { jar = \"lib/binary.jar\", sources = \"lib/sources.jar\" }\n",
            );
            let root_view = view(&[
                ("lib/binary.jar", b"binary"),
                ("lib/sources.jar", b"sources"),
            ]);
            let mut storage = MemoryStorage::memory(CodeTree::default());
            let graph = MemoryProjectGraph::discover(&root, &root_view)
                .await
                .unwrap()
                .preprocess(
                    storage.artifacts_mut(),
                    &BuildScriptEnvironment::new(),
                    &BuildScriptLimits::default(),
                )
                .await
                .unwrap();
            let assembly = graph.assemble(storage.artifacts_mut()).await;
            assert_eq!(assembly.plan.dependencies.len(), 1);
            assert_eq!(assembly.plan.source_archives.len(), 1);
            assert_eq!(assembly.compile_classpath.len(), 1);
        });
    }

    #[test]
    fn dependency_scripts_receive_environment_for_their_own_manifest() {
        jals_exec::block_on_inline(async {
            let root = manifest(
                "[package]\nname = \"root\"\nversion = \"9\"\n\
                 [dependencies]\nempty = { path = \"empty\" }\nmeta = { path = \"meta\" }\n",
            );
            let root_view = view(&[
                (
                    "empty/jals.toml",
                    b"[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                (
                    "empty/build.rhai",
                    br#"
                        if build.env("OUT_DIR") != "target/jals/build/rhai/out"
                            || build.env("JALS_MANIFEST_DIR") != "."
                            || build.env("JALS_PACKAGE_NAME") != ()
                            || build.env("JALS_PACKAGE_VERSION") != ()
                            || build.env("HOST_VALUE") != "kept" {
                            build.error("empty package environment was not derived locally");
                        }
                    "#,
                ),
                (
                    "meta/jals.toml",
                    b"[package]\nname = \"dependency\"\nversion = \"1.2.3\"\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                (
                    "meta/build.rhai",
                    br#"
                        if build.env("OUT_DIR") != "target/jals/build/rhai/out"
                            || build.env("JALS_MANIFEST_DIR") != "."
                            || build.env("JALS_PACKAGE_NAME") != "dependency"
                            || build.env("JALS_PACKAGE_VERSION") != "1.2.3"
                            || build.env("HOST_VALUE") != "kept" {
                            build.error("package environment was not derived locally");
                        }
                    "#,
                ),
            ]);
            let mut environment = BuildScriptEnvironment::new();
            environment.insert("OUT_DIR", "host-out");
            environment.insert("JALS_MANIFEST_DIR", "/host/root");
            environment.insert("JALS_PACKAGE_NAME", "root");
            environment.insert("JALS_PACKAGE_VERSION", "9");
            environment.insert("HOST_VALUE", "kept");
            let mut storage = MemoryStorage::memory(CodeTree::default());

            MemoryProjectGraph::discover(&root, &root_view)
                .await
                .unwrap()
                .preprocess(
                    storage.artifacts_mut(),
                    &environment,
                    &BuildScriptLimits::default(),
                )
                .await
                .unwrap();
        });
    }

    #[test]
    fn dependency_cache_persistence_failure_is_an_advisory_warning() {
        jals_exec::block_on_inline(async {
            let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");
            let root_view = view(&[
                (
                    "dep/jals.toml",
                    b"[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                (
                    "dep/build.rhai",
                    br#"
                        let source = output.write_text("Generated.java", "class Generated {}");
                        build.add_source(source);
                        build.warning("script completed");
                    "#,
                ),
            ]);
            let mut storage = MemoryStorage::memory(CodeTree::default());
            let limits = BuildScriptLimits {
                max_cache_state_size: 1,
                ..BuildScriptLimits::default()
            };
            let graph = MemoryProjectGraph::discover(&root, &root_view)
                .await
                .unwrap()
                .preprocess(
                    storage.artifacts_mut(),
                    &BuildScriptEnvironment::new(),
                    &limits,
                )
                .await
                .unwrap();
            let assembly = graph.assemble(storage.artifacts_mut()).await;
            assert!(assembly.errors.is_empty());
            assert!(
                assembly
                    .plan
                    .source_dependency_artifacts
                    .iter()
                    .any(|source| source.path.to_string().ends_with("Generated.java"))
            );
            assert!(
                assembly
                    .warnings
                    .iter()
                    .any(|warning| warning.message.contains("could not persist prepared"))
            );
        });
    }

    #[test]
    fn git_is_an_ordered_warning_without_a_node() {
        jals_exec::block_on_inline(async {
            let root = manifest(
                "[dependencies]\na = { git = \"https://example.invalid/a.git\" }\nb = { git = \"https://example.invalid/b.git\" }\n",
            );
            let graph = MemoryProjectGraph::discover(&root, &view(&[]))
                .await
                .unwrap();
            assert!(graph.metadata().nodes().is_empty());
            assert_eq!(
                graph
                    .warnings()
                    .iter()
                    .filter_map(|warning| warning.dependency.as_deref())
                    .collect::<Vec<_>>(),
                ["a", "b"]
            );
            assert!(
                graph
                    .warnings()
                    .iter()
                    .all(|warning| warning.message.contains("cannot be acquired"))
            );
        });
    }
}
