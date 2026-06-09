//! Maps `jals-syntax` byte offsets to LSP UTF-16 positions.
//!
//! `jals-syntax` reports ranges as UTF-8 byte offsets (`text_size::TextRange`), while
//! LSP `Position`s are zero-based line + UTF-16 code-unit column. This index precomputes
//! line starts so each conversion is a binary search plus a short per-line scan.

use async_lsp::lsp_types::{Position, Range};
use text_size::{TextRange, TextSize};

/// Precomputed line-start offsets for one document.
pub(crate) struct LineIndex {
    /// Byte offset of the start of each line; always begins with `0`.
    line_starts: Vec<u32>,
    /// Total byte length, used to clamp offsets past the end of the document.
    len: u32,
}

impl LineIndex {
    pub(crate) fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push((i + 1) as u32);
            }
        }
        LineIndex {
            line_starts,
            len: text.len() as u32,
        }
    }

    /// Convert a byte offset to an LSP `Position`.
    ///
    /// `text` must be the source this index was built from. Offsets past the end, or not
    /// on a char boundary, are clamped so this never panics (syntax invariant #2).
    pub(crate) fn position(&self, text: &str, offset: TextSize) -> Position {
        let off = clamp_to_boundary(text, u32::from(offset).min(self.len) as usize);
        let line = self
            .line_starts
            .partition_point(|&start| start as usize <= off)
            - 1;
        let line_start = self.line_starts[line] as usize;
        let character = text[line_start..off]
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum();
        Position {
            line: line as u32,
            character,
        }
    }

    /// Convert a byte range to an LSP `Range`.
    pub(crate) fn range(&self, text: &str, range: TextRange) -> Range {
        Range {
            start: self.position(text, range.start()),
            end: self.position(text, range.end()),
        }
    }
}

/// Round `off` down to the nearest UTF-8 char boundary in `text`.
fn clamp_to_boundary(text: &str, off: usize) -> usize {
    if off >= text.len() {
        return text.len();
    }
    (0..=off)
        .rev()
        .find(|&o| text.is_char_boundary(o))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: byte offset -> (line, character).
    fn at(idx: &LineIndex, text: &str, off: u32) -> (u32, u32) {
        let p = idx.position(text, TextSize::from(off));
        (p.line, p.character)
    }

    #[test]
    fn ascii_multiline() {
        let text = "abc\ndef\nghi";
        let idx = LineIndex::new(text);
        assert_eq!(at(&idx, text, 0), (0, 0));
        assert_eq!(at(&idx, text, 2), (0, 2));
        assert_eq!(at(&idx, text, 4), (1, 0)); // 'd'
        assert_eq!(at(&idx, text, 8), (2, 0)); // 'g'
        assert_eq!(at(&idx, text, 11), (2, 3)); // end of file
    }

    #[test]
    fn crlf_keeps_cr_on_previous_line() {
        let text = "ab\r\ncd";
        let idx = LineIndex::new(text);
        assert_eq!(at(&idx, text, 2), (0, 2)); // '\r'
        assert_eq!(at(&idx, text, 3), (0, 3)); // '\n'
        assert_eq!(at(&idx, text, 4), (1, 0)); // 'c'
    }

    #[test]
    fn empty_and_out_of_range_clamp() {
        let text = "";
        let idx = LineIndex::new(text);
        assert_eq!(at(&idx, text, 0), (0, 0));
        assert_eq!(at(&idx, text, 99), (0, 0));
    }

    #[test]
    fn bmp_multibyte_counts_one_utf16_unit() {
        // 'あ' = 3 UTF-8 bytes, 1 UTF-16 unit.
        let text = "aあb";
        let idx = LineIndex::new(text);
        assert_eq!(at(&idx, text, 1), (0, 1)); // after 'a'
        assert_eq!(at(&idx, text, 4), (0, 2)); // after 'あ'
        assert_eq!(at(&idx, text, 5), (0, 3)); // after 'b'
    }

    #[test]
    fn astral_counts_surrogate_pair() {
        // '😀' = 4 UTF-8 bytes, 2 UTF-16 units.
        let text = "x😀y";
        let idx = LineIndex::new(text);
        assert_eq!(at(&idx, text, 1), (0, 1)); // after 'x'
        assert_eq!(at(&idx, text, 5), (0, 3)); // after '😀' (1 + 2)
        assert_eq!(at(&idx, text, 6), (0, 4)); // after 'y'
    }

    #[test]
    fn non_boundary_offset_clamps_down() {
        let text = "あ"; // bytes 0..3
        let idx = LineIndex::new(text);
        assert_eq!(at(&idx, text, 1), (0, 0)); // inside 'あ' -> clamp to start
        assert_eq!(at(&idx, text, 3), (0, 1)); // after 'あ'
    }

    #[test]
    fn range_maps_both_ends() {
        let text = "ab\ncd";
        let idx = LineIndex::new(text);
        let r = idx.range(text, TextRange::new(TextSize::from(1), TextSize::from(3)));
        assert_eq!((r.start.line, r.start.character), (0, 1)); // 'b'
        assert_eq!((r.end.line, r.end.character), (1, 0)); // 'c'
    }
}
