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

pub(crate) use completion::{completions_local, completions_to_lsp};
pub(crate) use definition::goto_definition_local;
pub(crate) use diagnostics::{
    compute_diagnostics, compute_lint_diagnostics, compute_type_diagnostics,
    compute_type_mismatch_diagnostics,
};
pub(crate) use document_highlight::document_highlight;
pub(crate) use folding_range::folding_range;
pub(crate) use formatting::formatting_edits;
pub(crate) use hover::{hover_local, type_hover};
pub(crate) use references::references;
pub(crate) use rename::{
    is_renamable_kind, is_valid_identifier, prepare_rename_local, rename_local,
};
pub(crate) use selection_range::selection_ranges;
pub(crate) use semantic_tokens::{legend as semantic_tokens_legend, semantic_tokens};
pub(crate) use signature_help::{signature_help_local, signature_help_to_lsp};
pub(crate) use symbols::document_symbols;

/// The `IDENT` token at `offset`, preferring it when a token boundary yields two tokens — so a
/// cursor at the end of a word still anchors to it (standard editor UX). Shared by the
/// resolution-aware handlers (document-highlight, references) and the project workspace.
pub(crate) fn ident_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    root.token_at_offset(offset)
        .find(|token| token.kind() == SyntaxKind::IDENT)
}
