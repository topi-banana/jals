//! The result of formatting: the rendered text plus any warnings.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxError;

/// A non-fatal diagnostic surfaced while formatting.
///
/// Two kinds arrive here. The parser's syntax errors carry a source range and formatting still
/// proceeds best-effort, because the CST is lossless. Configuration diagnostics — a rule that
/// reads input whitespace being rounded to the single engine's canonical value
/// (`DESIGN.md` §17) — belong to the `Config`, not to any position in the file, so their
/// [`range`](Self::range) is `None`. Rounding is reported rather than applied silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Human-readable message.
    pub message: String,
    /// Byte range in the original source, or `None` for a diagnostic about the configuration.
    pub range: Option<Range<usize>>,
}

impl Warning {
    pub(crate) fn from_syntax_error(err: &SyntaxError) -> Self {
        let range = err.range();
        Self {
            message: err.message().to_owned(),
            range: Some(usize::from(range.start())..usize::from(range.end())),
        }
    }

    /// A diagnostic about the configuration itself, with no position in the source.
    pub(crate) const fn config(message: String) -> Self {
        Self {
            message,
            range: None,
        }
    }
}

/// The output of [`FormatOutput::format_source`].
#[derive(Debug, Clone)]
pub struct FormatOutput {
    /// The formatted source text.
    pub formatted: String,
    /// Warnings collected during formatting.
    pub warnings: Vec<Warning>,
}

impl FormatOutput {
    /// Whether any warnings were produced.
    pub const fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}
