//! In-memory server state: open documents, the per-project workspace adapter, and memoized
//! config discovery. All of it is `!Send` and owned exclusively by the actor
//! ([`Actor`](crate::actor)).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use async_lsp::lsp_types::{
    CompletionItem, Diagnostic, DocumentHighlight, Hover, Location, Position, Range,
    SemanticTokens, SignatureHelp, TextDocumentContentChangeEvent, Url, WorkspaceEdit,
};
use jals_config::FeatureSet;
use jals_editor::{Editor, ProjectLayout, Utf16Position};
use jals_exec::Exec;
use jals_exec::tokio_rt::on_blocking_pool;
use jals_storage::{
    CacheBackend, CodeTree, DirKey, FileKey, MemoryCache, MemorySource, MemoryStorage, Name,
    NativeCache, NativeScope, NativeSource, NativeStorage, ProjectStorage, RelativePath,
    SourceBackend,
};

use crate::host::LspHost;

/// An open document: the shared per-file caches (text, coordinate map, parsed CST) plus the
/// client's version.
///
/// The content lives in a [`jals_editor::Document`], whose fields are behind `Arc` so a snapshot
/// can be cheaply cloned out of the store — and shared with the owning workspace's overlay
/// without reparsing.
#[derive(Clone)]
pub(crate) struct Document {
    pub(crate) content: jals_editor::Document,
    pub(crate) version: i32,
}

impl Document {
    /// Parse `text` into the shared per-file caches. Async because parsing yields cooperatively.
    async fn new(text: String, version: i32) -> Self {
        Self {
            content: jals_editor::Document::new(text).await,
            version,
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
    pub(crate) async fn upsert(&mut self, uri: Url, text: String, version: i32) {
        self.docs.insert(uri, Document::new(text, version).await);
    }

    /// Apply `didChange` content changes to the document at `uri`, recording `version`.
    ///
    /// A change for a document that is not open is ignored (client protocol error;
    /// splicing into a nonexistent base would fabricate text). The version is recorded
    /// even when `changes` is empty.
    pub(crate) async fn apply_changes(
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
        let text = Self::apply_content_changes(&doc.content.text, changes);
        *doc = Document::new(text, version).await;
    }

    /// Snapshot the document for `uri` (cheap `Arc` clones), if open.
    pub(crate) fn get(&self, uri: &Url) -> Option<Document> {
        self.docs.get(uri).cloned()
    }

    /// Every open document's URI, in no particular order.
    pub(crate) fn uris(&self) -> impl Iterator<Item = &Url> {
        self.docs.keys()
    }

    pub(crate) fn remove(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }
}

impl DocumentStore {
    /// Apply LSP `didChange` content changes to `text`, in order.
    ///
    /// Per the LSP spec each event's range refers to the document state after the previous
    /// event, so a fresh [`jals_editor::LineIndex`] is built per ranged event. An event without a
    /// range replaces the whole document. Reversed ranges are normalized and out-of-range
    /// positions are clamped by the index's `offset`, so this never panics.
    fn apply_content_changes(text: &str, changes: &[TextDocumentContentChangeEvent]) -> String {
        /// Decode an LSP position against `index`/`text` to a byte offset.
        fn offset_of(index: &jals_editor::LineIndex, text: &str, position: Position) -> usize {
            index.offset(
                text,
                Utf16Position {
                    line: position.line,
                    character: position.character,
                },
            )
        }
        let mut text = text.to_owned();
        for change in changes {
            let Some(range) = change.range else {
                text.clone_from(&change.text);
                continue;
            };
            let index = jals_editor::LineIndex::new(&text);
            let start = offset_of(&index, &text, range.start);
            let end = offset_of(&index, &text, range.end);
            text.replace_range(start.min(end)..start.max(end), &change.text);
        }
        text
    }
}

/// The analysis half of a workspace: every query this server answers, keyed by the workspace's
/// own [`FileKey`].
///
/// Generic over the storage backends because that is the only thing the two kinds of workspace
/// differ in — a project's analysis reads the host filesystem, a detached group holds nothing but
/// the documents the client opened. The query surface is identical, so it is written once here
/// and neither of them restates it.
///
/// What is deliberately *absent* is how a `Url` becomes a `FileKey`: that rule belongs to the
/// owner (a path computation for a project, a mount table for a detached group), and
/// [`OpenDocument`] is where the two meet.
struct Analysis<S: SourceBackend, C: CacheBackend> {
    editor: Editor<S, C, LspHost>,
}

impl<S: SourceBackend, C: CacheBackend> Analysis<S, C> {
    async fn load(storage: ProjectStorage<S, C>, spec: ProjectLayout, host: LspHost) -> Self {
        Self {
            editor: Editor::load(storage, spec, host).await,
        }
    }

    /// Whether `path` belongs here: indexed, selected exactly, or under a source root.
    fn owns_path(&self, path: &FileKey) -> bool {
        self.editor.workspace().owns_path(path)
    }

    /// The project's resolved `[package] features`.
    const fn feature_set(&self) -> FeatureSet {
        self.editor.workspace().feature_set()
    }

    /// Replace the exact file membership.
    ///
    /// Must precede the [`set_overlay`](Self::set_overlay) of a key under no source root — which
    /// is every key a detached group mounts. Skipping it makes `set_overlay` refuse silently, and
    /// the document is then routed here and answers nothing.
    fn set_project_sources(&mut self, sources: Vec<FileKey>) {
        self.editor.workspace_mut().set_project_sources(sources);
    }

    /// The rendering, mutably — a detached group registers each mounted key's URI here.
    const fn host_mut(&mut self) -> &mut LspHost {
        self.editor.host_mut()
    }

    /// Publish a fresh snapshot and rebuild the index while preserving editor overlays.
    async fn refresh(&mut self) {
        let _ = self.editor.workspace_mut().refresh().await;
    }

    /// Re-read the current membership into the index, dropping whatever left it.
    ///
    /// The surviving files are re-parsed from their overlay text rather than reusing their cached
    /// parses. That is the cost of dropping a file at all — the index is a `Vec` addressed by file
    /// id, so removing one entry renumbers the rest — and it is paid only when a document leaves a
    /// group, which is rare and over a handful of files.
    async fn reload(&mut self) {
        self.editor.workspace_mut().reload_project_files().await;
    }

    /// Reflect an open document's current text into the index.
    ///
    /// `false` when `path` belongs to no part of this analysis; the caller must not ignore it,
    /// because a document routed here that was never indexed answers every query with nothing.
    async fn set_overlay(&mut self, path: &FileKey, doc: &jals_editor::Document) -> bool {
        self.editor
            .workspace_mut()
            .set_overlay(path, doc)
            .await
            .unwrap_or(false)
    }

    /// Drop one file's overlay, so a closed document's bytes do not outlive it.
    fn remove_overlay(&mut self, path: &FileKey) {
        let storage = self.editor.workspace_mut().storage_mut();
        let revision = storage.revision();
        let _ = storage.remove_overlay(revision, path);
    }

    // ---- Queries ---------------------------------------------------------------------------
    //
    // Each answers for a file this analysis has indexed. `None` / an empty vector means the
    // analysis found nothing — never "not my file", which the caller settled by resolving the key
    // at all.

    async fn definition(&self, path: &FileKey, position: Position) -> Option<Location> {
        self.editor.definition(path, &position).await
    }

    async fn hover(&self, path: &FileKey, position: Position) -> Option<Hover> {
        self.editor.hover(path, &position).await
    }

    async fn signature_help(&self, path: &FileKey, position: Position) -> Option<SignatureHelp> {
        self.editor.signature_help(path, &position).await
    }

    async fn prepare_rename(&self, path: &FileKey, position: Position) -> Option<Range> {
        self.editor.prepare_rename(path, &position).await
    }

    async fn semantic_tokens(&self, path: &FileKey) -> Option<SemanticTokens> {
        self.editor.semantic_tokens(path).await
    }

    async fn completions(&self, path: &FileKey, position: Position) -> Vec<CompletionItem> {
        self.editor.completions(path, &position).await
    }

    async fn document_highlight(
        &self,
        path: &FileKey,
        position: Position,
    ) -> Vec<DocumentHighlight> {
        self.editor.highlights(path, &position).await
    }

    async fn references(
        &self,
        path: &FileKey,
        position: Position,
        include_declaration: bool,
    ) -> Vec<Location> {
        self.editor
            .references(path, &position, include_declaration)
            .await
    }

    async fn diagnostics(
        &self,
        path: &FileKey,
        config: &jals_config::lint::Config,
    ) -> Vec<Diagnostic> {
        self.editor.diagnostics(path, config).await
    }

    /// The edit a rename produces. The caller validates `new_name` is a legal identifier.
    async fn rename(
        &self,
        path: &FileKey,
        position: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        let targets = self.editor.rename_targets(path, &position).await?;
        LspHost::workspace_edit(targets, new_name)
    }
}

/// One `jals.toml` project's analysis, plus the URI ↔ virtual-path mapping that is the LSP's only
/// remaining responsibility.
///
/// The actor holds one of these per project a client has a file open in (see
/// [`Actor`](crate::actor)), discovered lazily by walking up from each opened file — so it
/// only ever indexes the source roots of a real manifest, never a whole git checkout.
pub(crate) struct ProjectWorkspace {
    /// The `jals.toml` directory this workspace was discovered from; identifies the workspace so
    /// a later open in the same project reuses it instead of building a duplicate.
    project_root: PathBuf,
    /// The neutral workspace paired with the LSP rendering; owns all analysis state.
    analysis: Analysis<NativeSource, NativeCache>,
}

impl ProjectWorkspace {
    /// Load a project workspace off the host filesystem: walk `source_roots` for `.java`, fold
    /// the already-parsed classpath `.class` files into the index, register the library /
    /// source-dependency `.java`, and resolve `feature_set` into every lint run — all inside
    /// [`jals_editor::Workspace`]. The caller resolves the manifest and performs the dependency
    /// I/O; this keeps only the `PathBuf` → virtual-path lowering.
    #[allow(clippy::too_many_arguments)]
    async fn load(
        project_root: PathBuf,
        source_roots: &[PathBuf],
        classfiles: &[jals_classfile::ClassFile],
        library_sources: &[PathBuf],
        source_dep_sources: &[PathBuf],
        feature_set: FeatureSet,
        build_features: BTreeSet<String>,
        exec: Exec,
    ) -> Self {
        let scopes = source_roots.iter().filter_map(|path| {
            RelativePath::from_host_path(&project_root, path)
                .map(|relative| NativeScope::extension(relative, "java"))
        });
        let storage = NativeStorage::for_project_scoped(&project_root, scopes, exec)
            .await
            .expect("a discovered project root must be readable");
        let source_roots = source_roots
            .iter()
            .filter_map(|path| Self::dir_key(&project_root, path))
            .collect();
        let library_sources = library_sources
            .iter()
            .filter_map(|path| Self::file_key(&project_root, path))
            .collect();
        let source_dep_sources = source_dep_sources
            .iter()
            .filter_map(|path| Self::file_key(&project_root, path))
            .collect();
        Self::load_storage(
            project_root,
            storage,
            source_roots,
            Vec::new(),
            classfiles,
            library_sources,
            source_dep_sources,
            BTreeMap::new(),
            feature_set,
            build_features,
        )
        .await
    }

    /// A workspace over `root` alone — its own lone source root; no classpath, libraries, or
    /// features. The fallback when a manifest is missing, unparsable, or its inputs fail to
    /// assemble.
    ///
    /// This is *not* how a document outside every project is analysed: a bare workspace indexes
    /// its whole directory tree, which is only warranted once a real manifest has been found there
    /// (see [`DetachedWorkspace`], which mounts one file at a time and walks nothing).
    pub(crate) async fn bare(root: &Path, exec: Exec) -> Self {
        let root = root.to_path_buf();
        Self::load(
            root.clone(),
            std::slice::from_ref(&root),
            &[],
            &[],
            &[],
            FeatureSet::default(),
            BTreeSet::new(),
            exec,
        )
        .await
    }

    /// Construct from an already-open aggregate after dependency assembly. The same storage owns
    /// source revision, overlays, and artifact cache for the workspace lifetime. `materialized`
    /// maps mounted `.jals/…` navigation sources to the real files materialized out of the
    /// artifact cache, so their locations are rendered as openable `file://` URLs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn load_storage(
        project_root: PathBuf,
        storage: NativeStorage,
        source_roots: Vec<DirKey>,
        project_sources: Vec<FileKey>,
        classfiles: &[jals_classfile::ClassFile],
        library_sources: Vec<FileKey>,
        source_dep_sources: Vec<FileKey>,
        materialized: BTreeMap<FileKey, PathBuf>,
        feature_set: FeatureSet,
        build_features: BTreeSet<String>,
    ) -> Self {
        let spec = ProjectLayout {
            source_roots,
            project_sources,
            library_sources,
            source_dep_sources,
            feature_set,
            // What each project file's `#[cfg(feature = "…")]` evaluates against (used only
            // when `feature_set` enables the `attributes` dialect).
            build_features,
            ..ProjectLayout::default()
        }
        .with_classpath(classfiles)
        .await;
        // The materialized paths are host facts; the host renders addresses. Convert once, here,
        // at the boundary between them. A path that cannot be encoded as a file URL is dropped
        // rather than carried forward as an address that resolves to nothing — navigation into
        // that one file yields no target, and every other one is unaffected.
        let urls = materialized
            .into_iter()
            .filter_map(|(key, path)| Url::from_file_path(path).ok().map(|url| (key, url)))
            .collect();
        let host = LspHost::for_root(project_root.clone()).with_urls(urls);
        Self {
            project_root,
            analysis: Analysis::load(storage, spec, host).await,
        }
    }

    /// Host paths become the workspace's typed virtual paths through
    /// [`RelativePath::from_host_path`]; a path outside the root or with a non-portable
    /// component cannot be addressed and is skipped.
    fn file_key(root: &Path, path: &Path) -> Option<FileKey> {
        FileKey::new(RelativePath::from_host_path(root, path)?).ok()
    }

    fn dir_key(root: &Path, path: &Path) -> Option<DirKey> {
        Some(DirKey::new(RelativePath::from_host_path(root, path)?))
    }

    /// The workspace key of `uri`, when it is a file URL inside this project root.
    fn key(&self, uri: &Url) -> Option<FileKey> {
        Self::file_key(&self.project_root, &uri.to_file_path().ok()?)
    }

    /// The `jals.toml` project root this workspace was loaded from.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Publish a fresh native snapshot and rebuild the index while preserving editor overlays.
    pub(crate) async fn refresh(&mut self) {
        self.analysis.refresh().await;
    }

    /// Whether `uri` belongs to this workspace: a file already indexed, or a path under one of
    /// its source roots (so a project file the editor hasn't opened yet still resolves here).
    pub(crate) fn owns_uri(&self, uri: &Url) -> bool {
        self.key(uri)
            .is_some_and(|path| self.analysis.owns_path(&path))
    }

    /// This workspace's analysis of `uri`, when it owns it.
    pub(crate) fn open(&self, uri: &Url) -> Option<OpenDocument<'_>> {
        let key = self.key(uri)?;
        self.analysis
            .owns_path(&key)
            .then(|| OpenDocument::new(key, AnalysisRef::Project(&self.analysis)))
    }

    /// Reflect an open document into the index: replace the cached copy of `uri` with the open
    /// document's current text (or add it, if `uri` is a project file created after the initial
    /// load), then rebuild the index. Returns whether `uri` belongs to this workspace.
    pub(crate) async fn set_overlay(&mut self, uri: &Url, doc: &Document) -> bool {
        let Some(path) = self.key(uri) else {
            return false;
        };
        self.analysis.set_overlay(&path, &doc.content).await
    }
}

/// Which open documents a detached group holds together.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DetachedGroup {
    /// Every open document in one host directory. Same directory is same Java package, so these
    /// are exactly the files that may refer to one another by simple name.
    Directory(PathBuf),
    /// A document with no directory at all — an unsaved buffer, whose URI is its whole identity.
    /// Each is its own group: two of them share no package and no path, so neither is evidence
    /// about the other.
    Document(Url),
}

impl DetachedGroup {
    /// The group `uri` belongs to.
    fn of(uri: &Url) -> Self {
        uri.to_file_path()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .map_or_else(|| Self::Document(uri.clone()), Self::Directory)
    }
}

/// The analysis of the open documents that belong to no `jals.toml` project.
///
/// The group's directory is **never walked**: only documents the client opened are mounted, one
/// file to one key, so this can never become an index of a whole checkout — the property
/// [`Actor::ensure_workspace_for`](crate::actor) protects for project workspaces, kept here by
/// construction rather than by policy.
///
/// The keys are synthetic, so unlike a project workspace the `Url` ↔ `FileKey` relation is a table
/// rather than a computation. The same table is handed to the rendering, which is how a location
/// inside a mounted file comes back out addressed as the URI the client opened it with — including
/// a scheme no host path can spell.
struct DetachedWorkspace {
    keys: BTreeMap<Url, FileKey>,
    /// Never rewinds, so a key is unique among this group's mounts for the group's whole life.
    next_ordinal: usize,
    analysis: Analysis<MemorySource, MemoryCache>,
}

impl DetachedWorkspace {
    /// Where a detached document is mounted. Under no source root and captured by no snapshot
    /// scope, so it collides with nothing a project declares — the same `.jals/` convention
    /// `jals lint` mounts a reported file outside the project under, and this server already
    /// mounts navigation sources under.
    const MOUNT_ROOT: &'static str = ".jals/lsp";

    async fn new() -> Self {
        Self {
            keys: BTreeMap::new(),
            next_ordinal: 0,
            analysis: Analysis::load(
                // Memory-backed: a group holds exactly the documents the client opened, so there
                // is nothing on disk to snapshot — and an unsaved buffer has no directory to
                // anchor a native aggregate to at all. The `Exec::inline()` this constructor
                // carries is right here: no I/O to overlap, over a handful of files.
                MemoryStorage::memory(CodeTree::default()),
                ProjectLayout::default(),
                // Rootless: every key this group can produce is registered in the host's table as
                // it is mounted, so the root is never consulted. Should one ever escape the table,
                // the rootless join renders no address rather than inventing one.
                LspHost::for_root(PathBuf::new()),
            )
            .await,
        }
    }

    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Every document in this group.
    fn uris(&self) -> Vec<Url> {
        self.keys.keys().cloned().collect()
    }

    /// This group's analysis of `uri`, when it holds it.
    fn open(&self, uri: &Url) -> Option<OpenDocument<'_>> {
        let key = self.keys.get(uri)?.clone();
        Some(OpenDocument::new(
            key,
            AnalysisRef::Detached(&self.analysis),
        ))
    }

    /// Mount `uri`'s document, or refresh it in place when it is already a member.
    ///
    /// `true` when the membership changed, which is what tells the caller to republish the
    /// group's other documents: a new sibling can resolve a name that was unresolved in all of
    /// them.
    async fn mount(&mut self, uri: &Url, doc: &jals_editor::Document) -> bool {
        if let Some(key) = self.keys.get(uri).cloned() {
            self.analysis.set_overlay(&key, doc).await;
            return false;
        }
        let key = self.mount_key(uri);
        // The rendering learns the address before the index can produce a location inside the
        // file, so a `definition` landing here always has a URI to come back as.
        self.analysis
            .host_mut()
            .insert_url(key.clone(), uri.clone());
        self.keys.insert(uri.clone(), key.clone());
        // Membership before content: `set_overlay` refuses a key that is under no source root and
        // in no project-source list, and a refusal here would leave the document mounted but
        // unindexed — routed to this group and answering nothing.
        self.analysis
            .set_project_sources(self.keys.values().cloned().collect());
        self.analysis.set_overlay(&key, doc).await;
        true
    }

    /// Drop `uris` from this group in one pass. `true` when any of them was a member.
    ///
    /// One rebuild for the whole batch: installing a project workspace over a directory whose
    /// documents were all detached evicts them together, and rebuilding per document would rebuild
    /// a shrinking group once per file.
    async fn forget_all(&mut self, uris: &[Url]) -> bool {
        let mut dropped = false;
        for uri in uris {
            let Some(key) = self.keys.remove(uri) else {
                continue;
            };
            self.analysis.host_mut().remove_url(&key);
            self.analysis.remove_overlay(&key);
            dropped = true;
        }
        if !dropped {
            return false;
        }
        self.analysis
            .set_project_sources(self.keys.values().cloned().collect());
        self.analysis.reload().await;
        true
    }

    /// A key under [`MOUNT_ROOT`](Self::MOUNT_ROOT) for one document.
    ///
    /// The ordinal gives every mount its own directory, so the file/directory collision an overlay
    /// write rejects cannot arise between two members however they are named — which is what lets
    /// the file name fall back freely. The name is cosmetic: the index is handed a parse and never
    /// reads it. A basename no portable [`Name`] allows, or a URI with no path at all, takes a
    /// fixed one.
    fn mount_key(&mut self, uri: &Url) -> FileKey {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let path = uri.to_file_path().ok();
        // A URI with no host path at all is an unsaved buffer; one whose basename no portable
        // `Name` allows still has a path, and only its spelling is unusable.
        let unnamed = if path.is_some() {
            "source.java"
        } else {
            "untitled.java"
        };
        let stem = path
            .as_deref()
            .and_then(Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| Name::new(*name).is_ok())
            .unwrap_or(unnamed);
        let root = DirKey::parse(&format!("{}/{ordinal}", Self::MOUNT_ROOT))
            .expect("the mount root and a decimal ordinal are portable segments");
        root.file(Name::new(stem).expect("a checked basename, or a fixed fallback"))
    }
}

/// Every open document that belongs to no `jals.toml` project, grouped so that the documents which
/// may legally see one another are indexed together.
///
/// Kept beside the project workspaces rather than among them: a project slot is identified by its
/// manifest root and drives assembly, watching, and reassembly generations off it, none of which
/// means anything for a directory that has no manifest.
#[derive(Default)]
pub(crate) struct DetachedWorkspaces {
    groups: BTreeMap<DetachedGroup, DetachedWorkspace>,
}

impl DetachedWorkspaces {
    /// The analysis of `uri`, when some group holds it.
    pub(crate) fn open(&self, uri: &Url) -> Option<OpenDocument<'_>> {
        self.groups.get(&DetachedGroup::of(uri))?.open(uri)
    }

    // The three accessors below observe grouping, which the server itself never needs to ask
    // about — routing goes through `open`. They exist so the tests can state what the grouping
    // rule *is*, which is the part of this that a reader has to be able to check.

    /// How many groups exist.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.groups.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Whether any group holds `uri`.
    #[cfg(test)]
    pub(crate) fn holds(&self, uri: &Url) -> bool {
        self.open(uri).is_some()
    }

    /// Mount `uri`'s document into its group, creating the group if this is its first document.
    ///
    /// Returns the group's *other* documents when the membership changed — each of those now
    /// analyses differently and needs republishing — and nothing when this was an edit to a
    /// document already held.
    pub(crate) async fn mount(&mut self, uri: &Url, doc: &jals_editor::Document) -> Vec<Url> {
        let group = DetachedGroup::of(uri);
        if !self.groups.contains_key(&group) {
            self.groups
                .insert(group.clone(), DetachedWorkspace::new().await);
        }
        let workspace = self
            .groups
            .get_mut(&group)
            .expect("the group was just inserted");
        if !workspace.mount(uri, doc).await {
            return Vec::new();
        }
        workspace
            .uris()
            .into_iter()
            .filter(|held| held != uri)
            .collect()
    }

    /// Drop `uris` from their groups, dropping a group that ends up holding nothing.
    ///
    /// Returns the surviving documents of every affected group: losing a sibling can unresolve a
    /// name, so they analyse differently and need republishing.
    pub(crate) async fn forget(&mut self, uris: &[Url]) -> Vec<Url> {
        let mut batched: BTreeMap<DetachedGroup, Vec<Url>> = BTreeMap::new();
        for uri in uris {
            batched
                .entry(DetachedGroup::of(uri))
                .or_default()
                .push(uri.clone());
        }
        let mut survivors = Vec::new();
        for (group, evicted) in batched {
            let Some(workspace) = self.groups.get_mut(&group) else {
                continue;
            };
            if !workspace.forget_all(&evicted).await {
                continue;
            }
            if workspace.is_empty() {
                self.groups.remove(&group);
            } else {
                survivors.extend(workspace.uris());
            }
        }
        survivors
    }
}

/// The analysis that answers for one open document, with its key already resolved.
///
/// Resolving the key is the whole difference between the two kinds of workspace — a project
/// computes it from the host path, a detached group looks it up in its mount table — so it happens
/// once, where the document is routed, and never again per query. What remains twofold below is
/// only the storage backend, which no query cares about.
pub(crate) struct OpenDocument<'a> {
    key: FileKey,
    analysis: AnalysisRef<'a>,
}

/// The two concrete analyses, which differ only in their storage backends.
enum AnalysisRef<'a> {
    Project(&'a Analysis<NativeSource, NativeCache>),
    Detached(&'a Analysis<MemorySource, MemoryCache>),
}

/// Forward one query to whichever analysis holds the document.
///
/// A macro rather than eleven hand-written two-arm matches: the arms differ in no way a reader
/// should have to check, and writing them out is exactly the shape that lets one of them drift
/// from the other — the failure this whole seam exists to remove.
macro_rules! forward {
    ($self:ident, $query:ident $(, $arg:expr)*) => {
        match $self.analysis {
            AnalysisRef::Project(analysis) => analysis.$query(&$self.key $(, $arg)*).await,
            AnalysisRef::Detached(analysis) => analysis.$query(&$self.key $(, $arg)*).await,
        }
    };
}

impl<'a> OpenDocument<'a> {
    const fn new(key: FileKey, analysis: AnalysisRef<'a>) -> Self {
        Self { key, analysis }
    }

    /// The feature set the owning project resolved — the empty set for a detached group, which
    /// has no manifest to answer for it.
    pub(crate) const fn feature_set(&self) -> FeatureSet {
        match self.analysis {
            AnalysisRef::Project(analysis) => analysis.feature_set(),
            AnalysisRef::Detached(analysis) => analysis.feature_set(),
        }
    }

    pub(crate) async fn definition(&self, position: Position) -> Option<Location> {
        forward!(self, definition, position)
    }

    pub(crate) async fn hover(&self, position: Position) -> Option<Hover> {
        forward!(self, hover, position)
    }

    pub(crate) async fn signature_help(&self, position: Position) -> Option<SignatureHelp> {
        forward!(self, signature_help, position)
    }

    pub(crate) async fn prepare_rename(&self, position: Position) -> Option<Range> {
        forward!(self, prepare_rename, position)
    }

    pub(crate) async fn semantic_tokens(&self) -> Option<SemanticTokens> {
        forward!(self, semantic_tokens)
    }

    pub(crate) async fn completions(&self, position: Position) -> Vec<CompletionItem> {
        forward!(self, completions, position)
    }

    pub(crate) async fn document_highlight(&self, position: Position) -> Vec<DocumentHighlight> {
        forward!(self, document_highlight, position)
    }

    pub(crate) async fn references(
        &self,
        position: Position,
        include_declaration: bool,
    ) -> Vec<Location> {
        forward!(self, references, position, include_declaration)
    }

    pub(crate) async fn diagnostics(&self, config: &jals_config::lint::Config) -> Vec<Diagnostic> {
        forward!(self, diagnostics, config)
    }

    pub(crate) async fn rename(&self, position: Position, new_name: &str) -> Option<WorkspaceEdit> {
        forward!(self, rename, position, new_name)
    }
}

/// Resolves a config for a document URI: the URI's parent directory is walked upward on the
/// host filesystem for `C::FILE_NAME`, and the discovered root's config file — that one file,
/// never a project snapshot — is read and parsed once, memoized per root until
/// [`clear`](Self::clear).
///
/// This adapter owns the LSP-side policy — URI → path mapping and the "never fail a request
/// over a config" fallback to `C::default()` (non-file URIs such as `untitled:`, non-UTF-8
/// paths, read/parse errors). The parse and error shape live in `jals-config`
/// ([`from_text`](jals_config::DiscoverableConfig::from_text)), shared with the CLI. The
/// filesystem probes and the read are blocking syscalls, so they run through
/// [`on_blocking_pool`], keeping the actor free to serve other commands.
#[derive(Default)]
pub(crate) struct UriConfigs<C> {
    configs: HashMap<PathBuf, C>,
}

impl<C: jals_config::DiscoverableConfig + Clone + Default> UriConfigs<C> {
    /// Discover the config for a document URI.
    pub(crate) async fn for_uri(&mut self, uri: &Url) -> C {
        let Ok(path) = uri.to_file_path() else {
            return C::default();
        };
        let Some(start) = path.parent().map(Path::to_path_buf) else {
            return C::default();
        };
        // The ancestor walk probes the host filesystem (`is_file` per directory).
        let file_name = C::FILE_NAME;
        let Some(root) = on_blocking_pool(move || {
            start
                .ancestors()
                .find(|dir| dir.join(file_name).is_file())
                .map(Path::to_path_buf)
        })
        .await
        else {
            return C::default();
        };
        if let Some(config) = self.configs.get(&root) {
            return config.clone();
        }
        let config_path = root.join(file_name);
        let Ok(text) = on_blocking_pool(move || std::fs::read_to_string(config_path)).await else {
            return C::default();
        };
        let config = FileKey::parse(C::FILE_NAME)
            .ok()
            .and_then(|key| C::from_text(&key, &text).ok())
            .unwrap_or_default();
        self.configs.insert(root, config.clone());
        config
    }

    /// Forget all memoized configs, e.g. after a config file changes on disk. Discovery
    /// reruns lazily on the next request that needs a config.
    pub(crate) fn clear(&mut self) {
        self.configs.clear();
    }

    /// Whether `uri` refers to a config file named `C::FILE_NAME` (e.g. `jalsfmt.toml`), used
    /// to invalidate the discovery caches when a watched config file changes on disk.
    pub(crate) fn is_config_file(uri: &Url) -> bool {
        uri.to_file_path()
            .is_ok_and(|path| path.file_name().is_some_and(|name| name == C::FILE_NAME))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_lsp::lsp_types::NumberOrString;
    use jals_config::fmt::Config;
    use jals_exec::block_on_inline;

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
        block_on_inline(async {
            let mut store = DocumentStore::default();
            let uri = Url::parse("file:///a/B.java").unwrap();
            store.upsert(uri.clone(), "ab\ncd".into(), 1).await;
            store
                .apply_changes(&uri, &[ranged((1, 0), (1, 2), "XYZ")], 2)
                .await;
            let doc = store.get(&uri).unwrap();
            assert_eq!(&*doc.content.text, "ab\nXYZ");
            assert_eq!(doc.version, 2);
            // A stale index (built from "ab\ncd") would clamp this to 5.
            let end = doc.content.line_index.offset(
                &doc.content.text,
                Utf16Position {
                    line: 1,
                    character: 3,
                },
            );
            assert_eq!(end, 6);
        });
    }

    #[test]
    fn store_apply_changes_ignores_unopened_document() {
        block_on_inline(async {
            let mut store = DocumentStore::default();
            let uri = Url::parse("file:///a/B.java").unwrap();
            store
                .apply_changes(&uri, &[ranged((0, 0), (0, 0), "x")], 1)
                .await;
            assert!(store.get(&uri).is_none());
        });
    }

    #[test]
    fn store_apply_changes_empty_batch_bumps_version_only() {
        block_on_inline(async {
            let mut store = DocumentStore::default();
            let uri = Url::parse("file:///a/B.java").unwrap();
            store.upsert(uri.clone(), "abc".into(), 1).await;
            let before = store.get(&uri).unwrap();
            store.apply_changes(&uri, &[], 2).await;
            let after = store.get(&uri).unwrap();
            assert_eq!(&*after.content.text, "abc");
            assert_eq!(after.version, 2);
            // The text and line index are untouched, not rebuilt.
            assert!(Arc::ptr_eq(
                &before.content.line_index,
                &after.content.line_index
            ));
        });
    }

    #[test]
    fn store_upsert_get_remove() {
        block_on_inline(async {
            let mut store = DocumentStore::default();
            let uri = Url::parse("file:///a/B.java").unwrap();
            store.upsert(uri.clone(), "class B {}".into(), 1).await;
            let doc = store.get(&uri).unwrap();
            assert_eq!(&*doc.content.text, "class B {}");
            assert_eq!(doc.version, 1);
            store.remove(&uri);
            assert!(store.get(&uri).is_none());
        });
    }

    #[test]
    fn uri_configs_non_file_uri_uses_default() {
        block_on_inline(async {
            let mut configs = UriConfigs::<Config>::default();
            let uri = Url::parse("untitled:Untitled-1").unwrap();
            assert_eq!(configs.for_uri(&uri).await, Config::default());
        });
    }

    #[test]
    fn uri_configs_clear_picks_up_config_edits() {
        // End-to-end over the real filesystem: the URI → directory mapping finds the config root,
        // its file is parsed through the shared `DiscoverableConfig::from_text`, and `clear` is
        // the LSP's watched-file invalidation hook.
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            let config_path = dir.path().join("jalsfmt.toml");
            let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

            let mut configs = UriConfigs::<Config>::default();
            std::fs::write(&config_path, "[layout]\nindent-width = 7\n").unwrap();
            assert_eq!(configs.for_uri(&uri).await.layout.indent_width, 7);

            // The cached config survives an edit on disk until the cache is cleared.
            std::fs::write(&config_path, "[layout]\nindent-width = 3\n").unwrap();
            assert_eq!(configs.for_uri(&uri).await.layout.indent_width, 7);

            configs.clear();
            assert_eq!(configs.for_uri(&uri).await.layout.indent_width, 3);
        });
    }

    #[test]
    fn uri_configs_is_config_file_matches_only_its_file_name() {
        // Built from a host path rather than spelled out: `is_config_file` answers through
        // `Url::to_file_path`, and Windows rejects a `file:///p/…` URL that names no drive.
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\p")
        } else {
            PathBuf::from("/p")
        };
        let file_uri = |name: &str| Url::from_file_path(root.join(name)).unwrap();
        let fmt = file_uri("jalsfmt.toml");
        let lint = file_uri("jalslint.toml");
        let other = file_uri("other.toml");
        let non_file = Url::parse("untitled:jalsfmt.toml").unwrap();
        assert!(UriConfigs::<Config>::is_config_file(&fmt));
        assert!(!UriConfigs::<Config>::is_config_file(&lint));
        assert!(!UriConfigs::<Config>::is_config_file(&other));
        assert!(!UriConfigs::<Config>::is_config_file(&non_file));
        assert!(UriConfigs::<jals_config::lint::Config>::is_config_file(
            &lint
        ));
        assert!(!UriConfigs::<jals_config::lint::Config>::is_config_file(
            &fmt
        ));
    }

    // ---- ProjectWorkspace: the URI ↔ path adapter over jals-editor -----------------------------
    //
    // The analysis itself (cross-file resolution, overlays, rename gating, classpath folding) is
    // covered in `jals-editor`; these tests pin the adapter — `PathBuf`/`Url` lowering in, LSP
    // payloads with the right `file://` URLs out — end to end over a real tempdir.

    /// A workspace over `dir` alone (its own source root; no classpath, libraries, or features).
    async fn load_bare(dir: &Path) -> ProjectWorkspace {
        ProjectWorkspace::bare(dir, Exec::inline()).await
    }

    #[test]
    fn workspace_resolves_definition_across_files() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("Foo.java"), "package a; class Foo { }").unwrap();
            std::fs::write(
                dir.path().join("Bar.java"),
                "package a; class Bar { Foo f; }",
            )
            .unwrap();

            let ws = load_bare(dir.path()).await;
            let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
            let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
            assert!(ws.owns_uri(&bar_uri));
            assert_eq!(ws.project_root(), dir.path());

            // The `Foo` reference in Bar.java jumps to the class declaration in Foo.java.
            let bar = "package a; class Bar { Foo f; }";
            let use_col = bar.find("Foo").unwrap() as u32;
            let loc = ws
                .open(&bar_uri)
                .expect("Bar.java is a project file")
                .definition(Position::new(0, use_col))
                .await
                .expect("Foo resolves cross-file");
            assert_eq!(loc.uri, foo_uri);

            let foo = "package a; class Foo { }";
            let decl_col = foo.find("Foo").unwrap() as u32;
            assert_eq!(loc.range.start, Position::new(0, decl_col));
            assert_eq!(loc.range.end, Position::new(0, decl_col + 3));
        });
    }

    #[test]
    fn workspace_overlay_picks_up_a_new_file() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("Bar.java"),
                "package a; class Bar { Foo f; }",
            )
            .unwrap();

            let mut ws = load_bare(dir.path()).await;
            let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
            let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
            let bar = "package a; class Bar { Foo f; }";
            let use_col = bar.find("Foo").unwrap() as u32;

            // `Foo` is unresolved before any file declares it.
            assert!(
                ws.open(&bar_uri)
                    .expect("Bar.java is a project file")
                    .definition(Position::new(0, use_col))
                    .await
                    .is_none()
            );

            // The editor opens a new Foo.java under the source root; the overlay adds it to the
            // index.
            let doc = Document::new("package a; class Foo { }".to_owned(), 1).await;
            assert!(ws.set_overlay(&foo_uri, &doc).await);
            let loc = ws
                .open(&bar_uri)
                .expect("Bar.java is a project file")
                .definition(Position::new(0, use_col))
                .await
                .expect("Foo resolves after the overlay");
            assert_eq!(loc.uri, foo_uri);

            // A file outside every source root is rejected.
            let outside = Url::parse("file:///elsewhere/X.java").unwrap();
            assert!(!ws.set_overlay(&outside, &doc).await);
        });
    }

    #[test]
    fn workspace_rename_rewrites_a_project_type_across_files() {
        block_on_inline(async {
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

            let ws = load_bare(dir.path()).await;
            let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
            let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
            let baz_uri = Url::from_file_path(dir.path().join("Baz.java")).unwrap();

            // Rename `Foo` from its declaration: the edit rewrites the declaration plus every use
            // in every file, each to the new name.
            let decl_col = "package a; class Foo { }".find("Foo").unwrap() as u32;
            let edit = ws
                .open(&foo_uri)
                .expect("Foo.java is a project file")
                .rename(Position::new(0, decl_col), "Renamed")
                .await
                .expect("Foo is a renamable project type");
            let changes = edit.changes.expect("a plain-edit workspace edit");
            assert_eq!(changes[&foo_uri].len(), 1); // the declaration
            assert_eq!(changes[&bar_uri].len(), 1);
            assert_eq!(changes[&baz_uri].len(), 2);
            assert!(changes.values().flatten().all(|e| e.new_text == "Renamed"));

            // prepareRename on the same position reports the identifier's range.
            let range = ws
                .open(&foo_uri)
                .expect("Foo.java is a project file")
                .prepare_rename(Position::new(0, decl_col))
                .await
                .expect("Foo is renamable");
            assert_eq!(range.start, Position::new(0, decl_col));
            assert_eq!(range.end, Position::new(0, decl_col + 3));
        });
    }

    /// A project workspace answers for its own files and nothing else. This is the half of the
    /// routing rule that lives here; the actor's half is choosing a detached group for whatever
    /// comes back `None`.
    #[test]
    fn a_project_workspace_opens_only_its_own_files() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("Bar.java"), "class Bar { }").unwrap();
            let ws = load_bare(dir.path()).await;

            // A file elsewhere on disk, and a non-`file://` URI (no virtual path at all).
            for uri in [
                Url::parse("file:///elsewhere/Other.java").unwrap(),
                Url::parse("untitled:Untitled-1").unwrap(),
            ] {
                assert!(!ws.owns_uri(&uri));
                assert!(ws.open(&uri).is_none(), "{uri} is not this project's file");
            }
            // Its own file does open.
            let bar = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
            assert!(ws.owns_uri(&bar));
            assert!(ws.open(&bar).is_some());
        });
    }

    #[test]
    fn classpath_types_resolve_through_the_workspace() {
        // One end-to-end smoke over the classpath plumbing (`lower_classpath` + `ProjectSpec`):
        // a compiled `Box.class` on the classpath resolves, so the project file referencing it
        // has no `cannot-resolve` diagnostic. The full classpath behavior (member resolution,
        // skeleton navigation) is covered in `jals-editor` / `jals-classpath`.
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("Main.java"),
                // `use` is declared: an undeclared name is a `cannot-resolve` finding of its own
                // now, and this asserts on the *type* half.
                "class Main { void run() { Box b = new Box(); use(b); } void use(Box b) {} }",
            )
            .unwrap();
            let box_class = jals_classfile::ClassFile::read(
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/Box.class"
                ))
                .as_slice(),
            )
            .await
            .expect("parse Box.class");

            let ws = ProjectWorkspace::load(
                dir.path().to_path_buf(),
                &[dir.path().to_path_buf()],
                std::slice::from_ref(&box_class),
                &[],
                &[],
                FeatureSet::default(),
                BTreeSet::new(),
                Exec::inline(),
            )
            .await;
            let main_uri = Url::from_file_path(dir.path().join("Main.java")).unwrap();
            let diags = ws
                .open(&main_uri)
                .expect("Main.java is a project file")
                .diagnostics(&jals_config::lint::Config::default())
                .await;
            assert!(
                !diags.iter().any(|d| {
                    d.code == Some(NumberOrString::String("cannot-resolve".to_owned()))
                }),
                "Box resolves through the classpath: {diags:?}"
            );

            // Without the classpath, the same reference cannot resolve.
            let bare = load_bare(dir.path()).await;
            let diags = bare
                .open(&main_uri)
                .expect("Main.java is a project file")
                .diagnostics(&jals_config::lint::Config::default())
                .await;
            assert!(
                diags.iter().any(|d| {
                    d.code == Some(NumberOrString::String("cannot-resolve".to_owned()))
                }),
                "without the classpath Box is unresolved: {diags:?}"
            );
        });
    }
}
