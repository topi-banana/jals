//! The protocol-neutral editor workspace: one project's analysis state and query surface.
//!
//! Both editor hosts used to own this lifecycle — file identity, per-file parse/resolution/facts
//! caches, incremental index assembly, classpath and feature folding — each in its own dialect
//! (the LSP incrementally, the browser by re-parsing the whole project per query). It lives here
//! once: a [`Workspace`] owns a [`ProjectStorage`] aggregate through sealed source/cache adapters,
//! identifies files by validated [`FileKey`] values in one immutable revision, and answers every
//! query in neutral shapes (byte offsets, [`FileRange`]s). Hosts keep only protocol mapping and
//! coordinate conversion.

mod file_id;

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::OnceCell;
use core::ops::Range;

use jals_config::FeatureSet;
use jals_hir::{FileFacts, FileId, LoweredClasspath, ProjectIndex, Resolved, SourceLocations, Ty};
use jals_storage::{CacheBackend, DirKey, FileKey, ProjectStorage, ProjectView, SourceBackend};
use jals_syntax::{Parse, SyntaxNode};

use crate::document::Document;
use crate::{
    Completion, FileDiagnostic, FileDiagnostics, Highlight, Outline, OutlineNode, ProjectQueries,
    QueryFile, SemanticToken, SemanticTokens,
};
use file_id::WorkspaceFileId;

/// A file tracked by the [`Workspace`]: its virtual path, its cached [`Document`], and the two
/// lazily-computed analysis caches an edit invalidates by replacing the whole struct.
struct SourceFile {
    /// The `/`-separated virtual path identifying the file within the workspace's tree.
    path: FileKey,
    /// The file's text, coordinate map, and parsed CST.
    doc: Document,
    /// The file's name resolution, computed once on first use and cached. A pure function of the
    /// parse, so it stays valid for this file's lifetime — an edit replaces the whole struct
    /// (see [`Workspace::set_overlay`]), starting fresh. Lets a project-wide query that scans
    /// every file (find-references) resolve each one only once instead of on every request.
    resolved: OnceCell<Resolved>,
    /// The file's cached index facts — the CST-walking half of building the [`ProjectIndex`],
    /// computed once on first use. Like `resolved`, a pure function of the parse, so an edit
    /// (which replaces the whole struct) re-extracts them while every other file reuses its
    /// cache. This is what makes [`rebuild_index`](Workspace::rebuild_index) re-walk only the
    /// changed file rather than every file in the project.
    facts: OnceCell<FileFacts>,
}

impl SourceFile {
    fn new(path: FileKey, text: String) -> Self {
        Self::with_document(path, Document::new(text))
    }

    /// Wrap an already-parsed [`Document`] (an open-editor overlay), sharing its `Arc`s so the
    /// text is never reparsed, with fresh analysis caches.
    const fn with_document(path: FileKey, doc: Document) -> Self {
        Self {
            path,
            doc,
            resolved: OnceCell::new(),
            facts: OnceCell::new(),
        }
    }

    /// The file's cached name resolution (computed on first use).
    fn resolved(&self) -> &Resolved {
        self.resolved
            .get_or_init(|| Resolved::resolve_node(&self.doc.parse.syntax()))
    }

    /// The file's cached index facts (computed on first use), the input to the incremental
    /// [`ProjectIndex::assemble`].
    fn facts(&self) -> &FileFacts {
        self.facts
            .get_or_init(|| ProjectIndex::extract_file(&self.doc.parse.syntax()))
    }

    /// Read and parse each library `.java` in `paths` through `fs`, skipping unreadable ones and
    /// de-duplicating by path, into [`SourceFile`]s. Shared by both library-source kinds (the
    /// `-sources.jar` overlays and the `git`/`path` source dependencies).
    fn read_all(view: &ProjectView, paths: &[FileKey]) -> Vec<Self> {
        let mut files = Vec::new();
        let mut seen = BTreeSet::new();
        for path in paths {
            if let Ok(text) = view.file_text(path)
                && seen.insert(path.clone())
            {
                files.push(Self::new(path.clone(), text.to_owned()));
            }
        }
        files
    }

    /// Pair each file in `files` with a sequential [`FileId`] in the id-space named by `space`,
    /// mapping it through `extract` — the `(FileId, T)` inputs the index builds from. Shared by
    /// the project files ([`WorkspaceFileId::Project`]) and the two library-source id-spaces
    /// ([`Library`](WorkspaceFileId::Library) / [`SourceDep`](WorkspaceFileId::SourceDep)), so
    /// every id space derives its inputs the same way — the base offset lives only in
    /// [`WorkspaceFileId::to_raw`].
    ///
    /// `extract` picks the per-file input: the cached parse tree for the from-scratch
    /// [`ProjectIndex::builder`] path, or the cached index [`FileFacts`]
    /// ([`facts`](SourceFile::facts)) for the incremental [`ProjectIndex::assemble`] path —
    /// where only the just-edited file re-extracts and the rest return their cache, so a rebuild
    /// re-walks one file.
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
/// Where the sources live, the already-lowered classpath, the library `.java` navigation
/// sources, and the resolved language feature set. The host owns all I/O policy behind this
/// struct (which jars to download, which directories to walk for `.class` files); the workspace
/// owns everything after.
#[derive(Default)]
pub struct ProjectLayout {
    /// The directories walked for `.java` (virtual paths).
    pub source_roots: Vec<DirKey>,
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
/// All project I/O goes through [`ProjectStorage`], and only during [`load`](Workspace::load) —
/// queries answer from the cached parsed trees. Open documents are kept current via
/// [`set_overlay`](Workspace::set_overlay) / [`sync_overlay`](Workspace::sync_overlay), which
/// swap a file's cached text for the editor's and rebuild the (in-memory, no-I/O) index. The
/// rebuild re-walks only the changed file's CST but reassembles the whole index — linear in
/// project size, adequate until an incremental index is needed.
pub struct Workspace<S: SourceBackend, C: CacheBackend> {
    storage: ProjectStorage<S, C>,
    source_roots: Vec<DirKey>,
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
    /// The embedded `java.lang` stub facts, extracted once at construction and reused on every
    /// rebuild (they never change), so the stubs are never re-parsed per edit. Their reserved
    /// [`FileId`]s are disjoint from the project / library id-spaces.
    stub_facts: Vec<(FileId, FileFacts)>,
    index: ProjectIndex,
}

impl<S: SourceBackend, C: CacheBackend> Workspace<S, C> {
    /// Build a workspace over `fs`: walk the spec's source roots for `.java`, read and parse each
    /// (skipping unreadable ones), register the library / source-dependency `.java`, and build
    /// the symbol index. Paths are visited in sorted order so the index is deterministic.
    pub fn load(storage: ProjectStorage<S, C>, spec: ProjectLayout) -> Self {
        let view = storage.view();

        // Extracted library sources, each a navigation file in the `Library` id-space. Read and
        // parsed once; the resulting trees feed `index_source_locations` below.
        let library_files = SourceFile::read_all(&view, &spec.library_sources);
        let library_inputs =
            SourceFile::file_inputs(&library_files, WorkspaceFileId::Library, |f| {
                f.doc.parse.syntax()
            });

        // `git`/`path` library sources, read once; folded into the index as `Source`-origin types
        // on every rebuild (their `FileId`s are assigned in `rebuild_index`).
        let source_dep_files = SourceFile::read_all(&view, &spec.source_dep_sources);

        let mut ws = Self {
            storage,
            source_roots: spec.source_roots,
            files: Vec::new(),
            by_path: BTreeMap::new(),
            library_files,
            source_dep_files,
            index: ProjectIndex::builder(&[]).with_stdlib().build(),
            classpath: spec.classpath,
            source_locations: ProjectIndex::index_source_locations(&library_inputs),
            feature_set: spec.feature_set,
            // Extracted once; reused on every rebuild (the stubs never change).
            stub_facts: ProjectIndex::stub_facts(),
        };
        ws.reload_project_files(&view);
        ws.rebuild_index();
        ws
    }

    /// Walk the source roots for `.java`, read each (skipping unreadable ones), and register the
    /// files in sorted path order — the one place `FileId` assignment happens, so the initial load
    /// and every refresh stay deterministic and identical.
    fn reload_project_files(&mut self, view: &ProjectView) {
        let mut paths: Vec<FileKey> = self
            .source_roots
            .iter()
            .flat_map(|root| view.tree().files_under(root))
            .filter(|file| file.key().has_extension("java"))
            .map(|file| file.key().clone())
            .collect();
        paths.sort();
        paths.dedup();
        self.files.clear();
        self.by_path.clear();
        for path in paths {
            if let Ok(text) = view.file_text(&path) {
                let text = text.to_owned();
                let id = WorkspaceFileId::of_index(WorkspaceFileId::Project, self.files.len());
                self.by_path.insert(path.clone(), id);
                self.files.push(SourceFile::new(path, text));
            }
        }
    }

    /// Rebuild the symbol index from the cached per-file facts, stubs, and classpath. No I/O.
    ///
    /// Built with the embedded `java.lang` stubs and the project's classpath `.class` files
    /// folded in, so a core JDK type (`String`, `Object`, …) or an external library type resolves
    /// to a real item with members and supertypes — hover, completion, member navigation, and
    /// assignment checks see through it instead of stopping at a bare name.
    ///
    /// Incremental: [`file_inputs`](SourceFile::file_inputs) over each file's
    /// [`facts`](SourceFile::facts) re-extracts only the file whose facts cache was cleared (the
    /// one just edited, via [`set_overlay`](Workspace::set_overlay)); every other file, plus the
    /// stubs, reuses its cache. So a keystroke re-walks a single file's CST, and this step (which
    /// allocates and resolves supertypes but walks nothing) reassembles the whole index —
    /// identical to a from-scratch build, but without re-walking the project.
    fn rebuild_index(&mut self) {
        let project =
            SourceFile::file_inputs(&self.files, WorkspaceFileId::Project, SourceFile::facts);
        // The `git`/`path` library sources are *also* index inputs (as `Source`-origin types),
        // under their own `SourceDep` ids so they navigate back to the right files. The
        // `-sources.jar` overlays remain navigation-only (folded in via `source_locations`).
        let source_deps =
            SourceFile::file_inputs(&self.source_dep_files, WorkspaceFileId::SourceDep, |f| {
                f.facts()
            });
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
        );
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
    /// project file. Creating it does not resolve any other file; project-wide references receive
    /// their lazy iterator separately.
    fn queries(&self, file: FileId) -> Option<ProjectQueries<'_>> {
        let source = self.project_file(file)?;
        Some(ProjectQueries::new(
            &self.index,
            QueryFile::new(file, source.doc.parse.syntax(), source.resolved()),
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

    /// Replace the resolved feature set (the browser re-resolves it when the manifest buffer is
    /// edited).
    pub const fn set_feature_set(&mut self, feature_set: FeatureSet) {
        self.feature_set = feature_set;
    }

    /// Replace the lowered classpath and fold it into the index (the browser resolves
    /// dependencies asynchronously, after the workspace already exists).
    pub fn set_classpath(&mut self, classpath: LoweredClasspath) {
        self.classpath = classpath;
        self.rebuild_index();
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

    pub const fn storage_mut(&mut self) -> &mut ProjectStorage<S, C> {
        &mut self.storage
    }

    /// Publish a fresh backend snapshot and invalidate parse/HIR caches in the same revision.
    pub fn refresh(&mut self) -> Result<jals_storage::RefreshOutcome, jals_storage::Error> {
        let outcome = self.storage.refresh()?;
        if !outcome.changed {
            return Ok(outcome);
        }
        let view = self.storage.view();
        self.reload_project_files(&view);
        self.rebuild_index();
        Ok(outcome)
    }

    /// The id of the file at `path`, if it is part of this workspace.
    pub fn file_id(&self, path: &FileKey) -> Option<FileId> {
        self.by_path.get(path).copied()
    }

    /// Every indexed project file with its cached document, in path order. This is the one
    /// statement of project membership (source roots + extension, overlays included); hosts
    /// browse this set instead of re-deriving it from the raw tree.
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

    /// Whether `path` belongs to this workspace: a file already indexed, or a path under one of
    /// its source roots (so a project file the editor hasn't opened yet still resolves here).
    pub fn owns_path(&self, path: &FileKey) -> bool {
        self.by_path.contains_key(path) || self.under_source_root(path)
    }

    /// Whether `path` lies under one of this workspace's source roots.
    fn under_source_root(&self, path: &FileKey) -> bool {
        self.source_roots.iter().any(|root| Self::under(path, root))
    }

    /// Reflect an open document into the index: replace the cached copy of `path` with the
    /// editor's current text (or add it, if `path` is a project file created after the initial
    /// load), then rebuild the index. The document's `Arc`s are shared, so the text is never
    /// reparsed. Returns whether `path` belongs to this workspace.
    pub fn set_overlay(
        &mut self,
        path: &FileKey,
        doc: &Document,
    ) -> Result<bool, jals_storage::Error> {
        // Fresh analysis caches: this file re-extracts on the next rebuild; every other file
        // reuses its cache, so a keystroke re-walks only the edited file.
        let file = SourceFile::with_document(path.clone(), doc.clone());
        if let Some(id) = self.by_path.get(path).copied() {
            self.files[id.0 as usize] = file;
        } else {
            if !self.under_source_root(path) {
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
        self.rebuild_index();
        Ok(true)
    }

    /// Reflect the editor's live text for `path` into the index, parsing and rebuilding **only
    /// when it differs** from the cached copy — so a query storm over an unchanged buffer (hover
    /// after hover) hits the caches instead of re-analyzing per request. Returns whether `path`
    /// belongs to this workspace.
    pub fn sync_overlay(
        &mut self,
        path: &FileKey,
        text: &str,
    ) -> Result<bool, jals_storage::Error> {
        if let Some(source) = self.file_id(path).and_then(|id| self.project_file(id)) {
            if &*source.doc.text == text {
                return Ok(true);
            }
        } else if !self.under_source_root(path) {
            return Ok(false);
        }
        self.set_overlay(path, &Document::new(text.to_owned()))
    }

    /// Go-to-definition for the cursor at `offset` in `file`: a file-local binding if there is
    /// one, then the project type a reference names, then — for a member access — the member the
    /// receiver type declares.
    pub fn definition(&self, file: FileId, offset: usize) -> Option<crate::FileRange> {
        self.queries(file)?.definition(offset)
    }

    /// Find-references for the cursor at `offset` in `file`: every occurrence of the symbol under
    /// the cursor — across the whole project when it is a project type, or within this one file
    /// for a file-local binding. The declaration is included when `include_declaration`. Empty if
    /// the cursor is on no resolvable symbol.
    pub fn references(
        &self,
        file: FileId,
        offset: usize,
        include_declaration: bool,
    ) -> Vec<crate::FileRange> {
        let Some(queries) = self.queries(file) else {
            return Vec::new();
        };
        // Lazily resolved: a file-local binding returns before any other file resolves.
        let files = self.files.iter().enumerate().map(|(index, source)| {
            QueryFile::new(
                WorkspaceFileId::of_index(WorkspaceFileId::Project, index),
                source.doc.parse.syntax(),
                source.resolved(),
            )
        });
        queries.references(offset, include_declaration, files)
    }

    /// The inferred type under `offset` in `file`, or `None` for nothing informative.
    pub fn hover(&self, file: FileId, offset: usize) -> Option<Ty> {
        self.queries(file)?.hover(offset)
    }

    /// [`hover`](Self::hover) rendered as the shared Markdown (a fenced ` ```java ` block).
    pub fn hover_markdown(&self, file: FileId, offset: usize) -> Option<String> {
        self.queries(file)?.hover_markdown(offset)
    }

    /// Completions for the cursor at `offset` in `file`: the members after a `.`, otherwise the
    /// in-scope bindings, project types, and keywords.
    pub fn completions(&self, file: FileId, offset: usize) -> Vec<Completion> {
        self.queries(file)
            .map(|queries| queries.completions(offset))
            .unwrap_or_default()
    }

    /// Signature help for the call at `offset` in `file`, with cross-file type resolution.
    pub fn signature_help(&self, file: FileId, offset: usize) -> Option<jals_hir::SignatureHelp> {
        self.queries(file)?.signature_help(offset)
    }

    /// Occurrence highlights for the cursor at `offset` in `file`, resolved against the project
    /// so a cross-file type name highlights precisely.
    pub fn highlights(&self, file: FileId, offset: usize) -> Vec<Highlight> {
        self.queries(file)
            .map(|queries| queries.highlights(offset))
            .unwrap_or_default()
    }

    /// Classified semantic tokens for `file`, resolved against the project so a cross-file type
    /// name is classified by its declared kind rather than the generic `Type`.
    pub fn semantic_tokens(&self, file: FileId) -> Vec<SemanticToken> {
        self.project_file(file)
            .map(|source| {
                SemanticTokens::classify(&source.doc.parse.syntax(), Some((&self.index, file)))
            })
            .unwrap_or_default()
    }

    /// The document outline of `file`.
    pub fn outline(&self, file: FileId) -> Vec<OutlineNode> {
        self.project_file(file)
            .map(|source| Outline::of(&source.doc.parse.syntax()))
            .unwrap_or_default()
    }

    /// The canonical diagnostics of `file` under `config`, with the project's feature set and
    /// index folded in (see [`FileDiagnostics`]).
    pub fn diagnostics(
        &self,
        file: FileId,
        config: &jals_config::lint::Config,
    ) -> Vec<FileDiagnostic> {
        let Some(source) = self.project_file(file) else {
            return Vec::new();
        };
        let mut config = config.clone();
        config.features = self.feature_set;
        FileDiagnostics::assemble(
            &source.doc.parse,
            Some(source.resolved()),
            Some((&self.index, file)),
            &config,
        )
    }

    /// prepareRename for the cursor at `offset` in `file`: the byte range of the identifier under
    /// the cursor when it names a renamable symbol, else `None` (an external name, a
    /// keyword/literal, or a withheld member — see [`ProjectQueries::renamable_range`]).
    pub fn prepare_rename(&self, file: FileId, offset: usize) -> Option<Range<usize>> {
        self.queries(file)?.renamable_range(offset)
    }

    /// The occurrence set a rename of the symbol at `offset` in `file` rewrites — project-wide
    /// for a project type, within the file for a file-local binding — or `None` if the cursor is
    /// on no renamable symbol or there is nothing to change. The host validates the new name
    /// ([`crate::Ident::is_valid_java_identifier`]) and shapes the edit.
    pub fn rename_targets(&self, file: FileId, offset: usize) -> Option<Vec<crate::FileRange>> {
        self.prepare_rename(file, offset)?;
        let targets = self.references(file, offset, true);
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
    pub fn new(parse: &Parse) -> Self {
        let root = parse.syntax();
        let resolved = Resolved::resolve_node(&root);
        let index = ProjectIndex::builder(&[(Self::FILE, root.clone())])
            .with_stdlib()
            .build();
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
    pub fn diagnostics(
        &self,
        parse: &Parse,
        config: &jals_config::lint::Config,
    ) -> Vec<FileDiagnostic> {
        FileDiagnostics::assemble(
            parse,
            Some(&self.resolved),
            Some((&self.index, Self::FILE)),
            config,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use jals_storage::{CodeTree, Entry, MemoryCache, MemorySource, MemoryStorage};

    use super::*;

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

    fn sample_workspace() -> Workspace<MemorySource, MemoryCache> {
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
    }

    #[test]
    fn load_walks_sorts_and_indexes_the_tree() {
        let ws = sample_workspace();
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
    }

    #[test]
    fn definition_resolves_across_files() {
        let ws = sample_workspace();
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let text = &*ws.document(main).unwrap().text.clone();
        let offset = text.find("Greeter g").unwrap();
        let target = ws.definition(main, offset).expect("cross-file definition");
        assert_eq!(
            ws.path_of(target.file).map(ToString::to_string),
            Some("src/Greeter.java".to_owned())
        );
    }

    #[test]
    fn references_span_the_project_and_include_the_declaration() {
        let ws = sample_workspace();
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let text = &*ws.document(main).unwrap().text.clone();
        let offset = text.find("Greeter g").unwrap();
        let refs = ws.references(main, offset, true);
        let files: BTreeSet<_> = refs
            .iter()
            .filter_map(|r| ws.path_of(r.file).map(ToString::to_string))
            .collect();
        assert!(files.contains("src/Greeter.java"), "{refs:?}");
        assert!(files.contains("src/Main.java"), "{refs:?}");
        assert!(refs.len() >= 3, "two uses + declaration: {refs:?}");
    }

    #[test]
    fn set_overlay_updates_the_index_and_new_files_join_under_a_root() {
        let mut ws = sample_workspace();
        let main = ws.file_id(&key("src/Main.java")).unwrap();

        // Renaming `Greeter` in its defining file makes Main's reference unresolvable.
        assert!(
            ws.set_overlay(
                &key("src/Greeter.java"),
                &Document::new("public class Renamed { }".to_owned()),
            )
            .unwrap()
        );
        let diags = ws.diagnostics(main, &jals_config::lint::Config::default());
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
                &Document::new("public class Extra { }".to_owned()),
            )
            .unwrap()
        );
        assert!(ws.file_id(&key("src/Extra.java")).is_some());
        // ...but a file outside every root is rejected.
        assert!(
            !ws.set_overlay(&key("elsewhere/X.java"), &Document::new(String::new()))
                .unwrap()
        );
    }

    #[test]
    fn sync_overlay_is_a_no_op_for_unchanged_text() {
        let mut ws = sample_workspace();
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let text = ws.document(main).unwrap().text.clone();
        let parse_before = alloc::sync::Arc::as_ptr(&ws.document(main).unwrap().parse);
        assert!(ws.sync_overlay(&key("src/Main.java"), &text).unwrap());
        let parse_after = alloc::sync::Arc::as_ptr(&ws.document(main).unwrap().parse);
        assert_eq!(parse_before, parse_after, "unchanged text must not reparse");

        // A real edit replaces the document.
        assert!(
            ws.sync_overlay(&key("src/Main.java"), "public class Main { }")
                .unwrap()
        );
        assert_ne!(
            alloc::sync::Arc::as_ptr(&ws.document(main).unwrap().parse),
            parse_after
        );
    }

    #[test]
    fn hover_completion_and_highlights_answer_from_the_index() {
        let ws = sample_workspace();
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let text = &*ws.document(main).unwrap().text.clone();

        // Hover over the `new Greeter()` expression shows the cross-file type.
        let new_expr = text.find("new Greeter").unwrap();
        let hover = ws.hover_markdown(main, new_expr).expect("hover");
        assert!(hover.contains("Greeter"), "{hover}");

        // Scope completions offer the sibling type.
        let inside = text.find("Greeter g").unwrap();
        let completions = ws.completions(main, inside);
        assert!(
            completions.iter().any(|c| c.label == "Greeter"),
            "{completions:?}"
        );

        // Highlights find both occurrences of `Greeter` in this file.
        let hl = ws.highlights(main, inside);
        assert_eq!(hl.len(), 2, "{hl:?}");
    }

    #[test]
    fn semantic_tokens_and_outline_answer_neutrally() {
        let ws = sample_workspace();
        let greeter = ws.file_id(&key("src/Greeter.java")).unwrap();
        assert!(
            ws.semantic_tokens(greeter)
                .iter()
                .any(|t| t.kind == crate::SemanticTokenKind::Class && t.declaration)
        );
        let outline = ws.outline(greeter);
        assert_eq!(outline[0].name, "Greeter");
    }

    #[test]
    fn rename_gates_on_project_origin() {
        let ws = sample_workspace();
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let text = &*ws.document(main).unwrap().text.clone();

        // A cross-file *use* of a project type is renamable, and its targets span the project.
        let use_site = text.find("Greeter g").unwrap();
        assert!(ws.prepare_rename(main, use_site).is_some());
        let targets = ws.rename_targets(main, use_site).expect("targets");
        assert!(targets.len() >= 3, "{targets:?}");

        // A stdlib type (`String` in Greeter) is not host-editable: not renamable.
        let greeter = ws.file_id(&key("src/Greeter.java")).unwrap();
        let gtext = &*ws.document(greeter).unwrap().text.clone();
        let string_use = gtext.find("String name").unwrap();
        assert!(ws.prepare_rename(greeter, string_use).is_none());

        // A member (the method `greet`) is withheld.
        let method = gtext.find("greet").unwrap();
        assert!(ws.prepare_rename(greeter, method).is_none());
    }

    #[test]
    fn classpath_types_fold_into_the_index() {
        // A project whose classpath carries a compiled `Box.class`: the workspace folds it into
        // the index as a `Classpath`-origin type, so external library types resolve here.
        let class = jals_classfile::ClassFile::read(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Box.class"
        )))
        .expect("parse Box.class");
        let storage = memory(&[(
            "src/Main.java",
            "class Main { void run() { Box b = new Box(); } }",
        )]);
        let spec = ProjectLayout {
            classpath: ProjectIndex::lower_classpath(&[class]),
            ..ProjectLayout::new(vec![DirKey::parse("src").unwrap()])
        };
        let ws = Workspace::load(storage, spec);
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let diags = ws.diagnostics(main, &jals_config::lint::Config::default());
        assert!(
            !diags.iter().any(|d| d.code == Some("cannot-resolve")),
            "Box resolves through the classpath: {diags:?}"
        );
    }

    #[test]
    fn source_dep_files_are_indexed_and_navigable() {
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
        let ws = Workspace::load(storage, spec);
        let main = ws.file_id(&key("src/Main.java")).unwrap();
        let text = &*ws.document(main).unwrap().text.clone();
        let target = ws
            .definition(main, text.find("Lib l").unwrap())
            .expect("definition into the source dep");
        assert_eq!(
            ws.path_of(target.file).map(ToString::to_string),
            Some("deps/lib/Lib.java".to_owned())
        );
        // The dep is external: not renamable from the project.
        assert!(
            ws.prepare_rename(main, text.find("Lib l").unwrap())
                .is_none()
        );
    }

    #[test]
    fn out_of_space_ids_answer_empty_not_panic() {
        let ws = sample_workspace();
        let bogus = FileId(u32::MAX);
        assert!(ws.document(bogus).is_none());
        assert!(ws.path_of(bogus).is_none());
        assert!(ws.definition(bogus, 0).is_none());
        assert!(ws.references(bogus, 0, true).is_empty());
        assert!(
            ws.diagnostics(bogus, &jals_config::lint::Config::default())
                .is_empty()
        );
        assert!(ws.outline(bogus).is_empty());
    }

    #[test]
    fn single_file_project_answers_the_same_queries() {
        let parse = Parse::parse("class C { int f; void m() { int x = f; } }");
        let project = SingleFileProject::new(&parse);
        let text = "class C { int f; void m() { int x = f; } }";
        let offset = text.rfind('f').unwrap();
        let target = project.queries().definition(offset).expect("definition");
        assert_eq!(target.file, SingleFileProject::FILE);
        let diags = project.diagnostics(&parse, &jals_config::lint::Config::default());
        assert!(
            diags.iter().any(|d| d.code == Some("unused-local")),
            "{diags:?}"
        );
    }
}
