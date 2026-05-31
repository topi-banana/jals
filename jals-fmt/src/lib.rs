//! A pretty-printer for JALS/Java source, driven by the `jals-syntax` CST.
//!
//! [`format_source`] parses `src`, lowers the lossless CST into a Wadler/Prettier-style
//! document, and renders it back to text using a [`Config`]. It never panics: a source
//! with syntax errors is still formatted best-effort (the CST is lossless), and the
//! errors are surfaced as [`Warning`]s.
//!
//! Invariants the formatter upholds:
//! - **Significant tokens are preserved.** The sequence of non-trivia tokens of the
//!   output equals that of the input (only whitespace/comment layout changes).
//! - **Comments are never dropped or reordered.**
//! - **Idempotent.** `format(format(x)) == format(x)`.

mod comments;
mod config;
mod doc;
mod lower;
mod output;
mod render;

pub use config::{Config, ConfigError, IndentStyle, LineEnding};
pub use output::{FormatOutput, Warning};

/// Format `src` according to `config`.
pub fn format_source(src: &str, config: &Config) -> FormatOutput {
    let parse = jals_syntax::parse(src);
    let root = parse.syntax();
    let doc = lower::lower_root(&root);
    let formatted = render::print(&doc, config);
    let warnings = parse
        .errors()
        .iter()
        .map(Warning::from_syntax_error)
        .collect();
    FormatOutput {
        formatted,
        warnings,
    }
}
