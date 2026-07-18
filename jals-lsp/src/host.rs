//! The LSP protocol host: renders `jals-editor`'s neutral analysis payloads as `lsp_types`
//! shapes.
//!
//! [`LspHost`] implements [`EditorHost`] (plus the wire-format extensions) for the LSP: positions
//! are zero-based UTF-16 `Position`s, cross-file targets are `Location`s with `file://` URLs, and
//! every query payload is the corresponding `lsp_types` structure. All coordinate conversion goes
//! through the shared [`jals_editor::LineIndex`] cached on each [`Document`]; the analysis itself
//! lives entirely in `jals-editor`.

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::PathBuf;

use async_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, DiagnosticTag,
    DocumentHighlight, DocumentHighlightKind, DocumentSymbol, FoldingRange, FoldingRangeKind,
    Hover, HoverContents, Location, MarkupContent, MarkupKind, NumberOrString,
    ParameterInformation, ParameterLabel, Position, Range as LspRange, SelectionRange,
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensEdit,
    SemanticTokensLegend, SignatureHelp, SignatureInformation, SymbolKind, TextEdit, Url,
    WorkspaceEdit,
};
use jals_editor::{
    Completion, CompletionKind, Document, EditorHost, FileDiagnostic, Fold, FoldKind, FoldingHost,
    Highlight, HighlightKind, OutlineNode, SelectionHost, SemanticTokenKind, SemanticTokensHost,
    SignatureHelpUtf16, Utf16Position,
};
use jals_hir::DefKind;
use jals_storage::FileKey;

/// Token-type indices into the legend's `token_types`. Kept in sync with [`LspHost::legend`] by
/// the `legend_indices_match` test.
mod ty {
    pub(super) const NAMESPACE: u32 = 0;
    pub(super) const TYPE: u32 = 1;
    pub(super) const CLASS: u32 = 2;
    pub(super) const ENUM: u32 = 3;
    pub(super) const INTERFACE: u32 = 4;
    pub(super) const TYPE_PARAMETER: u32 = 5;
    pub(super) const PARAMETER: u32 = 6;
    pub(super) const VARIABLE: u32 = 7;
    pub(super) const PROPERTY: u32 = 8;
    pub(super) const ENUM_MEMBER: u32 = 9;
    pub(super) const METHOD: u32 = 10;
    pub(super) const KEYWORD: u32 = 11;
    pub(super) const COMMENT: u32 = 12;
    pub(super) const STRING: u32 = 13;
    pub(super) const NUMBER: u32 = 14;
    pub(super) const DECORATOR: u32 = 15;
}

/// The `declaration` modifier (bit 0, the only entry in the legend's `token_modifiers`).
const MOD_DECLARATION: u32 = 1 << 0;

/// The LSP rendering of every neutral query payload. Stateless — one shared unit value drives all
/// requests.
pub(crate) struct LspHost {
    root: PathBuf,
    /// Host locations of navigation sources materialized out of the artifact cache (mounted
    /// under `.jals/…` in the workspace overlay, where no real file exists). Consulted before
    /// the root join so their `file://` URLs point at readable files.
    materialized: BTreeMap<FileKey, PathBuf>,
}

#[allow(non_upper_case_globals)]
pub(crate) const LspHost: LspHost = LspHost {
    root: PathBuf::new(),
    materialized: BTreeMap::new(),
};

impl LspHost {
    pub(crate) const fn for_root(root: PathBuf) -> Self {
        Self {
            root,
            materialized: BTreeMap::new(),
        }
    }

    /// Register the host locations of materialized navigation sources (see
    /// [`materialized`](Self::materialized)).
    #[must_use]
    pub(crate) fn with_materialized(mut self, materialized: BTreeMap<FileKey, PathBuf>) -> Self {
        self.materialized = materialized;
        self
    }
    /// Convert a byte offset in `doc` to an LSP position through the document's cached index.
    pub(crate) fn position(doc: &Document, offset: usize) -> Position {
        let position = doc.line_index.position(&doc.text, offset);
        Position::new(position.line, position.character)
    }

    /// Convert a byte range in `doc` to an LSP range.
    fn byte_range(doc: &Document, range: &Range<usize>) -> LspRange {
        LspRange {
            start: Self::position(doc, range.start),
            end: Self::position(doc, range.end),
        }
    }

    /// The `file://` URL of a workspace virtual path, resolved against this host's project root.
    ///
    /// # Panics
    /// Panics when the resolved path cannot be encoded as a file URL. A rooted host
    /// ([`for_root`](Self::for_root)) always resolves to an absolute path, so this only fires if
    /// a location is ever rendered through the rootless shared const — which must not happen.
    fn url(&self, key: &FileKey) -> Url {
        let path = self
            .materialized
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.path().to_host_path(&self.root));
        Url::from_file_path(&path).unwrap_or_else(|()| {
            panic!(
                "project path cannot be encoded as a file URI: {}",
                path.display()
            )
        })
    }

    /// The LSP symbol kind for an outline node's `DefKind`.
    const fn symbol_kind(kind: DefKind) -> SymbolKind {
        match kind {
            DefKind::Class => SymbolKind::CLASS,
            // LSP has no annotation-type kind; interfaces are the closest fit.
            DefKind::Interface | DefKind::AnnotationType => SymbolKind::INTERFACE,
            DefKind::Record => SymbolKind::STRUCT,
            DefKind::Enum => SymbolKind::ENUM,
            DefKind::EnumConstant => SymbolKind::ENUM_MEMBER,
            DefKind::Field => SymbolKind::FIELD,
            DefKind::Method => SymbolKind::METHOD,
            DefKind::Constructor => SymbolKind::CONSTRUCTOR,
            // Value/type-parameter kinds never appear in an outline.
            _ => SymbolKind::VARIABLE,
        }
    }

    /// The LSP completion-item kind for a protocol-neutral completion category.
    const fn item_kind(kind: CompletionKind) -> CompletionItemKind {
        use CompletionKind::{
            Class, Enum, EnumMember, Field, Interface, Keyword, Method, TypeParameter, Variable,
        };
        match kind {
            Method => CompletionItemKind::METHOD,
            Field => CompletionItemKind::FIELD,
            EnumMember => CompletionItemKind::ENUM_MEMBER,
            Variable => CompletionItemKind::VARIABLE,
            TypeParameter => CompletionItemKind::TYPE_PARAMETER,
            Class => CompletionItemKind::CLASS,
            Interface => CompletionItemKind::INTERFACE,
            Enum => CompletionItemKind::ENUM,
            Keyword => CompletionItemKind::KEYWORD,
        }
    }

    /// The legend token-type index for a neutral [`SemanticTokenKind`].
    const fn token_index(kind: SemanticTokenKind) -> u32 {
        match kind {
            SemanticTokenKind::Namespace => ty::NAMESPACE,
            SemanticTokenKind::Type => ty::TYPE,
            SemanticTokenKind::Class => ty::CLASS,
            SemanticTokenKind::Enum => ty::ENUM,
            SemanticTokenKind::Interface => ty::INTERFACE,
            SemanticTokenKind::TypeParameter => ty::TYPE_PARAMETER,
            SemanticTokenKind::Parameter => ty::PARAMETER,
            SemanticTokenKind::Variable => ty::VARIABLE,
            SemanticTokenKind::Property => ty::PROPERTY,
            SemanticTokenKind::EnumMember => ty::ENUM_MEMBER,
            SemanticTokenKind::Method => ty::METHOD,
            SemanticTokenKind::Keyword => ty::KEYWORD,
            SemanticTokenKind::Comment => ty::COMMENT,
            SemanticTokenKind::String => ty::STRING,
            SemanticTokenKind::Number => ty::NUMBER,
            SemanticTokenKind::Decorator => ty::DECORATOR,
        }
    }

    /// The legend advertised on `initialize`. The order of `token_types` defines the indices in
    /// [`ty`]; the order of `token_modifiers` defines the [`MOD_DECLARATION`] bit.
    pub(crate) fn legend() -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types: vec![
                SemanticTokenType::NAMESPACE,
                SemanticTokenType::TYPE,
                SemanticTokenType::CLASS,
                SemanticTokenType::ENUM,
                SemanticTokenType::INTERFACE,
                SemanticTokenType::TYPE_PARAMETER,
                SemanticTokenType::PARAMETER,
                SemanticTokenType::VARIABLE,
                SemanticTokenType::PROPERTY,
                SemanticTokenType::ENUM_MEMBER,
                SemanticTokenType::METHOD,
                SemanticTokenType::KEYWORD,
                SemanticTokenType::COMMENT,
                SemanticTokenType::STRING,
                SemanticTokenType::NUMBER,
                SemanticTokenType::DECORATOR,
            ],
            token_modifiers: vec![SemanticTokenModifier::DECLARATION],
        }
    }

    /// The LSP semantic-tokens *delta* (edit script) turning `prev` into `next`, as a single splice of
    /// the one differing middle range. `prev` and `next` are the delta-encoded token arrays of two
    /// consecutive [`SemanticTokensHost::semantic_tokens`] results for the same document (the client's
    /// last copy and the current one).
    ///
    /// A `SemanticTokensEdit`'s `start` / `delete_count` count entries of the *flattened* integer array
    /// the tokens encode to — 5 ints per token — so the token indices are multiplied by 5. Returns an
    /// empty vector when the arrays are identical (no edits to apply).
    pub(crate) fn tokens_delta(
        prev: &[SemanticToken],
        next: &[SemanticToken],
    ) -> Vec<SemanticTokensEdit> {
        let max = prev.len().min(next.len());
        // The longest common prefix, then the longest common suffix that does not overlap it.
        let mut prefix = 0;
        while prefix < max && prev[prefix] == next[prefix] {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < max - prefix
            && prev[prev.len() - 1 - suffix] == next[next.len() - 1 - suffix]
        {
            suffix += 1;
        }
        let deleted = prev.len() - prefix - suffix;
        let inserted = &next[prefix..next.len() - suffix];
        if deleted == 0 && inserted.is_empty() {
            return Vec::new();
        }
        vec![SemanticTokensEdit {
            start: (prefix * 5) as u32,
            delete_count: (deleted * 5) as u32,
            data: Some(inserted.to_vec()),
        }]
    }

    /// Render a whole outline (each node's children recursively) as LSP document symbols — the
    /// store-based fallback path's counterpart to [`Editor::outline`](jals_editor::Editor),
    /// sharing the trait's [`render_outline`](EditorHost::render_outline) recursion.
    pub(crate) fn symbols(&self, doc: &Document, nodes: Vec<OutlineNode>) -> Vec<DocumentSymbol> {
        self.render_outline(doc, nodes)
    }

    /// Group rename target `locations` into a [`WorkspaceEdit`] rewriting each occurrence to
    /// `new_name`, keyed by file. `None` if there is nothing to rewrite.
    pub(crate) fn workspace_edit(
        locations: Vec<Location>,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
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
}

impl EditorHost for LspHost {
    type Position = Position;
    type Range = LspRange;
    type Location = Location;
    type Diagnostic = Diagnostic;
    type Symbol = DocumentSymbol;
    type Completion = CompletionItem;
    type Highlight = DocumentHighlight;
    type Hover = Hover;
    type SignatureHelp = SignatureHelp;

    fn offset(&self, doc: &Document, position: &Position) -> usize {
        doc.line_index.offset(
            &doc.text,
            Utf16Position {
                line: position.line,
                character: position.character,
            },
        )
    }

    fn range(&self, doc: &Document, range: Range<usize>) -> LspRange {
        Self::byte_range(doc, &range)
    }

    fn location(&self, path: &FileKey, doc: &Document, range: Range<usize>) -> Location {
        Location {
            uri: self.url(path),
            range: Self::byte_range(doc, &range),
        }
    }

    fn diagnostic(&self, doc: &Document, diagnostic: FileDiagnostic) -> Diagnostic {
        Diagnostic {
            range: Self::byte_range(doc, &diagnostic.range),
            severity: Some(match diagnostic.severity {
                jals_editor::DiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
                jals_editor::DiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
                jals_editor::DiagnosticSeverity::Hint => DiagnosticSeverity::HINT,
            }),
            code: diagnostic
                .code
                .map(|code| NumberOrString::String(code.to_owned())),
            source: Some("jals".to_owned()),
            message: diagnostic.message,
            // Unnecessary code (an unused local, a dead branch) renders faded in place.
            tags: diagnostic
                .unnecessary
                .then(|| vec![DiagnosticTag::UNNECESSARY]),
            ..Default::default()
        }
    }

    fn symbol(
        &self,
        doc: &Document,
        node: OutlineNode,
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        #[allow(deprecated)] // `DocumentSymbol::deprecated` is a mandatory-but-deprecated field.
        DocumentSymbol {
            name: node.name,
            detail: None,
            kind: Self::symbol_kind(node.kind),
            tags: None,
            deprecated: None,
            range: Self::byte_range(doc, &node.range),
            selection_range: Self::byte_range(doc, &node.selection_range),
            children: (!children.is_empty()).then_some(children),
        }
    }

    fn completion(&self, completion: Completion) -> CompletionItem {
        CompletionItem {
            label: completion.label,
            kind: Some(Self::item_kind(completion.kind)),
            detail: (!completion.detail.is_empty()).then_some(completion.detail),
            ..CompletionItem::default()
        }
    }

    fn highlight(&self, doc: &Document, highlight: Highlight) -> DocumentHighlight {
        DocumentHighlight {
            range: Self::byte_range(doc, &highlight.range),
            kind: Some(match highlight.kind {
                HighlightKind::Read => DocumentHighlightKind::READ,
                HighlightKind::Write => DocumentHighlightKind::WRITE,
            }),
        }
    }

    fn hover(&self, markdown: String) -> Hover {
        Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: None,
        }
    }

    fn signature_help(&self, help: SignatureHelpUtf16) -> SignatureHelp {
        SignatureHelp {
            signatures: help
                .signatures
                .into_iter()
                .map(|sig| SignatureInformation {
                    label: sig.label,
                    documentation: None,
                    parameters: Some(
                        sig.parameters
                            .into_iter()
                            .map(|span| ParameterInformation {
                                label: ParameterLabel::LabelOffsets(span.into()),
                                documentation: None,
                            })
                            .collect(),
                    ),
                    active_parameter: None,
                })
                .collect(),
            active_signature: Some(help.active_signature),
            active_parameter: Some(help.active_parameter),
        }
    }
}

impl SemanticTokensHost for LspHost {
    type SemanticTokens = SemanticTokens;

    /// Encode the classified tokens as LSP semantic tokens: legend indices, UTF-16 delta
    /// encoding, and the protocol's one-token-per-line splitting.
    fn semantic_tokens(
        &self,
        doc: &Document,
        tokens: Vec<jals_editor::SemanticToken>,
    ) -> SemanticTokens {
        let mut data: Vec<SemanticToken> = Vec::new();
        // Anchor for delta encoding: the line/start of the previously emitted token.
        let (mut prev_line, mut prev_start) = (0u32, 0u32);

        for token in tokens {
            let token_type = Self::token_index(token.kind);
            let token_modifiers_bitset = if token.declaration {
                MOD_DECLARATION
            } else {
                0
            };
            let start = doc.line_index.position(&doc.text, token.range.start);
            let token_text = doc.text.get(token.range.clone()).unwrap_or_default();
            // A semantic token may not span lines (LSP spec), so split on '\n'. Trivia tokens
            // are dropped, but comments are kept and a block comment / text block can be
            // multi-line; each line becomes its own token starting at column 0 (after line 0).
            for (line, (i, segment)) in (start.line..).zip(token_text.split('\n').enumerate()) {
                let segment = segment.strip_suffix('\r').unwrap_or(segment);
                let length: u32 = segment.chars().map(|c| c.len_utf16() as u32).sum();
                let char_start = if i == 0 { start.character } else { 0 };
                if length != 0 {
                    let delta_line = line.saturating_sub(prev_line);
                    let delta_start = if delta_line == 0 {
                        // Same line as the previous token; never underflows for in-order,
                        // non-overlapping tokens, but saturate defensively to never panic.
                        char_start.saturating_sub(prev_start)
                    } else {
                        char_start
                    };
                    data.push(SemanticToken {
                        delta_line,
                        delta_start,
                        length,
                        token_type,
                        token_modifiers_bitset,
                    });
                    (prev_line, prev_start) = (line, char_start);
                }
            }
        }

        SemanticTokens {
            result_id: None,
            data,
        }
    }
}

impl FoldingHost for LspHost {
    type FoldingRange = FoldingRange;

    fn fold(&self, fold: Fold) -> FoldingRange {
        FoldingRange {
            start_line: fold.start_line,
            end_line: fold.end_line,
            kind: match fold.kind {
                // A brace region carries no LSP kind (clients default it).
                FoldKind::Region => None,
                FoldKind::Comment => Some(FoldingRangeKind::Comment),
                FoldKind::Imports => Some(FoldingRangeKind::Imports),
            },
            ..FoldingRange::default()
        }
    }
}

impl SelectionHost for LspHost {
    type SelectionRange = SelectionRange;

    fn selection(&self, doc: &Document, chain: Vec<Range<usize>>) -> SelectionRange {
        // Link outermost -> innermost, so each inner range's `parent` points one step out.
        let mut selection: Option<SelectionRange> = None;
        for range in chain.iter().rev() {
            selection = Some(SelectionRange {
                range: Self::byte_range(doc, range),
                parent: selection.map(Box::new),
            });
        }
        // The chain always holds at least the root, but stay total just in case.
        selection.unwrap_or_else(|| SelectionRange {
            range: Self::byte_range(doc, &(0..0)),
            parent: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;

    use super::*;

    /// Parse `text` into the shared per-file document (text + line index + CST).
    fn doc(text: &str) -> Document {
        block_on_inline(Document::new(text.to_owned()))
    }

    // ---- Diagnostic mapping -------------------------------------------------------------------

    /// Assemble-and-map for `text` under the default config, with no project index.
    fn diagnostics(text: &str) -> Vec<Diagnostic> {
        let document = doc(text);
        block_on_inline(jals_editor::FileDiagnostics::assemble(
            &document.parse,
            None,
            None,
            &jals_config::lint::Config::default(),
        ))
        .into_iter()
        .map(|d| LspHost.diagnostic(&document, d))
        .collect()
    }

    #[test]
    fn neutral_diagnostics_map_to_lsp_shapes() {
        // The assembly policy is covered in `jals-editor`; this pins the adapter mapping —
        // severity vocabulary, `code`/`source`, the `Unnecessary` tag, and positions.
        let text = "class C { void m() { int unused = 1; if (true) { a(); } else { b(); } } }";
        let diags = diagnostics(text);

        let unused = diags
            .iter()
            .find(|d| d.code == Some(NumberOrString::String("unused-local".to_owned())))
            .expect("an unused-local diagnostic");
        assert_eq!(unused.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(unused.source.as_deref(), Some("jals"));
        assert_eq!(unused.tags, Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(unused.range.start.line, 0, "byte ranges become positions");

        let hint = diags
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::HINT))
            .expect("the dead-branch hint");
        assert_eq!(hint.tags, Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(hint.message, "this code is never executed");
    }

    #[test]
    fn syntax_errors_map_to_uncoded_errors() {
        let diags = diagnostics("class A { void m( {}");
        assert!(!diags.is_empty());
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].code, None);
        assert_eq!(diags[0].source.as_deref(), Some("jals"));
    }

    // ---- Symbol mapping -----------------------------------------------------------------------

    #[test]
    fn outline_maps_to_lsp_kinds_and_positions() {
        // The traversal itself is covered in `jals-editor`; this pins the adapter mapping —
        // kind vocabulary, empty children collapsed to `None`, and byte → position coordinates.
        let text = "class C { int x; void m() {} }\nenum E { A }";
        let document = doc(text);
        let syms = LspHost.symbols(
            &document,
            jals_editor::Outline::of(&document.parse.syntax()),
        );
        assert_eq!(syms.len(), 2);
        let c = &syms[0];
        assert_eq!((c.name.as_str(), c.kind), ("C", SymbolKind::CLASS));
        assert_eq!(c.range.start.line, 0);
        let children = c.children.as_ref().unwrap();
        assert_eq!(
            (children[0].name.as_str(), children[0].kind),
            ("x", SymbolKind::FIELD)
        );
        assert!(children[0].children.is_none(), "empty children become None");
        assert_eq!(
            (children[1].name.as_str(), children[1].kind),
            ("m", SymbolKind::METHOD)
        );
        let e = &syms[1];
        assert_eq!(e.kind, SymbolKind::ENUM);
        // The node range may include leading trivia (the newline), so pin the end position.
        assert_eq!(e.range.end.line, 1, "positions are line/character");
        assert_eq!(
            e.children.as_ref().unwrap()[0].kind,
            SymbolKind::ENUM_MEMBER
        );
    }

    // ---- Completion / highlight / hover / signature-help mapping -------------------------------

    #[test]
    fn completion_maps_kind_and_collapses_empty_detail() {
        let field = LspHost.completion(Completion {
            label: "size".to_owned(),
            kind: CompletionKind::Field,
            detail: "int".to_owned(),
        });
        assert_eq!(field.label, "size");
        assert_eq!(field.kind, Some(CompletionItemKind::FIELD));
        assert_eq!(field.detail.as_deref(), Some("int"));

        let keyword = LspHost.completion(Completion {
            label: "return".to_owned(),
            kind: CompletionKind::Keyword,
            detail: String::new(),
        });
        assert_eq!(keyword.kind, Some(CompletionItemKind::KEYWORD));
        assert_eq!(keyword.detail, None, "empty detail becomes None");
    }

    #[test]
    fn highlight_maps_kind_and_utf16_coordinates() {
        // '😀' is 4 UTF-8 bytes but 2 UTF-16 units: byte range 17..18 is UTF-16 column 15..16.
        let document = doc("String s = \"😀\"; x");
        let highlight = LspHost.highlight(
            &document,
            Highlight {
                range: 17..18,
                kind: HighlightKind::Write,
            },
        );
        assert_eq!(highlight.kind, Some(DocumentHighlightKind::WRITE));
        assert_eq!(highlight.range.start, Position::new(0, 15));
        assert_eq!(highlight.range.end, Position::new(0, 16));
    }

    #[test]
    fn hover_wraps_the_prerendered_markdown() {
        let hover = LspHost.hover("```java\nint\n```".to_owned());
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup hover");
        };
        assert_eq!(markup.kind, MarkupKind::Markdown);
        assert_eq!(markup.value, "```java\nint\n```");
    }

    #[test]
    fn signature_help_uses_label_offsets() {
        let help = LspHost.signature_help(SignatureHelpUtf16 {
            signatures: vec![jals_editor::SignatureUtf16 {
                label: "f(int 値, int b)".to_owned(),
                parameters: vec![(2, 7), (9, 14)],
            }],
            active_signature: 0,
            active_parameter: 1,
        });
        assert_eq!(help.active_signature, Some(0));
        assert_eq!(help.active_parameter, Some(1));
        let params = help.signatures[0].parameters.as_ref().unwrap();
        let offsets: Vec<[u32; 2]> = params
            .iter()
            .map(|p| match p.label {
                ParameterLabel::LabelOffsets(o) => o,
                ParameterLabel::Simple(_) => panic!("expected label offsets"),
            })
            .collect();
        assert_eq!(offsets, vec![[2, 7], [9, 14]]);
    }

    // ---- Location / URL mapping ---------------------------------------------------------------

    #[test]
    fn locations_carry_file_urls() {
        let document = doc("class C {}");
        let location = LspHost::for_root(PathBuf::from("/proj")).location(
            &FileKey::parse("src/C.java").unwrap(),
            &document,
            6..7,
        );
        assert_eq!(
            location.uri,
            Url::from_file_path("/proj/src/C.java").unwrap()
        );
        assert_eq!(location.range.start, Position::new(0, 6));
    }

    /// A mounted `.jals/…` navigation source has no file under the project root; its location
    /// must point at the file materialized out of the artifact cache instead.
    #[test]
    fn materialized_navigation_sources_resolve_to_their_cache_files() {
        let document = doc("class Lib {}");
        let key = FileKey::parse(".jals/library/dep/Lib.java").unwrap();
        let target = PathBuf::from("/proj/target/jals/cache/source-view/aa/bb/dep/Lib.java");
        let host = LspHost::for_root(PathBuf::from("/proj"))
            .with_materialized(BTreeMap::from([(key.clone(), target.clone())]));
        let location = host.location(&key, &document, 6..9);
        assert_eq!(location.uri, Url::from_file_path(&target).unwrap());
        // Unmapped keys still resolve against the project root.
        let plain = host.location(&FileKey::parse("src/C.java").unwrap(), &document, 0..1);
        assert_eq!(plain.uri, Url::from_file_path("/proj/src/C.java").unwrap());
    }

    // ---- Semantic-token encoding ----------------------------------------------------------------

    /// One decoded token: absolute `(line, start, length, type_name, modifier_bits)`.
    #[derive(Debug, PartialEq, Eq)]
    struct Tok {
        line: u32,
        start: u32,
        len: u32,
        ty: String,
        mods: u32,
    }

    /// Decode an already-built token stream back to absolute positions and type names, so tests
    /// can assert on what a client would actually render.
    fn decode_tokens(toks: &SemanticTokens) -> Vec<Tok> {
        let legend = LspHost::legend();
        let (mut line, mut start) = (0u32, 0u32);
        let mut out = Vec::new();
        for t in &toks.data {
            if t.delta_line == 0 {
                start += t.delta_start;
            } else {
                line += t.delta_line;
                start = t.delta_start;
            }
            out.push(Tok {
                line,
                start,
                len: t.length,
                ty: legend.token_types[t.token_type as usize]
                    .as_str()
                    .to_owned(),
                mods: t.token_modifiers_bitset,
            });
        }
        out
    }

    /// Decode the tokens produced for `text` (with no project index).
    fn decode(text: &str) -> Vec<Tok> {
        let document = doc(text);
        let classified = block_on_inline(jals_editor::SemanticTokens::classify(
            &document.parse.syntax(),
            None,
        ));
        decode_tokens(&LspHost.semantic_tokens(&document, classified))
    }

    #[test]
    fn legend_indices_match() {
        let legend = LspHost::legend();
        assert_eq!(legend.token_types.len(), 16);
        assert_eq!(legend.token_modifiers.len(), 1);
        // Spot-check that the index constants line up with the legend order.
        assert_eq!(legend.token_types[ty::CLASS as usize].as_str(), "class");
        assert_eq!(legend.token_types[ty::METHOD as usize].as_str(), "method");
        assert_eq!(
            legend.token_types[ty::DECORATOR as usize].as_str(),
            "decorator"
        );
        assert_eq!(
            legend.token_modifiers[0],
            SemanticTokenModifier::DECLARATION
        );
    }

    #[test]
    fn neutral_kinds_map_through_the_legend() {
        // The classification itself is covered in `jals-editor`; this pins the adapter — every
        // neutral kind lands on its legend index, with the declaration modifier as bit 0.
        let src = "class C<T> { int field; void m(int p) {} }";
        let toks = decode(src);
        let at = |needle: &str| {
            let col = src.find(needle).unwrap() as u32;
            toks.iter()
                .find(|t| t.line == 0 && t.start == col)
                .unwrap_or_else(|| panic!("no token at {needle:?}"))
        };
        assert_eq!(at("class").ty, "keyword");
        assert_eq!(
            (at("C").ty.as_str(), at("C").mods),
            ("class", MOD_DECLARATION)
        );
        assert_eq!(at("T").ty, "typeParameter");
        assert_eq!(at("field").ty, "property");
        assert_eq!(at("m").ty, "method");
        assert_eq!(at("p").ty, "parameter");
    }

    #[test]
    fn block_comment_splits_per_line() {
        let src = "/* a\n   b */ class C {}";
        let toks = decode(src);
        // Two comment tokens, one per physical line, both before `class` on line 1.
        let comments: Vec<&Tok> = toks.iter().filter(|t| t.ty == "comment").collect();
        assert_eq!(comments.len(), 2);
        assert_eq!((comments[0].line, comments[0].start), (0, 0));
        assert_eq!(comments[1].line, 1);
    }

    #[test]
    fn utf16_columns_are_not_byte_offsets() {
        // '😀' is 4 UTF-8 bytes but 2 UTF-16 code units; both token lengths and the columns
        // of everything after it must be counted in UTF-16, not bytes.
        let src = "class C { String s = \"😀\"; int x; }";
        let toks = decode(src);
        // "😀" = quote + astral(2 units) + quote = 4 UTF-16 units (would be 6 in bytes).
        let lit = toks.iter().find(|t| t.ty == "string").unwrap();
        assert_eq!(lit.len, 4);
        // Fields `s` and `x` are properties; `x` sits at UTF-16 column 31, not byte 33.
        let props: Vec<&Tok> = toks.iter().filter(|t| t.ty == "property").collect();
        assert_eq!(props.len(), 2);
        assert_eq!((props[1].line, props[1].start), (0, 31));
    }

    #[test]
    fn does_not_panic_on_garbage_or_empty() {
        // Invariant: the encoder never panics, even on broken / arbitrary input.
        for text in ["", "class", "@#$%^ <<< class {", "класс 类 😀 \0 /*"] {
            let _ = decode(text);
        }
    }

    // ---- Semantic-token delta -------------------------------------------------------------------

    /// A token with the given fields; modifiers default to none.
    fn tok(delta_line: u32, delta_start: u32, length: u32, token_type: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        }
    }

    #[test]
    fn tokens_delta_splices_only_the_changed_middle() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2), tok(1, 0, 5, 3)];
        // Change only the middle token: the edit deletes 1 token at index 1 and inserts its
        // replacement, in flattened-int units (×5).
        let mut b = a.clone();
        b[1] = tok(0, 4, 2, 7);
        let edits = LspHost::tokens_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 5);
        assert_eq!(edits[0].data.as_deref(), Some(&[tok(0, 4, 2, 7)][..]));
    }

    #[test]
    fn tokens_delta_identical_is_empty() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2)];
        assert!(LspHost::tokens_delta(&a, &a).is_empty());
    }

    #[test]
    fn tokens_delta_pure_append_deletes_nothing() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2)];
        let mut b = a.clone();
        b.push(tok(1, 0, 1, 4));
        let edits = LspHost::tokens_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        // Prefix of 2 tokens (10 ints), nothing deleted, one token inserted.
        assert_eq!(edits[0].start, 10);
        assert_eq!(edits[0].delete_count, 0);
        assert_eq!(edits[0].data.as_deref(), Some(&[tok(1, 0, 1, 4)][..]));
    }

    #[test]
    fn tokens_delta_pure_delete_inserts_nothing() {
        let a = vec![tok(0, 0, 3, 1), tok(0, 4, 2, 2), tok(1, 0, 5, 3)];
        let mut b = a.clone();
        b.remove(1); // drop the middle token
        let edits = LspHost::tokens_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 5);
        assert_eq!(edits[0].data.as_deref(), Some(&[][..]));
    }

    // ---- Folding / selection mapping ------------------------------------------------------------

    #[test]
    fn neutral_folds_map_to_lsp_kinds() {
        // The fold computation is covered in `jals-editor`; this pins the adapter mapping —
        // region folds carry no kind, comment/import folds carry theirs.
        let folds = |text: &str| -> Vec<(u32, u32, Option<FoldingRangeKind>)> {
            let document = doc(text);
            let mut v: Vec<_> = jals_editor::Folds::of(
                &document.parse.syntax(),
                &document.text,
                &document.line_index,
            )
            .into_iter()
            .map(|fold| LspHost.fold(fold))
            .map(|r| (r.start_line, r.end_line, r.kind))
            .collect();
            v.sort_by_key(|&(s, e, _)| (s, e));
            v
        };

        let f = folds("import a.B;\nimport a.C;\nclass C {\n  void m() {\n    x();\n  }\n}");
        assert!(
            f.contains(&(0, 1, Some(FoldingRangeKind::Imports))),
            "{f:?}"
        );
        assert!(f.contains(&(2, 5, None)), "class body: {f:?}");
        assert!(f.contains(&(3, 4, None)), "method block: {f:?}");

        let c = folds("/*\n * header\n */\nclass C {}");
        assert!(
            c.contains(&(0, 2, Some(FoldingRangeKind::Comment))),
            "{c:?}"
        );
    }

    #[test]
    fn selection_chains_are_parent_linked_in_lsp_coordinates() {
        // The chain computation is covered in `jals-editor`; this pins the adapter — byte-range
        // chain in, parent-linked `SelectionRange` with line/character coordinates out.
        // `class Cls { int xy; }` — cursor inside the field name `xy` (byte 17, on 'y').
        let text = "class Cls { int xy; }";
        let document = doc(text);
        let sr = LspHost.selection(
            &document,
            jals_editor::SelectionChains::at(&document.parse.syntax(), 17),
        );
        let mut chain = Vec::new();
        let mut cur = Some(&sr);
        while let Some(sr) = cur {
            let r = sr.range;
            chain.push((r.start.line, r.start.character, r.end.line, r.end.character));
            cur = sr.parent.as_deref();
        }
        assert_eq!(chain[0], (0, 16, 0, 18), "innermost = `xy`: {chain:?}");
        assert_eq!(
            *chain.last().unwrap(),
            (0, 0, 0, 21),
            "outermost: {chain:?}"
        );
        // An empty chain still yields a total (zero-range) selection.
        let empty = LspHost.selection(&document, Vec::new());
        assert_eq!(empty.range.start, Position::new(0, 0));
        assert!(empty.parent.is_none());
    }
}
