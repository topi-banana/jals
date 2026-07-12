//! Whole-document formatting via `jals-fmt`.

use async_lsp::lsp_types::{Position, Range, TextEdit};
use jals_config::fmt::Config;
use text_size::TextSize;

use crate::line_index::LineIndex;

/// Whole-document formatting via `jals-fmt`.
pub(crate) struct Formatting;

impl Formatting {
    /// Format the whole document. Returns a single full-range text edit, or no edits when the
    /// document is already formatted.
    pub(crate) fn formatting_edits(
        text: &str,
        config: &Config,
        line_index: &LineIndex,
    ) -> Vec<TextEdit> {
        let formatted = jals_fmt::FormatOutput::format_source(text, config).formatted;
        if formatted == text {
            return Vec::new();
        }
        let end = line_index.position(text, TextSize::from(text.len() as u32));
        vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end,
            },
            new_text: formatted,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_formatted_yields_no_edits() {
        let text = "class C {\n    int x = 1;\n}\n";
        let edits = Formatting::formatting_edits(text, &Config::default(), &LineIndex::new(text));
        assert!(edits.is_empty());
    }

    #[test]
    fn unformatted_source_yields_one_full_range_edit() {
        let text = "class C{int x=1;}";
        let edits = Formatting::formatting_edits(text, &Config::default(), &LineIndex::new(text));
        assert_eq!(edits.len(), 1);
        assert_eq!(
            edits[0].range.start,
            Position {
                line: 0,
                character: 0
            }
        );
        // The edit replaces the whole document with the formatted text.
        assert!(edits[0].new_text.starts_with("class C {"));
        assert_ne!(edits[0].new_text, text);
    }
}
