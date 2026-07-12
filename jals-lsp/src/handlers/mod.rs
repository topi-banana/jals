//! Pure request handlers: each maps document text (and config) to an LSP payload, with no
//! I/O or async. This is the unit-testable core of the server.

use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use text_size::TextSize;

mod completion;
mod definition;
mod diagnostics;
mod document_highlight;
mod folding_range;
mod formatting;
mod hover;
mod references;
mod rename;
mod selection_range;
mod semantic_tokens;
mod signature_help;
mod symbols;

pub(crate) use completion::Completions;
pub(crate) use definition::Definition;
pub(crate) use diagnostics::Diagnostics;
pub(crate) use document_highlight::DocumentHighlights;
pub(crate) use folding_range::FoldingRanges;
pub(crate) use formatting::Formatting;
pub(crate) use hover::Hovers;
pub(crate) use references::References;
pub(crate) use rename::Rename;
pub(crate) use selection_range::SelectionRanges;
pub(crate) use semantic_tokens::SemanticTokensBuilder;
pub(crate) use signature_help::SignatureHelpHandler;
pub(crate) use symbols::DocumentSymbols;

/// Cursor-anchoring helpers shared by the resolution-aware handlers (document-highlight,
/// references, rename) and the project workspace.
pub(crate) struct Cursor;

impl Cursor {
    /// The `IDENT` token at `offset`, preferring it when a token boundary yields two tokens — so a
    /// cursor at the end of a word still anchors to it (standard editor UX).
    pub(crate) fn ident_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
        root.token_at_offset(offset)
            .find(|token| token.kind() == SyntaxKind::IDENT)
    }
}
