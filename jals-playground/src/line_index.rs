//! Adapts shared UTF-8/UTF-16 coordinate conversion to Monaco's one-based positions.

use core::ops::Range;

use jals_editor::Utf16Position;

/// Coordinate index for one Monaco document.
pub struct LineIndex(jals_editor::LineIndex);

impl LineIndex {
    pub fn new(text: &str) -> Self {
        Self(jals_editor::LineIndex::new(text))
    }

    /// Convert a byte range to Monaco's one-based line and UTF-16 column coordinates.
    pub fn to_monaco(&self, text: &str, range: &Range<usize>) -> (u32, u32, u32, u32) {
        let start = self.0.position(text, range.start);
        let end = self.0.position(text, range.end);
        (
            start.line + 1,
            start.character + 1,
            end.line + 1,
            end.character + 1,
        )
    }

    /// Convert a Monaco one-based line and UTF-16 column to a byte offset.
    pub fn offset(&self, text: &str, line: u32, col: u32) -> usize {
        self.0.offset(
            text,
            Utf16Position {
                line: line.saturating_sub(1),
                character: col.saturating_sub(1),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monaco_ranges_are_one_based() {
        let text = "a😀\nb";
        let index = LineIndex::new(text);
        assert_eq!(index.to_monaco(text, &(1..6)), (1, 2, 2, 1));
    }

    #[test]
    fn monaco_positions_are_one_based() {
        let text = "a😀\nb";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, 1, 2), 1);
        assert_eq!(index.offset(text, 1, 4), 5);
        assert_eq!(index.offset(text, 2, 1), 6);
        assert_eq!(index.offset(text, 0, 0), 0);
    }
}
