//! Pure request handlers: each maps document text (and config) to an LSP payload, with no
//! I/O or async. This is the unit-testable core of the server.

mod diagnostics;
mod formatting;
mod semantic_tokens;
mod symbols;

pub(crate) use diagnostics::compute_diagnostics;
pub(crate) use formatting::formatting_edits;
pub(crate) use semantic_tokens::{legend as semantic_tokens_legend, semantic_tokens};
pub(crate) use symbols::document_symbols;
