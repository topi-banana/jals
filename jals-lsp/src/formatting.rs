//! Whole-document formatting via `jals-fmt`.

use async_lsp::lsp_types::{Position, Range, TextEdit};
use jals_config::fmt::Config;
use jals_editor::Document;

use crate::host::LspHost;

/// Whole-document formatting via `jals-fmt`.
pub(crate) struct Formatting;

impl Formatting {
    /// Format the whole document. Returns a single full-range text edit, or no edits when the
    /// document is already formatted. Async because formatting yields cooperatively.
    pub(crate) async fn formatting_edits(doc: &Document, config: &Config) -> Vec<TextEdit> {
        let formatted = jals_fmt::FormatOutput::format_source(&doc.text, config)
            .await
            .formatted;
        if formatted == *doc.text {
            return Vec::new();
        }
        vec![TextEdit {
            range: Range {
                start: Position::new(0, 0),
                end: LspHost::position(doc, doc.text.len()),
            },
            new_text: formatted,
        }]
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;

    use super::*;

    #[test]
    fn already_formatted_yields_no_edits() {
        block_on_inline(async {
            let doc = Document::new("class C {\n    int x = 1;\n}\n".to_owned()).await;
            assert!(
                Formatting::formatting_edits(&doc, &Config::default())
                    .await
                    .is_empty()
            );
        });
    }

    #[test]
    fn unformatted_source_yields_one_full_range_edit() {
        block_on_inline(async {
            let text = "class C{int x=1;}";
            let doc = Document::new(text.to_owned()).await;
            let edits = Formatting::formatting_edits(&doc, &Config::default()).await;
            assert_eq!(edits.len(), 1);
            assert_eq!(edits[0].range.start, Position::new(0, 0));
            assert_eq!(
                edits[0].range.end,
                Position::new(0, text.len() as u32),
                "the edit covers the whole document"
            );
            // The edit replaces the whole document with the formatted text.
            assert!(edits[0].new_text.starts_with("class C {"));
            assert_ne!(edits[0].new_text, text);
        });
    }
}
