//! In-memory server state: open documents and memoized config discovery.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_lsp::lsp_types::{
    Hover, Location, Position, SignatureHelp, TextDocumentContentChangeEvent, TextEdit, Url,
    WorkspaceEdit,
};
use jals_fmt::Config;
use jals_hir::{FileId, ItemId, Namespace, ProjectIndex, Resolution, Resolved, TypeResolution};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{Parse, SyntaxKind, SyntaxNode};
use walkdir::WalkDir;

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
    fn new(text: String, version: i32) -> Document {
        let line_index = Arc::new(LineIndex::new(&text));
        let parse = Arc::new(jals_syntax::parse(&text));
        Document {
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
        *doc = Document::new(apply_content_changes(&doc.text, changes), version);
    }

    /// Snapshot the document for `uri` (cheap `Arc` clones), if open.
    pub(crate) fn get(&self, uri: &Url) -> Option<Document> {
        self.docs.get(uri).cloned()
    }

    pub(crate) fn remove(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }
}

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
            text = change.text.clone();
            continue;
        };
        let index = LineIndex::new(&text);
        let start = u32::from(index.offset(&text, range.start)) as usize;
        let end = u32::from(index.offset(&text, range.end)) as usize;
        text.replace_range(start.min(end)..start.max(end), &change.text);
    }
    text
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
}

impl WorkspaceFile {
    fn new(uri: Url, text: String) -> WorkspaceFile {
        let line_index = Arc::new(LineIndex::new(&text));
        let parse = Arc::new(jals_syntax::parse(&text));
        WorkspaceFile {
            uri,
            text: Arc::from(text),
            line_index,
            parse,
            resolved: OnceLock::new(),
        }
    }

    /// The file's cached name resolution (computed on first use).
    fn resolved(&self) -> &Resolved {
        self.resolved
            .get_or_init(|| jals_hir::resolve_node(&self.parse.syntax()))
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
/// The host owns all I/O: [`load`](Workspace::load) walks the source roots and reads every `.java`
/// file, then hands the parsed trees to the pure [`ProjectIndex`]. Open documents are kept current
/// via [`set_overlay`](Workspace::set_overlay), which swaps a file's cached text for the editor's
/// and rebuilds the (in-memory, no-I/O) index. The rebuild walks the cached trees of every file, so
/// it is cheap per edit but linear in project size — adequate until an incremental index is needed.
pub(crate) struct Workspace {
    /// The `jals.toml` directory this workspace was discovered from; identifies the workspace so a
    /// later open in the same project reuses it instead of building a duplicate.
    project_root: PathBuf,
    source_roots: Vec<PathBuf>,
    files: Vec<WorkspaceFile>,
    by_uri: HashMap<Url, FileId>,
    index: ProjectIndex,
}

impl Workspace {
    /// Walk `source_roots`, parse every `.java` file found (skipping unreadable ones), and build
    /// the symbol index. `project_root` is the `jals.toml` directory this workspace was discovered
    /// from; it identifies the workspace so a later open in the same project reuses it. Paths are
    /// visited in sorted order so the index is deterministic.
    pub(crate) fn load(project_root: PathBuf, source_roots: Vec<PathBuf>) -> Workspace {
        let mut paths: Vec<PathBuf> = source_roots
            .iter()
            .flat_map(|root| WalkDir::new(root).into_iter().filter_map(Result::ok))
            .map(walkdir::DirEntry::into_path)
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "java"))
            .collect();
        paths.sort();
        paths.dedup();

        let mut ws = Workspace {
            project_root,
            source_roots,
            files: Vec::new(),
            by_uri: HashMap::new(),
            index: ProjectIndex::build(&[]),
        };
        for path in paths {
            if let (Ok(text), Ok(uri)) =
                (std::fs::read_to_string(&path), Url::from_file_path(&path))
                && !ws.by_uri.contains_key(&uri)
            {
                let id = FileId(ws.files.len() as u32);
                ws.by_uri.insert(uri.clone(), id);
                ws.files.push(WorkspaceFile::new(uri, text));
            }
        }
        ws.rebuild_index();
        ws
    }

    /// Rebuild the symbol index from the cached parses. No I/O.
    fn rebuild_index(&mut self) {
        let inputs: Vec<(FileId, SyntaxNode)> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f.parse.syntax()))
            .collect();
        self.index = ProjectIndex::build(&inputs);
    }

    /// The project symbol index.
    pub(crate) fn index(&self) -> &ProjectIndex {
        &self.index
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
        };
        match self.by_uri.get(uri).copied() {
            Some(id) => self.files[id.0 as usize] = file,
            None => {
                if !self.under_source_root(uri) {
                    return false;
                }
                let id = FileId(self.files.len() as u32);
                self.by_uri.insert(uri.clone(), id);
                self.files.push(file);
            }
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
            return Some(self.files[target_file.0 as usize].location(&range));
        }
        // A member access (`obj.field` / `recv.method()`): infer the receiver and resolve the member.
        let (target_file, range) = self.member_definition(file, &root, resolved, offset)?;
        Some(self.files[target_file.0 as usize].location(&range))
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
        let token = crate::handlers::ident_at(root, offset)?;
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
        let inference = jals_hir::infer(root, resolved, &self.index, file);
        let span = receiver.syntax().text_range();
        let owner = inference
            .type_of_expr(usize::from(span.start())..usize::from(span.end()))?
            .project_id()?;
        let member = self
            .index
            .member(self.index.resolve_member(owner, &name, namespace)?);
        Some((member.file, member.name_range.clone()))
    }

    /// The hover for the cursor at `position` in `uri`: the inferred type of the expression there,
    /// with reference type names resolved against the project. `None` if `uri` is not in the
    /// workspace or the expression has no inferred type.
    pub(crate) fn hover(&self, uri: &Url, position: Position) -> Option<Hover> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let resolved = source.resolved();
        let inference = jals_hir::infer(&root, resolved, &self.index, file);
        let offset = u32::from(source.line_index.offset(&source.text, position)) as usize;
        crate::handlers::type_hover(inference.type_at(offset)?)
    }

    /// The signature help for the call at `position` in `uri`, with cross-file type resolution (so a
    /// receiver of a sibling-file type resolves). `None` if `uri` is not in the workspace or the
    /// cursor is in no resolvable call.
    pub(crate) fn signature_help(&self, uri: &Url, position: Position) -> Option<SignatureHelp> {
        let file = self.file_id(uri)?;
        let source = &self.files[file.0 as usize];
        let root = source.parse.syntax();
        let offset = u32::from(source.line_index.offset(&source.text, position)) as usize;
        let help = jals_hir::signature_help(&root, source.resolved(), &self.index, file, offset)?;
        Some(crate::handlers::signature_help_to_lsp(&help))
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
        let Some(ident) =
            crate::handlers::ident_at(&root, source.line_index.offset(&source.text, position))
        else {
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
            let file = FileId(i as u32);
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
            let decl_source = &self.files[decl.file.0 as usize];
            locations.push(decl_source.location(&decl.name_range));
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
    /// [`is_renamable_kind`](crate::handlers::is_renamable_kind)); a cross-file *use* of a project
    /// type (one the file-local pass left unresolved) qualifies too, since the workspace rewrites it
    /// project-wide. Mirrors what [`references`](Workspace::references) actually gathers, so a
    /// renamable symbol always has a complete occurrence set.
    fn is_renamable(&self, file: FileId, resolved: &Resolved, anchor: usize) -> bool {
        if let Some(id) = resolved.symbol_at(anchor) {
            return crate::handlers::is_renamable_kind(resolved.def(id).kind);
        }
        resolved.reference_at(anchor).is_some_and(|reference| {
            reference.namespace == Namespace::Type
                && matches!(
                    self.index.resolve_reference(file, reference),
                    TypeResolution::Project(_)
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
        let ident =
            crate::handlers::ident_at(&root, source.line_index.offset(&source.text, position))?;
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
        // Gate on the same renamability check `prepareRename` performs, then rewrite every
        // occurrence the find-references pass gathers.
        self.prepare_rename(uri, position)?;
        workspace_edit(self.references(uri, position, true)?, new_name)
    }
}

/// Group `locations` into a [`WorkspaceEdit`] that rewrites each occurrence to `new_name`, keyed by
/// file. `None` if there is nothing to rewrite.
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

/// A config the LSP discovers by walking up from a document's directory to a well-known TOML
/// file. Implemented for both `jals_fmt::Config` and `jals_lint::Config` so one [`Discovery`]
/// cache serves the formatter and the linter alike.
pub(crate) trait DiscoverableConfig: Clone + Default {
    /// The config file name searched for (e.g. `jalsfmt.toml`).
    const FILE_NAME: &'static str;
    /// Discover the config from `dir` upward, falling back to the default on any error.
    fn discover_or_default(dir: &Path) -> Self;
}

impl DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalsfmt.toml";
    fn discover_or_default(dir: &Path) -> Self {
        Config::discover(dir).unwrap_or_default()
    }
}

impl DiscoverableConfig for jals_lint::Config {
    const FILE_NAME: &'static str = "jalslint.toml";
    fn discover_or_default(dir: &Path) -> Self {
        jals_lint::Config::discover(dir).unwrap_or_default()
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

/// Whether `uri` refers to a config file named `C::FILE_NAME` (e.g. `jalsfmt.toml`).
fn is_config_file_for<C: DiscoverableConfig>(uri: &Url) -> bool {
    uri.to_file_path()
        .is_ok_and(|path| path.file_name().is_some_and(|name| name == C::FILE_NAME))
}

/// Whether a watched-file URI refers to a `jalsfmt.toml` config file.
pub(crate) fn is_config_file(uri: &Url) -> bool {
    is_config_file_for::<Config>(uri)
}

/// Whether a watched-file URI refers to a `jalslint.toml` config file.
pub(crate) fn is_lint_config_file(uri: &Url) -> bool {
    is_config_file_for::<jals_lint::Config>(uri)
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{HoverContents, Position, Range};

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
        let out = apply_content_changes("class A {}", &[ranged((0, 9), (0, 9), "int x;")]);
        assert_eq!(out, "class A {int x;}");
    }

    #[test]
    fn apply_single_delete() {
        let out = apply_content_changes("abcdef", &[ranged((0, 1), (0, 4), "")]);
        assert_eq!(out, "aef");
    }

    #[test]
    fn apply_single_replace() {
        let out = apply_content_changes("abc", &[ranged((0, 1), (0, 2), "XY")]);
        assert_eq!(out, "aXYc");
    }

    #[test]
    fn apply_batch_uses_post_edit_coordinates() {
        // The second event's range is only meaningful against "aXYb", the state
        // after the first event: (0,2)..(0,3) deletes the "Y".
        let changes = [ranged((0, 1), (0, 1), "XY"), ranged((0, 2), (0, 3), "")];
        assert_eq!(apply_content_changes("ab", &changes), "aXb");
    }

    #[test]
    fn apply_counts_utf16_columns() {
        // '😀' = 4 UTF-8 bytes, 2 UTF-16 units, so 'y' starts at character 3.
        let out = apply_content_changes("x😀y", &[ranged((0, 1), (0, 3), "Z")]);
        assert_eq!(out, "xZy");
        let out = apply_content_changes("x😀y", &[ranged((0, 3), (0, 3), "!")]);
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
        assert_eq!(apply_content_changes("abc", &changes), "Anew");
    }

    #[test]
    fn apply_reversed_range_is_normalized() {
        let out = apply_content_changes("abcde", &[ranged((0, 3), (0, 1), "X")]);
        assert_eq!(out, "aXde");
    }

    #[test]
    fn apply_newline_insert_then_edit_new_line() {
        // After the first event the document has two lines; the second event
        // addresses the freshly created line 1.
        let changes = [ranged((0, 2), (0, 2), "\n"), ranged((1, 1), (1, 1), "X")];
        assert_eq!(apply_content_changes("abcd", &changes), "ab\ncXd");
    }

    #[test]
    fn apply_delete_spanning_newline_joins_lines() {
        let out = apply_content_changes("ab\ncd", &[ranged((0, 2), (1, 0), "")]);
        assert_eq!(out, "abcd");
    }

    #[test]
    fn apply_range_past_eof_clamps_to_append() {
        let out = apply_content_changes("ab", &[ranged((5, 0), (5, 0), "!")]);
        assert_eq!(out, "ab!");
    }

    #[test]
    fn apply_empty_changes_keeps_text() {
        assert_eq!(apply_content_changes("abc", &[]), "abc");
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
        assert!(is_config_file(&config));
        let other = Url::parse("file:///p/other.toml").unwrap();
        assert!(!is_config_file(&other));
        let non_file = Url::parse("untitled:jalsfmt.toml").unwrap();
        assert!(!is_config_file(&non_file));
    }

    #[test]
    fn lint_discovery_non_file_uri_uses_default() {
        let mut discovery = Discovery::<jals_lint::Config>::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(discovery.for_uri(&uri), jals_lint::Config::default());
    }

    #[test]
    fn lint_discovery_clear_picks_up_config_edits() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("jalslint.toml");
        let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

        let mut discovery = Discovery::<jals_lint::Config>::default();
        // The resolved severity of `wildcard-import` under the on-disk config.
        let wildcard = |d: &mut Discovery<jals_lint::Config>| {
            d.for_uri(&uri).rules.get("wildcard-import").copied()
        };

        std::fs::write(&config_path, "[rules]\nwildcard-import = \"allow\"\n").unwrap();
        assert_eq!(wildcard(&mut discovery), Some(jals_lint::Severity::Allow));

        // The cached config survives an edit on disk until the cache is cleared.
        std::fs::write(&config_path, "[rules]\nwildcard-import = \"error\"\n").unwrap();
        assert_eq!(wildcard(&mut discovery), Some(jals_lint::Severity::Allow));

        discovery.clear();
        assert_eq!(wildcard(&mut discovery), Some(jals_lint::Severity::Error));
    }

    #[test]
    fn is_lint_config_file_matches_only_jalslint_toml() {
        let config = Url::parse("file:///p/jalslint.toml").unwrap();
        assert!(is_lint_config_file(&config));
        let other = Url::parse("file:///p/jalsfmt.toml").unwrap();
        assert!(!is_lint_config_file(&other));
        let non_file = Url::parse("untitled:jalslint.toml").unwrap();
        assert!(!is_lint_config_file(&non_file));
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
        let doc = Document::new("package a; class Foo { }".to_string(), 1);
        assert!(ws.set_overlay(&foo_uri, &doc));
        let loc = ws
            .goto_definition(&bar_uri, Position::new(0, use_col))
            .expect("Foo resolves after the overlay");
        assert_eq!(loc.uri, foo_uri);
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

        // An external type has no project declaration to rewrite.
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
}
