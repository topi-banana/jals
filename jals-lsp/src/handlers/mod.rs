//! Pure request handlers: each maps document text (and config) to an LSP payload, with no
//! I/O or async. This is the unit-testable core of the server.

mod diagnostics;
mod document_highlight;
mod folding_range;
mod formatting;
mod selection_range;
mod semantic_tokens;
mod symbols;

pub(crate) use diagnostics::compute_diagnostics;
pub(crate) use document_highlight::document_highlight;
pub(crate) use folding_range::folding_range;
pub(crate) use formatting::formatting_edits;
pub(crate) use selection_range::selection_ranges;
pub(crate) use semantic_tokens::{legend as semantic_tokens_legend, semantic_tokens};
pub(crate) use symbols::document_symbols;
