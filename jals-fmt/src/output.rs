//! The result of formatting: the rendered text plus any warnings.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxError;

/// A non-fatal diagnostic surfaced while formatting.
///
/// Currently these are the syntax errors recorded by the parser; formatting still
/// proceeds best-effort because the CST is lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Human-readable message.
    pub message: String,
    /// Byte range in the original source.
    pub range: Range<usize>,
}

impl Warning {
    pub(crate) fn from_syntax_error(err: &SyntaxError) -> Self {
        let range = err.range();
        Self {
            message: err.message().to_string(),
            range: usize::from(range.start())..usize::from(range.end()),
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
