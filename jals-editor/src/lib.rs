#![no_std]
//! Editor-independent coordinates and semantic project queries.
//!
//! Parsers in the `jals` workspace use UTF-8 byte offsets, while editor protocols commonly use
//! line and UTF-16 code-unit coordinates. This crate owns that shared conversion without depending
//! on an editor protocol or coordinate base. Protocol adapters are responsible for mapping
//! [`Utf16Position`] to their own position types and applying any one-based coordinates.

extern crate alloc;

mod diagnostics;
mod document;
mod editor;
mod folding;
mod host;
mod outline;
mod queries;
mod selection;
mod semantic;
mod workspace;

pub use diagnostics::{DiagnosticSeverity, FileDiagnostic, FileDiagnostics};
pub use document::Document;
pub use editor::Editor;
pub use folding::{Fold, FoldKind, Folds};
pub use host::{EditorHost, FoldingHost, SelectionHost, SemanticTokensHost};
pub use outline::{Outline, OutlineNode};
pub use queries::{
    Completion, CompletionKind, FileRange, Highlight, HighlightKind, Ident, ProjectQueries,
    QueryFile, SignatureHelpUtf16, SignatureUtf16,
};
pub use selection::SelectionChains;
pub use semantic::{SemanticToken, SemanticTokenKind, SemanticTokens};
pub use workspace::{ProjectLayout, SingleFileProject, Workspace};

pub(crate) use ranges::{byte_range, sat_text_size};

/// `text_size` ↔ byte-range conversions shared by the query modules.
mod ranges {
    use core::ops::Range;

    use text_size::{TextRange, TextSize};

    /// A `text_size::TextRange` as a plain byte `Range<usize>`.
    pub(crate) fn byte_range(range: TextRange) -> Range<usize> {
        usize::from(range.start())..usize::from(range.end())
    }

    /// `offset` as a `TextSize`, saturating at `u32::MAX` instead of panicking on 64-bit hosts.
    pub(crate) fn sat_text_size(offset: usize) -> TextSize {
        TextSize::from(u32::try_from(offset).unwrap_or(u32::MAX))
    }
}

use alloc::{vec, vec::Vec};

/// A zero-based line and UTF-16 code-unit coordinate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Utf16Position {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based UTF-16 code-unit offset within the line.
    pub character: u32,
}

/// Precomputed line-start byte offsets for one document.
#[derive(Clone, Debug)]
pub struct LineIndex {
    line_starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Build an index for `text`.
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            line_starts,
            len: text.len(),
        }
    }

    /// Convert a UTF-8 byte offset to a zero-based UTF-16 position.
    ///
    /// `text` must be the source this index was built from. An offset past EOF is clamped to EOF,
    /// and an offset within a UTF-8 code point is rounded down to the nearest character boundary.
    pub fn position(&self, text: &str, byte_offset: usize) -> Utf16Position {
        let offset = Self::clamp_to_boundary(text, byte_offset.min(self.len));
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        let line_start = self.line_starts[line];
        let character = text[line_start..offset]
            .chars()
            .map(|character| match character.len_utf16() {
                1 => 1_u32,
                _ => 2_u32,
            })
            .fold(0_u32, u32::saturating_add);
        Utf16Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character,
        }
    }

    /// Convert a zero-based UTF-16 position to a UTF-8 byte offset.
    ///
    /// `text` must be the source this index was built from. Missing lines clamp to EOF and columns
    /// beyond a line clamp to the next line start (or EOF for the final line). A column within a
    /// surrogate pair stops at the start of that code point.
    pub fn offset(&self, text: &str, position: Utf16Position) -> usize {
        let line = position.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.len;
        };
        let line_end = self.line_starts.get(line + 1).copied().unwrap_or(self.len);

        let mut remaining = position.character;
        let mut offset = line_start;
        for character in text[line_start..line_end].chars() {
            let width = match character.len_utf16() {
                1 => 1,
                _ => 2,
            };
            if remaining < width {
                break;
            }
            remaining -= width;
            offset += character.len_utf8();
        }
        offset
    }

    fn clamp_to_boundary(text: &str, offset: usize) -> usize {
        if offset >= text.len() {
            return text.len();
        }
        (0..=offset)
            .rev()
            .find(|&candidate| text.is_char_boundary(candidate))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Utf16Position {
        fn new(line: u32, character: u32) -> Self {
            Self { line, character }
        }
    }

    #[test]
    fn positions_in_ascii_and_lf_text() {
        let text = "abc\ndef\nghi";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 0), Utf16Position::new(0, 0));
        assert_eq!(index.position(text, 2), Utf16Position::new(0, 2));
        assert_eq!(index.position(text, 4), Utf16Position::new(1, 0));
        assert_eq!(index.position(text, 8), Utf16Position::new(2, 0));
        assert_eq!(index.position(text, 11), Utf16Position::new(2, 3));
    }

    #[test]
    fn positions_in_empty_text_clamp_to_origin() {
        let text = "";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 0), Utf16Position::new(0, 0));
        assert_eq!(index.position(text, 99), Utf16Position::new(0, 0));
        assert_eq!(index.offset(text, Utf16Position::new(0, 99)), 0);
    }

    #[test]
    fn crlf_keeps_carriage_return_on_previous_line() {
        let text = "ab\r\ncd";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 2), Utf16Position::new(0, 2));
        assert_eq!(index.position(text, 3), Utf16Position::new(0, 3));
        assert_eq!(index.position(text, 4), Utf16Position::new(1, 0));
    }

    #[test]
    fn bmp_and_astral_characters_use_utf16_widths() {
        let text = "aあ😀b";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 1), Utf16Position::new(0, 1));
        assert_eq!(index.position(text, 4), Utf16Position::new(0, 2));
        assert_eq!(index.position(text, 8), Utf16Position::new(0, 4));
        assert_eq!(index.position(text, 9), Utf16Position::new(0, 5));

        assert_eq!(index.offset(text, Utf16Position::new(0, 1)), 1);
        assert_eq!(index.offset(text, Utf16Position::new(0, 2)), 4);
        assert_eq!(index.offset(text, Utf16Position::new(0, 4)), 8);
        assert_eq!(index.offset(text, Utf16Position::new(0, 5)), 9);
    }

    #[test]
    fn offsets_inside_utf8_and_utf16_sequences_round_down() {
        let text = "aあ😀b";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 2), Utf16Position::new(0, 1));
        assert_eq!(index.position(text, 6), Utf16Position::new(0, 2));
        assert_eq!(index.offset(text, Utf16Position::new(0, 3)), 4);
    }

    #[test]
    fn out_of_range_offsets_lines_and_columns_clamp() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 99), Utf16Position::new(1, 2));
        assert_eq!(index.offset(text, Utf16Position::new(0, 2)), 2);
        assert_eq!(index.offset(text, Utf16Position::new(0, 99)), 3);
        assert_eq!(index.offset(text, Utf16Position::new(99, 0)), 5);
    }

    #[test]
    fn every_character_boundary_round_trips() {
        let text = "aあ😀\r\nβ\n";
        let index = LineIndex::new(text);
        for byte_offset in (0..=text.len()).filter(|&offset| text.is_char_boundary(offset)) {
            let position = index.position(text, byte_offset);
            assert_eq!(
                index.offset(text, position),
                byte_offset,
                "offset {byte_offset}"
            );
        }
    }
}
