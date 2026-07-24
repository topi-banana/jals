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
}
