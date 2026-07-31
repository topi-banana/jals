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
    ///
    /// A run the formatter cannot vouch for also produces no edits — the text it hands back *is* the
    /// document — but for the opposite reason, so it is logged. Both cases look identical to the
    /// editor, which is precisely why the server has to be the one that can tell them apart; there is
    /// no diagnostic to publish, since the fail-safe's subject is the whole file and not a range in
    /// it.
    pub(crate) async fn formatting_edits(doc: &Document, config: &Config) -> Vec<TextEdit> {
        let out = jals_fmt::FormatOutput::format_source(&doc.text, config).await;
        if out.fell_back() {
            eprintln!(
                "jals-lsp: the formatter could not vouch for its output; the document was left \
                 unchanged"
            );
            return Vec::new();
        }
        let formatted = out.formatted;
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
