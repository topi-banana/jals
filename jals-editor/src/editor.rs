//! The host-facing facade: a [`Workspace`] paired with an [`EditorHost`], answering every query
//! directly in the host's protocol types.
//!
//! All sequencing — path → file id, position → offset, neutral query, result → protocol shape —
//! lives here, so a host's request handler is one call: `editor.hover(path, &position)`. The two
//! front-ends differ only in the [`EditorHost`] implementation they pass in.

use alloc::vec::Vec;
use core::mem;

use jals_fs::FileTree;
use jals_hir::FileId;

use crate::document::Document;
use crate::host::{EditorHost, FoldingHost, SelectionHost, SemanticTokensHost};
use crate::workspace::{ProjectSpec, Workspace};
use crate::{FileRange, OutlineNode, SignatureHelpUtf16};

/// One project's [`Workspace`] driven through a host's protocol vocabulary.
pub struct Editor<F: FileTree, H: EditorHost> {
    workspace: Workspace<F>,
    host: H,
}

impl<F: FileTree, H: EditorHost> Editor<F, H> {
    /// Pair an already-loaded workspace with its host.
    pub const fn new(workspace: Workspace<F>, host: H) -> Self {
        Self { workspace, host }
    }

    /// Load a workspace over `fs` (see [`Workspace::load`]) and pair it with `host`.
    pub fn load(fs: F, spec: ProjectSpec, host: H) -> Self {
        Self::new(Workspace::load(fs, spec), host)
    }

    /// The underlying workspace: identity/ownership queries (`file_id`, `owns_path`, `root`) and
    /// the neutral analysis surface.
    pub const fn workspace(&self) -> &Workspace<F> {
        &self.workspace
    }

    /// Mutable access for lifecycle updates: overlays, classpath, feature set.
    pub const fn workspace_mut(&mut self) -> &mut Workspace<F> {
        &mut self.workspace
    }

    /// The host this editor renders through.
    pub const fn host(&self) -> &H {
        &self.host
    }

    /// The file id and cached document of `path`, when it is an indexed project file.
    fn doc(&self, path: &str) -> Option<(FileId, &Document)> {
        let file = self.workspace.file_id(path)?;
        Some((file, self.workspace.document(file)?))
    }

    /// Decode a host position in `path` to `(file, byte offset)`.
    fn offset(&self, path: &str, position: &H::Position) -> Option<(FileId, usize)> {
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
        path: &str,
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
    pub fn outline(&self, path: &str) -> Vec<H::Symbol> {
        let Some((file, doc)) = self.doc(path) else {
            return Vec::new();
        };
        self.workspace
            .outline(file)
            .into_iter()
            .map(|node| self.symbol(doc, node))
            .collect()
    }

    /// Render one outline node bottom-up (children first, then the node around them).
    fn symbol(&self, doc: &Document, mut node: OutlineNode) -> H::Symbol {
        let children = mem::take(&mut node.children)
            .into_iter()
            .map(|child| self.symbol(doc, child))
            .collect();
        self.host.symbol(doc, node, children)
    }

    /// Go-to-definition at `position` in `path`.
    pub fn definition(&self, path: &str, position: &H::Position) -> Option<H::Location> {
        let (file, offset) = self.offset(path, position)?;
        let target = self.workspace.definition(file, offset)?;
        self.location(&target)
    }

    /// Find-references at `position` in `path` (project-wide for a project type).
    pub fn references(
        &self,
        path: &str,
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
    pub fn hover(&self, path: &str, position: &H::Position) -> Option<H::Hover> {
        let (file, offset) = self.offset(path, position)?;
        let markdown = self.workspace.hover_markdown(file, offset)?;
        Some(self.host.hover(markdown))
    }

    /// Completions at `position` in `path`.
    pub fn completions(&self, path: &str, position: &H::Position) -> Vec<H::Completion> {
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
    pub fn signature_help(&self, path: &str, position: &H::Position) -> Option<H::SignatureHelp> {
        let (file, offset) = self.offset(path, position)?;
        let help = self.workspace.signature_help(file, offset)?;
        Some(self.host.signature_help(SignatureHelpUtf16::of(&help)))
    }

    /// Occurrence highlights at `position` in `path`.
    pub fn highlights(&self, path: &str, position: &H::Position) -> Vec<H::Highlight> {
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
    pub fn prepare_rename(&self, path: &str, position: &H::Position) -> Option<H::Range> {
        let (file, offset) = self.offset(path, position)?;
        let range = self.workspace.prepare_rename(file, offset)?;
        let doc = self.workspace.document(file)?;
        Some(self.host.range(doc, range))
    }

    /// The locations a rename at `position` in `path` rewrites, or `None` when the symbol is not
    /// renamable / nothing would change. The host validates the new name
    /// ([`crate::Ident::is_valid_java_identifier`]) and shapes the edit.
    pub fn rename_targets(&self, path: &str, position: &H::Position) -> Option<Vec<H::Location>> {
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

impl<F: FileTree, H: SemanticTokensHost> Editor<F, H> {
    /// The encoded semantic tokens of `path`.
    pub fn semantic_tokens(&self, path: &str) -> Option<H::SemanticTokens> {
        let (file, doc) = self.doc(path)?;
        Some(
            self.host
                .semantic_tokens(doc, self.workspace.semantic_tokens(file)),
        )
    }
}

impl<F: FileTree, H: FoldingHost> Editor<F, H> {
    /// The folding ranges of `path`.
    pub fn folding_ranges(&self, path: &str) -> Vec<H::FoldingRange> {
        let Some((file, _)) = self.doc(path) else {
            return Vec::new();
        };
        self.workspace
            .folds(file)
            .into_iter()
            .map(|fold| self.host.fold(fold))
            .collect()
    }
}

impl<F: FileTree, H: SelectionHost> Editor<F, H> {
    /// The selection chains for each requested position in `path`, in request order.
    pub fn selection_ranges(
        &self,
        path: &str,
        positions: &[H::Position],
    ) -> Vec<H::SelectionRange> {
        let Some((file, doc)) = self.doc(path) else {
            return Vec::new();
        };
        positions
            .iter()
            .map(|position| {
                let offset = self.host.offset(doc, position);
                self.host
                    .selection(doc, self.workspace.selection_chain(file, offset))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::ops::Range;

    use jals_fs::InMemoryFileTree;

    use super::*;
    use crate::host::EditorHost;
    use crate::{Completion, FileDiagnostic, Highlight, LineIndex, Utf16Position};

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
        fn location(&self, path: &str, _doc: &Document, range: Range<usize>) -> Self::Location {
            (path.to_owned(), range)
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

    fn sample_editor() -> Editor<InMemoryFileTree, TupleHost> {
        let fs = InMemoryFileTree::new()
            .with_file(
                "/proj/src/Greeter.java",
                "public class Greeter { public String greet(String name) { return name; } }",
            )
            .with_file(
                "/proj/src/Main.java",
                "public class Main { void run() { Greeter g = new Greeter(); } }",
            );
        Editor::load(
            fs,
            ProjectSpec::new("/proj", vec!["/proj/src".to_owned()]),
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
            .definition("/proj/src/Main.java", &pos_of(main, "Greeter g"))
            .expect("definition");
        assert_eq!(path, "/proj/src/Greeter.java");

        // Hover renders through the host.
        let hover = editor
            .hover("/proj/src/Main.java", &pos_of(main, "new Greeter"))
            .expect("hover");
        assert!(hover.contains("```java"), "{hover}");

        // Outline renders bottom-up with children.
        let outline = editor.outline("/proj/src/Greeter.java");
        assert_eq!(outline[0].0, "Greeter");
        assert_eq!(outline[0].1[0].0, "greet");

        // References map every target through the host.
        let refs = editor.references("/proj/src/Main.java", &pos_of(main, "Greeter g"), true);
        assert!(refs.iter().any(|(p, _)| p == "/proj/src/Greeter.java"));

        // An unknown path answers empty, never panicking.
        assert!(editor.outline("/nowhere.java").is_empty());
        assert!(editor.definition("/nowhere.java", &(0, 0)).is_none());
    }

    #[test]
    fn diagnostics_render_through_the_host() {
        let fs = InMemoryFileTree::new().with_file(
            "/proj/src/Main.java",
            "class Main { void run() { int unused = 1; } }",
        );
        let editor = Editor::load(
            fs,
            ProjectSpec::new("/proj", vec!["/proj/src".to_owned()]),
            TupleHost,
        );
        let diags =
            editor.diagnostics("/proj/src/Main.java", &jals_config::lint::Config::default());
        assert!(
            diags.iter().any(|message| message.contains("unused")),
            "{diags:?}"
        );
    }

    #[test]
    fn signature_help_arrives_in_utf16() {
        let text = "class C { void f(int 値, int b) {} void g() { f(1, ); } }";
        let fs = InMemoryFileTree::new().with_file("/proj/src/C.java", text);
        let editor = Editor::load(
            fs,
            ProjectSpec::new("/proj", vec!["/proj/src".to_owned()]),
            TupleHost,
        );
        let index = LineIndex::new(text);
        let offset = text.find("f(1, ").unwrap() + "f(1, ".len();
        let position = index.position(text, offset);
        let (active_signature, active_parameter) = editor
            .signature_help("/proj/src/C.java", &(position.line, position.character))
            .expect("signature help");
        assert_eq!((active_signature, active_parameter), (0, 1));
    }

    #[test]
    fn rename_targets_gate_and_map() {
        let editor = sample_editor();
        let main = "public class Main { void run() { Greeter g = new Greeter(); } }";
        let targets = editor
            .rename_targets("/proj/src/Main.java", &pos_of(main, "Greeter g"))
            .expect("renamable");
        assert!(targets.len() >= 3, "{targets:?}");
        // `String` is a stdlib name: not renamable.
        let greeter = "public class Greeter { public String greet(String name) { return name; } }";
        assert!(
            editor
                .rename_targets("/proj/src/Greeter.java", &pos_of(greeter, "String name"))
                .is_none()
        );
    }
}
