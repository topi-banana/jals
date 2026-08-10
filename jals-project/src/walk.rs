//! The recursive dependency walk, written once.
//!
//! Discovery differs between hosts in exactly one thing: how a declared string — `path = "../lib"`,
//! `jar = "libs/x.jar"`, `source-dirs = ["src"]` — becomes something readable. Everything above
//! that is the same graph: depth-first order, cycle detection, edge dedup, the `external-*` names
//! given to what falls outside the declaring project, and the shape of a node's identity.
//!
//! So the seam is [`GraphHost`], and it sits at the *acquisition*, not at the walk. Both adapters
//! end up reading through a [`ProjectView`] — the portable one selects a subtree of the tree it was
//! handed, the native one snapshots a host directory — which is why a single walk over views can
//! serve both.
//!
//! # The two error channels
//!
//! They are not interchangeable, and the seam keeps them apart on purpose:
//!
//! - `Result<_, String>` — **warn and continue**. A dependency that cannot be acquired, a
//!   `[build]` entry that names nothing: the project is still worth resolving without it.
//! - [`GraphError`] — **stop**. The tree behind an acquired dependency could not be read at all, or
//!   its manifest is malformed. Continuing would resolve a project silently missing a dependency.
//!
//! [`acquire_path`](GraphHost::acquire_path)/[`acquire_git`](GraphHost::acquire_git) are on the
//! first; [`open`](GraphHost::open) is on the second. Collapsing them into one step is how a failed
//! snapshot would quietly become a warning.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_config::{Dependency, GitDependency, JarDependency, Manifest, PathDependency};
use jals_exec::LocalBoxFuture;
use jals_storage::{DirKey, EntryRef, FileKey, Name, ProjectView, RelativePath};

use crate::graph::{
    BinaryInput, CapturedClasspathEntry, CapturedClasspathKind, CapturedFile, CycleEdge,
    DeclaredBinaryEdge, DeclaredEdgeFeatures, EdgeRemap, GraphEdge, GraphError, GraphWarning,
    NodeBody, NodeId, ResolvedNode, SourceNode,
};

/// Where a declared path landed relative to the project that declared it.
///
/// The adapter answers only *whether* it is inside; what an outside one is called is
/// [`GraphWalk`]'s to decide, so the four `external-*` spellings live in one place.
pub(crate) enum Placement {
    /// Inside: this is its path relative to the declaring project.
    Local(RelativePath),
    /// Outside: nothing in the declaring project addresses it.
    External,
}

/// One declared file, read.
///
/// The bytes rather than a view and a key: a native host resolves a `[build] classpath` entry to a
/// path outside the project, which sits in no [`ProjectView`] at all. Reading is the host's job for
/// the same reason acquiring is.
pub(crate) struct DeclaredFile {
    pub(crate) bytes: Vec<u8>,
    /// The file's own name, for the synthesized path it gets when it sits outside the declaring
    /// project.
    pub(crate) name: Name,
    /// Identity payload for a `[dependencies]` jar. Unused for a `[build] classpath` file, which
    /// becomes no node of its own.
    pub(crate) identity: String,
    pub(crate) placement: Placement,
}

/// One declared directory, resolved and ready to walk.
pub(crate) struct DeclaredTree {
    pub(crate) view: ProjectView,
    pub(crate) root: DirKey,
    pub(crate) placement: Placement,
}

/// What a `[build] classpath` entry turned out to be.
pub(crate) enum DeclaredEntry {
    File(DeclaredFile),
    Tree(DeclaredTree),
}

/// A dependency source located but not yet read.
pub(crate) struct Acquired<H: GraphHost> {
    /// Identity payload. [`GraphWalk`] folds [`GraphHost::SCOPE`] in front of it, which is the one
    /// place a node id takes its shape.
    pub(crate) identity: String,
    /// How a diagnostic names this node — in whatever terms the host acquired it, per
    /// [`ResolvedNode::location`].
    pub(crate) location: String,
    pub(crate) site: H::Site,
    pub(crate) guard: H::Guard,
}

/// An acquired source, opened for reading.
pub(crate) struct Opened<H: GraphHost> {
    /// The dependency's own tree. Moved into the node.
    pub(crate) view: ProjectView,
    /// What its own declarations resolve against. Borrowed for the recursion.
    pub(crate) project: H::Project,
    /// Anything the host noticed while opening, to be attributed to this node.
    pub(crate) notes: Vec<String>,
    /// Why `jals.toml` could not be read, when the view does not have it but the host knows it is
    /// there. Consulted **only** when the lookup misses — a permission failure is not missing data,
    /// but neither is a project that genuinely has no manifest.
    pub(crate) manifest_unreadable: Option<String>,
}

/// How one host acquires and addresses what a manifest declares.
pub(crate) trait GraphHost: Sized {
    /// A located-but-unopened dependency source.
    type Site;
    /// The project a declaration is resolved against. Built by [`open`](Self::open), because a
    /// native one needs both the location (known at acquisition) and the view (made by opening).
    type Project;
    /// Whatever an acquired source must outlive. `()` where acquiring copies nothing.
    type Guard;

    /// Namespace for every node id this host mints. Part of cache identity, so it is frozen once
    /// released.
    const SCOPE: &'static str;

    /// Where a malformed-manifest diagnostic says the manifest was.
    fn manifest_location(&self, id: &NodeId, acquired: &Acquired<Self>) -> String;

    /// Locate a `path` dependency. Warn-and-continue.
    async fn acquire_path(
        &mut self,
        project: &Self::Project,
        dependency: &PathDependency,
    ) -> Result<Acquired<Self>, String>;

    /// Locate a `git` dependency. Warn-and-continue.
    async fn acquire_git(
        &mut self,
        project: &Self::Project,
        name: &str,
        dependency: &GitDependency,
    ) -> Result<Acquired<Self>, String>;

    /// Read the acquired source's tree. Stop-the-walk.
    async fn open(&mut self, acquired: &Acquired<Self>) -> Result<Opened<Self>, GraphError>;

    /// The node is being admitted to the graph — not a repeat and not a cycle.
    fn admitted(&mut self, acquired: &Acquired<Self>);

    /// Done with an acquired source's subtree. Warn-and-continue: failing to tidy up is not a
    /// failure to resolve.
    async fn release(&mut self, guard: Self::Guard) -> Result<(), String>;

    /// Resolve a `[dependencies]` `jar`/`sources` locator that is not a URL.
    async fn resolve_declared_file(
        &mut self,
        project: &Self::Project,
        raw: &str,
        role: &str,
    ) -> Result<DeclaredFile, String>;

    /// Resolve one `[build] source-dirs` entry.
    async fn resolve_source_dir(
        &mut self,
        project: &Self::Project,
        raw: &str,
        notes: &mut Vec<String>,
    ) -> Result<DeclaredTree, String>;

    /// Resolve one `[build] classpath` entry, which may be a file or a directory.
    async fn resolve_classpath_entry(
        &mut self,
        project: &Self::Project,
        raw: &str,
        notes: &mut Vec<String>,
    ) -> Result<DeclaredEntry, String>;
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

/// What one completed walk produces. The adapter's `discover` turns this into a
/// `ResolvedProjectGraph`, because only it knows what host residue to attach.
pub(crate) struct WalkOutput {
    pub(crate) nodes: Vec<ResolvedNode>,
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) order: Vec<usize>,
    pub(crate) warnings: Vec<GraphWarning>,
}

pub(crate) struct GraphWalk<'h, H: GraphHost> {
    /// Borrowed rather than owned: a host accumulates its own residue while the walk runs — the
    /// native one collects watch paths — and it goes back to the caller who knows what it is for.
    host: &'h mut H,
    nodes: Vec<ResolvedNode>,
    seen_nodes: BTreeSet<NodeId>,
    states: BTreeMap<NodeId, VisitState>,
    edges: Vec<GraphEdge>,
    stack: Vec<StackEntry>,
    order: Vec<usize>,
    warnings: Vec<GraphWarning>,
}

impl<H: GraphHost> GraphWalk<'_, H> {
    /// Walk `root_manifest`'s dependencies, and theirs, from `root`.
    ///
    /// `warnings` seeds the output with whatever the caller already had to say about the root
    /// itself, so those come before anything the walk finds.
    pub(crate) async fn run(
        host: &mut H,
        root: &H::Project,
        root_manifest: &Manifest,
        warnings: Vec<GraphWarning>,
    ) -> Result<WalkOutput, GraphError> {
        let mut walk = GraphWalk {
            host,
            nodes: Vec::new(),
            seen_nodes: BTreeSet::new(),
            states: BTreeMap::new(),
            edges: Vec::new(),
            stack: Vec::new(),
            order: Vec::new(),
            warnings,
        };
        walk.visit_dependencies(None, root, root_manifest).await?;
        Ok(WalkOutput {
            nodes: walk.nodes,
            edges: walk.edges,
            order: walk.order,
            warnings: walk.warnings,
        })
    }

    /// This host's spelling of one node id.
    ///
    /// The scope prefix is applied here and nowhere else. Two hosts discovering what a reader would
    /// call the same project produce different ids on purpose — a captured subtree and a host
    /// directory are not the same bytes reached two ways — and doing it in one place is what keeps
    /// that a decision rather than an accident of two format strings.
    fn node_id(payload: &str) -> NodeId {
        NodeId::from_identity(format!("{}\0{payload}", H::SCOPE).as_bytes())
    }

    fn visit_dependencies<'a>(
        &'a mut self,
        parent: Option<NodeId>,
        declaring: &'a H::Project,
        manifest: &'a Manifest,
    ) -> LocalBoxFuture<'a, Result<(), GraphError>> {
        Box::pin(async move {
            for (name, dependency) in &manifest.dependencies {
                // A jar is a leaf, and the walk is over once its bytes are in hand. A path and a
                // repository are the same walk from the moment either has been acquired, so only
                // the acquiring tells them apart.
                let acquired = match dependency {
                    Dependency::Jar(jar) => {
                        self.visit_jar(parent.as_ref(), declaring, name, manifest, dependency, jar)
                            .await;
                        continue;
                    }
                    Dependency::Path(path) => self.host.acquire_path(declaring, path).await,
                    Dependency::Git(git) => self.host.acquire_git(declaring, name, git).await,
                };
                match acquired {
                    Ok(acquired) => {
                        let declared = DeclaredEdgeFeatures::of(dependency);
                        self.visit_source(
                            parent.clone(),
                            name,
                            declared,
                            dependency.is_optional(),
                            acquired,
                        )
                        .await?;
                    }
                    Err(message) => self.warn_declared(parent.as_ref(), name, message),
                }
            }
            Ok(())
        })
    }

    /// One `[dependencies]` jar entry, and the companion sources archive it may name.
    ///
    /// Every failure here is warn-and-continue: a jar that cannot be read costs the classpath one
    /// entry, where a source project that cannot be read costs the graph a whole subtree.
    async fn visit_jar(
        &mut self,
        parent: Option<&NodeId>,
        declaring: &H::Project,
        name: &str,
        manifest: &Manifest,
        dependency: &Dependency,
        jar: &JarDependency,
    ) {
        let remap = match EdgeRemap::of(manifest, dependency) {
            Ok(remap) => remap,
            Err(message) => {
                self.warn_declared(parent, name, message);
                None
            }
        };
        let optional = dependency.is_optional();
        let classes = DeclaredBinaryEdge::classes(jar.recursive.unwrap_or(false), remap, optional);
        if let Err(message) = self
            .visit_binary(parent.cloned(), declaring, name, &jar.jar, classes)
            .await
        {
            self.warn_declared(parent, name, message);
        }
        if let Some(sources) = &jar.sources
            && let Err(message) = self
                .visit_binary(
                    parent.cloned(),
                    declaring,
                    name,
                    sources,
                    DeclaredBinaryEdge::sources(optional),
                )
                .await
        {
            self.warn_declared(parent, name, message);
        }
    }

    async fn visit_source(
        &mut self,
        parent: Option<NodeId>,
        dependency: &str,
        declared: DeclaredEdgeFeatures,
        optional: bool,
        acquired: Acquired<H>,
    ) -> Result<(), GraphError> {
        let id = Self::node_id(&acquired.identity);
        let incoming = GraphEdge {
            from: parent.clone(),
            dependency: dependency.to_owned(),
            to: id.clone(),
            recursive: false,
            features: declared.features,
            default_features: declared.default_features,
            // Only a `jar` entry can carry one, and this is the source-form edge.
            remap: None,
            optional,
        };
        self.edges.push(incoming.clone());
        match self.states.get(&id) {
            Some(VisitState::Complete) => return Ok(()),
            Some(VisitState::Visiting) => return Err(self.cycle(&incoming)),
            None => {}
        }

        // The id, not the location: only the cleanup failure at the end names the declaring
        // project, and only a host that copies anything can reach it. Resolving a location here
        // would scan the nodes and allocate for every dependency, where nothing reads the result.
        let declared_by = parent;
        let result = self.visit_opened(&id, incoming, &acquired).await;
        // Housekeeping, not part of resolving the graph, so it runs whether or not the visit
        // succeeded and never turns a resolved graph into a failed one.
        let cleanup = self.host.release(acquired.guard).await;
        result?;
        if let Err(message) = cleanup {
            // The declaring project resolves the same now as it would have before the visit: it
            // was pushed as a node before it began declaring anything.
            self.warn_declared(declared_by.as_ref(), dependency, message);
        }
        Ok(())
    }

    /// The half of [`visit_source`](Self::visit_source) that runs with the source open, split off
    /// so the release above it happens on every path out.
    async fn visit_opened(
        &mut self,
        id: &NodeId,
        incoming: GraphEdge,
        acquired: &Acquired<H>,
    ) -> Result<(), GraphError> {
        let opened = self.host.open(acquired).await?;
        let Opened {
            view,
            project,
            notes,
            manifest_unreadable,
        } = opened;
        // Node-level: what the host noticed opening this project belongs to the project, not to any
        // one entry it declares.
        for note in notes {
            self.warnings
                .push(GraphWarning::node(acquired.location.clone(), note));
        }
        let manifest = self.probe_manifest(id, acquired, &view, manifest_unreadable.as_deref())?;
        let body = match &manifest {
            Some(manifest) => {
                let authored_sources = self
                    .capture_manifest_sources(&project, &acquired.location, manifest)
                    .await;
                let classpath = self
                    .capture_manifest_classpath(&project, &acquired.location, manifest)
                    .await;
                NodeBody::JalsSource {
                    source: SourceNode {
                        view,
                        authored_sources,
                        classpath,
                    },
                    manifest: Box::new(manifest.clone()),
                }
            }
            // No manifest, so nothing declares anything: no `[build]` section to capture, and no
            // dependencies to recurse into.
            None => NodeBody::PlainSource(SourceNode {
                authored_sources: Self::capture_plain_sources(&view),
                view,
                classpath: Vec::new(),
            }),
        };
        let index = self.nodes.len();
        self.seen_nodes.insert(id.clone());
        self.nodes.push(ResolvedNode {
            id: id.clone(),
            location: acquired.location.clone(),
            body,
        });
        self.states.insert(id.clone(), VisitState::Visiting);
        self.stack.push(StackEntry {
            id: id.clone(),
            incoming,
        });
        self.host.admitted(acquired);
        if let Some(manifest) = &manifest {
            self.visit_dependencies(Some(id.clone()), &project, manifest)
                .await?;
        }
        self.stack.pop();
        self.states.insert(id.clone(), VisitState::Complete);
        self.order.push(index);
        Ok(())
    }

    async fn visit_binary(
        &mut self,
        parent: Option<NodeId>,
        declaring: &H::Project,
        dependency: &str,
        locator: &str,
        declared: DeclaredBinaryEdge,
    ) -> Result<(), String> {
        let DeclaredBinaryEdge {
            recursive,
            source_archive,
            remap,
            optional,
        } = declared;
        let role = if source_archive { "source" } else { "binary" };
        let (id, input) = if jals_classpath::ExternalLocator::is_remote(locator) {
            let input = if source_archive {
                BinaryInput::ExternalSource {
                    locator: locator.to_owned(),
                }
            } else {
                BinaryInput::External {
                    locator: locator.to_owned(),
                }
            };
            (Self::node_id(&format!("{role}-external\0{locator}")), input)
        } else {
            let raw = locator.strip_prefix("file://").unwrap_or(locator);
            let declared = self
                .host
                .resolve_declared_file(declaring, raw, role)
                .await?;
            let captured = CapturedFile {
                path: Self::external_file(&declared.placement, &declared.name, role),
                bytes: declared.bytes,
            };
            let input = if source_archive {
                BinaryInput::CapturedSource(captured)
            } else {
                BinaryInput::Captured(captured)
            };
            (Self::node_id(&declared.identity), input)
        };
        let binary = DeclaredEdgeFeatures::binary();
        self.edges.push(GraphEdge {
            from: parent,
            dependency: dependency.to_owned(),
            to: id.clone(),
            recursive,
            features: binary.features,
            default_features: binary.default_features,
            remap,
            optional,
        });
        if !self.seen_nodes.insert(id.clone()) {
            return Ok(());
        }
        let index = self.nodes.len();
        self.nodes.push(ResolvedNode {
            id,
            location: locator.to_owned(),
            body: NodeBody::Binary(input),
        });
        self.order.push(index);
        Ok(())
    }

    /// The dependency's own manifest, or `None` when it simply has none.
    ///
    /// The view is asked first. `unreadable` is what the host has to say about a `jals.toml` it can
    /// see but could not read, and it is consulted only on the miss — a permission failure is not
    /// missing data, and asking the other way round would let it shadow a manifest that parsed.
    fn probe_manifest(
        &self,
        id: &NodeId,
        acquired: &Acquired<H>,
        view: &ProjectView,
        unreadable: Option<&str>,
    ) -> Result<Option<Manifest>, GraphError> {
        let key = FileKey::parse("jals.toml").expect("constant is a portable file key");
        let file = match view.tree().lookup_file(&key) {
            Some(EntryRef::File(file)) => file,
            Some(EntryRef::Directory(_)) => {
                return Err(GraphError::Acquisition {
                    operation: format!("reading dependency manifest for {id}"),
                    message: "`jals.toml` is not a file".to_owned(),
                });
            }
            // A project that has no manifest, unless the host saw one it could not read.
            None => {
                return unreadable.map_or(Ok(None), |message| {
                    Err(GraphError::Acquisition {
                        operation: format!("reading dependency manifest for {id}"),
                        message: message.to_owned(),
                    })
                });
            }
        };
        let malformed = |error: &dyn core::fmt::Display| GraphError::MalformedManifest {
            node: id.clone(),
            location: self.host.manifest_location(id, acquired),
            message: error.to_string(),
        };
        let text = file.text().map_err(|error| malformed(&error))?;
        text.parse::<Manifest>()
            .map(Some)
            .map_err(|error| malformed(&error))
    }

    /// Warn about a `[dependencies]` entry, attributed to the project whose manifest declares it.
    ///
    /// The entry name alone is not enough for a transitive project: `lib` says which line to look
    /// at, not which `jals.toml` it is on.
    fn warn_declared(&mut self, parent: Option<&NodeId>, name: &str, message: impl Into<String>) {
        let declaring = ResolvedNode::location_of(&self.nodes, parent);
        self.warnings
            .push(GraphWarning::declared(declaring, name, message));
    }

    /// Warn about a `[build] source-dirs` or `[build] classpath` entry of the node being visited.
    ///
    /// Attributed the same way a `[dependencies]` entry is, and for a stronger reason: `lib` is at
    /// least a name the reader chose, where `src/main/java` is the default every project writes.
    /// Never the root — its `[build]` section is lowered by `jals-classpath`, not here — so the
    /// declaring project is always a node and always worth naming.
    fn warn_entry(&mut self, location: &str, entry: &str, message: impl Into<String>) {
        self.warnings.push(GraphWarning::declared(
            Some(location.to_owned()),
            entry,
            message,
        ));
    }

    /// The path captured bytes are addressed by: where they sit in the declaring project, or a
    /// synthesized name when they sit outside it.
    ///
    /// One of the four places the `external-*` vocabulary is spelled, and the only one for a file
    /// that becomes a node. The name is qualified by `role` rather than being the bare file name:
    /// two out-of-project jars can share a basename, and the bare form silently made them one.
    fn external_file(placement: &Placement, name: &Name, role: &str) -> RelativePath {
        match placement {
            Placement::Local(path) if !path.is_root() => path.clone(),
            Placement::Local(_) | Placement::External => RelativePath::new([
                Name::new(format!("external-{role}")).expect("generated prefix is portable"),
                name.clone(),
            ]),
        }
    }

    /// The prefix a declared directory's members are addressed under.
    fn external_prefix(placement: &Placement, role: &str, index: usize) -> RelativePath {
        match placement {
            Placement::Local(path) => path.clone(),
            Placement::External => {
                RelativePath::new([Name::new(format!("external-{role}-{index}"))
                    .expect("generated prefix is portable")])
            }
        }
    }

    async fn capture_manifest_sources(
        &mut self,
        declaring: &H::Project,
        location: &str,
        manifest: &Manifest,
    ) -> Vec<CapturedFile> {
        // Deduplicates overlapping `source-dirs` and imposes one deterministic order.
        let mut files = BTreeMap::new();
        for (index, source) in manifest.build.source_dirs.iter().enumerate() {
            let mut notes = Vec::new();
            let resolved = self
                .host
                .resolve_source_dir(declaring, source, &mut notes)
                .await;
            for note in notes {
                self.warn_entry(location, source, note);
            }
            let tree = match resolved {
                Ok(tree) => tree,
                Err(message) => {
                    self.warn_entry(location, source, message);
                    continue;
                }
            };
            let prefix = Self::external_prefix(&tree.placement, "source", index);
            let prefix_len = tree.root.path().segments().len();
            for file in tree
                .view
                .tree()
                .files_under(&tree.root)
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

    /// A manifest-less project's sources: the first conventional root it actually has.
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
        declaring: &H::Project,
        location: &str,
        manifest: &Manifest,
    ) -> Vec<CapturedClasspathEntry> {
        let mut entries = Vec::new();
        for (index, entry) in manifest.build.classpath.iter().enumerate() {
            let mut notes = Vec::new();
            let resolved = self
                .host
                .resolve_classpath_entry(declaring, entry, &mut notes)
                .await;
            for note in notes {
                self.warn_entry(location, entry, note);
            }
            let kind = match resolved {
                Ok(DeclaredEntry::File(declared)) => {
                    let role = format!("classpath-{index}");
                    CapturedClasspathKind::File(CapturedFile {
                        path: Self::external_file(&declared.placement, &declared.name, &role),
                        bytes: declared.bytes,
                    })
                }
                Ok(DeclaredEntry::Tree(tree)) => {
                    let path = Self::external_prefix(&tree.placement, "classpath", index);
                    let prefix_len = tree.root.path().segments().len();
                    let members = tree
                        .view
                        .tree()
                        .files_under(&tree.root)
                        .filter(|file| file.key().has_extension("class"))
                        .map(|file| CapturedFile {
                            path: RelativePath::new(
                                file.key().path().segments().skip(prefix_len).cloned(),
                            ),
                            bytes: file.bytes().to_vec(),
                        })
                        .collect();
                    CapturedClasspathKind::Tree { path, members }
                }
                Err(message) => {
                    self.warn_entry(location, entry, message);
                    continue;
                }
            };
            entries.push(CapturedClasspathEntry {
                declared: entry.clone(),
                kind,
            });
        }
        entries
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
