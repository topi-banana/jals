//! In-memory server state: open documents, the per-project workspace adapter, and memoized
//! config discovery.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use async_lsp::lsp_types::{
    CompletionItem, Diagnostic, DocumentHighlight, Hover, Location, Position, Range,
    SemanticTokens, SignatureHelp, TextDocumentContentChangeEvent, Url, WorkspaceEdit,
};
use jals_config::FeatureSet;
use jals_editor::{Editor, ProjectLayout, Utf16Position};
use jals_hir::ProjectIndex;
use jals_storage::{
    DirKey, FileKey, NativeCache, NativeScope, NativeSource, NativeStorage, RelativePath,
};

use crate::host::LspHost;

/// An open document: the shared per-file caches (text, coordinate map, parsed CST) plus the
/// client's version.
///
/// The content lives in a [`jals_editor::Document`], whose fields are behind `Arc` so a snapshot
/// can be cheaply cloned out of the store and moved into an async request handler — and shared
/// with the owning workspace's overlay without reparsing.
#[derive(Clone)]
pub(crate) struct Document {
    pub(crate) content: jals_editor::Document,
    pub(crate) version: i32,
}

impl Document {
    fn new(text: String, version: i32) -> Self {
        Self {
            content: jals_editor::Document::new(text),
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
        *doc = Document::new(
            Self::apply_content_changes(&doc.content.text, changes),
            version,
        );
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
    /// event, so a fresh [`jals_editor::LineIndex`] is built per ranged event. An event without a
    /// range replaces the whole document. Reversed ranges are normalized and out-of-range
    /// positions are clamped by the index's `offset`, so this never panics.
    pub(crate) fn apply_content_changes(
        text: &str,
        changes: &[TextDocumentContentChangeEvent],
    ) -> String {
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

/// One `jals.toml` project's analysis: the protocol-neutral [`jals_editor::Workspace`] (driven
/// through the [`Editor`] facade with the [`LspHost`] rendering) plus the URI ↔ virtual-path
/// mapping that is the LSP's only remaining responsibility.
///
/// The server holds one of these per project a client has a file open in (see
/// [`ServerState`](crate::server)), discovered lazily by walking up from each opened file — so it
/// only ever indexes the source roots of a real manifest, never a whole git checkout.
pub(crate) struct ProjectWorkspace {
    /// The `jals.toml` directory this workspace was discovered from; identifies the workspace so
    /// a later open in the same project reuses it instead of building a duplicate.
    project_root: PathBuf,
    /// The neutral workspace paired with the LSP rendering; owns all analysis state.
    editor: Editor<NativeSource, NativeCache, LspHost>,
}

impl ProjectWorkspace {
    /// Load a project workspace off the host filesystem: walk `source_roots` for `.java`, fold
    /// the already-parsed classpath `.class` files into the index, register the library /
    /// source-dependency `.java`, and resolve `feature_set` into every lint run — all inside
    /// [`jals_editor::Workspace`]. The caller (the server) resolves the manifest and performs the
    /// dependency I/O; this keeps only the `PathBuf` → virtual-path lowering.
    pub(crate) fn load(
        project_root: PathBuf,
        source_roots: &[PathBuf],
        classfiles: &[jals_classfile::ClassFile],
        library_sources: &[PathBuf],
        source_dep_sources: &[PathBuf],
        feature_set: FeatureSet,
    ) -> Self {
        let scopes = source_roots.iter().filter_map(|path| {
            RelativePath::from_host_path(&project_root, path)
                .map(|relative| NativeScope::extension(relative, "java"))
        });
        let storage = NativeStorage::for_project_scoped(&project_root, scopes)
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
            classfiles,
            library_sources,
            source_dep_sources,
            BTreeMap::new(),
            feature_set,
        )
    }

    /// A workspace over `root` alone — its own lone source root; no classpath, libraries, or
    /// features. The fallback when a manifest is missing, unparsable, or its inputs fail to
    /// assemble.
    pub(crate) fn bare(root: &Path) -> Self {
        let root = root.to_path_buf();
        Self::load(
            root.clone(),
            std::slice::from_ref(&root),
            &[],
            &[],
            &[],
            FeatureSet::default(),
        )
    }

    /// Construct from an already-open aggregate after dependency assembly. The same storage owns
    /// source revision, overlays, and artifact cache for the workspace lifetime. `materialized`
    /// maps mounted `.jals/…` navigation sources to the real files materialized out of the
    /// artifact cache, so their locations are rendered as openable `file://` URLs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn load_storage(
        project_root: PathBuf,
        storage: NativeStorage,
        source_roots: Vec<DirKey>,
        classfiles: &[jals_classfile::ClassFile],
        library_sources: Vec<FileKey>,
        source_dep_sources: Vec<FileKey>,
        materialized: BTreeMap<FileKey, PathBuf>,
        feature_set: FeatureSet,
    ) -> Self {
        let spec = ProjectLayout {
            source_roots,
            classpath: ProjectIndex::lower_classpath(classfiles),
            library_sources,
            source_dep_sources,
            feature_set,
        };
        let host = LspHost::for_root(project_root.clone()).with_materialized(materialized);
        Self {
            project_root,
            editor: Editor::load(storage, spec, host),
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

    /// The virtual path of `uri` when it addresses an *indexed* file of this workspace, so a
    /// query wrapper can answer `None` (and the server fall back) for anything else.
    fn indexed_path(&self, uri: &Url) -> Option<FileKey> {
        let path = self.key(uri)?;
        self.editor.workspace().file_id(&path)?;
        Some(path)
    }

    /// The `jals.toml` project root this workspace was loaded from.
    pub(crate) fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub(crate) fn refresh(&mut self) {
        let _ = self.editor.workspace_mut().refresh();
    }

    /// Whether `uri` belongs to this workspace: a file already indexed, or a path under one of
    /// its source roots (so a project file the editor hasn't opened yet still resolves here).
    pub(crate) fn owns_uri(&self, uri: &Url) -> bool {
        self.key(uri)
            .is_some_and(|path| self.editor.workspace().owns_path(&path))
    }

    /// Reflect an open document into the index: replace the cached copy of `uri` with the
    /// editor's current text (or add it, if `uri` is a project file created after the initial
    /// load), then rebuild the index. Returns whether `uri` belongs to this workspace.
    pub(crate) fn set_overlay(&mut self, uri: &Url, doc: &Document) -> bool {
        self.key(uri).is_some_and(|path| {
            self.editor
                .workspace_mut()
                .set_overlay(&path, &doc.content)
                .unwrap_or(false)
        })
    }

    /// Go-to-definition for the cursor at `position` in `uri`. `None` if `uri` is not in the
    /// workspace or nothing resolves.
    pub(crate) fn goto_definition(&self, uri: &Url, position: Position) -> Option<Location> {
        self.editor.definition(&self.key(uri)?, &position)
    }

    /// The hover for the cursor at `position` in `uri`. `None` if `uri` is not in the workspace
    /// or the expression has no inferred type.
    pub(crate) fn hover(&self, uri: &Url, position: Position) -> Option<Hover> {
        self.editor.hover(&self.key(uri)?, &position)
    }

    /// The signature help for the call at `position` in `uri`, with cross-file type resolution.
    /// `None` if `uri` is not in the workspace or the cursor is in no resolvable call.
    pub(crate) fn signature_help(&self, uri: &Url, position: Position) -> Option<SignatureHelp> {
        self.editor.signature_help(&self.key(uri)?, &position)
    }

    /// Completions for the cursor at `position` in `uri`, resolved against the project. `None` if
    /// `uri` is not in the workspace (the server then falls back to the one-file project).
    pub(crate) fn completions(&self, uri: &Url, position: Position) -> Option<Vec<CompletionItem>> {
        Some(self.editor.completions(&self.indexed_path(uri)?, &position))
    }

    /// Occurrence highlights for the cursor at `position` in `uri`, resolved against the project
    /// so a cross-file type name highlights precisely. `None` if `uri` is not in the workspace.
    pub(crate) fn document_highlight(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<Vec<DocumentHighlight>> {
        Some(self.editor.highlights(&self.indexed_path(uri)?, &position))
    }

    /// Semantic tokens for `uri`, resolved against the project so a cross-file type name is
    /// classified by its declared kind. `None` if `uri` is not in the workspace.
    pub(crate) fn semantic_tokens(&self, uri: &Url) -> Option<SemanticTokens> {
        self.editor.semantic_tokens(&self.key(uri)?)
    }

    /// Find-references for the cursor at `position` in `uri` — project-wide for a project type,
    /// within the file for a file-local binding. `None` if `uri` is not in the workspace; an
    /// empty vector if the cursor is on no resolvable symbol.
    pub(crate) fn references(
        &self,
        uri: &Url,
        position: Position,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        Some(
            self.editor
                .references(&self.indexed_path(uri)?, &position, include_declaration),
        )
    }

    /// prepareRename for the cursor at `position` in `uri`: the range of the identifier under the
    /// cursor when it names a renamable symbol, else `None` (an external name, a keyword/literal,
    /// or a withheld member).
    pub(crate) fn prepare_rename(&self, uri: &Url, position: Position) -> Option<Range> {
        self.editor.prepare_rename(&self.key(uri)?, &position)
    }

    /// Rename the symbol under `position` in `uri` to `new_name`: a [`WorkspaceEdit`] rewriting
    /// every occurrence. `None` if `uri` is not in the workspace, the cursor is on no renamable
    /// symbol, or there is nothing to change. The caller validates `new_name` is a legal
    /// identifier.
    pub(crate) fn rename(
        &self,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        let targets = self.editor.rename_targets(&self.key(uri)?, &position)?;
        LspHost::workspace_edit(targets, new_name)
    }

    /// The canonical diagnostics of `uri` under `config`, with the project index and the
    /// project's resolved feature set folded in. `None` if `uri` is not an indexed file of this
    /// workspace (the server then falls back to the one-file project).
    pub(crate) fn diagnostics(
        &self,
        uri: &Url,
        config: &jals_config::lint::Config,
    ) -> Option<Vec<Diagnostic>> {
        Some(self.editor.diagnostics(&self.indexed_path(uri)?, config))
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
/// ([`from_text`](jals_config::DiscoverableConfig::from_text)), shared with the CLI.
#[derive(Default)]
pub(crate) struct UriConfigs<C> {
    configs: HashMap<PathBuf, C>,
}

impl<C: jals_config::DiscoverableConfig + Clone + Default> UriConfigs<C> {
    /// Discover the config for a document URI.
    pub(crate) fn for_uri(&mut self, uri: &Url) -> C {
        let Ok(path) = uri.to_file_path() else {
            return C::default();
        };
        let Some(start) = path.parent() else {
            return C::default();
        };
        let Some(root) = start
            .ancestors()
            .find(|dir| dir.join(C::FILE_NAME).is_file())
        else {
            return C::default();
        };
        if let Some(config) = self.configs.get(root) {
            return config.clone();
        }
        let Ok(text) = std::fs::read_to_string(root.join(C::FILE_NAME)) else {
            return C::default();
        };
        let config = FileKey::parse(C::FILE_NAME)
            .ok()
            .and_then(|key| C::from_text(&key, &text).ok())
            .unwrap_or_default();
        self.configs.insert(root.to_path_buf(), config.clone());
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
        assert_eq!(&*after.content.text, "abc");
        assert_eq!(after.version, 2);
        // The text and line index are untouched, not rebuilt.
        assert!(Arc::ptr_eq(
            &before.content.line_index,
            &after.content.line_index
        ));
    }

    #[test]
    fn store_upsert_get_remove() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "class B {}".into(), 1);
        let doc = store.get(&uri).unwrap();
        assert_eq!(&*doc.content.text, "class B {}");
        assert_eq!(doc.version, 1);
        store.remove(&uri);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn uri_configs_non_file_uri_uses_default() {
        let mut configs = UriConfigs::<Config>::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(configs.for_uri(&uri), Config::default());
    }

    #[test]
    fn uri_configs_clear_picks_up_config_edits() {
        // End-to-end over the real filesystem: the URI → directory mapping finds the config root,
        // its file is parsed through the shared `DiscoverableConfig::from_text`, and `clear` is
        // the LSP's watched-file invalidation hook.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("jalsfmt.toml");
        let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

        let mut configs = UriConfigs::<Config>::default();
        std::fs::write(&config_path, "indent-width = 7\n").unwrap();
        assert_eq!(configs.for_uri(&uri).indent_width, 7);

        // The cached config survives an edit on disk until the cache is cleared.
        std::fs::write(&config_path, "indent-width = 3\n").unwrap();
        assert_eq!(configs.for_uri(&uri).indent_width, 7);

        configs.clear();
        assert_eq!(configs.for_uri(&uri).indent_width, 3);
    }

    #[test]
    fn uri_configs_is_config_file_matches_only_its_file_name() {
        let fmt = Url::parse("file:///p/jalsfmt.toml").unwrap();
        let lint = Url::parse("file:///p/jalslint.toml").unwrap();
        let other = Url::parse("file:///p/other.toml").unwrap();
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
    fn load_bare(dir: &Path) -> ProjectWorkspace {
        ProjectWorkspace::load(
            dir.to_path_buf(),
            &[dir.to_path_buf()],
            &[],
            &[],
            &[],
            FeatureSet::default(),
        )
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

        let ws = load_bare(dir.path());
        let bar_uri = Url::from_file_path(dir.path().join("Bar.java")).unwrap();
        let foo_uri = Url::from_file_path(dir.path().join("Foo.java")).unwrap();
        assert!(ws.owns_uri(&bar_uri));
        assert_eq!(ws.project_root(), dir.path());

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
    fn workspace_overlay_picks_up_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Bar.java"),
            "package a; class Bar { Foo f; }",
        )
        .unwrap();

        let mut ws = load_bare(dir.path());
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

        // A file outside every source root is rejected.
        let outside = Url::parse("file:///elsewhere/X.java").unwrap();
        assert!(!ws.set_overlay(&outside, &doc));
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

        let ws = load_bare(dir.path());
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
    fn workspace_queries_answer_none_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Bar.java"), "class Bar { }").unwrap();
        let ws = load_bare(dir.path());

        // A file elsewhere on disk, and a non-`file://` URI (no virtual path at all): every
        // wrapper answers `None`, so the server falls back to the one-file project.
        for uri in [
            Url::parse("file:///elsewhere/Other.java").unwrap(),
            Url::parse("untitled:Untitled-1").unwrap(),
        ] {
            assert!(!ws.owns_uri(&uri));
            assert!(ws.goto_definition(&uri, Position::new(0, 0)).is_none());
            assert!(ws.hover(&uri, Position::new(0, 0)).is_none());
            assert!(ws.completions(&uri, Position::new(0, 0)).is_none());
            assert!(ws.references(&uri, Position::new(0, 0), true).is_none());
            assert!(ws.semantic_tokens(&uri).is_none());
            assert!(
                ws.diagnostics(&uri, &jals_config::lint::Config::default())
                    .is_none()
            );
        }
    }

    #[test]
    fn classpath_types_resolve_through_the_workspace() {
        // One end-to-end smoke over the classpath plumbing (`lower_classpath` + `ProjectSpec`):
        // a compiled `Box.class` on the classpath resolves, so the project file referencing it
        // has no `cannot-resolve` diagnostic. The full classpath behavior (member resolution,
        // skeleton navigation) is covered in `jals-editor` / `jals-classpath`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Main.java"),
            "class Main { void run() { Box b = new Box(); use(b); } }",
        )
        .unwrap();
        let box_class = jals_classfile::ClassFile::read(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/Box.class"
        )))
        .expect("parse Box.class");

        let ws = ProjectWorkspace::load(
            dir.path().to_path_buf(),
            &[dir.path().to_path_buf()],
            std::slice::from_ref(&box_class),
            &[],
            &[],
            FeatureSet::default(),
        );
        let main_uri = Url::from_file_path(dir.path().join("Main.java")).unwrap();
        let diags = ws
            .diagnostics(&main_uri, &jals_config::lint::Config::default())
            .expect("Main.java is indexed");
        assert!(
            !diags
                .iter()
                .any(|d| { d.code == Some(NumberOrString::String("cannot-resolve".to_owned())) }),
            "Box resolves through the classpath: {diags:?}"
        );

        // Without the classpath, the same reference cannot resolve.
        let bare = load_bare(dir.path());
        let diags = bare
            .diagnostics(&main_uri, &jals_config::lint::Config::default())
            .expect("Main.java is indexed");
        assert!(
            diags
                .iter()
                .any(|d| { d.code == Some(NumberOrString::String("cannot-resolve".to_owned())) }),
            "without the classpath Box is unresolved: {diags:?}"
        );
    }
}
