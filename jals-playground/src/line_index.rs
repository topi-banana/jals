//! Maps `jals` byte offsets to Monaco editor positions.
//!
//! `jals-syntax` / `jals-hir` / `jals-lint` report ranges as UTF-8 byte offsets, while Monaco
//! positions are one-based line + one-based UTF-16 code-unit column. This index precomputes line
//! starts so each conversion is a binary search plus a short per-line scan.
//!
//! Ported from `jals-lsp`'s `line_index.rs`, with the LSP `Position`/`Range` types dropped in
//! favour of plain byte-offset `usize` inputs and Monaco's one-based coordinates.

use core::ops::Range;

/// Precomputed line-start offsets for one document.
pub struct LineIndex {
    /// Byte offset of the start of each line; always begins with `0`.
    line_starts: Vec<usize>,
    /// Total byte length, used to clamp offsets past the end of the document.
    len: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex {
            line_starts,
            len: text.len(),
        }
    }

    /// Convert a byte offset to a zero-based `(line, utf16_column)`.
    ///
    /// `text` must be the source this index was built from. Offsets past the end, or not on a
    /// char boundary, are clamped so this never panics.
    fn position(&self, text: &str, offset: usize) -> (u32, u32) {
        let off = clamp_to_boundary(text, offset.min(self.len));
        let line = self.line_starts.partition_point(|&start| start <= off) - 1;
        let line_start = self.line_starts[line];
        let character: u32 = text[line_start..off]
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum();
        (line as u32, character)
    }

    /// Convert a byte range to a Monaco `(startLine, startColumn, endLine, endColumn)` tuple —
    /// one-based line and UTF-16 column, for `monaco.editor.IMarkerData`.
    pub fn to_monaco(&self, text: &str, range: &Range<usize>) -> (u32, u32, u32, u32) {
        let (sl, sc) = self.position(text, range.start);
        let (el, ec) = self.position(text, range.end);
        (sl + 1, sc + 1, el + 1, ec + 1)
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

    /// Helper: byte offset -> zero-based (line, character).
    fn at(idx: &LineIndex, text: &str, off: usize) -> (u32, u32) {
        idx.position(text, off)
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
    fn to_monaco_is_one_based_over_both_ends() {
        let text = "ab\ncd";
        let idx = LineIndex::new(text);
        // bytes 1..3 = 'b' on line 0 through the start of line 1.
        assert_eq!(idx.to_monaco(text, &(1..3)), (1, 2, 2, 1));
    }
}
