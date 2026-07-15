//! Adapts shared UTF-8/UTF-16 coordinate conversion to LSP types.

use async_lsp::lsp_types::{Position, Range};
use jals_editor::Utf16Position;
use text_size::{TextRange, TextSize};

/// Coordinate index for one LSP document.
pub(crate) struct LineIndex(jals_editor::LineIndex);

impl LineIndex {
    pub(crate) fn new(text: &str) -> Self {
        Self(jals_editor::LineIndex::new(text))
    }

    /// Convert a byte offset to a zero-based LSP UTF-16 position.
    pub(crate) fn position(&self, text: &str, offset: TextSize) -> Position {
        let position = self.0.position(text, u32::from(offset) as usize);
        Position {
            line: position.line,
            character: position.character,
        }
    }

    /// Convert a byte range to an LSP range.
    pub(crate) fn range(&self, text: &str, range: TextRange) -> Range {
        Range {
            start: self.position(text, range.start()),
            end: self.position(text, range.end()),
        }
    }

    /// Convert a standard byte range to an LSP range.
    pub(crate) fn byte_range(&self, text: &str, range: &std::ops::Range<usize>) -> Range {
        self.range(
            text,
            TextRange::new(
                TextSize::from(u32::try_from(range.start).unwrap_or(u32::MAX)),
                TextSize::from(u32::try_from(range.end).unwrap_or(u32::MAX)),
            ),
        )
    }

    /// Convert a zero-based LSP UTF-16 position to a byte offset.
    pub(crate) fn offset(&self, text: &str, position: Position) -> TextSize {
        let offset = self.0.offset(
            text,
            Utf16Position {
                line: position.line,
                character: position.character,
            },
        );
        TextSize::from(u32::try_from(offset).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_positions_are_zero_based_and_use_text_size() {
        let text = "a😀\nb";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, TextSize::from(5)), Position::new(0, 3));
        assert_eq!(index.position(text, TextSize::from(6)), Position::new(1, 0));
        assert_eq!(index.offset(text, Position::new(0, 3)), TextSize::from(5));
    }

    #[test]
    fn typed_range_maps_both_ends() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        let range = index.range(text, TextRange::new(TextSize::from(1), TextSize::from(3)));
        assert_eq!(range, Range::new(Position::new(0, 1), Position::new(1, 0)));
    }

    #[test]
    fn standard_byte_range_maps_both_ends() {
        let text = "aあ\nb";
        let index = LineIndex::new(text);
        let range = index.byte_range(text, &(1..5));
        assert_eq!(range, Range::new(Position::new(0, 1), Position::new(1, 0)));
    }
}
