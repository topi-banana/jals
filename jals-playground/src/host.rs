//! The Monaco [`EditorHost`]: the playground's coordinate encoding and provider payload shapes.
//!
//! Every semantic query runs in `jals-editor`'s neutral vocabulary (byte offsets,
//! [`FileDiagnostic`]s, [`OutlineNode`]s); this module is the one place those results are turned
//! into Monaco's dialect — one-based UTF-16 line/column coordinates and the plain payload structs
//! the providers marshal into `JsValue`s. Both directions of the zero- ↔ one-based shift live
//! here: [`MonacoHost::offset`] (position → byte) and [`MonacoRange::of`] (byte range → range).

use core::ops::Range;

use jals_editor::{
    Completion, CompletionKind, DiagnosticSeverity, Document, EditorHost, FileDiagnostic,
    HighlightKind, OutlineNode, SignatureHelpUtf16, Utf16Position,
};
use jals_hir::DefKind;

/// One diagnostic over the active file, in Monaco coordinates — marshalled straight into a Monaco
/// marker by the UI layer. Aggregates syntax errors, lint rule findings (including the cross-file
/// `type-mismatch`), and cross-file unresolved type names.
pub struct PlaygroundDiagnostic {
    /// Range in Monaco coordinates (one-based UTF-16, both ends).
    pub range: MonacoRange,
    /// Human-readable message, prefixed with the producing rule when there is one.
    pub message: String,
    /// Presentation severity from the shared assembly.
    pub severity: DiagnosticSeverity,
}

/// A range in Monaco coordinates — one-based line and one-based UTF-16 column, both ends. The
/// shape the language-feature methods return; the UI layer marshals it into Monaco's `IRange`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MonacoRange {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl MonacoRange {
    /// Map a byte `range` to one-based Monaco coordinates through a prebuilt UTF-16 `index` over
    /// `text` — the single home of the zero- to one-based (+1) shift.
    pub fn of(index: &jals_editor::LineIndex, text: &str, range: &Range<usize>) -> Self {
        let start = index.position(text, range.start);
        let end = index.position(text, range.end);
        MonacoRange {
            start_line: start.line + 1,
            start_col: start.character + 1,
            end_line: end.line + 1,
            end_col: end.character + 1,
        }
    }
}

/// A navigation target: a workspace file path plus a range within it. Returned by
/// go-to-definition and, one per occurrence, by find-references.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    pub path: String,
    pub range: MonacoRange,
}

/// One node of the document-symbol outline: a named declaration, its full range, and its members.
/// `kind` is a [`DefKind`]; the UI maps it to a Monaco `SymbolKind`.
pub struct SymbolNode {
    pub name: String,
    pub kind: DefKind,
    pub range: MonacoRange,
    pub children: Vec<SymbolNode>,
}

/// One occurrence highlight: its range and whether it is a write (declaration/binding name or a
/// mutating use) as opposed to a read.
#[derive(Debug)]
pub struct Highlight {
    pub range: MonacoRange,
    pub write: bool,
}

/// One completion candidate: its label, its [`CompletionKind`] (driving the editor icon), and the
/// detail shown beside it.
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: String,
}

/// One signature in signature help: its rendered label and, per parameter, the `(start, end)`
/// UTF-16 code-unit offsets of that parameter's span within the label.
pub struct SigInfo {
    pub label: String,
    pub parameters: Vec<(u32, u32)>,
}

/// Signature help for a call: the overloads and which signature / parameter is active.
pub struct SigHelp {
    pub signatures: Vec<SigInfo>,
    pub active_signature: u32,
    pub active_parameter: u32,
}

/// The Monaco front-end of the shared [`jals_editor::Editor`]: positions are one-based
/// `(line, col)` UTF-16 pairs and every payload is one of the plain structs above.
pub struct MonacoHost;

impl EditorHost for MonacoHost {
    type Position = (u32, u32);
    type Range = MonacoRange;
    type Location = Target;
    type Diagnostic = PlaygroundDiagnostic;
    type Symbol = SymbolNode;
    type Completion = CompletionEntry;
    type Highlight = Highlight;
    type Hover = String;
    type SignatureHelp = SigHelp;

    fn offset(&self, doc: &Document, &(line, col): &(u32, u32)) -> usize {
        // The one- to zero-based (-1) shift, mirroring `MonacoRange::of`.
        doc.line_index.offset(
            &doc.text,
            Utf16Position {
                line: line.saturating_sub(1),
                character: col.saturating_sub(1),
            },
        )
    }

    fn range(&self, doc: &Document, range: Range<usize>) -> MonacoRange {
        MonacoRange::of(&doc.line_index, &doc.text, &range)
    }

    fn location(&self, path: &str, doc: &Document, range: Range<usize>) -> Target {
        Target {
            path: path.to_string(),
            range: self.range(doc, range),
        }
    }

    fn diagnostic(&self, doc: &Document, diagnostic: FileDiagnostic) -> PlaygroundDiagnostic {
        PlaygroundDiagnostic {
            range: self.range(doc, diagnostic.range),
            // Prefix the producing rule for display; a syntax error carries no code.
            message: match diagnostic.code {
                Some(code) => format!("{code}: {}", diagnostic.message),
                None => diagnostic.message,
            },
            severity: diagnostic.severity,
        }
    }

    fn symbol(&self, doc: &Document, node: OutlineNode, children: Vec<SymbolNode>) -> SymbolNode {
        SymbolNode {
            name: node.name,
            kind: node.kind,
            range: self.range(doc, node.range),
            children,
        }
    }

    fn completion(&self, completion: Completion) -> CompletionEntry {
        CompletionEntry {
            label: completion.label,
            kind: completion.kind,
            detail: completion.detail,
        }
    }

    fn highlight(&self, doc: &Document, highlight: jals_editor::Highlight) -> Highlight {
        Highlight {
            range: self.range(doc, highlight.range),
            write: highlight.kind == HighlightKind::Write,
        }
    }

    fn hover(&self, markdown: String) -> String {
        markdown
    }

    fn signature_help(&self, help: SignatureHelpUtf16) -> SigHelp {
        // The parameter spans are already UTF-16 code-unit offsets — a direct copy.
        SigHelp {
            signatures: help
                .signatures
                .into_iter()
                .map(|sig| SigInfo {
                    label: sig.label,
                    parameters: sig.parameters,
                })
                .collect(),
            active_signature: help.active_signature,
            active_parameter: help.active_parameter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monaco_ranges_are_one_based() {
        let text = "a😀\nb";
        let index = jals_editor::LineIndex::new(text);
        assert_eq!(
            MonacoRange::of(&index, text, &(1..6)),
            MonacoRange {
                start_line: 1,
                start_col: 2,
                end_line: 2,
                end_col: 1,
            }
        );
    }

    #[test]
    fn monaco_positions_are_one_based() {
        let doc = Document::new("a😀\nb".to_string());
        assert_eq!(MonacoHost.offset(&doc, &(1, 2)), 1);
        assert_eq!(MonacoHost.offset(&doc, &(1, 4)), 5);
        assert_eq!(MonacoHost.offset(&doc, &(2, 1)), 6);
        // Out-of-range coordinates saturate instead of underflowing.
        assert_eq!(MonacoHost.offset(&doc, &(0, 0)), 0);
    }
}
