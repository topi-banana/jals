//! 構文エラー(メッセージ + ソース範囲)。`jals-lsp` の診断などで使う。

use alloc::string::String;
use core::fmt;

use text_size::TextRange;

/// パース中に検出した構文エラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    message: String,
    range: TextRange,
}

impl SyntaxError {
    /// メッセージと範囲からエラーを作る。
    pub fn new(message: impl Into<String>, range: TextRange) -> Self {
        Self {
            message: message.into(),
            range,
        }
    }

    /// エラーメッセージ。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// ソース内の範囲(バイト)。
    pub fn range(&self) -> TextRange {
        self.range
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.message, self.range)
    }
}
