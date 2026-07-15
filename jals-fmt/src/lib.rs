#![cfg_attr(not(test), no_std)]
//! A pretty-printer for JALS/Java source, driven by the `jals-syntax` CST.
//!
//! [`FormatOutput::format_source`] parses `src`, lowers the lossless CST into a Wadler/Prettier-style
//! document, and renders it back to text using a [`Config`]. It never panics: a source
//! with syntax errors is still formatted best-effort (the CST is lossless), and the
//! errors are surfaced as [`Warning`]s.
//!
//! Invariants the formatter upholds:
//! - **Significant tokens are preserved.** The sequence of non-trivia tokens of the output
//!   equals that of the input (only whitespace/comment layout changes), with seven opt-in
//!   exceptions, each off by default: [`Config::reorder_imports`] may reorder import
//!   declarations (the token *multiset* is still preserved), [`Config::group_imports`] may
//!   reorder imports into prefix-defined groups (multiset preserved; it overrides
//!   `reorder_imports`), [`Config::reorder_modifiers`] may reorder a declaration's modifiers
//!   into canonical order, hoisting annotations to the front (multiset preserved),
//!   [`Config::trailing_comma`] (when not [`Preserve`](jals_config::fmt::TrailingComma::Preserve))
//!   may add or drop the single trailing comma of an array initializer,
//!   [`Config::hex_literal_case`] (when not [`Preserve`](jals_config::fmt::HexLiteralCase::Preserve))
//!   may rewrite the case of the hex digits of an integer / float literal,
//!   [`Config::float_literal_trailing_zero`] (when not
//!   [`Preserve`](jals_config::fmt::FloatLiteralTrailingZero::Preserve)) may add or strip the
//!   trailing zero of a decimal float literal, and [`Config::literal_suffix_case`] (when not
//!   [`Preserve`](jals_config::fmt::LiteralSuffixCase::Preserve)) may rewrite the case of an
//!   integer / float
//!   literal's trailing `l` / `f` / `d` type suffix (the token *kind* sequence is preserved
//!   exactly; only a literal's text may change).
//! - **Comments are never dropped.** Each stays glued to its anchoring token, so a comment
//!   moves with its token when that token is reordered; a dropped trailing comma that carries a
//!   comment is kept.
//! - **Idempotent.** `format(format(x)) == format(x)`.

mod comments;
mod config;
mod doc;
mod lower;
mod output;
mod render;
mod rules;
mod wrap;

use config::Config;

pub use output::{FormatOutput, Warning};

impl FormatOutput {
    /// Format `src` according to `config`.
    pub fn format_source(src: &str, config: &Config) -> Self {
        let parse = jals_syntax::Parse::parse(src);
        let root = parse.syntax();
        let doc = lower::Ctx::lower_root(&root, config);
        let formatted = render::Out::print(&doc, config, src);
        let warnings = parse
            .errors()
            .iter()
            .map(Warning::from_syntax_error)
            .collect();
        Self {
            formatted,
            warnings,
        }
    }
}
