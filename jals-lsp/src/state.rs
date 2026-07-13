//! In-memory server state: open documents and memoized config discovery.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_lsp::lsp_types::{
    DocumentHighlight, Hover, Location, Position, SemanticTokens, SignatureHelp,
    TextDocumentContentChangeEvent, TextEdit, Url, WorkspaceEdit,
};
use jals_config::FeatureSet;
use jals_config::fmt::Config;
use jals_fs::{FileTree, OsFileTree};
use jals_hir::{
    FileFacts, FileId, ItemId, LoweredClasspath, Namespace, ProjectIndex, Resolution, Resolved,
    SourceLocations, TypeResolution,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{Parse, SyntaxKind, SyntaxNode};

use crate::file_id::WorkspaceFileId;
use crate::line_index::LineIndex;

/// An open document: its text, the client's version, a precomputed line index, and the
/// parsed CST.
///
/// `text`, `line_index`, and `parse` are behind `Arc` so a snapshot can be cheaply cloned
/// out of the store and moved into an async request handler. The CST is parsed once here,
/// when the document is built, so each request handler reuses it instead of reparsing.
#[derive(Clone)]
pub(crate) struct Document {
    pub(crate) text: Arc<str>,
    pub(crate) version: i32,
    pub(crate) line_index: Arc<LineIndex>,
    pub(crate) parse: Arc<Parse>,
}

impl Document {
    fn new(text: String, version: i32) -> Self {
        let line_index = Arc::new(LineIndex::new(&text));
        let parse = Arc::new(jals_syntax::Parse::parse(&text));
        Self {
            text: Arc::from(text),
            version,
            line_index,
            parse,
        }
    }
}

/// In-memory store of open documents, keyed by URI. Incremental text sync:
/// `apply_changes` splices `didChange` events into the stored text and rebuilds the
/// line index, while `upsert` (didOpen) replaces the document wholesale.
#[derive(Default)]
pub(crate) struct DocumentStore {
    docs: HashMap<Url, Document>,
}

impl DocumentStore {
    pub(crate) fn upsert(&mut self, uri: Url, text: String, version: i32) {
        self.docs.insert(uri, Document::new(text, version));
    }

    /// Apply `didChange` content changes to the document at `uri`, recording `version`.
    ///
    /// A change for a document that is not open is ignored (client protocol error;
    /// splicing into a nonexistent base would fabricate text). The version is recorded
    /// even when `changes` is empty.
    pub(crate) fn apply_changes(
        &mut self,
        uri: &Url,
        changes: &[TextDocumentContentChangeEvent],
        version: i32,
    ) {
        let Some(doc) = self.docs.get_mut(uri) else {
            return;
        };
        if changes.is_empty() {
            doc.version = version;
            return;
        }
        *doc = Document::new(Self::apply_content_changes(&doc.text, changes), version);
    }

    /// Snapshot the document for `uri` (cheap `Arc` clones), if open.
    pub(crate) fn get(&self, uri: &Url) -> Option<Document> {
        self.docs.get(uri).cloned()
    }

    pub(crate) fn remove(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }
}

impl DocumentStore {
    /// Apply LSP `didChange` content changes to `text`, in order.
    ///
    /// Per the LSP spec each event's range refers to the document state after the previous
    /// event, so a fresh `LineIndex` is built per ranged event. An event without a range
    /// replaces the whole document. Reversed ranges are normalized and out-of-range
    /// positions are clamped by `LineIndex::offset`, so this never panics.
    pub(crate) fn apply_content_changes(
        text: &str,
        changes: &[TextDocumentContentChangeEvent],
    ) -> String {
        let mut text = text.to_owned();
        for change in changes {
            let Some(range) = change.range else {
                text.clone_from(&change.text);
                continue;
            };
            let index = LineIndex::new(&text);
            let start = u32::from(index.offset(&text, range.start)) as usize;
            let end = u32::from(index.offset(&text, range.end)) as usize;
            text.replace_range(start.min(end)..start.max(end), &change.text);
        }
        text
    }
}

/// A file tracked by the project [`Workspace`]: its URI and cached CST + coordinate map. Mirrors
/// the open-document [`Document`], but covers every project source file (open or not) so a
/// cross-file go-to-definition can land in a file the editor has never opened.
struct WorkspaceFile {
    uri: Url,
    text: Arc<str>,
    line_index: Arc<LineIndex>,
    parse: Arc<Parse>,
    /// The file's name resolution, computed once on first use and cached. A pure function of
    /// `parse`, so it stays valid for this file's lifetime — an edit replaces the whole struct
    /// (see [`Workspace::set_overlay`]), starting fresh. Lets a project-wide query that scans every
    /// file (find-references) resolve each one only once instead of on every request.
    resolved: OnceLock<Resolved>,
    /// The file's cached index facts — the CST-walking half of building the [`ProjectIndex`],
    /// computed once on first use. Like `resolved`, a pure function of `parse`, so an edit (which
    /// replaces the whole struct) re-extracts them while every other file reuses its cache. This is
    /// what makes [`rebuild_index`](Workspace::rebuild_index) re-walk only the changed file rather
    /// than every file in the project.
    facts: OnceLock<FileFacts>,
}

impl WorkspaceFile {
    fn new(uri: Url, text: String) -> Self {
        let line_index = Arc::new(LineIndex::new(&text));
        let parse = Arc::new(jals_syntax::Parse::parse(&text));
        Self {
            uri,
            text: Arc::from(text),
            line_index,
            parse,
            resolved: OnceLock::new(),
            facts: OnceLock::new(),
        }
    }

    /// The file's cached name resolution (computed on first use).
    fn resolved(&self) -> &Resolved {
        self.resolved
            .get_or_init(|| jals_hir::Resolved::resolve_node(&self.parse.syntax()))
    }

    /// The file's cached index facts (computed on first use), the input to the incremental
    /// [`ProjectIndex::assemble`].
    fn facts(&self) -> &FileFacts {
        self.facts
            .get_or_init(|| ProjectIndex::extract_file(&self.parse.syntax()))
    }

    /// A `Location` in this file spanning byte `range`.
    fn location(&self, range: &Range<usize>) -> Location {
        Location {
            uri: self.uri.clone(),
            range: self.line_index.byte_range(&self.text, range),
        }
    }
}

/// A single `jals.toml` project's symbol index plus the per-file data needed to answer cross-file
/// queries. The server holds one of these per project a client has a file open in (see
/// [`ServerState`](crate::server)), discovered lazily by walking up from each opened file — so it
/// only ever indexes the source roots of a real manifest, never a whole git checkout.
///
/// All file I/O goes through the [`FileTree`] `F`: [`load`](Workspace::load) (and friends) read the
/// source roots off the host filesystem via [`OsFileTree`], while [`load_in`](Workspace::load_in)
/// can build the whole workspace off any tree — an [`jals_fs::InMemoryFileTree`] drives it with no
/// real filesystem (wasm / tests). The parsed trees are handed to the pure [`ProjectIndex`]. Open
/// documents are kept current via [`set_overlay`](Workspace::set_overlay), which swaps a file's
/// cached text for the editor's and rebuilds the (in-memory, no-I/O) index. The rebuild walks the
/// cached trees of every file, so it is cheap per edit but linear in project size — adequate until
/// an incremental index is needed.
pub(crate) struct Workspace<F: FileTree = OsFileTree> {
    /// The file tree every load reads through. `OsFileTree` (the default) on the host; an in-memory
    /// tree in tests. Only used during construction — queries answer from the cached parsed trees.
    fs: F,
    /// The `jals.toml` directory this workspace was discovered from; identifies the workspace so a
    /// later open in the same project reuses it instead of building a duplicate.
    project_root: PathBuf,
    source_roots: Vec<PathBuf>,
    files: Vec<WorkspaceFile>,
    by_uri: HashMap<Url, FileId>,
    /// Extracted library *source* files (the `.java` of a `[dependencies]` `sources` jar), kept so a
    /// classpath type/member can be navigated into its real source. Addressed by a
    /// [`Library`](WorkspaceFileId::Library) [`FileId`], disjoint from the project files' low ids, so
    /// [`ws_file`](Workspace::ws_file) can route a go-to-definition target to the right vec. Never
    /// project inputs and never linted — they are navigation targets only.
    library_files: Vec<WorkspaceFile>,
    /// Library **source** files of a `git` / `path` `[dependencies]` entry. Unlike
    /// [`library_files`](Workspace::library_files) (navigation-only overlays paired with a binary
    /// `-sources.jar`), these have no `.class` backing them, so they *are* index inputs — folded in as
    /// [`Source`](jals_hir::ItemOrigin::Source)-origin types that resolve for inference/hover and are
    /// go-to-definition targets in their own right. Addressed by a
    /// [`SourceDep`](WorkspaceFileId::SourceDep) [`FileId`], a third id space disjoint from both the
    /// project files and [`library_files`](Workspace::library_files). Still never linted — they are not
    /// project files.
    source_dep_files: Vec<WorkspaceFile>,
    /// The project's classpath `.class` files (parsed from the `[build] classpath` jars/dirs), lowered
    /// once at construction to the facts the index folds in. Reused on every index rebuild so external
    /// library types resolve; static for the workspace's lifetime (a dependency jar does not change
    /// under us), so the expensive lowering happens here once instead of on every edit.
    classpath: LoweredClasspath,
    /// Where each library type/member is declared in [`library_files`](Workspace::library_files),
    /// indexed once at construction (the sources of a fixed dependency do not change) and folded into
    /// every rebuild so a classpath item gets a real-source go-to-definition target.
    source_locations: SourceLocations,
    /// The project's resolved language feature set, from the manifest's `[package] features`. Fed
    /// to the feature-gated lint rules (e.g. `compact-source-file`); empty when the manifest
    /// declares none (or could not be parsed), disabling those gates.
    feature_set: FeatureSet,
    /// The embedded `java.lang` stub facts, extracted once at construction and reused on every rebuild
    /// (they never change), so the stubs are never re-parsed per edit. Their reserved [`FileId`]s are
    /// disjoint from the project / library id-spaces.
    stub_facts: Vec<(FileId, FileFacts)>,
    index: ProjectIndex,
}

impl WorkspaceFile {
    /// Read and parse each library `.java` in `paths` through `fs`, skipping unreadable / non-UTF-8 /
    /// non-`file://` ones and de-duplicating by URI, into [`WorkspaceFile`]s. Shared by both
    /// library-source kinds (the `-sources.jar` overlays and the `git`/`path` source dependencies). The
    /// virtual path fed to `fs` is each `PathBuf`'s UTF-8 form; the `Url` keeps the original path.
    fn read_library_files(fs: &dyn FileTree, paths: Vec<PathBuf>) -> Vec<Self> {
        let mut files = Vec::new();
        let mut seen = HashSet::new();
        for path in paths {
            let Some(vpath) = path.to_str() else {
                continue;
            };
            if let (Ok(text), Ok(uri)) = (fs.read_to_string(vpath), Url::from_file_path(&path))
                && seen.insert(uri.clone())
            {
                files.push(Self::new(uri, text));
            }
        }
        files
    }

    /// Pair each file in `files` with a sequential [`FileId`] in the id-space named by `space`, mapping
    /// it through `extract` — the `(FileId, T)` inputs the index builds from. Shared by the project files
    /// ([`WorkspaceFileId::Project`]) and the two library-source id-spaces
    /// ([`Library`](WorkspaceFileId::Library) / [`SourceDep`](WorkspaceFileId::SourceDep)), so every id
    /// space derives its inputs the same way — the base offset lives only in [`WorkspaceFileId::to_raw`].
    ///
    /// `extract` picks the per-file input: the cached parse tree ([`parse`](WorkspaceFile::parse)) for the
    /// from-scratch [`ProjectIndex::builder`] path, or the cached index [`FileFacts`]
    /// ([`facts`](WorkspaceFile::facts)) for the incremental [`ProjectIndex::assemble`] path — where only
    /// the just-edited file re-extracts and the rest return their cache, so a rebuild re-walks one file.
    fn file_inputs<'a, T>(
        files: &'a [Self],
        space: fn(u32) -> WorkspaceFileId,
        extract: impl Fn(&'a Self) -> T,
    ) -> Vec<(FileId, T)> {
        files
            .iter()
            .enumerate()
            .map(|(k, f)| (space(k as u32).to_raw(), extract(f)))
            .collect()
    }
}

impl Workspace<OsFileTree> {
    /// Walk `source_roots` on the host filesystem, parse every `.java` file found (skipping
    /// unreadable ones), and build the symbol index. `project_root` is the `jals.toml` directory
    /// this workspace was discovered from; it identifies the workspace so a later open in the same
    /// project reuses it. Paths are visited in sorted order so the index is deterministic.
    ///
    /// The workspace has an empty classpath; use [`load_with_classpath`](Workspace::load_with_classpath)
    /// to fold a project's external library types into the index.
    pub(crate) fn load(project_root: PathBuf, source_roots: Vec<PathBuf>) -> Self {
        Self::load_with_classpath(project_root, source_roots, Vec::new())
    }

    /// Like [`load`](Workspace::load), but also folds the project's classpath `.class` files into the
    /// symbol index (as `Classpath`-origin types), so references to external library types resolve to
    /// real items with members and generics — for hover, completion, and the `type-mismatch` check.
    /// The caller (the server) reads and parses the jars/dirs; this keeps the I/O at the edge.
    pub(crate) fn load_with_classpath(
        project_root: PathBuf,
        source_roots: Vec<PathBuf>,
        classfiles: Vec<jals_classfile::ClassFile>,
    ) -> Self {
        Self::load_with_classpath_and_sources(
            project_root,
            source_roots,
            classfiles,
            Vec::new(),
            Vec::new(),
            FeatureSet::default(),
        )
    }

    /// Like [`load_with_classpath`](Workspace::load_with_classpath), but also registers two kinds of
    /// library `.java`:
    /// - `library_sources` — the `.java` of each `[dependencies]` `sources` jar, navigation-only
    ///   overlays so go-to-definition can land in a classpath type/member's real source (see
    ///   `jals_classpath::resolve_project_sources`);
    /// - `source_dep_sources` — the `.java` of each `git`/`path` `[dependencies]` entry, which are
    ///   *also* index inputs (`Source`-origin types that resolve for analysis) as well as navigation
    ///   targets (see `jals_classpath::resolve_project_source_deps`).
    ///
    /// Both are read and parsed once here; neither is ever linted (they are not project files).
    ///
    /// `feature_set` is the project's resolved language feature set (from `[package] features`),
    /// used by the feature-gated lint rules; an empty set leaves those gates off.
    pub(crate) fn load_with_classpath_and_sources(
        project_root: PathBuf,
        source_roots: Vec<PathBuf>,
        classfiles: Vec<jals_classfile::ClassFile>,
        library_sources: Vec<PathBuf>,
        source_dep_sources: Vec<PathBuf>,
        feature_set: FeatureSet,
    ) -> Self {
        Self::load_in(
            OsFileTree,
            project_root,
            source_roots,
            classfiles,
            library_sources,
            source_dep_sources,
            feature_set,
        )
    }
}

impl<F: FileTree> Workspace<F> {
    /// Build a workspace over an arbitrary [`FileTree`] `fs`: walk `source_roots` for `.java`, read
    /// and parse each through `fs` (skipping unreadable ones), register the library /
    /// source-dependency `.java`, and build the symbol index. The host entry points
    /// ([`load`](Workspace::load) & friends) call this with an [`OsFileTree`]; a
    /// [`jals_fs::InMemoryFileTree`] drives the whole workspace off an in-memory tree (wasm /
    /// tests). Paths (`source_roots`, `library_sources`, `source_dep_sources`) are `PathBuf`s
    /// (host-supplied); their UTF-8 form is the `/`-separated virtual path fed to `fs`, and the file
    /// identity stays a `file://` [`Url`] derived from it. See
    /// [`load_with_classpath_and_sources`](Workspace::load_with_classpath_and_sources) for the roles
    /// of `library_sources` / `source_dep_sources` / `feature_set`.
    // The load chain moves owned inputs through a uniform signature; `classfiles` is only read (to
    // lower the classpath) before being dropped, but keeping it owned matches the rest of the chain.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn load_in(
        fs: F,
        project_root: PathBuf,
        source_roots: Vec<PathBuf>,
        classfiles: Vec<jals_classfile::ClassFile>,
        library_sources: Vec<PathBuf>,
        source_dep_sources: Vec<PathBuf>,
        feature_set: FeatureSet,
    ) -> Self {
        let mut paths: Vec<String> = source_roots
            .iter()
            .filter_map(|root| root.to_str())
            .flat_map(|root| fs.walk_ext(root, "java").unwrap_or_default())
            .collect();
        paths.sort();
        paths.dedup();

        // Extracted library sources, each a navigation file in the `Library` id-space. Read and
        // parsed once; the resulting trees feed `index_source_locations` below.
        let library_files = WorkspaceFile::read_library_files(&fs, library_sources);
        let library_inputs =
            WorkspaceFile::file_inputs(&library_files, WorkspaceFileId::Library, |f| {
                f.parse.syntax()
            });

        // `git`/`path` library sources, read once; folded into the index as `Source`-origin types on
        // every rebuild (their `FileId`s are assigned in `rebuild_index`).
        let source_dep_files = WorkspaceFile::read_library_files(&fs, source_dep_sources);

        let mut ws = Self {
            fs,
            project_root,
            source_roots,
            files: Vec::new(),
            by_uri: HashMap::new(),
            library_files,
            source_dep_files,
            index: ProjectIndex::builder(&[]).with_stdlib().build(),
            classpath: ProjectIndex::lower_classpath(&classfiles),
            source_locations: ProjectIndex::index_source_locations(&library_inputs),
            feature_set,
            // Extracted once; reused on every rebuild (the stubs never change).
            stub_facts: ProjectIndex::stub_facts(),
        };
        for vpath in paths {
            if let (Ok(text), Ok(uri)) = (ws.fs.read_to_string(&vpath), Url::from_file_path(&vpath))
                && !ws.by_uri.contains_key(&uri)
            {
                let id = WorkspaceFileId::Project(ws.files.len() as u32).to_raw();
                ws.by_uri.insert(uri.clone(), id);
                ws.files.push(WorkspaceFile::new(uri, text));
            }
        }
        ws.rebuild_index();
        ws
    }

    /// Rebuild the symbol index from the cached per-file facts, stubs, and classpath. No I/O.
    ///
    /// Built with the embedded `java.lang` stubs and the project's classpath `.class` files folded
    /// in, so a core JDK type (`String`, `Object`, …) or an external library type resolves to a real
    /// item with members and supertypes — hover, completion, member navigation, and assignment
    /// checks see through it instead of stopping at a bare name.
    ///
    /// Incremental: [`file_inputs`](WorkspaceFile::file_inputs) over each file's [`facts`](WorkspaceFile::facts) re-extracts only
    /// the file whose facts cache was cleared (the one just edited, via
    /// [`set_overlay`](Workspace::set_overlay)); every other file, plus the stubs,
    /// reuses its cache. So a keystroke re-walks a single file's CST, and this step (which allocates
    /// and resolves supertypes but walks nothing) reassembles the whole index — identical to a
    /// from-scratch build, but without re-walking the project.
    fn rebuild_index(&mut self) {
        let project =
            WorkspaceFile::file_inputs(&self.files, WorkspaceFileId::Project, WorkspaceFile::facts);
        // The `git`/`path` library sources are *also* index inputs (as `Source`-origin types),
        // under their own `SourceDep` ids so they navigate back to the right files. The
        // `-sources.jar` overlays remain navigation-only (folded in via `source_locations`).
        let source_deps =
            WorkspaceFile::file_inputs(&self.source_dep_files, WorkspaceFileId::SourceDep, |f| {
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
    /// `git`/`path` library source ([`SourceDep`](WorkspaceFileId::SourceDep)), or a `-sources.jar`
    /// overlay ([`Library`](WorkspaceFileId::Library)). `None` when the within-space index addresses no
    /// real file — e.g. a classpath member with no source, whose reserved id decodes into `SourceDep`
    /// far past any extracted file — so a go-to-definition target that points nowhere openable yields
    /// no location instead of panicking.
    fn ws_file(&self, id: FileId) -> Option<&WorkspaceFile> {
        match WorkspaceFileId::from_raw(id) {
            WorkspaceFileId::Project(i) => self.files.get(i as usize),
            WorkspaceFileId::Library(i) => self.library_files.get(i as usize),
            WorkspaceFileId::SourceDep(i) => self.source_dep_files.get(i as usize),
        }
    }

    /// The project symbol index.
    pub(crate) const fn index(&self) -> &ProjectIndex {
        &self.index
    }

    /// The project's resolved language feature set (from `[package] features`), empty when none is
    /// declared. Feeds the feature-gated lint rules.
    pub(crate) const fn feature_set(&self) -> FeatureSet {
        self.feature_set
    }

    /// The id of the file at `uri`, if it is part of this workspace.
    pub(crate) fn file_id(&self, uri: &Url) -> Option<FileId> {
        self.by_uri.get(uri).copied()
    }

    /// The `jals.toml` project root this workspace was loaded from.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Whether `uri` belongs to this workspace: a file already indexed, or a path under one of its
    /// source roots (so a project file the editor hasn't opened yet still resolves here).
    pub(crate) fn owns_uri(&self, uri: &Url) -> bool {
        self.by_uri.contains_key(uri) || self.under_source_root(uri)
    }

    /// Whether `uri`'s path lies under one of this workspace's source roots.
    fn under_source_root(&self, uri: &Url) -> bool {
        uri.to_file_path()
            .is_ok_and(|p| self.source_roots.iter().any(|r| p.starts_with(r)))
    }

    /// Reflect an open document into the index: replace the cached copy of `uri` with the editor's
    /// current text (or add it, if `uri` is a project file created after the initial load), then
    /// rebuild the index. Returns whether `uri` belongs to this workspace.
    pub(crate) fn set_overlay(&mut self, uri: &Url, doc: &Document) -> bool {
        let file = WorkspaceFile {
            uri: uri.clone(),
            text: doc.text.clone(),
            line_index: doc.line_index.clone(),
            parse: doc.parse.clone(),
            resolved: OnceLock::new(),
            // Fresh facts cache: this file re-extracts on the next rebuild; every other file reuses
            // its cache, so a keystroke re-walks only the edited file.
            facts: OnceLock::new(),
        };
        if let Some(id) = self.by_uri.get(uri).copied() {
            self.files[id.0 as usize] = file;
        } else {
            if !self.under_source_root(uri) {
                return false;
            }
            let id = WorkspaceFileId::Project(self.files.len() as u32).to_raw();
            self.by_uri.insert(uri.clone(), id);
            self.files.push(file);
        }
        self.rebuild_index();
        true
    }

    /// Go-to-definition for the cursor at `position` in `uri`: a file-local binding if there is one,
    /// then the project type a reference names, then — for a member access — the member the receiver
    /// type declares. `None` if `uri` is not in the workspace or nothing resolves.
    pub(crate) fn goto_definition(&self, uri: &Url, position: Position) -> Option<Location> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let offset = source.line_index.offset(&source.text, position);
        let resolved = source.resolved();
        // A file-local binding, or the project type a reference names.
        if let Some((target_file, range)) =
            self.index
                .definition_at(file, resolved, usize::from(offset))
        {
            return Some(self.ws_file(target_file)?.location(&range));
        }
        // A member access (`obj.field` / `recv.method()`): infer the receiver and resolve the member.
        let (target_file, range) = self.member_definition(file, &root, resolved, offset)?;
        Some(self.ws_file(target_file)?.location(&range))
    }

    /// Go-to-definition for the member access under `offset`: when the cursor is on the name of a
    /// `receiver.field` / `receiver.method()`, infer the receiver's type and, if it is a project
    /// type, resolve the member on it (through its project-internal supertypes). Returns the member
    /// declaration's file and name range.
    fn member_definition(
        &self,
        file: FileId,
        root: &SyntaxNode,
        resolved: &Resolved,
        offset: text_size::TextSize,
    ) -> Option<(FileId, Range<usize>)> {
        // The member-name identifier sits directly under a `FIELD_ACCESS` node — both for a plain
        // `obj.field` and for the `recv.method` callee of a call.
        let token = crate::handlers::Cursor::ident_at(root, offset)?;
        let field_access = token
            .parent()
            .filter(|p| p.kind() == SyntaxKind::FIELD_ACCESS)?;
        let access = ast::FieldAccess::cast(field_access.clone())?;
        let name = access.field()?;
        let receiver = access.receiver()?;
        // A field-access used as a call's callee names a method; otherwise a field.
        let namespace = if field_access.parent().map(|p| p.kind()) == Some(SyntaxKind::CALL_EXPR) {
            Namespace::Method
        } else {
            Namespace::Value
        };
        let inference = jals_hir::TypeInference::infer(root, resolved, &self.index, file);
        let span = receiver.syntax().text_range();
        let owner = inference
            .type_of_expr(usize::from(span.start())..usize::from(span.end()))?
            .project_id()?;
        let member = self
            .index
            .member(self.index.resolve_member(owner, &name, namespace)?);
        // Prefer the library-source location (a classpath member with sources); otherwise the
        // member's own declaration (a project member). A classpath member without sources keeps a
        // reserved id `ws_file` will reject, so navigation simply yields nothing.
        Some(
            member
                .source_location
                .clone()
                .unwrap_or_else(|| (member.file, member.name_range.clone())),
        )
    }

    /// The hover for the cursor at `position` in `uri`: the inferred type of the expression there,
    /// with reference type names resolved against the project. `None` if `uri` is not in the
    /// workspace or the expression has no inferred type.
    pub(crate) fn hover(&self, uri: &Url, position: Position) -> Option<Hover> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let resolved = source.resolved();
        let inference = jals_hir::TypeInference::infer(&root, resolved, &self.index, file);
        let offset = u32::from(source.line_index.offset(&source.text, position)) as usize;
        crate::handlers::Hovers::type_hover(inference.type_at(offset)?)
    }

    /// The signature help for the call at `position` in `uri`, with cross-file type resolution (so a
    /// receiver of a sibling-file type resolves). `None` if `uri` is not in the workspace or the
    /// cursor is in no resolvable call.
    pub(crate) fn signature_help(&self, uri: &Url, position: Position) -> Option<SignatureHelp> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let offset = u32::from(source.line_index.offset(&source.text, position)) as usize;
        let help = self
            .index
            .signature_help(&root, source.resolved(), file, offset)?;
        Some(crate::handlers::SignatureHelpHandler::signature_help_to_lsp(&help))
    }

    /// Completions for the cursor at `position` in `uri`, resolved against the project (so a receiver
    /// or a type name from a sibling file completes): the members after a `.`, otherwise the in-scope
    /// bindings, project types, and keywords. `None` if `uri` is not in the workspace.
    pub(crate) fn completions(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<Vec<async_lsp::lsp_types::CompletionItem>> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let offset = u32::from(source.line_index.offset(&source.text, position)) as usize;
        Some(crate::handlers::Completions::completions(
            &root,
            source.resolved(),
            &self.index,
            file,
            offset,
        ))
    }

    /// Occurrence highlights for the cursor at `position` in `uri`, resolved against the project so a
    /// cross-file type name highlights precisely (only its references in this file, never a
    /// same-spelled variable). `None` if `uri` is not in the workspace.
    pub(crate) fn document_highlight(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<Vec<DocumentHighlight>> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        Some(crate::handlers::DocumentHighlights::document_highlight(
            &source.parse,
            &source.text,
            &source.line_index,
            position,
            Some((&self.index, file)),
        ))
    }

    /// Semantic tokens for `uri`, resolved against the project so a cross-file type name is classified
    /// by its declared kind (`class` / `enum` / `interface`) rather than the generic `type`. `None` if
    /// `uri` is not in the workspace.
    pub(crate) fn semantic_tokens(&self, uri: &Url) -> Option<SemanticTokens> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        Some(crate::handlers::SemanticTokensBuilder::semantic_tokens(
            &source.parse,
            &source.text,
            &source.line_index,
            Some((&self.index, file)),
        ))
    }

    /// Find-references for the cursor at `position` in `uri`: every occurrence of the symbol under
    /// the cursor — across the whole project when it is a project type, or within this one file for
    /// a file-local binding (a local, parameter, field, method, or type parameter). The declaration
    /// is included when `include_declaration`. `None` if `uri` is not in the workspace; an empty
    /// vector if the cursor is on no resolvable symbol.
    pub(crate) fn references(
        &self,
        uri: &Url,
        position: Position,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let resolved = source.resolved();
        // Anchor on the identifier under the cursor (boundary-aware), as the find-references handler
        // does, then ask name resolution for the binding it denotes.
        let Some(ident) = crate::handlers::Cursor::ident_at(
            &root,
            source.line_index.offset(&source.text, position),
        ) else {
            return Some(Vec::new());
        };
        let anchor = usize::from(ident.text_range().start());

        // The cursor denotes a file-local binding.
        if let Some(def_id) = resolved.symbol_at(anchor) {
            // A binding that is also a project type: gather references across every file.
            if let Some(item) = self
                .index
                .item_by_decl(file, resolved.def(def_id).name_range.start)
            {
                return Some(self.item_references(item, include_declaration));
            }
            // Otherwise a local/parameter/field/method/type-parameter: occurrences within this file.
            let locations = resolved
                .occurrences(def_id, include_declaration)
                .into_iter()
                .map(|range| source.location(&range))
                .collect();
            return Some(locations);
        }

        // The cursor is on a cross-file type reference (one the file-local pass left unresolved).
        if let Some(reference) = resolved.reference_at(anchor)
            && reference.namespace == Namespace::Type
            && let TypeResolution::Project(item) = self.index.resolve_reference(file, reference)
        {
            return Some(self.item_references(item, include_declaration));
        }
        Some(Vec::new())
    }

    /// Every reference to the project type `item` across all workspace files (plus its declaration
    /// when `include_declaration`), as `Location`s sorted by file then position. A same-file type
    /// reference resolves file-locally to the declaration, so it is matched through
    /// [`ProjectIndex::item_by_decl`]; references in other files resolve through the project index.
    fn item_references(&self, item: ItemId, include_declaration: bool) -> Vec<Location> {
        let mut locations = Vec::new();
        for (i, source) in self.files.iter().enumerate() {
            let file = WorkspaceFileId::Project(i as u32).to_raw();
            let resolved = source.resolved();
            for reference in &resolved.references {
                if reference.namespace != Namespace::Type {
                    continue;
                }
                let hit = match reference.resolution {
                    Resolution::Def(id) => {
                        self.index
                            .item_by_decl(file, resolved.def(id).name_range.start)
                            == Some(item)
                    }
                    Resolution::Unresolved => matches!(
                        self.index.resolve_reference(file, reference),
                        TypeResolution::Project(target) if target == item
                    ),
                };
                if hit {
                    locations.push(source.location(&reference.range));
                }
            }
        }
        if include_declaration {
            let decl = self.index.item(item);
            // Route through `ws_file`, not a raw `self.files` index: a `Source`-origin (library) type's
            // declaration lives in `source_dep_files`, and a stub / source-less classpath type has a
            // reserved id that is no real file — both of which a raw index would panic on.
            if let Some(decl_source) = self.ws_file(decl.file) {
                locations.push(decl_source.location(&decl.name_range));
            }
        }
        locations.sort_by(|a, b| {
            a.uri
                .as_str()
                .cmp(b.uri.as_str())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });
        locations
    }

    /// Whether the symbol anchored at byte `anchor` in `file` may be renamed soundly. A file-local
    /// binding qualifies by kind (locals and project types yes, members no — see
    /// [`is_renamable_kind`](crate::handlers::Rename::is_renamable_kind)); a cross-file *use* of a project
    /// type (one the file-local pass left unresolved) qualifies too, since the workspace rewrites it
    /// project-wide. A use that resolves to anything *outside* the project's own sources — a
    /// `java.lang` stub, a classpath `.class` type, or a `git`/`path` library-source type — does
    /// *not* qualify: those have no host-editable project file, so their names are as un-renamable as
    /// any external one (mirroring how [`definition_at`](jals_hir::ProjectIndex::definition_at) treats
    /// non-project origins). Mirrors what [`references`](Workspace::references) actually rewrites, so a
    /// renamable symbol always has a complete, in-project occurrence set.
    fn is_renamable(&self, file: FileId, resolved: &Resolved, anchor: usize) -> bool {
        if let Some(id) = resolved.symbol_at(anchor) {
            return crate::handlers::Rename::is_renamable_kind(resolved.def(id).kind);
        }
        resolved.reference_at(anchor).is_some_and(|reference| {
            reference.namespace == Namespace::Type
                && matches!(
                    self.index.resolve_reference(file, reference),
                    TypeResolution::Project(id) if self.index.item(id).origin.is_host_editable()
                )
        })
    }

    /// prepareRename for the cursor at `position` in `uri`: the range of the identifier under the
    /// cursor when it names a renamable symbol, else `None` (an external name, a keyword/literal, or
    /// a withheld member). The editor uses this to validate a rename before prompting.
    pub(crate) fn prepare_rename(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<async_lsp::lsp_types::Range> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let ident = crate::handlers::Cursor::ident_at(
            &root,
            source.line_index.offset(&source.text, position),
        )?;
        if !self.is_renamable(
            file,
            source.resolved(),
            usize::from(ident.text_range().start()),
        ) {
            return None;
        }
        Some(source.line_index.range(&source.text, ident.text_range()))
    }

    /// Rename the symbol under `position` in `uri` to `new_name`: a [`WorkspaceEdit`] rewriting every
    /// occurrence — project-wide for a project type, within the file for a file-local binding.
    /// `None` if `uri` is not in the workspace, the cursor is on no renamable symbol, or there is
    /// nothing to change. The caller validates `new_name` is a legal identifier.
    pub(crate) fn rename(
        &self,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        /// Group `locations` into a [`WorkspaceEdit`] that rewrites each occurrence to `new_name`,
        /// keyed by file. `None` if there is nothing to rewrite.
        fn workspace_edit(locations: Vec<Location>, new_name: &str) -> Option<WorkspaceEdit> {
            if locations.is_empty() {
                return None;
            }
            let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
            for location in locations {
                changes.entry(location.uri).or_default().push(TextEdit {
                    range: location.range,
                    new_text: new_name.to_owned(),
                });
            }
            Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            })
        }
        // Gate on the same renamability check `prepareRename` performs, then rewrite every
        // occurrence the find-references pass gathers.
        self.prepare_rename(uri, position)?;
        workspace_edit(self.references(uri, position, true)?, new_name)
    }
}

/// A config the LSP discovers by walking up from a document's directory to a well-known TOML
/// file. Implemented for both `jals_config::fmt::Config` and `jals_config::lint::Config` so one [`Discovery`]
/// cache serves the formatter and the linter alike.
pub(crate) trait DiscoverableConfig: Clone + Default {
    /// The config file name searched for (e.g. `jalsfmt.toml`).
    const FILE_NAME: &'static str;
    /// Discover the config from `dir` (a UTF-8 virtual path) upward, `None` on any read/parse error.
    fn discover_str(dir: &str) -> Option<Self>;
    /// Discover the config from `dir` upward, falling back to the default on a non-UTF-8 path or any
    /// read/parse error.
    fn discover_or_default(dir: &Path) -> Self {
        dir.to_str()
            .and_then(Self::discover_str)
            .unwrap_or_default()
    }

    /// Whether `uri` refers to a config file named [`Self::FILE_NAME`] (e.g. `jalsfmt.toml`), used to
    /// invalidate the discovery caches when a watched config file changes on disk.
    fn is_config_file(uri: &Url) -> bool {
        uri.to_file_path()
            .is_ok_and(|path| path.file_name().is_some_and(|name| name == Self::FILE_NAME))
    }
}

impl DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalsfmt.toml";
    fn discover_str(dir: &str) -> Option<Self> {
        Self::discover(&OsFileTree, dir).ok()
    }
}

impl DiscoverableConfig for jals_config::lint::Config {
    const FILE_NAME: &'static str = "jalslint.toml";
    fn discover_str(dir: &str) -> Option<Self> {
        Self::discover(&OsFileTree, dir).ok()
    }
}

/// Resolves a `C` for a document by discovering its
/// [`FILE_NAME`](DiscoverableConfig::FILE_NAME) from the file's directory upward, memoized per
/// directory. Mirrors the `jals` CLI behavior.
#[derive(Default)]
pub(crate) struct Discovery<C> {
    cache: HashMap<PathBuf, C>,
}

impl<C: DiscoverableConfig> Discovery<C> {
    /// Discover the config for a document URI. Falls back to `C::default()` for non-file URIs
    /// (e.g. `untitled:`) and when discovery fails.
    pub(crate) fn for_uri(&mut self, uri: &Url) -> C {
        let Some(dir) = uri
            .to_file_path()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            return C::default();
        };
        if let Some(cfg) = self.cache.get(&dir) {
            return cfg.clone();
        }
        let cfg = C::discover_or_default(&dir);
        self.cache.insert(dir, cfg.clone());
        cfg
    }

    /// Forget all memoized configs, e.g. after a config file changes on disk. Discovery
    /// reruns lazily on the next request that needs a config.
    pub(crate) fn clear(&mut self) {
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{HoverContents, Position, Range};
    use jals_hir::ItemOrigin;

    use super::*;

    /// Helper: a ranged (incremental) change event from (line, character) pairs.
    fn ranged(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_owned(),
        }
    }

    /// Helper: a full-document replacement event (no range).
    fn full(text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn apply_single_insert() {
        let out =
            DocumentStore::apply_content_changes("class A {}", &[ranged((0, 9), (0, 9), "int x;")]);
        assert_eq!(out, "class A {int x;}");
    }

    #[test]
    fn apply_single_delete() {
        let out = DocumentStore::apply_content_changes("abcdef", &[ranged((0, 1), (0, 4), "")]);
        assert_eq!(out, "aef");
    }

    #[test]
    fn apply_single_replace() {
        let out = DocumentStore::apply_content_changes("abc", &[ranged((0, 1), (0, 2), "XY")]);
        assert_eq!(out, "aXYc");
    }

    #[test]
    fn apply_batch_uses_post_edit_coordinates() {
        // The second event's range is only meaningful against "aXYb", the state
        // after the first event: (0,2)..(0,3) deletes the "Y".
        let changes = [ranged((0, 1), (0, 1), "XY"), ranged((0, 2), (0, 3), "")];
        assert_eq!(DocumentStore::apply_content_changes("ab", &changes), "aXb");
    }

    #[test]
    fn apply_counts_utf16_columns() {
        // '😀' = 4 UTF-8 bytes, 2 UTF-16 units, so 'y' starts at character 3.
        let out = DocumentStore::apply_content_changes("x😀y", &[ranged((0, 1), (0, 3), "Z")]);
        assert_eq!(out, "xZy");
        let out = DocumentStore::apply_content_changes("x😀y", &[ranged((0, 3), (0, 3), "!")]);
        assert_eq!(out, "x😀!y");
    }

    #[test]
    fn apply_full_replacement_mid_batch() {
        // A no-range event discards everything before it; later events apply to it.
        let changes = [
            ranged((0, 0), (0, 1), "Z"),
            full("new"),
            ranged((0, 0), (0, 0), "A"),
        ];
        assert_eq!(
            DocumentStore::apply_content_changes("abc", &changes),
            "Anew"
        );
    }

    #[test]
    fn apply_reversed_range_is_normalized() {
        let out = DocumentStore::apply_content_changes("abcde", &[ranged((0, 3), (0, 1), "X")]);
        assert_eq!(out, "aXde");
    }

    #[test]
    fn apply_newline_insert_then_edit_new_line() {
        // After the first event the document has two lines; the second event
        // addresses the freshly created line 1.
        let changes = [ranged((0, 2), (0, 2), "\n"), ranged((1, 1), (1, 1), "X")];
        assert_eq!(
            DocumentStore::apply_content_changes("abcd", &changes),
            "ab\ncXd"
        );
    }

    #[test]
    fn apply_delete_spanning_newline_joins_lines() {
        let out = DocumentStore::apply_content_changes("ab\ncd", &[ranged((0, 2), (1, 0), "")]);
        assert_eq!(out, "abcd");
    }

    #[test]
    fn apply_range_past_eof_clamps_to_append() {
        let out = DocumentStore::apply_content_changes("ab", &[ranged((5, 0), (5, 0), "!")]);
        assert_eq!(out, "ab!");
    }

    #[test]
    fn apply_empty_changes_keeps_text() {
        assert_eq!(DocumentStore::apply_content_changes("abc", &[]), "abc");
    }

    #[test]
    fn store_apply_changes_updates_text_version_and_index() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "ab\ncd".into(), 1);
        store.apply_changes(&uri, &[ranged((1, 0), (1, 2), "XYZ")], 2);
        let doc = store.get(&uri).unwrap();
        assert_eq!(&*doc.text, "ab\nXYZ");
        assert_eq!(doc.version, 2);
        // A stale index (built from "ab\ncd") would clamp this to 5.
        let end = doc.line_index.offset(&doc.text, Position::new(1, 3));
        assert_eq!(u32::from(end), 6);
    }

    #[test]
    fn store_apply_changes_ignores_unopened_document() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.apply_changes(&uri, &[ranged((0, 0), (0, 0), "x")], 1);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn store_apply_changes_empty_batch_bumps_version_only() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "abc".into(), 1);
        let before = store.get(&uri).unwrap();
        store.apply_changes(&uri, &[], 2);
        let after = store.get(&uri).unwrap();
        assert_eq!(&*after.text, "abc");
        assert_eq!(after.version, 2);
        // The text and line index are untouched, not rebuilt.
        assert!(Arc::ptr_eq(&before.line_index, &after.line_index));
    }

    #[test]
    fn store_upsert_get_remove() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "class B {}".into(), 1);
        let doc = store.get(&uri).unwrap();
        assert_eq!(&*doc.text, "class B {}");
        assert_eq!(doc.version, 1);
        store.remove(&uri);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn discovery_non_file_uri_uses_default() {
        let mut discovery = Discovery::<Config>::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(discovery.for_uri(&uri), Config::default());
    }

    #[test]
    fn discovery_clear_picks_up_config_edits() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("jalsfmt.toml");
        let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

        let mut discovery = Discovery::<Config>::default();
        std::fs::write(&config_path, "indent-width = 7\n").unwrap();
        assert_eq!(discovery.for_uri(&uri).indent_width, 7);

        // The cached config survives an edit on disk until the cache is cleared.
        std::fs::write(&config_path, "indent-width = 3\n").unwrap();
        assert_eq!(discovery.for_uri(&uri).indent_width, 7);

        discovery.clear();
        assert_eq!(discovery.for_uri(&uri).indent_width, 3);
    }

    #[test]
    fn is_config_file_matches_only_jalsfmt_toml() {
        let config = Url::parse("file:///p/jalsfmt.toml").unwrap();
        assert!(Config::is_config_file(&config));
        let other = Url::parse("file:///p/other.toml").unwrap();
        assert!(!Config::is_config_file(&other));
        let non_file = Url::parse("untitled:jalsfmt.toml").unwrap();
        assert!(!Config::is_config_file(&non_file));
    }

    #[test]
    fn lint_discovery_non_file_uri_uses_default() {
        let mut discovery = Discovery::<jals_config::lint::Config>::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(
            discovery.for_uri(&uri),
            jals_config::lint::Config::default()
        );
    }

    #[test]
    fn lint_discovery_clear_picks_up_config_edits() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("jalslint.toml");
        let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

        let mut discovery = Discovery::<jals_config::lint::Config>::default();
        // The resolved severity of `wildcard-import` under the on-disk config.
        let wildcard = |d: &mut Discovery<jals_config::lint::Config>| {
            d.for_uri(&uri).rules.get("wildcard-import").copied()
        };

        std::fs::write(&config_path, "[rules]\nwildcard-import = \"allow\"\n").unwrap();
        assert_eq!(wildcard(&mut discovery), Some(jals_config::Severity::Allow));

        // The cached config survives an edit on disk until the cache is cleared.
        std::fs::write(&config_path, "[rules]\nwildcard-import = \"error\"\n").unwrap();
        assert_eq!(wildcard(&mut discovery), Some(jals_config::Severity::Allow));

        discovery.clear();
        assert_eq!(wildcard(&mut discovery), Some(jals_config::Severity::Error));
    }

    #[test]
    fn is_lint_config_file_matches_only_jalslint_toml() {
        let config = Url::parse("file:///p/jalslint.toml").unwrap();
        assert!(jals_config::lint::Config::is_config_file(&config));
        let other = Url::parse("file:///p/jalsfmt.toml").unwrap();
        assert!(!jals_config::lint::Config::is_config_file(&other));
        let non_file = Url::parse("untitled:jalslint.toml").unwrap();
        assert!(!jals_config::lint::Config::is_config_file(&non_file));
    }

    #[test]
    fn workspace_folds_classpath_types_into_the_index() {
        // A project whose classpath carries a compiled `Box.class`: the workspace folds it into the
        // index as a `Classpath`-origin type, so external library types resolve here.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Main.java"), "class Main { }").unwrap();
        let box_class = jals_classfile::ClassFile::read(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Box.class"
        )))
        .expect("parse Box.class");

        let ws = Workspace::load_with_classpath(
            dir.path().to_path_buf(),
            vec![dir.path().to_path_buf()],
            vec![box_class],
        );
        assert!(
            ws.index()
                .items()
                .any(|item| item.origin == ItemOrigin::Classpath),
            "the classpath `.class` file should be indexed as a Classpath-origin type"
        );
        // Without a classpath, the same project has no Classpath-origin items.
        let bare = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        assert!(
            !bare
                .index()
                .items()
                .any(|item| item.origin == ItemOrigin::Classpath)
        );
    }

    /// Shared scaffolding for the go-to-definition-into-`Box` tests: a `src/Main.java` referencing
    /// `Box<String>`, the `Box.class` fixture on the classpath, and a navigation `.java` for `Box`
    /// folded in as a library source. `nav_source` produces that `.java` (real or synthesized) given
    /// the workspace dir and the parsed class, returning its path and its text. Returns the workspace,
    /// the project file URI, its text, the navigation source URI, and its text.
    fn workspace_with_box(
        nav_source: impl FnOnce(&Path, &jals_classfile::ClassFile) -> (PathBuf, String),
    ) -> (Workspace, Url, &'static str, Url, String) {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let main = "class Main { void m(Box<String> b) { b.get(); } }";
        std::fs::write(src_dir.join("Main.java"), main).unwrap();

        let box_class = jals_classfile::ClassFile::read(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Box.class"
        )))
        .expect("parse Box.class");
        let (box_java, box_src) = nav_source(dir.path(), &box_class);

        let ws = Workspace::load_with_classpath_and_sources(
            dir.path().to_path_buf(),
            vec![src_dir.clone()],
            vec![box_class],
            vec![box_java.clone()],
            Vec::new(),
            FeatureSet::default(),
        );
        let main_uri = Url::from_file_path(src_dir.join("Main.java")).unwrap();
        let box_uri = Url::from_file_path(&box_java).unwrap();
        // The workspace read every file eagerly at load, and go-to-definition returns only cached
        // URIs/ranges, so the tempdir can drop here without affecting the assertions.
        drop(dir);
        (ws, main_uri, main, box_uri, box_src)
    }

    /// [`workspace_with_box`] with a real `Box.java` library source on disk (a `file://` URL the
    /// editor can open), placed outside the project's source root so it is a navigation file, not a
    /// project source.
    fn workspace_with_box_sources() -> (Workspace, Url, &'static str, Url, String) {
        workspace_with_box(|dir, _class| {
            let box_src = "public class Box<T> { public T get() { return null; } }".to_owned();
            let lib_dir = dir.join("libsrc");
            std::fs::create_dir_all(&lib_dir).unwrap();
            let box_java = lib_dir.join("Box.java");
            std::fs::write(&box_java, &box_src).unwrap();
            (box_java, box_src)
        })
    }

    #[test]
    fn goto_definition_into_classpath_type_lands_in_library_source() {
        let (ws, main_uri, main, box_uri, box_src) = workspace_with_box_sources();
        // Cursor on the `Box` type reference in the project file.
        let col = main.find("Box").unwrap() as u32;
        let loc = ws
            .goto_definition(&main_uri, Position::new(0, col))
            .expect("Box navigates into its library source");
        assert_eq!(loc.uri, box_uri);
        let want = box_src.find("class Box").unwrap() + "class ".len();
        assert_eq!(loc.range.start, Position::new(0, want as u32));
    }

    #[test]
    fn goto_definition_into_classpath_member_lands_in_library_source() {
        let (ws, main_uri, main, box_uri, box_src) = workspace_with_box_sources();
        // Cursor on the `get` member name in `b.get()`.
        let col = main.find("get(").unwrap() as u32;
        let loc = ws
            .goto_definition(&main_uri, Position::new(0, col))
            .expect("Box.get navigates into its library source");
        assert_eq!(loc.uri, box_uri);
        let want = box_src.find("get(").unwrap();
        assert_eq!(loc.range.start, Position::new(0, want as u32));
    }

    /// [`workspace_with_box`] with **no real source** — instead a signature-only `.java` skeleton is
    /// synthesized from the class file (exactly as the server does for a dependency that ships no
    /// `-sources.jar`) and fed as the navigation source.
    fn workspace_with_synthesized_box() -> (Workspace, Url, &'static str, Url, String) {
        workspace_with_box(|dir, box_class| {
            let synthesized = jals_classpath::SkeletonGroup::synthesize_classpath_sources(
                std::slice::from_ref(box_class),
                dir,
                |message| panic!("unexpected synthesis warning: {message}"),
            );
            assert_eq!(synthesized.len(), 1);
            let box_java = synthesized[0].clone();
            let box_src = std::fs::read_to_string(&box_java).unwrap();
            (box_java, box_src)
        })
    }

    #[test]
    fn goto_definition_into_a_synthesized_skeleton_type() {
        let (ws, main_uri, main, box_uri, box_src) = workspace_with_synthesized_box();
        let col = main.find("Box").unwrap() as u32;
        let loc = ws
            .goto_definition(&main_uri, Position::new(0, col))
            .expect("Box navigates into its synthesized skeleton");
        assert_eq!(loc.uri, box_uri);
        // The range starts exactly on the `Box` name token in the parsed skeleton.
        let line_index = crate::line_index::LineIndex::new(&box_src);
        let off = u32::from(line_index.offset(&box_src, loc.range.start)) as usize;
        assert!(box_src[off..].starts_with("Box"), "{box_src}");
    }

    #[test]
    fn goto_definition_into_a_synthesized_skeleton_member() {
        let (ws, main_uri, main, box_uri, box_src) = workspace_with_synthesized_box();
        let col = main.find("get(").unwrap() as u32;
        let loc = ws
            .goto_definition(&main_uri, Position::new(0, col))
            .expect("Box.get navigates into its synthesized skeleton");
        assert_eq!(loc.uri, box_uri);
        let line_index = crate::line_index::LineIndex::new(&box_src);
        let off = u32::from(line_index.offset(&box_src, loc.range.start)) as usize;
        assert!(box_src[off..].starts_with("get"), "{box_src}");
    }

    /// Build a workspace whose project (under `src/`) references `Box`, supplied by a `git`/`path`
    /// **source dependency** (`Box.java` outside the source root, fed as a source-dep input — NOT on
    /// the classpath and with NO `.class`). Returns the workspace, the project file URI + text, and the
    /// library source URI + text.
    fn workspace_with_source_dep() -> (Workspace, Url, &'static str, Url, &'static str) {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let main = "class Main { void m(Box<String> b) { var x = b.get(); } }";
        std::fs::write(src_dir.join("Main.java"), main).unwrap();

        // The dependency's source, outside the project source root (a `git`/`path` checkout).
        let box_src = "public class Box<T> { public T get() { return null; } }";
        let lib_dir = dir.path().join("dep");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let box_java = lib_dir.join("Box.java");
        std::fs::write(&box_java, box_src).unwrap();

        let ws = Workspace::load_with_classpath_and_sources(
            dir.path().to_path_buf(),
            vec![src_dir.clone()],
            Vec::new(),             // no classpath `.class`
            Vec::new(),             // no `-sources.jar` overlay
            vec![box_java.clone()], // the source dependency's `.java`
            FeatureSet::default(),
        );
        let main_uri = Url::from_file_path(src_dir.join("Main.java")).unwrap();
        let box_uri = Url::from_file_path(&box_java).unwrap();
        drop(dir);
        (ws, main_uri, main, box_uri, box_src)
    }

    #[test]
    fn goto_definition_into_source_dep_type_lands_in_library_source() {
        let (ws, main_uri, main, box_uri, box_src) = workspace_with_source_dep();
        let col = main.find("Box").unwrap() as u32;
        let loc = ws
            .goto_definition(&main_uri, Position::new(0, col))
            .expect("a source-dep type navigates to its source");
        assert_eq!(loc.uri, box_uri);
        let want = box_src.find("class Box").unwrap() + "class ".len();
        assert_eq!(loc.range.start, Position::new(0, want as u32));
    }

    #[test]
    fn goto_definition_into_source_dep_member_lands_in_library_source() {
        let (ws, main_uri, main, box_uri, box_src) = workspace_with_source_dep();
        let col = main.find("get(").unwrap() as u32;
        let loc = ws
            .goto_definition(&main_uri, Position::new(0, col))
            .expect("a source-dep member navigates to its source");
        assert_eq!(loc.uri, box_uri);
        let want = box_src.find("get(").unwrap();
        assert_eq!(loc.range.start, Position::new(0, want as u32));
    }

    #[test]
    fn source_dep_type_resolves_for_hover() {
        // Folding the source dependency in as a `Source`-origin type means it is an analysis input,
        // not just a navigation target: the parameter `b` hovers as the resolved generic type
        // `Box<String>` (an unresolved external `Box` would carry no type argument).
        let (ws, main_uri, main, _, _) = workspace_with_source_dep();
        let col = main.find("b.get()").unwrap() as u32;
        let hover = ws
            .hover(&main_uri, Position::new(0, col))
            .expect("b has an inferred type");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert_eq!(markup.value, "```java\nBox<String>\n```");
    }

    #[test]
    fn workspace_resolves_definition_across_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "package a; class Foo { }").unwrap();
        std::fs::write(
            dir.path().join("Bar.java"),
            "package a; class Bar { Foo f; }",
        )
        .unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
        let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
        assert!(ws.file_id(&bar_uri).is_some());

        // The `Foo` reference in Bar.java jumps to the class declaration in Foo.java.
        let bar = "package a; class Bar { Foo f; }";
        let use_col = bar.find("Foo").unwrap() as u32;
        let loc = ws
            .goto_definition(&bar_uri, Position::new(0, use_col))
            .expect("Foo resolves cross-file");
        assert_eq!(loc.uri, foo_uri);

        let foo = "package a; class Foo { }";
        let decl_col = foo.find("Foo").unwrap() as u32;
        assert_eq!(loc.range.start, Position::new(0, decl_col));
        assert_eq!(loc.range.end, Position::new(0, decl_col + 3));
    }

    #[test]
    fn workspace_load_in_drives_from_an_in_memory_tree() {
        // The whole workspace can be built off an `InMemoryFileTree` with no real filesystem — the
        // same cross-file resolution the tempfile tests exercise, driven purely in memory.
        let fs = jals_fs::InMemoryFileTree::new()
            .with_file("/proj/src/Foo.java", "package a; class Foo { }")
            .with_file("/proj/src/Bar.java", "package a; class Bar { Foo f; }");
        let ws = Workspace::load_in(
            fs,
            PathBuf::from("/proj"),
            vec![PathBuf::from("/proj/src")],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            FeatureSet::default(),
        );

        let bar_uri = Url::from_file_path("/proj/src/Bar.java").unwrap();
        let foo_uri = Url::from_file_path("/proj/src/Foo.java").unwrap();
        assert!(ws.file_id(&bar_uri).is_some());

        // The `Foo` reference in Bar.java jumps to the class declaration in Foo.java.
        let bar = "package a; class Bar { Foo f; }";
        let use_col = bar.find("Foo").unwrap() as u32;
        let loc = ws
            .goto_definition(&bar_uri, Position::new(0, use_col))
            .expect("Foo resolves cross-file, in memory");
        assert_eq!(loc.uri, foo_uri);
        let foo = "package a; class Foo { }";
        let decl_col = foo.find("Foo").unwrap() as u32;
        assert_eq!(loc.range.start, Position::new(0, decl_col));
        assert_eq!(loc.range.end, Position::new(0, decl_col + 3));
    }

    #[test]
    fn workspace_overlay_picks_up_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Bar.java"),
            "package a; class Bar { Foo f; }",
        )
        .unwrap();

        let mut ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
        let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
        let bar = "package a; class Bar { Foo f; }";
        let use_col = bar.find("Foo").unwrap() as u32;

        // `Foo` is unresolved before any file declares it.
        assert!(
            ws.goto_definition(&bar_uri, Position::new(0, use_col))
                .is_none()
        );

        // The editor opens a new Foo.java under the source root; the overlay adds it to the index.
        let doc = Document::new("package a; class Foo { }".to_owned(), 1);
        assert!(ws.set_overlay(&foo_uri, &doc));
        let loc = ws
            .goto_definition(&bar_uri, Position::new(0, use_col))
            .expect("Foo resolves after the overlay");
        assert_eq!(loc.uri, foo_uri);
    }

    #[test]
    fn workspace_overlay_reindexes_an_edited_file() {
        // The incremental path: editing one file re-extracts only it, and the reassembled index
        // reflects the change for a sibling file that depends on it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Types.java"), "package a; class A { }").unwrap();
        std::fs::write(dir.path().join("Use.java"), "package a; class Use { B b; }").unwrap();

        let mut ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let types_uri = Url::from_file_path(dir.path().join("Types.java")).unwrap();
        let use_uri = Url::from_file_path(dir.path().join("Use.java")).unwrap();
        let use_src = "package a; class Use { B b; }";
        let use_col = use_src.find("B b").unwrap() as u32;

        // `B` is unresolved: Types.java declares only `A`.
        assert!(
            ws.goto_definition(&use_uri, Position::new(0, use_col))
                .is_none()
        );

        // Editing Types.java to add a sibling `B` re-extracts just that file; Use.java's reference
        // to `B` now resolves through the reassembled index.
        let edited = Document::new("package a; class A { } class B { }".to_owned(), 2);
        assert!(ws.set_overlay(&types_uri, &edited));
        let loc = ws
            .goto_definition(&use_uri, Position::new(0, use_col))
            .expect("B resolves after the edit");
        assert_eq!(loc.uri, types_uri);
    }

    #[test]
    fn workspace_hover_shows_a_cross_file_project_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "package a; class Foo { }").unwrap();
        let bar = "package a; class Bar { void m() { var f = new Foo(); } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // Hovering the `new Foo()` expression shows `Foo`, resolved against the other file.
        let col = bar.find("new Foo()").unwrap() as u32;
        let hover = ws
            .hover(&bar_uri, Position::new(0, col))
            .expect("new Foo() has an inferred type");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert_eq!(markup.value, "```java\nFoo\n```");

        // A document outside the workspace has no workspace hover.
        let other = Url::parse("file:///elsewhere/Other.java").unwrap();
        assert!(ws.hover(&other, Position::new(0, 0)).is_none());
    }

    #[test]
    fn workspace_file_id_is_none_for_unknown_uri() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Bar.java"), "class Bar { }").unwrap();
        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let other = Url::parse("file:///elsewhere/Other.java").unwrap();
        assert!(ws.file_id(&other).is_none());
    }

    #[test]
    fn workspace_references_find_a_project_type_across_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "package a; class Foo { }").unwrap();
        std::fs::write(
            dir.path().join("Bar.java"),
            "package a; class Bar { Foo f; }",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Baz.java"),
            "package a; class Baz { Foo g; Foo h; }",
        )
        .unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
        let baz_uri = Url::from_file_path(dir.path().join("Baz.java")).unwrap();

        // From the declaration of `Foo`, including the declaration: the decl plus all three uses.
        let decl_col = "package a; class Foo { }".find("Foo").unwrap() as u32;
        let refs = ws
            .references(&foo_uri, Position::new(0, decl_col), true)
            .expect("Foo.java is in the workspace");
        assert_eq!(refs.len(), 4);
        assert_eq!(refs.iter().filter(|l| l.uri == foo_uri).count(), 1); // declaration
        assert_eq!(refs.iter().filter(|l| l.uri == bar_uri).count(), 1);
        assert_eq!(refs.iter().filter(|l| l.uri == baz_uri).count(), 2);

        // From a use in Bar.java, excluding the declaration: only the three uses, never Foo.java.
        let use_col = "package a; class Bar { Foo f; }".find("Foo").unwrap() as u32;
        let refs = ws
            .references(&bar_uri, Position::new(0, use_col), false)
            .expect("Bar.java is in the workspace");
        assert_eq!(refs.len(), 3);
        assert!(refs.iter().all(|l| l.uri != foo_uri));
    }

    #[test]
    fn workspace_references_keep_a_local_binding_within_its_file() {
        let dir = tempfile::tempdir().unwrap();
        // A sibling file also declares an `x`, to prove the local does not leak across files.
        std::fs::write(
            dir.path().join("Foo.java"),
            "package a; class Foo { int x; }",
        )
        .unwrap();
        let bar = "package a; class Bar { void m() { int x = 1; use(x); } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // From the use `x` in `use(x)`: only the two occurrences in Bar.java (declaration + use).
        let use_col = bar.rfind('x').unwrap() as u32;
        let refs = ws
            .references(&bar_uri, Position::new(0, use_col), true)
            .expect("Bar.java is in the workspace");
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|l| l.uri == bar_uri));
    }

    #[test]
    fn workspace_rename_rewrites_a_project_type_across_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "package a; class Foo { }").unwrap();
        std::fs::write(
            dir.path().join("Bar.java"),
            "package a; class Bar { Foo f; }",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Baz.java"),
            "package a; class Baz { Foo g; Foo h; }",
        )
        .unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
        let baz_uri = Url::from_file_path(dir.path().join("Baz.java")).unwrap();

        // Rename `Foo` from its declaration: the edit rewrites the declaration plus every use in
        // every file, each to the new name.
        let decl_col = "package a; class Foo { }".find("Foo").unwrap() as u32;
        let edit = ws
            .rename(&foo_uri, Position::new(0, decl_col), "Renamed")
            .expect("Foo is a renamable project type");
        let changes = edit.changes.expect("a plain-edit workspace edit");
        assert_eq!(changes[&foo_uri].len(), 1); // the declaration
        assert_eq!(changes[&bar_uri].len(), 1);
        assert_eq!(changes[&baz_uri].len(), 2);
        assert!(changes.values().flatten().all(|e| e.new_text == "Renamed"));

        // prepareRename on the same position reports the identifier's range.
        let range = ws
            .prepare_rename(&foo_uri, Position::new(0, decl_col))
            .expect("Foo is renamable");
        assert_eq!(range.start, Position::new(0, decl_col));
        assert_eq!(range.end, Position::new(0, decl_col + 3));
    }

    #[test]
    fn workspace_rename_withholds_members_and_external_names() {
        let dir = tempfile::tempdir().unwrap();
        let foo = "package a; class Foo { int size; String s; }";
        std::fs::write(dir.path().join("Foo.java"), foo).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();

        // A field is a member: its cross-file uses are not indexed, so rename is withheld.
        let size_col = foo.find("size").unwrap() as u32;
        assert!(
            ws.prepare_rename(&foo_uri, Position::new(0, size_col))
                .is_none()
        );
        assert!(
            ws.rename(&foo_uri, Position::new(0, size_col), "len")
                .is_none()
        );

        // `String` now resolves to a `java.lang` stub item, but a stub has no host-editable file,
        // so rename stays withheld (just as navigation into it is).
        let string_col = foo.find("String").unwrap() as u32;
        assert!(
            ws.prepare_rename(&foo_uri, Position::new(0, string_col))
                .is_none()
        );
    }

    #[test]
    fn workspace_goto_definition_jumps_to_a_member_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let box_src = "package a; class Box { int size; int area() { return 0; } }";
        std::fs::write(dir.path().join("Box.java"), box_src).unwrap();
        let bar = "package a; class Bar { void m(Box b) { var s = b.size; var a = b.area(); } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
        let box_uri = Url::from_file_path(dir.path().join("Box.java")).unwrap();

        // `b.size` jumps to the field declaration in Box.java.
        let size_col = bar.find("b.size").unwrap() as u32 + 2;
        let loc = ws
            .goto_definition(&bar_uri, Position::new(0, size_col))
            .expect("the field `size` resolves to its declaration");
        assert_eq!(loc.uri, box_uri);
        assert_eq!(
            loc.range.start,
            Position::new(0, box_src.find("size").unwrap() as u32)
        );

        // `b.area()` jumps to the method declaration (a call's callee is a method).
        let area_col = bar.find("b.area").unwrap() as u32 + 2;
        let loc = ws
            .goto_definition(&bar_uri, Position::new(0, area_col))
            .expect("the method `area` resolves to its declaration");
        assert_eq!(loc.uri, box_uri);
        assert_eq!(
            loc.range.start,
            Position::new(0, box_src.find("area").unwrap() as u32)
        );
    }

    #[test]
    fn workspace_hover_shows_a_member_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Box.java"),
            "package a; class Box { long id; }",
        )
        .unwrap();
        let bar = "package a; class Bar { void m(Box b) { var v = b.id; } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // Hovering the field access `b.id` shows the field's type, resolved cross-file.
        let col = bar.find("b.id").unwrap() as u32 + 2;
        let hover = ws
            .hover(&bar_uri, Position::new(0, col))
            .expect("b.id has an inferred type");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert_eq!(markup.value, "```java\nlong\n```");
    }

    #[test]
    fn workspace_signature_help_on_a_cross_file_method() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Box.java"),
            "package a; class Box { int area(int w, int h) { return 0; } }",
        )
        .unwrap();
        let bar = "package a; class Bar { void g(Box b) { b.area(1, ); } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // Cursor on the second argument of `b.area(1, )`: the receiver `b` is a cross-file `Box`.
        let needle = "area(1, ";
        let col = bar.find(needle).unwrap() as u32 + needle.len() as u32;
        let help = ws
            .signature_help(&bar_uri, Position::new(0, col))
            .expect("signature help on b.area");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "area(int w, int h)");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn workspace_completes_members_of_a_cross_file_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Box.java"),
            "package a; class Box { int size; int area() { return 0; } }",
        )
        .unwrap();
        let bar = "package a; class Bar { void g(Box b) { b. } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // Cursor right after `b.`: the receiver `b` is a cross-file `Box`, so its members complete.
        let col = bar.find("b. ").unwrap() as u32 + 2;
        let mut labels: Vec<String> = ws
            .completions(&bar_uri, Position::new(0, col))
            .expect("Bar.java is in the workspace")
            .into_iter()
            .map(|item| item.label)
            .collect();
        labels.sort();
        assert_eq!(labels, ["area", "size"]);

        // A document outside the workspace has no workspace completions.
        let other = Url::parse("file:///elsewhere/Other.java").unwrap();
        assert!(ws.completions(&other, Position::new(0, 0)).is_none());
    }

    #[test]
    fn workspace_scope_completion_offers_a_cross_file_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Helper.java"),
            "package a; class Helper { }",
        )
        .unwrap();
        let main = "package a; class Main { void m() { int x = 1;  } }";
        std::fs::write(dir.path().join("Main.java"), main).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let main_uri = Url::from_file_path(dir.path().join("Main.java")).unwrap();

        // A bare position inside `m`: the sibling type `Helper`, the local `x`, and keywords are all
        // offered (not a member access, so the scope path runs).
        let col = main.find("1; ").unwrap() as u32 + 3;
        let labels: Vec<String> = ws
            .completions(&main_uri, Position::new(0, col))
            .expect("Main.java is in the workspace")
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert!(
            labels.contains(&"Helper".to_owned()),
            "cross-file type in {labels:?}"
        );
        assert!(labels.contains(&"x".to_owned()));
        assert!(labels.contains(&"return".to_owned()));
    }

    #[test]
    fn workspace_document_highlight_is_precise_for_a_cross_file_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.java"), "package a; class Foo { }").unwrap();
        // A same-package sibling uses `Foo` twice and also has a same-spelled local.
        let bar = "package a; class Bar { Foo a; Foo b; void m() { int Foo = 0; } }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // Cursor on the first `Foo` type use: only the two cross-file type references highlight,
        // never the `int Foo` local.
        let col = bar.find("Foo").unwrap() as u32;
        let highlights = ws
            .document_highlight(&bar_uri, Position::new(0, col))
            .expect("Bar.java is in the workspace");
        assert_eq!(highlights.len(), 2);
    }

    #[test]
    fn workspace_semantic_tokens_classify_a_cross_file_type() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Color.java"),
            "package a; enum Color { RED }",
        )
        .unwrap();
        let bar = "package a; class Bar { Color c; }";
        std::fs::write(dir.path().join("Bar.java"), bar).unwrap();

        let ws = Workspace::load(dir.path().to_path_buf(), vec![dir.path().to_path_buf()]);
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();

        // The `Color` reference is delta-encoded; decode to the first token's absolute column and
        // confirm the cross-file `enum` kind (index 3 in the legend) was applied.
        let tokens = ws
            .semantic_tokens(&bar_uri)
            .expect("Bar.java is in the workspace");
        let legend = crate::handlers::SemanticTokensBuilder::legend();
        let enum_index = legend
            .token_types
            .iter()
            .position(|t| t.as_str() == "enum")
            .unwrap() as u32;
        let color_col = bar.find("Color").unwrap() as u32;
        let (mut line, mut start) = (0u32, 0u32);
        let mut found = None;
        for token in &tokens.data {
            if token.delta_line == 0 {
                start += token.delta_start;
            } else {
                line += token.delta_line;
                start = token.delta_start;
            }
            if line == 0 && start == color_col {
                found = Some(token.token_type);
            }
        }
        assert_eq!(found, Some(enum_index));
    }
}
