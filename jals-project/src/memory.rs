//! Portable recursive project-graph discovery over one immutable in-memory tree.
//!
//! The walk itself is [`crate::walk`]'s; this is the half that differs. An in-memory project has
//! one address space, so acquiring a dependency means selecting a subtree of the tree already in
//! hand: nothing is copied, nothing can fail to be read afterwards, and a Git remote is simply out
//! of reach.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::future::{Future, ready};

use jals_config::{DependencyScope, GitDependency, Manifest, PathDependency};
use jals_storage::{
    CodeTree, DirKey, Entry, EntryRef, FileKey, MemoryStorage, Name, ProjectView, RelativePath,
};

use crate::graph::{GraphError, NodeId, ResolvedProjectGraph};
use crate::walk::{
    Acquired, DeclaredEntry, DeclaredFile, DeclaredTree, GraphHost, GraphWalk, Opened, Placement,
};

/// Portable entry point for recursive dependency discovery inside one captured [`CodeTree`].
pub struct MemoryProjectGraph;

/// The tree every declaration in this graph is addressed against.
///
/// One field, because that is the whole of what a portable host needs to acquire anything: there is
/// no second address space to reach into.
struct MemoryHost {
    root_view: ProjectView,
}

/// A selected subtree, already cut. Selecting one is the acquisition; there is nothing left to
/// open, which is why [`MemoryHost::open`] cannot fail.
struct Selected {
    root: RelativePath,
    view: ProjectView,
}

impl MemoryProjectGraph {
    /// Discover all path and jar dependencies from one immutable root snapshot.
    ///
    /// Path dependencies select subtrees of `root_view`. Their manifests and scripts see a view
    /// rooted at that selected subtree, so every key remains project-relative.
    pub(crate) async fn discover(
        root_manifest: &Manifest,
        scope: DependencyScope,
        root_view: &ProjectView,
    ) -> Result<ResolvedProjectGraph, GraphError> {
        root_manifest
            .validate()
            .map_err(|error| GraphError::InvalidRootManifest {
                message: error.to_string(),
            })?;
        let mut host = MemoryHost {
            root_view: root_view.clone(),
        };
        let output = GraphWalk::run(
            &mut host,
            &RelativePath::ROOT,
            root_manifest,
            scope,
            Vec::new(),
        )
        .await?;
        Ok(ResolvedProjectGraph {
            nodes: output.nodes,
            edges: output.edges,
            order: output.order,
            warnings: output.warnings,
            #[cfg(feature = "native")]
            native: crate::native::NativeGraphState::default(),
        })
    }
}

impl MemoryHost {
    /// One declared path, resolved against the project that wrote it.
    ///
    /// The fold is [`RelativePath::resolve`]'s, shared with `jals-classpath`'s `[build]` lowering.
    /// What is the graph's own is the base: a dependency spells `../lib` for a sibling, so `..` has
    /// to climb above the declaring project — just never above the captured tree.
    fn normalize(base: &RelativePath, raw: &str) -> Result<RelativePath, String> {
        RelativePath::resolve(base, raw).map_err(|error| error.to_string())
    }

    /// A file key's own last segment, which is the name a synthesized `external-*` path uses.
    fn file_name(key: &FileKey) -> Name {
        key.path()
            .name()
            .cloned()
            .expect("a file key always has a last segment")
    }

    /// Where a declared path sits relative to the project that declared it.
    fn placement(declaring: &RelativePath, path: &RelativePath) -> Placement {
        path.strip_prefix(declaring)
            .map_or(Placement::External, Placement::Local)
    }

    /// How a diagnostic names a node. Inside one captured tree that is the subtree it selected.
    fn node_location(root: &RelativePath) -> String {
        if root.is_root() {
            ".".to_owned()
        } else {
            root.to_string()
        }
    }

    /// The selected subtree as a view of its own, so a dependency's keys stay project-relative.
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
}

/// The always-ready primitives behind the [`GraphHost`] impl below. An in-memory graph resolves
/// every declaration out of one captured tree, so nothing here reaches a host or suspends; the
/// trait impl wraps each in `ready`.
impl MemoryHost {
    fn acquire_path_now(
        &self,
        project: &RelativePath,
        dependency: &PathDependency,
    ) -> Result<Acquired<Self>, String> {
        let base = Self::normalize(project, &dependency.path)?;
        let selected = match dependency.dir.as_deref() {
            Some(dir) => Self::normalize(&base, dir)?,
            None => base,
        };
        self.root_view
            .directory(&DirKey::new(selected.clone()))
            .map_err(|error| {
                format!("selected dependency root `{selected}` is unavailable: {error}")
            })?;
        let view = Self::subtree(&self.root_view, &selected)?;
        Ok(Acquired {
            identity: format!("path\0{selected}"),
            location: Self::node_location(&selected),
            site: Selected {
                root: selected,
                view,
            },
            guard: (),
        })
    }

    fn resolve_declared_file_now(
        &self,
        project: &RelativePath,
        raw: &str,
    ) -> Result<DeclaredFile, String> {
        let path = Self::normalize(project, raw)?;
        let key = FileKey::new(path.clone())
            .map_err(|error| format!("dependency file path is invalid: {error:?}"))?;
        let file = self
            .root_view
            .file(&key)
            .map_err(|error| format!("dependency file `{path}` is unavailable: {error}"))?;
        Ok(DeclaredFile {
            bytes: file.bytes().to_vec(),
            name: Self::file_name(&key),
            identity: format!("file\0{path}"),
            placement: Self::placement(project, &path),
        })
    }

    fn resolve_source_dir_now(
        &self,
        project: &RelativePath,
        raw: &str,
    ) -> Result<DeclaredTree, String> {
        let path = Self::normalize(project, raw)
            .map_err(|message| format!("source directory is unavailable: {message}"))?;
        let root = DirKey::new(path.clone());
        self.root_view
            .directory(&root)
            .map_err(|error| format!("source directory is unavailable: {error}"))?;
        Ok(DeclaredTree {
            view: self.root_view.clone(),
            root,
            placement: Self::placement(project, &path),
        })
    }

    fn resolve_classpath_entry_now(
        &self,
        project: &RelativePath,
        raw: &str,
    ) -> Result<DeclaredEntry, String> {
        let path = Self::normalize(project, raw)
            .map_err(|message| format!("classpath entry is unavailable: {message}"))?;
        let placement = Self::placement(project, &path);
        // A file and a directory can share neither a path nor a key, so one probe answers both.
        match self.root_view.tree().lookup_dir(&DirKey::new(path.clone())) {
            Some(EntryRef::File(file)) => {
                let key = FileKey::new(path)
                    .map_err(|error| format!("classpath entry is unavailable: {error:?}"))?;
                Ok(DeclaredEntry::File(DeclaredFile {
                    bytes: file.bytes().to_vec(),
                    name: Self::file_name(&key),
                    identity: String::new(),
                    placement,
                }))
            }
            Some(EntryRef::Directory(_)) => Ok(DeclaredEntry::Tree(DeclaredTree {
                view: self.root_view.clone(),
                root: DirKey::new(path),
                placement,
            })),
            None => Err("classpath entry is unavailable".to_owned()),
        }
    }
}

impl GraphHost for MemoryHost {
    type Site = Selected;
    type Project = RelativePath;
    /// Selecting a subtree copies nothing that has to be cleaned up.
    type Guard = ();

    const SCOPE: &'static str = "memory";

    fn manifest_location(&self, _id: &NodeId, acquired: &Acquired<Self>) -> String {
        let root = &acquired.site.root;
        if root.is_root() {
            "jals.toml".to_owned()
        } else {
            format!("{root}/jals.toml")
        }
    }

    fn acquire_path(
        &mut self,
        project: &Self::Project,
        dependency: &PathDependency,
    ) -> impl Future<Output = Result<Acquired<Self>, String>> {
        ready(self.acquire_path_now(project, dependency))
    }

    fn acquire_git(
        &mut self,
        _project: &Self::Project,
        _name: &str,
        _dependency: &GitDependency,
    ) -> impl Future<Output = Result<Acquired<Self>, String>> {
        ready(Err(
            "Git dependencies cannot be acquired from a portable memory graph".to_owned(),
        ))
    }

    fn open(
        &mut self,
        acquired: &Acquired<Self>,
    ) -> impl Future<Output = Result<Opened<Self>, GraphError>> {
        ready(Ok(Opened {
            view: acquired.site.view.clone(),
            project: acquired.site.root.clone(),
            notes: Vec::new(),
            // Every file in the captured tree is already read. A `jals.toml` that is not here is
            // one the project does not have.
            manifest_unreadable: None,
        }))
    }

    fn admitted(&mut self, _acquired: &Acquired<Self>) {}

    fn release(&mut self, (): Self::Guard) -> impl Future<Output = Result<(), String>> {
        ready(Ok(()))
    }

    fn resolve_declared_file(
        &mut self,
        project: &Self::Project,
        raw: &str,
        _role: &str,
    ) -> impl Future<Output = Result<DeclaredFile, String>> {
        ready(self.resolve_declared_file_now(project, raw))
    }

    fn resolve_source_dir(
        &mut self,
        project: &Self::Project,
        raw: &str,
        _notes: &mut Vec<String>,
    ) -> impl Future<Output = Result<DeclaredTree, String>> {
        ready(self.resolve_source_dir_now(project, raw))
    }

    fn resolve_classpath_entry(
        &mut self,
        project: &Self::Project,
        raw: &str,
        _notes: &mut Vec<String>,
    ) -> impl Future<Output = Result<DeclaredEntry, String>> {
        ready(self.resolve_classpath_entry_now(project, raw))
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;

    use jals_build::build_script::{BuildScriptEnvironment, BuildScriptLimits};
    use jals_config::ResolvedBuildFeatures;
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::*;
    use crate::graph::{GraphPreprocess, NodeKind, PreprocessedProjectGraph};

    /// A fetch capability for graphs that declare no task plan. Reaching it is the failure.
    struct UnreachableFetcher;

    impl UnreachableFetcher {
        /// Diverges: being asked at all is the failure this fixture asserts against.
        fn refuse(locator: &str) -> Result<Vec<u8>, jals_classpath::FetchError> {
            panic!("this graph must not fetch, but asked for `{locator}`")
        }
    }

    impl jals_classpath::Fetcher for UnreachableFetcher {
        // `Online`: the panic is the assertion — `Offline` would refuse first and pass blind.
        fn network(&self) -> jals_classpath::NetworkPolicy {
            jals_classpath::NetworkPolicy::Online
        }

        fn retry(&self) -> jals_classpath::RetrySchedule {
            jals_classpath::RetrySchedule::none()
        }

        fn delay(&self, _: u32) -> impl Future<Output = ()> {
            ready(())
        }

        fn fetch_admitted(
            &self,
            locator: &str,
        ) -> impl Future<Output = Result<Vec<u8>, jals_classpath::FetchError>> {
            ready(Self::refuse(locator))
        }
    }

    /// Preprocessing inputs for a graph under test, defaulting everything a task plan would need.
    ///
    /// A macro rather than a helper function because the borrowed defaults have to outlive the call
    /// and nothing here owns them; as temporaries in the calling statement they live exactly long
    /// enough.
    macro_rules! inert {
        () => {
            inert!(
                &BuildScriptEnvironment::new(),
                &ResolvedBuildFeatures::default(),
                &BuildScriptLimits::default()
            )
        };
        ($environment:expr, $features:expr, $limits:expr) => {
            GraphPreprocess {
                exec: &jals_exec::Exec::inline(),
                fetcher: &UnreachableFetcher,
                environment: $environment,
                root_features: $features,
                limits: $limits,
            }
        };
    }

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
            let graph = MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
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
                .preprocess(storage.artifacts_mut(), inert!())
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
            let graph = MemoryProjectGraph::discover(&root, DependencyScope::Build, &absent)
                .await
                .unwrap();
            assert_eq!(graph.metadata().nodes()[0].kind, NodeKind::PlainSource);

            let malformed = view(&[("base/selected/jals.toml", b"[build\n")]);
            assert!(matches!(
                MemoryProjectGraph::discover(&root, DependencyScope::Build, &malformed).await,
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
            let graph =
                MemoryProjectGraph::discover(&diamond, DependencyScope::Build, &diamond_view)
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
            let GraphError::Cycle { chain } =
                MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
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
            let graph = MemoryProjectGraph::discover(&escaped, DependencyScope::Build, &root_view)
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
            let graph = MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
                .await
                .unwrap()
                .preprocess(storage.artifacts_mut(), inert!())
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

            MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
                .await
                .unwrap()
                .preprocess(
                    storage.artifacts_mut(),
                    inert!(
                        &environment,
                        &ResolvedBuildFeatures::default(),
                        &BuildScriptLimits::default()
                    ),
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
            let graph = MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
                .await
                .unwrap()
                .preprocess(
                    storage.artifacts_mut(),
                    inert!(
                        &BuildScriptEnvironment::new(),
                        &ResolvedBuildFeatures::default(),
                        &limits
                    ),
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
            let graph = MemoryProjectGraph::discover(&root, DependencyScope::Build, &view(&[]))
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
            // The root declares these, so there is no node to name: its `jals.toml` is the one the
            // reader is already in. The sibling test below is the case that does need one.
            assert!(
                graph
                    .warnings()
                    .iter()
                    .all(|warning| warning.node.is_none())
            );
        });
    }

    /// The entry alone is not enough once a *dependency* declares it — `a` appears in as many
    /// `jals.toml` files as care to write it, and the one to open is the declaring project's.
    #[test]
    fn a_transitive_entry_names_the_project_that_declared_it() {
        jals_exec::block_on_inline(async {
            let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");
            let graph = MemoryProjectGraph::discover(
                &root,
                DependencyScope::Build,
                &view(&[
                    (
                        "dep/jals.toml",
                        b"[dependencies]\na = { git = \"https://example.invalid/a.git\" }\n",
                    ),
                    // Present only so the default source directory resolves; a missing one warns,
                    // and this asserts the whole warning list.
                    ("dep/src/main/java/D.java", b"class D {}"),
                ]),
            )
            .await
            .unwrap();
            assert_eq!(
                graph
                    .warnings()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                [
                    "dependency `a` of project `dep`: Git dependencies cannot be acquired from a \
                     portable memory graph"
                ]
            );
        });
    }

    /// A `[build]` entry needs the declaring project more than a `[dependencies]` one does. `a` is
    /// a name the reader chose; `src/main/java` is the default every project in the tree writes, so
    /// on its own it narrows the search to nothing.
    #[test]
    fn a_transitive_build_entry_names_the_project_that_declared_it() {
        jals_exec::block_on_inline(async {
            let root = manifest("[dependencies]\ndep = { path = \"dep\" }\n");
            let graph = MemoryProjectGraph::discover(
                &root,
                DependencyScope::Build,
                &view(&[(
                    "dep/jals.toml",
                    b"[build]\nsource-dirs = [\"src/main/java\"]\n",
                )]),
            )
            .await
            .unwrap();
            let rendered = graph
                .warnings()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            assert_eq!(rendered.len(), 1, "{rendered:?}");
            // The tail is a `ProjectView` error and is not what this pins; the subject is.
            assert!(
                rendered[0].starts_with(
                    "dependency `src/main/java` of project `dep`: source directory is unavailable"
                ),
                "{rendered:?}"
            );
        });
    }

    /// A dependency `build.rhai` that registers one source per feature it is asked about, so the
    /// generated file names spell out the exact set the script saw.
    const FEATURE_PROBE: &[u8] = br#"
        for name in ["hello", "world", "root-only", "a", "b", "ok", "soft", "vulkan", "spirv"] {
            if build.feature(name) {
                let source = output.write_text(name + ".java", "class X {}");
                build.add_source(source);
            }
        }
    "#;

    const PROBE_MANIFEST: &[u8] = b"[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n";

    /// The basenames the graph's dependency scripts generated, sorted.
    fn generated(graph: &PreprocessedProjectGraph) -> Vec<String> {
        let mut names: Vec<String> = graph
            .exports
            .values()
            .flat_map(|exports| exports.sources.iter())
            .filter_map(|file| file.path.name().map(ToString::to_string))
            .collect();
        names.sort();
        names
    }

    async fn preprocess_with(
        root: &Manifest,
        root_view: &ProjectView,
        environment: &BuildScriptEnvironment,
    ) -> PreprocessedProjectGraph {
        preprocess_selecting(root, root_view, environment, &[]).await
    }

    /// Discover then preprocess, returning the error. For asserting the build-feature edge check,
    /// which fires at the start of `preprocess` before any script runs.
    async fn preprocess_error(root: &Manifest, root_view: &ProjectView) -> GraphError {
        let mut storage = MemoryStorage::memory(CodeTree::default());
        MemoryProjectGraph::discover(root, DependencyScope::Build, root_view)
            .await
            .unwrap()
            .preprocess(storage.artifacts_mut(), inert!())
            .await
            .unwrap_err()
    }

    /// Preprocess under a root `--features` selection, as a host does: the root resolves its own
    /// `[features]` and the graph receives what that selection forwards.
    async fn preprocess_selecting(
        root: &Manifest,
        root_view: &ProjectView,
        environment: &BuildScriptEnvironment,
        selected: &[&str],
    ) -> PreprocessedProjectGraph {
        let selected: Vec<String> = selected.iter().map(|name| (*name).to_owned()).collect();
        let features = root
            .resolve_build_features(&selected, false, false)
            .unwrap();
        let mut storage = MemoryStorage::memory(CodeTree::default());
        MemoryProjectGraph::discover(root, DependencyScope::Build, root_view)
            .await
            .unwrap()
            .preprocess(
                storage.artifacts_mut(),
                inert!(environment, &features, &BuildScriptLimits::default()),
            )
            .await
            .unwrap()
    }

    #[test]
    fn dependency_scripts_see_only_their_own_edge_features() {
        jals_exec::block_on_inline(async {
            // Features are per package: the declaring project's selection must not steer a
            // dependency's script. A leaked `root-only` would mean anyone's `--features` silently
            // rebuilds every dependency in the graph differently.
            let root =
                manifest("[dependencies]\ndep = { path = \"dep\", features = [\"hello\"] }\n");
            let root_view = view(&[
                (
                    "dep/jals.toml",
                    b"[features]\nhello = []\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                ("dep/build.rhai", FEATURE_PROBE),
            ]);
            let environment = BuildScriptEnvironment::new()
                .with_features(BTreeSet::from(["root-only".to_owned()]));
            let graph = preprocess_with(&root, &root_view, &environment).await;
            assert_eq!(generated(&graph), ["hello.java"]);
        });
    }

    #[test]
    fn a_dependency_resolves_its_own_features_table() {
        jals_exec::block_on_inline(async {
            // What arrives on the edge is a *seed*, not the finished set: the dependency closes it
            // over its own `enables` map (`hello` pulls in `b`) and adds its own `default` list
            // (`a`), exactly as Cargo does for a dependency nobody passed `default-features` to.
            let root =
                manifest("[dependencies]\ndep = { path = \"dep\", features = [\"hello\"] }\n");
            let root_view = view(&[
                (
                    "dep/jals.toml",
                    b"[features]\ndefault = [\"a\"]\na = []\nb = []\nhello = [\"b\"]\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                ("dep/build.rhai", FEATURE_PROBE),
            ]);
            let graph = preprocess_with(&root, &root_view, &BuildScriptEnvironment::new()).await;
            assert_eq!(generated(&graph), ["a.java", "b.java", "hello.java"]);
        });
    }

    #[test]
    fn default_features_false_suppresses_only_the_dependency_default_list() {
        jals_exec::block_on_inline(async {
            // `default-features = false` drops `a`; everything the edge asked for, and everything
            // that follows from it through the dependency's own table, still applies.
            let root = manifest(
                "[dependencies]\n\
                 dep = { path = \"dep\", features = [\"hello\"], default-features = false }\n",
            );
            let root_view = view(&[
                (
                    "dep/jals.toml",
                    b"[features]\ndefault = [\"a\"]\na = []\nb = []\nhello = [\"b\"]\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                ("dep/build.rhai", FEATURE_PROBE),
            ]);
            let graph = preprocess_with(&root, &root_view, &BuildScriptEnvironment::new()).await;
            assert_eq!(generated(&graph), ["b.java", "hello.java"]);
        });
    }

    #[test]
    fn one_edge_asking_for_the_defaults_turns_them_on_for_the_shared_node() {
        jals_exec::block_on_inline(async {
            // `default-features` unifies additively, like every other feature input: the node is
            // shared, so suppressing the defaults on one edge cannot subtract them from the build
            // another edge asked for. Otherwise a diamond's result would depend on edge order.
            let root = manifest(
                "[dependencies]\n\
                 direct = { path = \"dep\", default-features = false }\n\
                 mid = { path = \"mid\" }\n",
            );
            let root_view = view(&[
                (
                    "dep/jals.toml",
                    b"[features]\ndefault = [\"a\"]\na = []\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                ("dep/build.rhai", FEATURE_PROBE),
                (
                    "mid/jals.toml",
                    b"[dependencies]\nshared = { path = \"../dep\" }\n",
                ),
            ]);
            let graph = preprocess_with(&root, &root_view, &BuildScriptEnvironment::new()).await;
            assert_eq!(generated(&graph), ["a.java"]);
        });
    }

    #[test]
    fn a_root_feature_forwards_to_its_dependency() {
        jals_exec::block_on_inline(async {
            // Cargo's `std = ["serde/std"]`, at the root: only the selection that names `gpu` sends
            // `vulkan` down the edge, and `gpu` itself stays a feature of the root.
            let root = manifest(
                "[features]\ngpu = [\"dep/vulkan\"]\n\
                 [dependencies]\ndep = { path = \"dep\" }\n",
            );
            let root_view = view(&[
                ("dep/jals.toml", PROBE_MANIFEST),
                ("dep/build.rhai", FEATURE_PROBE),
            ]);
            let selected =
                preprocess_selecting(&root, &root_view, &BuildScriptEnvironment::new(), &["gpu"])
                    .await;
            assert_eq!(generated(&selected), ["vulkan.java"]);
            let unselected =
                preprocess_with(&root, &root_view, &BuildScriptEnvironment::new()).await;
            assert!(generated(&unselected).is_empty());
        });
    }

    #[test]
    fn forwarded_features_route_on_through_the_graph() {
        jals_exec::block_on_inline(async {
            // The routing is transitive because a node resolves its own table: `mid` receives
            // `vulkan`, expands it, and forwards `spirv` to *its* dependency. A feature the root has
            // no name for reaches a project the root never mentions.
            let root = manifest(
                "[features]\ndefault = [\"gpu\"]\ngpu = [\"mid/vulkan\"]\n\
                 [dependencies]\nmid = { path = \"mid\" }\n",
            );
            let root_view = view(&[
                (
                    "mid/jals.toml",
                    b"[features]\nvulkan = [\"sub/spirv\"]\n\
                      [dependencies]\nsub = { path = \"../sub\" }\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                ("mid/build.rhai", FEATURE_PROBE),
                ("sub/jals.toml", PROBE_MANIFEST),
                ("sub/build.rhai", FEATURE_PROBE),
            ]);
            let graph = preprocess_with(&root, &root_view, &BuildScriptEnvironment::new()).await;
            assert_eq!(generated(&graph), ["spirv.java", "vulkan.java"]);
        });
    }

    #[test]
    fn features_unify_across_every_edge_reaching_a_node() {
        jals_exec::block_on_inline(async {
            // Cargo's feature unification: two entries reaching the same project give it one set and
            // one build, rather than two nodes whose classes would both land on the classpath.
            let root = manifest(
                "[dependencies]\n\
                 direct = { path = \"dep\", features = [\"hello\"] }\n\
                 mid = { path = \"mid\" }\n",
            );
            let root_view = view(&[
                (
                    "dep/jals.toml",
                    b"[features]\nhello = []\nworld = []\n\
                      [build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                ),
                ("dep/build.rhai", FEATURE_PROBE),
                (
                    "mid/jals.toml",
                    b"[dependencies]\nshared = { path = \"../dep\", features = [\"world\"] }\n",
                ),
            ]);
            let graph = preprocess_with(&root, &root_view, &BuildScriptEnvironment::new()).await;
            assert_eq!(
                graph
                    .metadata()
                    .nodes()
                    .iter()
                    .filter(|node| node.kind == NodeKind::JalsSource)
                    .count(),
                2,
                "the shared dependency stays one node"
            );
            assert_eq!(generated(&graph), ["hello.java", "world.java"]);
        });
    }

    #[test]
    fn an_undeclared_edge_feature_is_rejected() {
        jals_exec::block_on_inline(async {
            // A `[dependencies] features` name the target does not declare is a typo, not an empty
            // selection: reject it rather than silently expanding it to nothing and building the
            // default. This is the exact failure the `[build.features]` -> `[features]` move made a
            // hard error to avoid, now closed on the dependency edge too.
            let root =
                manifest("[dependencies]\ndep = { path = \"dep\", features = [\"typo\"] }\n");
            let root_view = view(&[("dep/jals.toml", b"[features]\nreal = []\n")]);
            let GraphError::InvalidDependency {
                dependency,
                message,
                ..
            } = preprocess_error(&root, &root_view).await
            else {
                panic!("expected an invalid dependency");
            };
            assert_eq!(dependency, "dep");
            assert!(message.contains("typo"), "{message}");
        });
    }

    #[test]
    fn an_undeclared_edge_feature_is_caught_on_a_second_diamond_edge() {
        jals_exec::block_on_inline(async {
            // `dep` is already `Complete` when the second edge reaches it, so a check done at visit
            // time would miss this. The scan reads every edge, so the typo on `mid`'s entry is still
            // caught — and the error names that edge (`shared`), not the one that arrived first.
            let root = manifest(
                "[dependencies]\n\
                 direct = { path = \"dep\", features = [\"good\"] }\n\
                 mid = { path = \"mid\" }\n",
            );
            let root_view = view(&[
                ("dep/jals.toml", b"[features]\ngood = []\n"),
                (
                    "mid/jals.toml",
                    b"[dependencies]\nshared = { path = \"../dep\", features = [\"typo\"] }\n",
                ),
            ]);
            let GraphError::InvalidDependency {
                dependency,
                message,
                ..
            } = preprocess_error(&root, &root_view).await
            else {
                panic!("expected an invalid dependency");
            };
            assert_eq!(dependency, "shared");
            assert!(message.contains("typo"), "{message}");
        });
    }

    #[test]
    fn a_reserved_feature_name_is_rejected_wherever_the_manifest_sits() {
        jals_exec::block_on_inline(async {
            // `discover` validates the root, and every dependency manifest is validated by the parse
            // inside `probe_manifest` — so `Dependency::validate_features` reaches a transitively
            // declared entry too, and the graph never has to re-check what an edge carries.
            let root = manifest("[dependencies]\nmid = { path = \"mid\" }\n");
            let root_view = view(&[
                (
                    "mid/jals.toml",
                    b"[dependencies]\ndep = { path = \"../dep\", features = [\"default\"] }\n",
                ),
                ("dep/jals.toml", PROBE_MANIFEST),
                ("dep/build.rhai", FEATURE_PROBE),
            ]);
            let error = MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
                .await
                .unwrap_err();
            let GraphError::MalformedManifest {
                location, message, ..
            } = error
            else {
                panic!("expected a malformed dependency manifest, got {error:?}");
            };
            assert_eq!(location, "mid/jals.toml");
            assert!(
                message.contains("lists the reserved feature `default`"),
                "{message}"
            );
        });
    }

    #[test]
    fn declared_edge_features_are_visible_on_the_graph_metadata() {
        jals_exec::block_on_inline(async {
            // The edges are the single source of truth the per-node union reads, and they are a pure
            // function of the manifests — nothing about the host or its selection reaches them. That
            // is also what keeps a dependency's build-script fingerprint stable across a changed
            // root `--features`, so switching sides no longer re-runs every dependency script.
            let root = manifest(
                "[dependencies]\n\
                 src = { path = \"dep\", features = [\"hello\", \"world\"] }\n\
                 lib = { jar = \"libs/x.jar\" }\n",
            );
            let root_view = view(&[
                ("dep/jals.toml", PROBE_MANIFEST),
                ("dep/build.rhai", FEATURE_PROBE),
                ("libs/x.jar", b"not really a jar"),
            ]);
            let graph = MemoryProjectGraph::discover(&root, DependencyScope::Build, &root_view)
                .await
                .unwrap();
            let metadata = graph.metadata();
            let edges: Vec<(&str, Vec<&str>)> = metadata
                .edges()
                .iter()
                .map(|edge| {
                    (
                        edge.dependency.as_str(),
                        edge.features.iter().map(String::as_str).collect(),
                    )
                })
                .collect();
            assert_eq!(
                edges,
                [("lib", vec![]), ("src", vec!["hello", "world"]),],
                "a jar carries no features; a source edge carries exactly what it declared"
            );
        });
    }
}
