//! The host-facing facade: a [`Workspace`] paired with an [`EditorHost`], answering every query
//! directly in the host's protocol types.
//!
//! All sequencing — path → file id, position → offset, neutral query, result → protocol shape —
//! lives here, so a host's request handler is one call: `editor.hover(path, &position)`. The two
//! front-ends differ only in the [`EditorHost`] implementation they pass in.

use alloc::vec::Vec;

use jals_hir::FileId;
use jals_storage::{CacheBackend, FileKey, ProjectStorage, SourceBackend};

use crate::document::Document;
use crate::host::{EditorHost, SemanticTokensHost};
use crate::workspace::{ProjectLayout, Workspace};
use crate::{FileRange, SignatureHelpUtf16};

/// One project's [`Workspace`] driven through a host's protocol vocabulary.
pub struct Editor<S: SourceBackend, C: CacheBackend, H: EditorHost> {
    workspace: Workspace<S, C>,
    host: H,
}

impl<S: SourceBackend, C: CacheBackend, H: EditorHost> Editor<S, C, H> {
    /// Pair an already-loaded workspace with its host.
    pub const fn new(workspace: Workspace<S, C>, host: H) -> Self {
        Self { workspace, host }
    }

    /// Load a workspace over `fs` (see [`Workspace::load`]) and pair it with `host`.
    pub fn load(storage: ProjectStorage<S, C>, spec: ProjectLayout, host: H) -> Self {
        Self::new(Workspace::load(storage, spec), host)
    }

    /// The underlying workspace: identity/ownership queries (`file_id`, `owns_path`) and the
    /// neutral analysis surface.
    pub const fn workspace(&self) -> &Workspace<S, C> {
        &self.workspace
    }

    /// Mutable access for lifecycle updates: overlays, classpath, feature set.
    pub const fn workspace_mut(&mut self) -> &mut Workspace<S, C> {
        &mut self.workspace
    }

    /// The file id and cached document of `path`, when it is an indexed project file.
    fn doc(&self, path: &FileKey) -> Option<(FileId, &Document)> {
        let file = self.workspace.file_id(path)?;
        Some((file, self.workspace.document(file)?))
    }

    /// Decode a host position in `path` to `(file, byte offset)`.
    fn offset(&self, path: &FileKey, position: &H::Position) -> Option<(FileId, usize)> {
        let (file, doc) = self.doc(path)?;
        Some((file, self.host.offset(doc, position)))
    }

    /// Map a neutral cross-file target to the host's location, suppressing source-less targets
    /// (a classpath member with no extracted source).
    fn location(&self, target: &FileRange) -> Option<H::Location> {
        let path = self.workspace.path_of(target.file)?;
        let doc = self.workspace.document(target.file)?;
        Some(self.host.location(path, doc, target.range.clone()))
    }

    /// The canonical diagnostics of `path` under `config` (the project's feature set and index
    /// fold in automatically), rendered by the host.
    pub fn diagnostics(
        &self,
        path: &FileKey,
        config: &jals_config::lint::Config,
    ) -> Vec<H::Diagnostic> {
        let Some((file, doc)) = self.doc(path) else {
            return Vec::new();
        };
        self.workspace
            .diagnostics(file, config)
            .into_iter()
            .map(|diagnostic| self.host.diagnostic(doc, diagnostic))
            .collect()
    }

    /// The document outline of `path`, rendered by the host.
    pub fn outline(&self, path: &FileKey) -> Vec<H::Symbol> {
        let Some((file, doc)) = self.doc(path) else {
            return Vec::new();
        };
        self.host.render_outline(doc, self.workspace.outline(file))
    }

    /// Go-to-definition at `position` in `path`.
    pub fn definition(&self, path: &FileKey, position: &H::Position) -> Option<H::Location> {
        let (file, offset) = self.offset(path, position)?;
        let target = self.workspace.definition(file, offset)?;
        self.location(&target)
    }

    /// Find-references at `position` in `path` (project-wide for a project type).
    pub fn references(
        &self,
        path: &FileKey,
        position: &H::Position,
        include_declaration: bool,
    ) -> Vec<H::Location> {
        let Some((file, offset)) = self.offset(path, position) else {
            return Vec::new();
        };
        self.workspace
            .references(file, offset, include_declaration)
            .iter()
            .filter_map(|target| self.location(target))
            .collect()
    }

    /// The hover at `position` in `path`.
    pub fn hover(&self, path: &FileKey, position: &H::Position) -> Option<H::Hover> {
        let (file, offset) = self.offset(path, position)?;
        let markdown = self.workspace.hover_markdown(file, offset)?;
        Some(self.host.hover(markdown))
    }

    /// Completions at `position` in `path`.
    pub fn completions(&self, path: &FileKey, position: &H::Position) -> Vec<H::Completion> {
        let Some((file, offset)) = self.offset(path, position) else {
            return Vec::new();
        };
        self.workspace
            .completions(file, offset)
            .into_iter()
            .map(|completion| self.host.completion(completion))
            .collect()
    }

    /// Signature help at `position` in `path`.
    pub fn signature_help(
        &self,
        path: &FileKey,
        position: &H::Position,
    ) -> Option<H::SignatureHelp> {
        let (file, offset) = self.offset(path, position)?;
        let help = self.workspace.signature_help(file, offset)?;
        Some(self.host.signature_help(SignatureHelpUtf16::of(&help)))
    }

    /// Occurrence highlights at `position` in `path`.
    pub fn highlights(&self, path: &FileKey, position: &H::Position) -> Vec<H::Highlight> {
        let Some((file, offset)) = self.offset(path, position) else {
            return Vec::new();
        };
        let Some(doc) = self.workspace.document(file) else {
            return Vec::new();
        };
        self.workspace
            .highlights(file, offset)
            .into_iter()
            .map(|highlight| self.host.highlight(doc, highlight))
            .collect()
    }

    /// prepareRename at `position` in `path`: the identifier's host range when it names a
    /// renamable symbol.
    pub fn prepare_rename(&self, path: &FileKey, position: &H::Position) -> Option<H::Range> {
        let (file, offset) = self.offset(path, position)?;
        let range = self.workspace.prepare_rename(file, offset)?;
        let doc = self.workspace.document(file)?;
        Some(self.host.range(doc, range))
    }

    /// The locations a rename at `position` in `path` rewrites, or `None` when the symbol is not
    /// renamable / nothing would change. The host validates the new name
    /// ([`crate::Ident::is_valid_java_identifier`]) and shapes the edit.
    pub fn rename_targets(
        &self,
        path: &FileKey,
        position: &H::Position,
    ) -> Option<Vec<H::Location>> {
        let (file, offset) = self.offset(path, position)?;
        let targets = self.workspace.rename_targets(file, offset)?;
        Some(
            targets
                .iter()
                .filter_map(|target| self.location(target))
                .collect(),
        )
    }
}

impl<S: SourceBackend, C: CacheBackend, H: SemanticTokensHost> Editor<S, C, H> {
    /// The encoded semantic tokens of `path`.
    pub fn semantic_tokens(&self, path: &FileKey) -> Option<H::SemanticTokens> {
        let (file, doc) = self.doc(path)?;
        Some(
            self.host
                .semantic_tokens(doc, self.workspace.semantic_tokens(file)),
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ops::Range;

    use jals_storage::{CodeTree, DirKey, Entry, MemoryCache, MemorySource, MemoryStorage};

    use super::*;
    use crate::host::EditorHost;
    use crate::{Completion, FileDiagnostic, Highlight, LineIndex, OutlineNode, Utf16Position};

    /// A minimal test host: positions are `(line, character)` UTF-16 pairs, payloads are plain
    /// tuples/strings — enough to pin the facade's sequencing without a real protocol.
    struct TupleHost;

    impl EditorHost for TupleHost {
        type Position = (u32, u32);
        type Range = Range<usize>;
        type Location = (String, Range<usize>);
        type Diagnostic = String;
        type Symbol = (String, Vec<(String, Vec<()>)>);
        type Completion = String;
        type Highlight = (Range<usize>, bool);
        type Hover = String;
        type SignatureHelp = (u32, u32);

        fn offset(&self, doc: &Document, position: &(u32, u32)) -> usize {
            doc.line_index.offset(
                &doc.text,
                Utf16Position {
                    line: position.0,
                    character: position.1,
                },
            )
        }
        fn range(&self, _doc: &Document, range: Range<usize>) -> Range<usize> {
            range
        }
        fn location(&self, path: &FileKey, _doc: &Document, range: Range<usize>) -> Self::Location {
            (path.to_string(), range)
        }
        fn diagnostic(&self, _doc: &Document, diagnostic: FileDiagnostic) -> String {
            diagnostic.message
        }
        fn symbol(
            &self,
            _doc: &Document,
            node: OutlineNode,
            children: Vec<Self::Symbol>,
        ) -> Self::Symbol {
            (
                node.name,
                children.into_iter().map(|(n, _)| (n, vec![])).collect(),
            )
        }
        fn completion(&self, completion: Completion) -> String {
            completion.label
        }
        fn highlight(&self, _doc: &Document, highlight: Highlight) -> (Range<usize>, bool) {
            (
                highlight.range,
                highlight.kind == crate::HighlightKind::Write,
            )
        }
        fn hover(&self, markdown: String) -> String {
            markdown
        }
        fn signature_help(&self, help: SignatureHelpUtf16) -> (u32, u32) {
            (help.active_signature, help.active_parameter)
        }
    }

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

    fn sample_editor() -> Editor<MemorySource, MemoryCache, TupleHost> {
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
        Editor::load(
            storage,
            ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
            TupleHost,
        )
    }

    /// The `(line, character)` of `needle`'s first occurrence in `text` (single-line samples).
    fn pos_of(text: &str, needle: &str) -> (u32, u32) {
        let index = LineIndex::new(text);
        let position = index.position(text, text.find(needle).unwrap());
        (position.line, position.character)
    }

    #[test]
    fn the_facade_sequences_path_position_query_and_rendering() {
        let editor = sample_editor();
        let main = "public class Main { void run() { Greeter g = new Greeter(); } }";

        // Definition: host position in, host location out, path already mapped.
        let (path, _) = editor
            .definition(&key("src/Main.java"), &pos_of(main, "Greeter g"))
            .expect("definition");
        assert_eq!(path, "src/Greeter.java");

        // Hover renders through the host.
        let hover = editor
            .hover(&key("src/Main.java"), &pos_of(main, "new Greeter"))
            .expect("hover");
        assert!(hover.contains("```java"), "{hover}");

        // Outline renders bottom-up with children.
        let outline = editor.outline(&key("src/Greeter.java"));
        assert_eq!(outline[0].0, "Greeter");
        assert_eq!(outline[0].1[0].0, "greet");

        // References map every target through the host.
        let refs = editor.references(&key("src/Main.java"), &pos_of(main, "Greeter g"), true);
        assert!(refs.iter().any(|(p, _)| p == "src/Greeter.java"));

        // An unknown path answers empty, never panicking.
        assert!(editor.outline(&key("nowhere.java")).is_empty());
        assert!(editor.definition(&key("nowhere.java"), &(0, 0)).is_none());
    }

    #[test]
    fn diagnostics_render_through_the_host() {
        let storage = memory(&[(
            "src/Main.java",
            "class Main { void run() { int unused = 1; } }",
        )]);
        let editor = Editor::load(
            storage,
            ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
            TupleHost,
        );
        let diags =
            editor.diagnostics(&key("src/Main.java"), &jals_config::lint::Config::default());
        assert!(
            diags.iter().any(|message| message.contains("unused")),
            "{diags:?}"
        );
    }

    #[test]
    fn signature_help_arrives_in_utf16() {
        let text = "class C { void f(int 値, int b) {} void g() { f(1, ); } }";
        let storage = memory(&[("src/C.java", text)]);
        let editor = Editor::load(
            storage,
            ProjectLayout::new(vec![DirKey::parse("src").unwrap()]),
            TupleHost,
        );
        let index = LineIndex::new(text);
        let offset = text.find("f(1, ").unwrap() + "f(1, ".len();
        let position = index.position(text, offset);
        let (active_signature, active_parameter) = editor
            .signature_help(&key("src/C.java"), &(position.line, position.character))
            .expect("signature help");
        assert_eq!((active_signature, active_parameter), (0, 1));
    }

    #[test]
    fn rename_targets_gate_and_map() {
        let editor = sample_editor();
        let main = "public class Main { void run() { Greeter g = new Greeter(); } }";
        let targets = editor
            .rename_targets(&key("src/Main.java"), &pos_of(main, "Greeter g"))
            .expect("renamable");
        assert!(targets.len() >= 3, "{targets:?}");
        // `String` is a stdlib name: not renamable.
        let greeter = "public class Greeter { public String greet(String name) { return name; } }";
        assert!(
            editor
                .rename_targets(&key("src/Greeter.java"), &pos_of(greeter, "String name"))
                .is_none()
        );
    }
}
