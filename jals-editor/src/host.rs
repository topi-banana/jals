//! The host abstraction: how an editor front-end encodes coordinates and renders the neutral
//! analysis payloads.
//!
//! Every semantic computation in this crate answers in neutral shapes — byte offsets,
//! [`FileRange`](crate::FileRange)s, [`FileDiagnostic`]s, [`OutlineNode`]s. What differs between
//! front-ends is only the *rendering*: the LSP encodes positions as zero-based UTF-16
//! `Position`s and diagnostics as `lsp_types::Diagnostic`, Monaco as one-based UTF-16 ranges and
//! markers. [`EditorHost`] captures exactly that rendering surface, so the
//! [`Editor`](crate::Editor) facade can drive the whole query pipeline generically and the two
//! hosts stay symmetric: each is one implementation of the same trait, with no analysis
//! sequencing of its own.
//!
//! The wire-format extensions ([`SemanticTokensHost`], [`FoldingHost`], [`SelectionHost`]) are
//! separate traits so a host that does not surface a feature (the browser today) simply does not
//! implement it — the corresponding [`Editor`](crate::Editor) methods only exist for hosts that
//! do.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;
use jals_storage::FileKey;

use crate::document::Document;
use crate::{
    Completion, FileDiagnostic, Fold, Highlight, OutlineNode, SemanticToken, SignatureHelpUtf16,
};

/// A protocol front-end: coordinate encoding plus the payload constructors for each query result.
///
/// Implementations are stateless (a zero-sized type per host); every method receives the
/// [`Document`] whose cached [`LineIndex`](crate::LineIndex) the coordinate conversion needs.
pub trait EditorHost {
    /// The host's cursor position (LSP `Position`, Monaco's one-based line/column pair).
    type Position;
    /// The host's range within one document.
    type Range;
    /// The host's cross-file target (a URI/path plus a range).
    type Location;
    /// The host's diagnostic payload.
    type Diagnostic;
    /// The host's document-symbol payload.
    type Symbol;
    /// The host's completion payload.
    type Completion;
    /// The host's occurrence-highlight payload.
    type Highlight;
    /// The host's hover payload.
    type Hover;
    /// The host's signature-help payload.
    type SignatureHelp;

    /// Decode a host position in `doc` to a byte offset.
    fn offset(&self, doc: &Document, position: &Self::Position) -> usize;
    /// Encode a byte range in `doc` as a host range.
    fn range(&self, doc: &Document, range: Range<usize>) -> Self::Range;
    /// Encode a byte range in the file at `path` (whose cached document is `doc`) as a host
    /// cross-file target.
    fn location(&self, path: &FileKey, doc: &Document, range: Range<usize>) -> Self::Location;
    /// Render one neutral diagnostic of `doc`.
    fn diagnostic(&self, doc: &Document, diagnostic: FileDiagnostic) -> Self::Diagnostic;
    /// Render one outline node of `doc`, whose children are already rendered.
    fn symbol(
        &self,
        doc: &Document,
        node: OutlineNode,
        children: Vec<Self::Symbol>,
    ) -> Self::Symbol;
    /// Render a whole outline of `doc` bottom-up through [`symbol`](Self::symbol) (each node's
    /// children first, then the node around them). [`Editor::outline`](crate::Editor::outline)
    /// drives it for indexed files; a host's fallback path for a document outside any workspace
    /// renders a raw [`Outline`](crate::Outline) through the same recursion.
    fn render_outline(&self, doc: &Document, nodes: Vec<OutlineNode>) -> Vec<Self::Symbol> {
        nodes
            .into_iter()
            .map(|mut node| {
                let children = self.render_outline(doc, core::mem::take(&mut node.children));
                self.symbol(doc, node, children)
            })
            .collect()
    }
    /// Render one completion candidate.
    fn completion(&self, completion: Completion) -> Self::Completion;
    /// Render one occurrence highlight of `doc`.
    fn highlight(&self, doc: &Document, highlight: Highlight) -> Self::Highlight;
    /// Render a hover's shared Markdown.
    fn hover(&self, markdown: String) -> Self::Hover;
    /// Render signature help (parameter spans already in UTF-16 code units).
    fn signature_help(&self, help: SignatureHelpUtf16) -> Self::SignatureHelp;
}

/// A host that surfaces semantic tokens: it owns the wire encoding (the LSP's legend indices,
/// delta encoding, and one-token-per-line splitting) over the neutral classification.
pub trait SemanticTokensHost: EditorHost {
    /// The host's encoded token set for one document.
    type SemanticTokens;

    /// Encode `doc`'s classified tokens (in document order, byte ranges).
    fn semantic_tokens(&self, doc: &Document, tokens: Vec<SemanticToken>) -> Self::SemanticTokens;
}

/// A host that surfaces folding ranges. Folds are already line-based; the host maps the line
/// numbers and kind to its protocol shape.
pub trait FoldingHost: EditorHost {
    /// The host's folding-range payload.
    type FoldingRange;

    /// Render one fold.
    fn fold(&self, fold: Fold) -> Self::FoldingRange;
}

/// A host that surfaces selection (expand/shrink) ranges.
pub trait SelectionHost: EditorHost {
    /// The host's selection payload for one cursor position.
    type SelectionRange;

    /// Render one nested chain (innermost first, strictly nesting) for a cursor in `doc`.
    fn selection(&self, doc: &Document, chain: Vec<Range<usize>>) -> Self::SelectionRange;
}
