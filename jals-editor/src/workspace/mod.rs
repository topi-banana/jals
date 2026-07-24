//! The protocol-neutral editor workspace: one project's analysis state and query surface.
//!
//! Both editor hosts used to own this lifecycle — file identity, per-file parse/resolution/facts
//! caches, incremental index assembly, classpath and feature folding — each in its own dialect
//! (the LSP incrementally, the browser by re-parsing the whole project per query). It lives here
//! once: a [`Workspace`] owns a [`ProjectStorage`] aggregate through sealed source/cache adapters,
//! identifies files by validated [`FileKey`] values in one immutable revision, and answers every
//! query in neutral shapes (byte offsets, [`FileRange`]s). Hosts keep only protocol mapping and
//! coordinate conversion.
//!
//! Analysis is `async` end to end: parsing, resolution, and index assembly yield cooperatively,
//! and a full load/refresh/reload distributes per-file parse + fact extraction across the
//! storage's [`Exec`] via [`Exec::fan_out`] (ordered, so ids and index inputs stay deterministic).
//! Queries answer from cached trees; only lazily-computed per-file analysis awaits.

mod file_id;

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::OnceCell;
use core::ops::Range;

use jals_config::FeatureSet;
use jals_exec::Exec;
use jals_hir::{FileFacts, FileId, LoweredClasspath, ProjectIndex, Resolved, SourceLocations, Ty};
use jals_storage::{CacheBackend, DirKey, FileKey, ProjectStorage, ProjectView, SourceBackend};
use jals_syntax::cfg::CfgMap;
use jals_syntax::{Parse, SyntaxNode};

use crate::document::Document;
use crate::{
    Completion, FileDiagnostic, FileDiagnostics, Highlight, Outline, OutlineNode, ProjectQueries,
    QueryFile, SemanticToken, SemanticTokens,
};
use file_id::WorkspaceFileId;

/// What the load fan-out extracts alongside each parse: nothing (navigation-only overlays),
/// plain [`FileFacts`] (source dependencies, attribute-free projects), or facts filtered by
/// `#[cfg(...)]` against a build-feature set (project files with the `attributes` dialect on).
#[derive(Clone)]
enum ExtractFacts {
    No,
    Plain,
    Cfg(BTreeSet<String>),
}

/// A file tracked by the [`Workspace`]: its virtual path, its cached [`Document`], and the two
/// lazily-computed analysis caches an edit invalidates by replacing the whole struct.
struct SourceFile {
    /// The `/`-separated virtual path identifying the file within the workspace's tree.
    path: FileKey,
    /// The file's text, coordinate map, and parsed CST.
    doc: Document,
    /// The file's `#[cfg(...)]` evaluation against the workspace's build features, computed once
    /// on first use. A pure function of the parse *and* the workspace's feature selection — an
    /// edit replaces the whole struct, and a feature change resets it via
    /// [`reset_analysis`](SourceFile::reset_analysis). Empty (and free) when the `attributes`
    /// dialect feature is off.
    cfg: OnceCell<CfgMap>,
    /// The file's name resolution, computed once on first use and cached. A pure function of the
    /// parse and the `cfg` map, so it stays valid for this file's lifetime — an edit replaces the
    /// whole struct (see [`Workspace::set_overlay`]), starting fresh. Lets a project-wide query
    /// that scans every file (find-references) resolve each one only once instead of on every
    /// request.
    resolved: OnceCell<Resolved>,
    /// The file's cached index facts — the CST-walking half of building the [`ProjectIndex`],
    /// computed once on first use. Like `resolved`, a pure function of the parse and the `cfg`
    /// map, so an edit (which replaces the whole struct) re-extracts them while every other file
    /// reuses its cache. This is what makes [`rebuild_index`](Workspace::rebuild_index) re-walk
    /// only the changed file rather than every file in the project.
    facts: OnceCell<FileFacts>,
}

impl SourceFile {
    /// Wrap an already-parsed [`Document`] (an open-editor overlay), sharing its `Arc`s so the
    /// text is never reparsed, with fresh analysis caches.
    const fn with_document(path: FileKey, doc: Document) -> Self {
        Self {
            path,
            doc,
            cfg: OnceCell::new(),
            resolved: OnceCell::new(),
            facts: OnceCell::new(),
        }
    }

    /// The file's cached `#[cfg(...)]` evaluation (computed on first use): against
    /// `build_features` when the `attributes` dialect feature is on, the empty map otherwise —
    /// so an attribute-free project pays nothing beyond one cell.
    fn cfg_map(&self, build_features: &BTreeSet<String>, attributes: bool) -> &CfgMap {
        self.cfg.get_or_init(|| {
            if attributes {
                CfgMap::compute(&self.doc.parse, build_features)
            } else {
                CfgMap::default()
            }
        })
    }

    /// Reset the per-file analysis caches (the `cfg` map and everything derived from it), for a
    /// feature-selection change — the parse itself is a pure function of the text and survives.
    fn reset_analysis(&mut self) {
        self.cfg = OnceCell::new();
        self.resolved = OnceCell::new();
        self.facts = OnceCell::new();
    }

    /// The file's cached name resolution (computed on first use), skipping `cfg`-disabled hosts.
    ///
    /// Async-once over the `OnceCell`: compute, then publish. The workspace is single-threaded,
    /// but two queries interleaved at an await point can both see the empty cell and both
    /// compute — the value is a pure function of the parse and `cfg`, so the duplicate work is
    /// benign and the first `set` wins. No locking or single-flight gate keeps the pattern
    /// cancellation-safe (a dropped query leaves the cell either empty or fully published).
    async fn resolved(&self, cfg: &CfgMap) -> &Resolved {
        if self.resolved.get().is_none() {
            let resolved = Resolved::resolve_node_with_cfg(&self.doc.parse.syntax(), cfg).await;
            let _ = self.resolved.set(resolved);
        }
        self.resolved.get().expect("published just above")
    }

    /// The file's cached index facts (computed on first use), the input to the incremental
    /// [`ProjectIndex::assemble`], skipping `cfg`-disabled hosts. Same async-once pattern (and
    /// duplicate-compute window) as [`resolved`](Self::resolved).
    async fn facts(&self, cfg: &CfgMap) -> &FileFacts {
        if self.facts.get().is_none() {
            let facts = ProjectIndex::extract_file_with_cfg(&self.doc.parse.syntax(), cfg).await;
            let _ = self.facts.set(facts);
        }
        self.facts.get().expect("published just above")
    }

    /// Parse every `(path, text)` into a [`SourceFile`] through one ordered [`Exec::fan_out`].
    ///
    /// Each worker receives one file's text (`Send`), parses it, and — unless `extract_facts` is
    /// [`ExtractFacts::No`] — walks the fresh CST for its index [`FileFacts`], entirely on that worker; only
    /// plain `Send` data (the green-tree [`Parse`], the [`LineIndex`], the facts) crosses back.
    /// [`ExtractFacts::Cfg`]'s worker-side [`CfgMap`] holds `!Send` tree nodes, so it is computed
    /// *and dropped* on the worker; the per-file `cfg` cell refills lazily on the main task when
    /// a query needs it. The results come back in input order regardless of completion order, so
    /// [`FileId`] assignment and index inputs stay deterministic across runtimes and worker
    /// counts. On the inline/wasm executors the jobs run sequentially on the calling task —
    /// identical output.
    async fn parse_all(
        exec: &Exec,
        files: Vec<(FileKey, String)>,
        extract_facts: ExtractFacts,
    ) -> Vec<Self> {
        let (paths, texts): (Vec<FileKey>, Vec<String>) = files.into_iter().unzip();
        let parsed = exec
            .fan_out(texts, move |text: String| {
                let extract = extract_facts.clone();
                async move {
                    let parse = Parse::parse(&text).await;
                    let facts = match extract {
                        ExtractFacts::No => None,
                        ExtractFacts::Plain => {
                            Some(ProjectIndex::extract_file(&parse.syntax()).await)
                        }
                        ExtractFacts::Cfg(features) => {
                            let cfg = CfgMap::compute(&parse, &features);
                            Some(ProjectIndex::extract_file_with_cfg(&parse.syntax(), &cfg).await)
                        }
                    };
                    (text, parse, facts)
                }
            })
            .await;
        paths
            .into_iter()
            .zip(parsed)
            .map(|(path, (text, parse, facts))| {
                let file = Self::with_document(path, Document::with_parse(text, parse));
                if let Some(facts) = facts {
                    let _ = file.facts.set(facts);
                }
                file
            })
            .collect()
    }

    /// Read and parse each library `.java` in `paths` through `view`, skipping unreadable ones
    /// and de-duplicating by path, into [`SourceFile`]s (fanned out per file). Shared by both
    /// library-source kinds; `extract_facts` pre-fills the (never `cfg`-filtered — a dependency
    /// is indexed under its own authority) facts cache for the kind that is an index input (the
    /// `git`/`path` source dependencies), while the navigation-only `-sources.jar` overlays skip
    /// the walk their facts cache never needs.
    async fn read_all(
        exec: &Exec,
        view: &ProjectView,
        paths: &[FileKey],
        extract_facts: ExtractFacts,
    ) -> Vec<Self> {
        let mut files = Vec::new();
        let mut seen = BTreeSet::new();
        for path in paths {
            if let Ok(text) = view.file_text(path)
                && seen.insert(path.clone())
            {
                files.push((path.clone(), text.to_owned()));
            }
        }
        Self::parse_all(exec, files, extract_facts).await
    }

    /// Pair each file in `files` with a sequential [`FileId`] in the id-space named by `space`,
    /// mapping it through `extract` — the `(FileId, T)` inputs the index builds from. Every id
    /// space derives its inputs the same way — the base offset lives only in
    /// [`WorkspaceFileId::to_raw`].
    fn file_inputs<'a, T>(
        files: &'a [Self],
        space: fn(u32) -> WorkspaceFileId,
        extract: impl Fn(&'a Self) -> T,
    ) -> Vec<(FileId, T)> {
        files
            .iter()
            .enumerate()
            .map(|(k, f)| (WorkspaceFileId::of_index(space, k), extract(f)))
            .collect()
    }
}

/// Everything a host resolves about a project before the workspace takes over.
///
/// Where the directory and exact-file sources live, the already-lowered classpath, the library
/// `.java` navigation sources, and the resolved language feature set. The host owns all I/O policy
/// behind this struct (which jars to download, which directories to walk for `.class` files); the
/// workspace owns everything after.
#[derive(Default)]
pub struct ProjectLayout {
    /// The directories walked for `.java` (virtual paths).
    pub source_roots: Vec<DirKey>,
    /// Exact project source files outside (or alongside) the source-root walk. Hosts use this for
    /// generated sources explicitly selected by identity; siblings are not implicitly included.
    pub project_sources: Vec<FileKey>,
    /// The project's classpath `.class` files, already lowered by the host via
    /// [`ProjectIndex::lower_classpath`] — reused on every index rebuild so external library
    /// types resolve; static for the workspace's lifetime (a dependency jar does not change
    /// under us).
    pub classpath: LoweredClasspath,
    /// The `.java` of each `[dependencies]` `sources` jar (virtual paths): navigation-only
    /// overlays so go-to-definition can land in a classpath type/member's real source.
    pub library_sources: Vec<FileKey>,
    /// The `.java` of each `git`/`path` `[dependencies]` entry (virtual paths): index inputs
    /// (`Source`-origin types that resolve for analysis) *and* navigation targets.
    pub source_dep_sources: Vec<FileKey>,
    /// The project's resolved language feature set (from `[package] features`); empty when the
    /// manifest declares none, disabling the feature-gated lint rules.
    pub feature_set: FeatureSet,
    /// The root project's resolved build features (the `[features]` names a build script queries
    /// and `#[cfg(feature = "…")]` tests). Consulted only when `feature_set` enables the
    /// `attributes` dialect; empty otherwise, so an attribute-free project's analysis is
    /// independent of the selection.
    pub build_features: BTreeSet<String>,
}

impl ProjectLayout {
    /// A spec with just a root and its source roots — no classpath, library sources, or features.
    pub fn new(source_roots: Vec<DirKey>) -> Self {
        Self {
            source_roots,
            ..Self::default()
        }
    }
}

/// A single project's symbol index plus the per-file data needed to answer cross-file queries.
///
/// All project I/O goes through [`ProjectStorage`], and only during [`load`](Workspace::load),
/// [`refresh`](Workspace::refresh), or [`reload_project_files`](Workspace::reload_project_files) —
/// queries answer from the cached parsed trees. Open documents are kept current via
/// [`set_overlay`](Workspace::set_overlay) /
/// [`sync_overlay`](Workspace::sync_overlay), which swap a file's cached text for the editor's
/// and rebuild the (in-memory, no-I/O) index. The rebuild re-walks only the changed file's CST
/// but reassembles the whole index — linear in project size, adequate until an incremental index
/// is needed.
pub struct Workspace<S: SourceBackend, C: CacheBackend> {
    /// The execution context, cloned from the storage's at [`load`](Workspace::load): fan-outs
    /// during full loads/refreshes, cooperative yields everywhere else.
    exec: Exec,
    storage: ProjectStorage<S, C>,
    source_roots: Vec<DirKey>,
    project_sources: Vec<FileKey>,
    files: Vec<SourceFile>,
    by_path: BTreeMap<FileKey, FileId>,
    /// Extracted library *source* files (the `.java` of a `[dependencies]` `sources` jar), kept
    /// so a classpath type/member can be navigated into its real source. Addressed by a
    /// [`Library`](WorkspaceFileId::Library) [`FileId`], disjoint from the project files' low
    /// ids, so [`ws_file`](Workspace::ws_file) can route a go-to-definition target to the right
    /// vec. Never project inputs and never linted — they are navigation targets only.
    library_files: Vec<SourceFile>,
    /// Library **source** files of a `git` / `path` `[dependencies]` entry. Unlike
    /// [`library_files`](Workspace::library_files) (navigation-only overlays paired with a
    /// binary `-sources.jar`), these have no `.class` backing them, so they *are* index inputs —
    /// folded in as [`Source`](jals_hir::ItemOrigin::Source)-origin types that resolve for
    /// inference/hover and are go-to-definition targets in their own right. Addressed by a
    /// [`SourceDep`](WorkspaceFileId::SourceDep) [`FileId`], a third id space disjoint from both
    /// the project files and [`library_files`](Workspace::library_files). Still never linted —
    /// they are not project files.
    source_dep_files: Vec<SourceFile>,
    /// The already-lowered classpath from [`ProjectLayout`], reused on every rebuild.
    classpath: LoweredClasspath,
    /// Where each library type/member is declared in [`library_files`](Workspace::library_files),
    /// indexed once at construction (the sources of a fixed dependency do not change) and folded
    /// into every rebuild so a classpath item gets a real-source go-to-definition target.
    source_locations: SourceLocations,
    /// The project's resolved language feature set, folded into every
    /// [`diagnostics`](Workspace::diagnostics) run for the feature-gated lint rules.
    feature_set: FeatureSet,
    /// The root project's resolved build features, the set each project file's `#[cfg(...)]`
    /// evaluates against when the `attributes` dialect feature is on (see
    /// [`SourceFile::cfg_map`]).
    build_features: BTreeSet<String>,
    /// The embedded `java.lang` stub facts, extracted once at construction and reused on every
    /// rebuild (they never change), so the stubs are never re-parsed per edit. Their reserved
    /// [`FileId`]s are disjoint from the project / library id-spaces.
    stub_facts: Vec<(FileId, FileFacts)>,
    index: ProjectIndex,
}

impl<S: SourceBackend, C: CacheBackend> Workspace<S, C> {
    /// Build a workspace over `storage`: union `.java` under the spec's source roots with its exact
    /// project sources, read and parse each (skipping unreadable ones), register library /
    /// source-dependency `.java`, and build the symbol index. Paths are visited in sorted order and
    /// per-file work fans out in that order, so the index is deterministic. The execution context
    /// is taken from the storage ([`ProjectStorage::exec`]) — one handle threads through the whole
    /// aggregate.
    pub async fn load(storage: ProjectStorage<S, C>, spec: ProjectLayout) -> Self {
        let exec = storage.exec().clone();
        let view = storage.view();
        let mut project_sources = spec.project_sources;
        project_sources.sort();
        project_sources.dedup();

        // Extracted library sources, each a navigation file in the `Library` id-space. Read and
        // parsed once; the resulting trees feed `index_source_locations` below.
        let library_files =
            SourceFile::read_all(&exec, &view, &spec.library_sources, ExtractFacts::No).await;
        let source_locations = Self::library_source_locations(&library_files).await;

        // `git`/`path` library sources, read once; folded into the index as `Source`-origin types
        // on every rebuild (their `FileId`s are assigned in `rebuild_index`). They are facts
        // inputs, so the fan-out pre-extracts their facts alongside the parse.
        let source_dep_files =
            SourceFile::read_all(&exec, &view, &spec.source_dep_sources, ExtractFacts::Plain).await;

        let mut ws = Self {
            exec,
            storage,
            source_roots: spec.source_roots,
            project_sources,
            files: Vec::new(),
            by_path: BTreeMap::new(),
            library_files,
            source_dep_files,
            index: ProjectIndex::builder(&[]).with_stdlib().build().await,
            classpath: spec.classpath,
            source_locations,
            feature_set: spec.feature_set,
            build_features: spec.build_features,
            // Extracted once; reused on every rebuild (the stubs never change).
            stub_facts: ProjectIndex::stub_facts().await,
        };
        ws.reload_project_files().await;
        ws
    }

    /// Replace the exact project source files included alongside the source-root walks.
    ///
    /// Call [`reload_project_files`](Self::reload_project_files) to apply the new membership to the
    /// cached files and symbol index.
    pub fn set_project_sources(&mut self, mut project_sources: Vec<FileKey>) {
        project_sources.sort();
        project_sources.dedup();
        self.project_sources = project_sources;
    }

    /// Reload project `.java` files from the aggregate's current [`ProjectView`] and rebuild the
    /// symbol index.
    ///
    /// Unlike [`refresh`](Self::refresh), this does not refresh the source backend and always
    /// rebuilds. Use it after a transaction through [`storage_mut`](Self::storage_mut), which has
    /// already updated the aggregate's view and therefore produces no backend change on refresh.
    /// Files are registered in sorted path order so [`FileId`] assignment stays deterministic.
    pub async fn reload_project_files(&mut self) {
        let view = self.storage.view();
        let mut paths: Vec<FileKey> = self
            .source_roots
            .iter()
            .flat_map(|root| view.tree().files_under(root))
            .filter(|file| file.key().has_extension("java"))
            .map(|file| file.key().clone())
            .collect();
        paths.extend(
            self.project_sources
                .iter()
                .filter_map(|path| view.tree().lookup_file(path).map(|_| path.clone())),
        );
        paths.sort();
        paths.dedup();
        let mut inputs = Vec::new();
        for path in paths {
            if let Ok(text) = view.file_text(&path) {
                inputs.push((path, text.to_owned()));
            }
        }
        self.files = SourceFile::parse_all(&self.exec, inputs, self.project_extract()).await;
        self.by_path = self
            .files
            .iter()
            .enumerate()
            .map(|(k, file)| {
                (
                    file.path.clone(),
                    WorkspaceFileId::of_index(WorkspaceFileId::Project, k),
                )
            })
            .collect();
        self.rebuild_index().await;
    }

    /// Rebuild the symbol index from the cached per-file facts, stubs, and classpath. No I/O.
    ///
    /// Built with the embedded `java.lang` stubs and the project's classpath `.class` files
    /// folded in, so a core JDK type (`String`, `Object`, …) or an external library type resolves
    /// to a real item with members and supertypes — hover, completion, member navigation, and
    /// assignment checks see through it instead of stopping at a bare name.
    ///
    /// Incremental: awaiting each file's [`facts`](SourceFile::facts) re-extracts only the file
    /// whose facts cache was cleared (the one just edited, via
    /// [`set_overlay`](Workspace::set_overlay)); every other file — pre-filled by the load /
    /// refresh fan-out — plus the stubs, reuses its cache. So a keystroke re-walks a single
    /// file's CST, and [`ProjectIndex::assemble`] (order-sensitive, one task) reassembles the
    /// whole index — identical to a from-scratch build, but without re-walking the project.
    async fn rebuild_index(&mut self) {
        // Project facts are `cfg`-filtered against the workspace's build features; the
        // source-dependency facts never are (a dependency's own feature selection does not reach
        // this seam — its files are indexed in full), which the shared empty map encodes.
        let empty = CfgMap::default();
        let mut project = Vec::with_capacity(self.files.len());
        for (k, file) in self.files.iter().enumerate() {
            project.push((
                WorkspaceFileId::of_index(WorkspaceFileId::Project, k),
                file.facts(self.cfg_of(file)).await,
            ));
        }
        // The `git`/`path` library sources are *also* index inputs (as `Source`-origin types),
        // under their own `SourceDep` ids so they navigate back to the right files. The
        // `-sources.jar` overlays remain navigation-only (folded in via `source_locations`).
        let mut source_deps = Vec::with_capacity(self.source_dep_files.len());
        for (k, file) in self.source_dep_files.iter().enumerate() {
            source_deps.push((
                WorkspaceFileId::of_index(WorkspaceFileId::SourceDep, k),
                file.facts(&empty).await,
            ));
        }
        let stub: Vec<(FileId, &FileFacts)> = self
            .stub_facts
            .iter()
            .map(|(file, ff)| (*file, ff))
            .collect();
        self.index = ProjectIndex::assemble(
            &project,
            &source_deps,
            &stub,
            &self.classpath,
            &self.source_locations,
        )
        .await;
    }

    /// Whether the `attributes` dialect feature is on — the gate for every `cfg` evaluation.
    const fn attributes_on(&self) -> bool {
        self.feature_set.contains(jals_config::Feature::Attributes)
    }

    /// What the project-file load fan-out extracts: `cfg`-filtered facts when the `attributes`
    /// dialect is on, plain facts otherwise.
    fn project_extract(&self) -> ExtractFacts {
        if self.attributes_on() {
            ExtractFacts::Cfg(self.build_features.clone())
        } else {
            ExtractFacts::Plain
        }
    }

    /// The cached `cfg` map of one project file, under the workspace's feature selection.
    fn cfg_of<'f>(&self, source: &'f SourceFile) -> &'f CfgMap {
        source.cfg_map(&self.build_features, self.attributes_on())
    }

    /// The workspace file a [`FileId`] addresses, routed by its id-space: a project file, a
    /// `git`/`path` library source ([`SourceDep`](WorkspaceFileId::SourceDep)), or a
    /// `-sources.jar` overlay ([`Library`](WorkspaceFileId::Library)). `None` when the
    /// within-space index addresses no real file — e.g. a classpath member with no source, whose
    /// reserved id decodes into `SourceDep` far past any extracted file — so a go-to-definition
    /// target that points nowhere openable yields nothing instead of panicking.
    fn ws_file(&self, id: FileId) -> Option<&SourceFile> {
        match WorkspaceFileId::from_raw(id) {
            WorkspaceFileId::Project(i) => self.files.get(i as usize),
            WorkspaceFileId::Library(i) => self.library_files.get(i as usize),
            WorkspaceFileId::SourceDep(i) => self.source_dep_files.get(i as usize),
        }
    }

    /// The *project* file `id` addresses, or `None` for a library / source-dep / out-of-range id.
    /// The per-file queries anchor here: only project files are edited and queried directly.
    fn project_file(&self, id: FileId) -> Option<&SourceFile> {
        match WorkspaceFileId::from_raw(id) {
            WorkspaceFileId::Project(i) => self.files.get(i as usize),
            WorkspaceFileId::Library(_) | WorkspaceFileId::SourceDep(_) => None,
        }
    }

    /// Shared semantic query module for one project file, or `None` for an id that addresses no
    /// project file. Awaits only this file's lazy resolution; project-wide references receive
    /// the other files separately.
    async fn queries(&self, file: FileId) -> Option<ProjectQueries<'_>> {
        let source = self.project_file(file)?;
        Some(ProjectQueries::new(
            &self.index,
            QueryFile::new(
                file,
                source.doc.parse.syntax(),
                source.resolved(self.cfg_of(source)).await,
            ),
        ))
    }

    /// The project symbol index.
    pub const fn index(&self) -> &ProjectIndex {
        &self.index
    }

    /// The project's resolved language feature set (from `[package] features`), empty when none
    /// is declared. Folded into [`diagnostics`](Workspace::diagnostics) automatically; exposed
    /// for hosts that assemble a lint config elsewhere.
    pub const fn feature_set(&self) -> FeatureSet {
        self.feature_set
    }

    /// Replace the resolved feature selection — the language feature set (the browser
    /// re-resolves it when the manifest buffer is edited) and the build features that
    /// `#[cfg(feature = "…")]` tests.
    ///
    /// The `cfg` maps — and everything derived from them (name resolution, index facts) — are
    /// functions of this selection, so a change resets every project file's analysis caches and
    /// rebuilds the index. Non-incremental by design: a feature change is a rare, settings-level
    /// event, unlike the per-keystroke single-file invalidation. No-op when nothing changed, so
    /// hosts can call it unconditionally on config re-reads.
    pub async fn set_features(
        &mut self,
        feature_set: FeatureSet,
        build_features: BTreeSet<String>,
    ) {
        if self.feature_set == feature_set && self.build_features == build_features {
            return;
        }
        self.feature_set = feature_set;
        self.build_features = build_features;
        for file in &mut self.files {
            file.reset_analysis();
        }
        self.rebuild_index().await;
    }

    /// Replace the lowered classpath and fold it into the index (the browser resolves
    /// dependencies asynchronously, after the workspace already exists).
    pub async fn set_classpath(&mut self, classpath: LoweredClasspath) {
        self.classpath = classpath;
        self.rebuild_index().await;
    }

    /// Replace the exact dependency source files used for navigation and source-dependency
    /// indexing. The paths are reread from the current project view; they never become editable
    /// project files and do not widen any project source root.
    pub async fn set_dependency_sources(
        &mut self,
        mut library_sources: Vec<FileKey>,
        mut source_dep_sources: Vec<FileKey>,
    ) {
        library_sources.sort();
        library_sources.dedup();
        source_dep_sources.sort();
        source_dep_sources.dedup();
        let view = self.storage.view();
        let library_files =
            SourceFile::read_all(&self.exec, &view, &library_sources, ExtractFacts::No).await;
        let source_dep_files =
            SourceFile::read_all(&self.exec, &view, &source_dep_sources, ExtractFacts::Plain).await;
        self.replace_dependency_files(library_files, source_dep_files)
            .await;
    }

    /// Replace dependency sources from owned text without adding them to the project view.
    ///
    /// This is the portable host path for detached cache artifacts. Keys remain available for
    /// navigation, but the files are neither editable project inputs nor members of
    /// [`ProjectStorage`]. Inputs are sorted and de-duplicated before parsing so file ids and index
    /// assembly remain deterministic.
    pub async fn set_dependency_source_texts(
        &mut self,
        mut library_sources: Vec<(FileKey, String)>,
        mut source_dep_sources: Vec<(FileKey, String)>,
    ) {
        library_sources.sort();
        library_sources.dedup_by(|left, right| left.0 == right.0);
        source_dep_sources.sort();
        source_dep_sources.dedup_by(|left, right| left.0 == right.0);
        let library_files =
            SourceFile::parse_all(&self.exec, library_sources, ExtractFacts::No).await;
        let source_dep_files =
            SourceFile::parse_all(&self.exec, source_dep_sources, ExtractFacts::Plain).await;
        self.replace_dependency_files(library_files, source_dep_files)
            .await;
    }

    /// Publish already-parsed dependency files as one coherent navigation/index replacement.
    async fn replace_dependency_files(
        &mut self,
        library_files: Vec<SourceFile>,
        source_dep_files: Vec<SourceFile>,
    ) {
        self.library_files = library_files;
        self.source_locations = Self::library_source_locations(&self.library_files).await;
        self.source_dep_files = source_dep_files;
        self.rebuild_index().await;
    }

    async fn library_source_locations(files: &[SourceFile]) -> SourceLocations {
        let inputs = SourceFile::file_inputs(files, WorkspaceFileId::Library, |file| {
            file.doc.parse.syntax()
        });
        ProjectIndex::index_source_locations(&inputs).await
    }

    /// The file tree the workspace was loaded from, for the host's own browsing (the playground's
    /// file sidebar reads directories; its editor writes files back).
    pub fn view(&self) -> ProjectView {
        self.storage.view()
    }

    /// Mutable access to the file tree. Writing a file does *not* update the index — reflect an
    /// edit with [`set_overlay`](Workspace::set_overlay) / [`sync_overlay`](Workspace::sync_overlay).
    pub const fn storage(&self) -> &ProjectStorage<S, C> {
        &self.storage
    }

    /// Mutable access to project storage. Call [`reload_project_files`](Self::reload_project_files)
    /// after committing a transaction that changes project Java files.
    pub const fn storage_mut(&mut self) -> &mut ProjectStorage<S, C> {
        &mut self.storage
    }

    /// Publish a fresh backend snapshot and invalidate parse/HIR caches in the same revision.
    pub async fn refresh(&mut self) -> Result<jals_storage::RefreshOutcome, jals_storage::Error> {
        let outcome = self.storage.refresh().await?;
        if !outcome.changed {
            return Ok(outcome);
        }
        self.reload_project_files().await;
        Ok(outcome)
    }

    /// The id of the file at `path`, if it is part of this workspace.
    pub fn file_id(&self, path: &FileKey) -> Option<FileId> {
        self.by_path.get(path).copied()
    }

    /// Every indexed project file with its cached document, in path order. This is the one
    /// statement of project membership (source-root Java plus exact project sources, overlays
    /// included); hosts browse this set instead of re-deriving it from the raw tree.
    pub fn files(&self) -> impl Iterator<Item = (&FileKey, &Document)> {
        self.by_path
            .iter()
            .filter_map(|(path, id)| Some((path, &self.project_file(*id)?.doc)))
    }

    /// The virtual path of the file `id` addresses (any id-space), or `None` for an id that
    /// addresses no real file.
    pub fn path_of(&self, id: FileId) -> Option<&FileKey> {
        Some(&self.ws_file(id)?.path)
    }

    /// The cached document of the file `id` addresses (any id-space) — the text/coordinates a
    /// host needs to map a [`FileRange`] target into its protocol. `None` for an id that
    /// addresses no real file.
    pub fn document(&self, id: FileId) -> Option<&Document> {
        Some(&self.ws_file(id)?.doc)
    }

    /// Whether `path` belongs to this workspace: indexed, selected exactly, or under a source root
    /// (so a project file the editor hasn't opened yet still resolves here).
    pub fn owns_path(&self, path: &FileKey) -> bool {
        self.by_path.contains_key(path)
            || self.under_source_root(path)
            || self.project_sources.binary_search(path).is_ok()
    }

    /// Whether `path` lies under one of this workspace's source roots.
    fn under_source_root(&self, path: &FileKey) -> bool {
        self.source_roots.iter().any(|root| Self::under(path, root))
    }

    /// Reflect an open document into the index: replace the cached copy of `path` with the
    /// editor's current text (or add it, if `path` is a project file created after the initial
    /// load), then rebuild the index. The document's `Arc`s are shared, so the text is never
    /// reparsed. Returns whether `path` belongs to this workspace.
    pub async fn set_overlay(
        &mut self,
        path: &FileKey,
        doc: &Document,
    ) -> Result<bool, jals_storage::Error> {
        // Fresh analysis caches: this file re-extracts on the next rebuild; every other file
        // reuses its cache, so a keystroke re-walks only the edited file (no fan-out — one file).
        let file = SourceFile::with_document(path.clone(), doc.clone());
        if let Some(id) = self.by_path.get(path).copied() {
            self.files[id.0 as usize] = file;
        } else {
            if !self.under_source_root(path) && self.project_sources.binary_search(path).is_err() {
                return Ok(false);
            }
            let id = WorkspaceFileId::of_index(WorkspaceFileId::Project, self.files.len());
            self.by_path.insert(path.clone(), id);
            self.files.push(file);
        }
        self.storage.set_overlay(
            self.storage.revision(),
            path.clone(),
            doc.text.as_bytes().to_vec(),
        )?;
        self.rebuild_index().await;
        Ok(true)
    }

    /// Reflect the editor's live text for `path` into the index, parsing and rebuilding **only
    /// when it differs** from the cached copy — so a query storm over an unchanged buffer (hover
    /// after hover) hits the caches instead of re-analyzing per request. Returns whether `path`
    /// belongs to this workspace.
    pub async fn sync_overlay(
        &mut self,
        path: &FileKey,
        text: &str,
    ) -> Result<bool, jals_storage::Error> {
        if let Some(source) = self.file_id(path).and_then(|id| self.project_file(id)) {
            if &*source.doc.text == text {
                return Ok(true);
            }
        } else if !self.under_source_root(path) && self.project_sources.binary_search(path).is_err()
        {
            return Ok(false);
        }
        let doc = Document::new(text.to_owned()).await;
        self.set_overlay(path, &doc).await
    }

    /// Go-to-definition for the cursor at `offset` in `file`: a file-local binding if there is
    /// one, then the project type a reference names, then — for a member access — the member the
    /// receiver type declares.
    pub async fn definition(&self, file: FileId, offset: usize) -> Option<crate::FileRange> {
        self.queries(file).await?.definition(offset).await
    }

    /// Find-references for the cursor at `offset` in `file`: every occurrence of the symbol under
    /// the cursor — across the whole project when it is a project type, or within this one file
    /// for a file-local binding. The declaration is included when `include_declaration`. Empty if
    /// the cursor is on no resolvable symbol.
    pub async fn references(
        &self,
        file: FileId,
        offset: usize,
        include_declaration: bool,
    ) -> Vec<crate::FileRange> {
        let Some(queries) = self.queries(file).await else {
            return Vec::new();
        };
        // Lazily resolved: a file-local binding returns before any other file resolves.
        if let Some(local) = queries.local_references(offset, include_declaration) {
            return local;
        }
        // A project type: resolve every file (cached across queries), then scan project-wide.
        let mut files = Vec::with_capacity(self.files.len());
        for (index, source) in self.files.iter().enumerate() {
            files.push(QueryFile::new(
                WorkspaceFileId::of_index(WorkspaceFileId::Project, index),
                source.doc.parse.syntax(),
                source.resolved(self.cfg_of(source)).await,
            ));
        }
        queries.references(offset, include_declaration, files)
    }

    /// The inferred type under `offset` in `file`, or `None` for nothing informative.
    pub async fn hover(&self, file: FileId, offset: usize) -> Option<Ty> {
        self.queries(file).await?.hover(offset).await
    }

    /// [`hover`](Self::hover) rendered as the shared Markdown (a fenced ` ```java ` block).
    pub async fn hover_markdown(&self, file: FileId, offset: usize) -> Option<String> {
        self.queries(file).await?.hover_markdown(offset).await
    }

    /// Completions for the cursor at `offset` in `file`: the members after a `.`, otherwise the
    /// in-scope bindings, project types, and keywords.
    pub async fn completions(&self, file: FileId, offset: usize) -> Vec<Completion> {
        match self.queries(file).await {
            Some(queries) => queries.completions(offset).await,
            None => Vec::new(),
        }
    }

    /// Signature help for the call at `offset` in `file`, with cross-file type resolution.
    pub async fn signature_help(
        &self,
        file: FileId,
        offset: usize,
    ) -> Option<jals_hir::SignatureHelp> {
        self.queries(file).await?.signature_help(offset).await
    }

    /// Occurrence highlights for the cursor at `offset` in `file`, resolved against the project
    /// so a cross-file type name highlights precisely.
    pub async fn highlights(&self, file: FileId, offset: usize) -> Vec<Highlight> {
        self.queries(file)
            .await
            .map(|queries| queries.highlights(offset))
            .unwrap_or_default()
    }

    /// Classified semantic tokens for `file`, resolved against the project so a cross-file type
    /// name is classified by its declared kind rather than the generic `Type`.
    pub async fn semantic_tokens(&self, file: FileId) -> Vec<SemanticToken> {
        match self.project_file(file) {
            Some(source) => {
                SemanticTokens::classify(&source.doc.parse.syntax(), Some((&self.index, file)))
                    .await
            }
            None => Vec::new(),
        }
    }

    /// The document outline of `file`. Purely syntactic over the cached tree, so it stays a
    /// synchronous read.
    pub fn outline(&self, file: FileId) -> Vec<OutlineNode> {
        self.project_file(file)
            .map(|source| Outline::of(&source.doc.parse.syntax()))
            .unwrap_or_default()
    }

    /// The canonical diagnostics of `file` under `config`, with the project's feature set, its
    /// `cfg` evaluation, and the index folded in (see [`FileDiagnostics`]).
    pub async fn diagnostics(
        &self,
        file: FileId,
        config: &jals_config::lint::Config,
    ) -> Vec<FileDiagnostic> {
        let Some(source) = self.project_file(file) else {
            return Vec::new();
        };
        let mut config = config.clone();
        config.features = self.feature_set;
        let cfg = self.cfg_of(source);
        FileDiagnostics::assemble(
            &source.doc.parse,
            Some(source.resolved(cfg).await),
            Some((&self.index, file)),
            &config,
            Some(cfg),
        )
        .await
    }

    /// prepareRename for the cursor at `offset` in `file`: the byte range of the identifier under
    /// the cursor when it names a renamable symbol, else `None` (an external name, a
    /// keyword/literal, or a withheld member — see [`ProjectQueries::renamable_range`]).
    pub async fn prepare_rename(&self, file: FileId, offset: usize) -> Option<Range<usize>> {
        self.queries(file).await?.renamable_range(offset)
    }

    /// The occurrence set a rename of the symbol at `offset` in `file` rewrites — project-wide
    /// for a project type, within the file for a file-local binding — or `None` if the cursor is
    /// on no renamable symbol or there is nothing to change. The host validates the new name
    /// ([`crate::Ident::is_valid_java_identifier`]) and shapes the edit.
    pub async fn rename_targets(
        &self,
        file: FileId,
        offset: usize,
    ) -> Option<Vec<crate::FileRange>> {
        self.prepare_rename(file, offset).await?;
        let targets = self.references(file, offset, true).await;
        (!targets.is_empty()).then_some(targets)
    }

    fn under(file: &FileKey, dir: &DirKey) -> bool {
        file.path().starts_with(dir.path())
    }
}

/// The semantic inputs for a document outside any indexed workspace.
///
/// A one-file, stdlib-aware project model: the LSP's fallback for files that belong to no
/// `jals.toml` project. Every fallback request drives the same [`ProjectQueries`] the workspace
/// does.
pub struct SingleFileProject {
    root: SyntaxNode,
    resolved: Resolved,
    index: ProjectIndex,
}

impl SingleFileProject {
    /// The single [`FileId`] every one-file query resolves against — the open document itself. A
    /// query target carrying any other id (a source-less library stub keeps a reserved id) is not
    /// openable in this document and must not be mapped onto its text.
    pub const FILE: FileId = FileId(0);

    /// Build the one-file project over an already-parsed document.
    pub async fn new(parse: &Parse) -> Self {
        let root = parse.syntax();
        let resolved = Resolved::resolve_node(&root).await;
        let index = ProjectIndex::builder(&[(Self::FILE, root.clone())])
            .with_stdlib()
            .build()
            .await;
        Self {
            root,
            resolved,
            index,
        }
    }

    /// The query module over this one file.
    pub fn queries(&self) -> ProjectQueries<'_> {
        ProjectQueries::new(&self.index, self.file())
    }

    /// The file's query inputs (for the project-files iterator of a references query).
    pub fn file(&self) -> QueryFile<'_> {
        QueryFile::new(Self::FILE, self.root.clone(), &self.resolved)
    }

    /// The one-file symbol index (with the stdlib stubs folded in).
    pub const fn index(&self) -> &ProjectIndex {
        &self.index
    }

    /// The canonical diagnostics of the file under `config`, with the one-file index folded in
    /// (so in-file subtyping and stdlib-classified exceptions still check).
    pub async fn diagnostics(
        &self,
        parse: &Parse,
        config: &jals_config::lint::Config,
    ) -> Vec<FileDiagnostic> {
        FileDiagnostics::assemble(
            parse,
            Some(&self.resolved),
            Some((&self.index, Self::FILE)),
            config,
            // A single detached file has no manifest, so no `attributes` dialect and no `cfg`.
            None,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use jals_exec::block_on_inline;
    use jals_storage::{CodeTree, Entry, MemoryCache, MemorySource, MemoryStorage};

    use super::*;
    use crate::DiagnosticSeverity;

    /// A two-file project: `Main` references `Greeter` across files.
    fn memory(files: &[(&str, &str)]) -> MemoryStorage {
        MemoryStorage::memory(
            CodeTree::new(files.iter().map(|(path, text)| {
                Entry::File(FileKey::parse(path).unwrap(), text.as_bytes().to_vec())
            }))
            .unwrap(),
        )
    }

    fn key(path: &str) -> FileKey {
        FileKey::parse(path).unwrap()
    }

    async fn sample_workspace() -> Workspace<MemorySource, MemoryCache> {
        let storage = memory(&[
            (
                "src/Greeter.java",
                "public class Greeter { public String greet(String name) { return name; } }",
            ),
            (
                "src/Main.java",
                "public class Main { void run() { Greeter g = new Greeter(); } }",
            ),
        ]);
        Workspace::load(
            storage,
            ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
        )
        .await
    }

    #[test]
    fn load_walks_sorts_and_indexes_the_tree() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            // Sorted walk: Greeter before Main.
            assert_eq!(
                ws.path_of(FileId(0)).map(ToString::to_string),
                Some("src/Greeter.java".to_owned())
            );
            assert_eq!(
                ws.path_of(FileId(1)).map(ToString::to_string),
                Some("src/Main.java".to_owned())
            );
            assert_eq!(ws.file_id(&key("src/Main.java")), Some(FileId(1)));
            assert!(
                ws.owns_path(&key("src/New.java")),
                "unopened but under root"
            );
            assert!(!ws.owns_path(&key("elsewhere/X.java")));
        });
    }

    #[test]
    fn exact_project_sources_are_indexed_without_widening_to_siblings() {
        block_on_inline(async {
            let generated = key("generated/Selected.java");
            let sibling = key("generated/Sibling.java");
            let storage = memory(&[
                ("src/Main.java", "class Main { Selected value; }"),
                ("generated/Selected.java", "class Selected { }"),
                ("generated/Sibling.java", "class Sibling { }"),
            ]);
            let mut ws = Workspace::load(
                storage,
                ProjectLayout {
                    project_sources: vec![generated.clone(), generated.clone()],
                    ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
                },
            )
            .await;

            assert!(ws.owns_path(&generated));
            assert!(ws.file_id(&generated).is_some());
            assert!(!ws.owns_path(&sibling));
            assert!(ws.file_id(&sibling).is_none());

            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &ws.document(main).unwrap().text;
            let target = ws
                .definition(main, text.find("Selected value").unwrap())
                .await
                .expect("the exact generated source resolves as a project type");
            assert_eq!(ws.path_of(target.file), Some(&generated));

            ws.set_project_sources(vec![sibling.clone()]);
            ws.reload_project_files().await;
            assert!(!ws.owns_path(&generated));
            assert!(ws.file_id(&generated).is_none());
            assert!(ws.owns_path(&sibling));
            assert!(ws.file_id(&sibling).is_some());
        });
    }

    #[test]
    fn definition_resolves_across_files() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &*ws.document(main).unwrap().text.clone();
            let offset = text.find("Greeter g").unwrap();
            let target = ws
                .definition(main, offset)
                .await
                .expect("cross-file definition");
            assert_eq!(
                ws.path_of(target.file).map(ToString::to_string),
                Some("src/Greeter.java".to_owned())
            );
        });
    }

    #[test]
    fn references_span_the_project_and_include_the_declaration() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &*ws.document(main).unwrap().text.clone();
            let offset = text.find("Greeter g").unwrap();
            let refs = ws.references(main, offset, true).await;
            let files: BTreeSet<_> = refs
                .iter()
                .filter_map(|r| ws.path_of(r.file).map(ToString::to_string))
                .collect();
            assert!(files.contains("src/Greeter.java"), "{refs:?}");
            assert!(files.contains("src/Main.java"), "{refs:?}");
            assert!(refs.len() >= 3, "two uses + declaration: {refs:?}");
        });
    }

    #[test]
    fn set_overlay_updates_the_index_and_new_files_join_under_a_root() {
        block_on_inline(async {
            let mut ws = sample_workspace().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();

            // Renaming `Greeter` in its defining file makes Main's reference unresolvable.
            assert!(
                ws.set_overlay(
                    &key("src/Greeter.java"),
                    &Document::new("public class Renamed { }".to_owned()).await,
                )
                .await
                .unwrap()
            );
            let diags = ws
                .diagnostics(main, &jals_config::lint::Config::default())
                .await;
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == Some("cannot-resolve") && d.message.contains("Greeter")),
                "{diags:?}"
            );

            // A brand-new file under a source root joins the project...
            assert!(
                ws.set_overlay(
                    &key("src/Extra.java"),
                    &Document::new("public class Extra { }".to_owned()).await,
                )
                .await
                .unwrap()
            );
            assert!(ws.file_id(&key("src/Extra.java")).is_some());
            // ...but a file outside every root is rejected.
            assert!(
                !ws.set_overlay(
                    &key("elsewhere/X.java"),
                    &Document::new(String::new()).await
                )
                .await
                .unwrap()
            );
        });
    }

    #[test]
    fn sync_overlay_is_a_no_op_for_unchanged_text() {
        block_on_inline(async {
            let mut ws = sample_workspace().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = ws.document(main).unwrap().text.clone();
            let parse_before = alloc::sync::Arc::as_ptr(&ws.document(main).unwrap().parse);
            assert!(ws.sync_overlay(&key("src/Main.java"), &text).await.unwrap());
            let parse_after = alloc::sync::Arc::as_ptr(&ws.document(main).unwrap().parse);
            assert_eq!(parse_before, parse_after, "unchanged text must not reparse");

            // A real edit replaces the document.
            assert!(
                ws.sync_overlay(&key("src/Main.java"), "public class Main { }")
                    .await
                    .unwrap()
            );
            assert_ne!(
                alloc::sync::Arc::as_ptr(&ws.document(main).unwrap().parse),
                parse_after
            );
        });
    }

    #[test]
    fn explicit_reload_indexes_files_added_by_a_storage_transaction() {
        block_on_inline(async {
            let storage = memory(&[("src/Main.java", "class Main { Generated generated; }")]);
            let mut ws = Workspace::load(
                storage,
                ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
            )
            .await;
            let generated = key("src/Generated.java");

            let revision = ws.storage().revision();
            let mut transaction = ws.storage_mut().transaction(revision).unwrap();
            transaction
                .create_file(generated.clone(), b"class Generated { }".to_vec())
                .unwrap();
            transaction.commit().await.unwrap();
            assert!(ws.view().file(&generated).is_ok());
            assert!(ws.file_id(&generated).is_none());

            let outcome = ws.refresh().await.unwrap();
            assert!(!outcome.changed);
            assert!(ws.file_id(&generated).is_none());

            ws.reload_project_files().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &ws.document(main).unwrap().text;
            let target = ws
                .definition(main, text.find("Generated generated").unwrap())
                .await
                .expect("generated type resolves after explicit reload");
            assert_eq!(ws.path_of(target.file), Some(&generated));
        });
    }

    #[test]
    fn hover_completion_and_highlights_answer_from_the_index() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &*ws.document(main).unwrap().text.clone();

            // Hover over the `new Greeter()` expression shows the cross-file type.
            let new_expr = text.find("new Greeter").unwrap();
            let hover = ws.hover_markdown(main, new_expr).await.expect("hover");
            assert!(hover.contains("Greeter"), "{hover}");

            // Scope completions offer the sibling type.
            let inside = text.find("Greeter g").unwrap();
            let completions = ws.completions(main, inside).await;
            assert!(
                completions.iter().any(|c| c.label == "Greeter"),
                "{completions:?}"
            );

            // Highlights find both occurrences of `Greeter` in this file.
            let hl = ws.highlights(main, inside).await;
            assert_eq!(hl.len(), 2, "{hl:?}");
        });
    }

    #[test]
    fn semantic_tokens_and_outline_answer_neutrally() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            let greeter = ws.file_id(&key("src/Greeter.java")).unwrap();
            assert!(
                ws.semantic_tokens(greeter)
                    .await
                    .iter()
                    .any(|t| t.kind == crate::SemanticTokenKind::Class && t.declaration)
            );
            let outline = ws.outline(greeter);
            assert_eq!(outline[0].name, "Greeter");
        });
    }

    #[test]
    fn rename_gates_on_project_origin() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &*ws.document(main).unwrap().text.clone();

            // A cross-file *use* of a project type is renamable, and its targets span the project.
            let use_site = text.find("Greeter g").unwrap();
            assert!(ws.prepare_rename(main, use_site).await.is_some());
            let targets = ws.rename_targets(main, use_site).await.expect("targets");
            assert!(targets.len() >= 3, "{targets:?}");

            // A stdlib type (`String` in Greeter) is not host-editable: not renamable.
            let greeter = ws.file_id(&key("src/Greeter.java")).unwrap();
            let gtext = &*ws.document(greeter).unwrap().text.clone();
            let string_use = gtext.find("String name").unwrap();
            assert!(ws.prepare_rename(greeter, string_use).await.is_none());

            // A member (the method `greet`) is withheld.
            let method = gtext.find("greet").unwrap();
            assert!(ws.prepare_rename(greeter, method).await.is_none());
        });
    }

    #[test]
    fn classpath_types_fold_into_the_index() {
        block_on_inline(async {
            // A project whose classpath carries a compiled `Box.class`: the workspace folds it
            // into the index as a `Classpath`-origin type, so external library types resolve here.
            let class = jals_classfile::ClassFile::read(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/Box.class"
                ))
                .as_slice(),
            )
            .await
            .expect("parse Box.class");
            let storage = memory(&[(
                "src/Main.java",
                "class Main { void run() { Box b = new Box(); } }",
            )]);
            let spec = ProjectLayout {
                classpath: ProjectIndex::lower_classpath(&[class]).await,
                ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
            };
            let ws = Workspace::load(storage, spec).await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let diags = ws
                .diagnostics(main, &jals_config::lint::Config::default())
                .await;
            assert!(
                !diags.iter().any(|d| d.code == Some("cannot-resolve")),
                "Box resolves through the classpath: {diags:?}"
            );
        });
    }

    #[test]
    fn source_dep_files_are_indexed_and_navigable() {
        block_on_inline(async {
            // A `git`/`path` source dependency's `.java` resolves for analysis *and* is a
            // go-to-definition target, under the `SourceDep` id-space.
            let storage = memory(&[
                ("src/Main.java", "class Main { Lib l; }"),
                ("deps/lib/Lib.java", "public class Lib { }"),
            ]);
            let spec = ProjectLayout {
                source_dep_sources: vec![key("deps/lib/Lib.java")],
                ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
            };
            let ws = Workspace::load(storage, spec).await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let text = &*ws.document(main).unwrap().text.clone();
            let target = ws
                .definition(main, text.find("Lib l").unwrap())
                .await
                .expect("definition into the source dep");
            assert_eq!(
                ws.path_of(target.file).map(ToString::to_string),
                Some("deps/lib/Lib.java".to_owned())
            );
            // The dep is external: not renamable from the project.
            assert!(
                ws.prepare_rename(main, text.find("Lib l").unwrap())
                    .await
                    .is_none()
            );
        });
    }

    #[test]
    fn dependency_source_replacement_rereads_exact_files_without_project_membership() {
        block_on_inline(async {
            let storage = memory(&[
                ("src/Main.java", "class Main { First value; }"),
                ("deps/First.java", "class First { }"),
                ("deps/Second.java", "class Second { }"),
            ]);
            let mut ws = Workspace::load(
                storage,
                ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
            )
            .await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();

            ws.set_dependency_sources(Vec::new(), vec![key("deps/First.java")])
                .await;
            let offset = ws.document(main).unwrap().text.find("First value").unwrap();
            let target = ws
                .definition(main, offset)
                .await
                .expect("first dependency source resolves");
            assert_eq!(ws.path_of(target.file), Some(&key("deps/First.java")));
            assert!(ws.file_id(&key("deps/First.java")).is_none());
            assert!(!ws.owns_path(&key("deps/First.java")));

            ws.set_dependency_sources(Vec::new(), vec![key("deps/Second.java")])
                .await;
            assert!(
                ws.definition(main, offset).await.is_none(),
                "the stale dependency source must leave the index"
            );
            assert!(ws.file_id(&key("deps/Second.java")).is_none());
        });
    }

    #[test]
    fn dependency_source_replacement_refreshes_library_navigation_locations() {
        block_on_inline(async {
            let class = jals_classfile::ClassFile::read(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/Box.class"
                ))
                .as_slice(),
            )
            .await
            .unwrap();
            let storage = memory(&[("src/Main.java", "class Main { Box value; }")]);
            let mut ws = Workspace::load(
                storage,
                ProjectLayout {
                    classpath: ProjectIndex::lower_classpath(&[class]).await,
                    ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
                },
            )
            .await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let offset = ws.document(main).unwrap().text.find("Box value").unwrap();
            assert!(ws.definition(main, offset).await.is_none());

            ws.set_dependency_source_texts(
                vec![(key("deps/Box.java"), "public class Box<T> { }".to_owned())],
                Vec::new(),
            )
            .await;
            let target = ws
                .definition(main, offset)
                .await
                .expect("classpath type navigates into its exact library source");
            assert_eq!(ws.path_of(target.file), Some(&key("deps/Box.java")));
            assert!(ws.file_id(&key("deps/Box.java")).is_none());
            assert!(ws.view().file(&key("deps/Box.java")).is_err());
        });
    }

    #[test]
    fn detached_dependency_texts_never_join_storage_and_empty_replacement_removes_types() {
        block_on_inline(async {
            let dependency = key(".jals/source-dependency/Detached.java");
            let storage = memory(&[("src/Main.java", "class Main { Detached value; }")]);
            let mut ws = Workspace::load(
                storage,
                ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
            )
            .await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let offset = ws
                .document(main)
                .unwrap()
                .text
                .find("Detached value")
                .unwrap();

            ws.set_dependency_source_texts(
                Vec::new(),
                vec![(dependency.clone(), "class Detached { }".to_owned())],
            )
            .await;
            let target = ws
                .definition(main, offset)
                .await
                .expect("detached dependency type resolves");
            assert_eq!(ws.path_of(target.file), Some(&dependency));
            assert!(ws.file_id(&dependency).is_none());
            assert!(!ws.owns_path(&dependency));
            assert!(ws.view().file(&dependency).is_err());
            assert!(ws.storage().clone().view().file(&dependency).is_err());

            ws.set_dependency_source_texts(Vec::new(), Vec::new()).await;
            assert!(
                ws.definition(main, offset).await.is_none(),
                "an empty replacement must remove the stale dependency type"
            );
            assert!(ws.view().file(&dependency).is_err());
        });
    }

    #[test]
    fn out_of_space_ids_answer_empty_not_panic() {
        block_on_inline(async {
            let ws = sample_workspace().await;
            let bogus = FileId(u32::MAX);
            assert!(ws.document(bogus).is_none());
            assert!(ws.path_of(bogus).is_none());
            assert!(ws.definition(bogus, 0).await.is_none());
            assert!(ws.references(bogus, 0, true).await.is_empty());
            assert!(
                ws.diagnostics(bogus, &jals_config::lint::Config::default())
                    .await
                    .is_empty()
            );
            assert!(ws.outline(bogus).is_empty());
        });
    }

    #[test]
    fn single_file_project_answers_the_same_queries() {
        block_on_inline(async {
            let text = "class C { int f; void m() { int x = f; } }";
            let parse = Parse::parse(text).await;
            let project = SingleFileProject::new(&parse).await;
            let offset = text.rfind('f').unwrap();
            let target = project
                .queries()
                .definition(offset)
                .await
                .expect("definition");
            assert_eq!(target.file, SingleFileProject::FILE);
            let diags = project
                .diagnostics(&parse, &jals_config::lint::Config::default())
                .await;
            assert!(
                diags.iter().any(|d| d.code == Some("unused-local")),
                "{diags:?}"
            );
        });
    }

    /// A workspace over one `#[cfg]`-bearing project, with `attributes` on and the given build
    /// features.
    async fn cfg_workspace(build_features: &[&str]) -> Workspace<MemorySource, MemoryCache> {
        let storage = memory(&[
            (
                "src/Gated.java",
                "#[cfg(feature = \"fancy\")]\npublic class Gated {}",
            ),
            ("src/Main.java", "public class Main { Gated g; }"),
        ]);
        Workspace::load(
            storage,
            ProjectLayout {
                feature_set: jals_config::FeatureSet::resolve(&[jals_config::Feature::Attributes]),
                build_features: build_features.iter().map(ToString::to_string).collect(),
                ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
            },
        )
        .await
    }

    #[test]
    fn cfg_disabled_type_unresolves_across_files_and_fades() {
        block_on_inline(async {
            let config = jals_config::lint::Config::default();

            // Feature off: `Gated` is not indexed — the cross-file reference cannot resolve —
            // and the disabled declaration is reported as a faded `cfg` hint in its own file.
            let ws = cfg_workspace(&[]).await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let diags = ws.diagnostics(main, &config).await;
            assert!(
                diags.iter().any(|d| d.code == Some("cannot-resolve")),
                "{diags:?}"
            );
            let gated = ws.file_id(&key("src/Gated.java")).unwrap();
            let diags = ws.diagnostics(gated, &config).await;
            assert!(
                diags.iter().any(|d| d.code == Some("cfg")
                    && d.unnecessary
                    && d.severity == DiagnosticSeverity::Hint),
                "{diags:?}"
            );

            // Feature on: the type resolves and nothing is faded.
            let ws = cfg_workspace(&["fancy"]).await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            let diags = ws.diagnostics(main, &config).await;
            assert!(
                !diags.iter().any(|d| d.code == Some("cannot-resolve")),
                "{diags:?}"
            );
            let gated = ws.file_id(&key("src/Gated.java")).unwrap();
            let diags = ws.diagnostics(gated, &config).await;
            assert!(!diags.iter().any(|d| d.unnecessary), "{diags:?}");
        });
    }

    #[test]
    fn set_features_invalidates_the_cfg_analysis() {
        block_on_inline(async {
            let config = jals_config::lint::Config::default();
            let mut ws = cfg_workspace(&[]).await;
            let main = ws.file_id(&key("src/Main.java")).unwrap();
            assert!(
                ws.diagnostics(main, &config)
                    .await
                    .iter()
                    .any(|d| d.code == Some("cannot-resolve"))
            );

            // Flipping the build features re-evaluates every file's `cfg` map and the index.
            let attrs = jals_config::FeatureSet::resolve(&[jals_config::Feature::Attributes]);
            ws.set_features(attrs, BTreeSet::from(["fancy".to_owned()]))
                .await;
            assert!(
                !ws.diagnostics(main, &config)
                    .await
                    .iter()
                    .any(|d| d.code == Some("cannot-resolve"))
            );

            // And back off again (also exercising the no-op short-circuit first).
            ws.set_features(attrs, BTreeSet::from(["fancy".to_owned()]))
                .await;
            ws.set_features(attrs, BTreeSet::new()).await;
            assert!(
                ws.diagnostics(main, &config)
                    .await
                    .iter()
                    .any(|d| d.code == Some("cannot-resolve"))
            );
        });
    }

    #[test]
    fn cfg_attribute_errors_surface_as_editor_diagnostics() {
        block_on_inline(async {
            let storage = memory(&[("src/Bad.java", "#[derive(Debug)]\npublic class Bad {}")]);
            let ws = Workspace::load(
                storage,
                ProjectLayout {
                    feature_set: jals_config::FeatureSet::resolve(&[
                        jals_config::Feature::Attributes,
                    ]),
                    ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
                },
            )
            .await;
            let bad = ws.file_id(&key("src/Bad.java")).unwrap();
            let diags = ws
                .diagnostics(bad, &jals_config::lint::Config::default())
                .await;
            assert!(
                diags.iter().any(|d| d.code == Some("cfg")
                    && d.severity == DiagnosticSeverity::Error
                    && d.message.contains("unknown attribute `derive`")),
                "{diags:?}"
            );
        });
    }
}
