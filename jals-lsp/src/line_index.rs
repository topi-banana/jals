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

    /// Convert an LSP `Position` to a byte offset (the inverse of `position`).
    ///
    /// Lines past the end of the document, and characters past the end of a line, are
    /// clamped; a character column that would land inside a multi-byte char stops at that
    /// char's start. So this never panics and never returns a non-boundary offset.
    pub(crate) fn offset(&self, text: &str, position: Position) -> TextSize {
        let Some(&line_start) = self.line_starts.get(position.line as usize) else {
            return TextSize::from(self.len); // line past EOF -> clamp to end of document
        };
        let line_start = line_start as usize;
        let line_end = self
            .line_starts
            .get(position.line as usize + 1)
            .map_or(self.len as usize, |&s| s as usize);

        // Walk UTF-16 columns from the line start until `character` units are consumed.
        let mut remaining = position.character;
        let mut off = line_start;
        for c in text[line_start..line_end].chars() {
            let w = c.len_utf16() as u32;
            if remaining < w {
                break; // not enough columns left to step over this char
            }
            remaining -= w;
            off += c.len_utf8();
        }
        TextSize::from(off as u32)
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

    /// Helper: (line, character) -> byte offset.
    fn off(idx: &LineIndex, text: &str, line: u32, character: u32) -> u32 {
        u32::from(idx.offset(text, Position { line, character }))
    }

    #[test]
    fn offset_round_trips_with_position() {
        let text = "abc\ndef\nghi";
        let idx = LineIndex::new(text);
        for o in 0..=text.len() as u32 {
            let p = idx.position(text, TextSize::from(o));
            assert_eq!(off(&idx, text, p.line, p.character), o, "offset {o}");
        }
    }

    #[test]
    fn offset_counts_utf16_columns() {
        // 'あ' = 3 UTF-8 bytes / 1 UTF-16 unit; '😀' = 4 bytes / 2 units.
        let text = "aあ😀b";
        let idx = LineIndex::new(text);
        assert_eq!(off(&idx, text, 0, 0), 0); // before 'a'
        assert_eq!(off(&idx, text, 0, 1), 1); // after 'a'
        assert_eq!(off(&idx, text, 0, 2), 4); // after 'あ'
        assert_eq!(off(&idx, text, 0, 4), 8); // after '😀' (2 UTF-16 units)
        assert_eq!(off(&idx, text, 0, 5), 9); // after 'b'
    }

    #[test]
    fn offset_clamps_out_of_range() {
        let text = "ab\ncd";
        let idx = LineIndex::new(text);
        assert_eq!(off(&idx, text, 0, 2), 2); // exact end of line 0 content
        assert_eq!(off(&idx, text, 0, 99), 3); // past line 0 -> clamps to its end
        assert_eq!(off(&idx, text, 9, 0), 5); // line past EOF -> end of document
    }
}
