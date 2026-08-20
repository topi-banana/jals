//! Whole-document formatting via `jals-fmt`.

use async_lsp::lsp_types::{Position, Range, TextEdit};
use jals_config::FeatureSet;
use jals_config::fmt::Config;
use jals_editor::Document;

use crate::host::LspHost;

/// One formatting response: the edits to apply, and whether the fail-safe refused the run.
///
/// The two are separate because they travel to different places. The edits answer the request; the
/// refusal is a message to the *person*, and only the actor holds the client handle to send it.
pub(crate) struct Formatted {
    /// The edits turning the document into its formatted form — empty when there are none to make.
    pub(crate) edits: Vec<TextEdit>,
    /// Whether the formatter refused its own output, so the document was left as it is.
    pub(crate) fell_back: bool,
}

pub(crate) use api::formatting_edits;

/// Whole-document formatting via `jals-fmt`.
mod api {
    use super::{Config, Document, FeatureSet, Formatted, LspHost, Position, Range, TextEdit};

    /// Format the whole document. Returns a single full-range text edit, or no edits when the
    /// document is already formatted. Async because formatting yields cooperatively.
    ///
    /// A run the formatter cannot vouch for also produces no edits — the text it hands back *is* the
    /// document — but for the opposite reason, so it is reported rather than merged into "nothing to
    /// do". Both cases look identical to the editor, which is precisely why the server has to be the
    /// one that can tell them apart; there is no diagnostic to publish, since the fail-safe's subject
    /// is the whole file and not a range in it.
    ///
    /// Stderr is not enough on its own: most clients keep the server's log out of sight, so a
    /// `warning:` line there reads to the user as the *absence* of a reaction — the same symptom the
    /// fallback already has. The line is kept as the server's own log, and [`Formatted::fell_back`]
    /// carries the fact out to the actor, which has the client handle a `window/showMessage` needs.
    ///
    /// `features` is the owning project's `[package] features`, or the empty set for a document
    /// that belongs to no workspace — the same fallback the rest of this file already makes for a
    /// detached document, and the safe one: the formatter's single dialect-emitting rule rounds
    /// itself away rather than writing syntax the project cannot compile.
    pub(crate) async fn formatting_edits(
        doc: &Document,
        config: &Config,
        features: FeatureSet,
    ) -> Formatted {
        let out = jals_fmt::FormatOutput::format_source(&doc.text, config, features).await;
        if out.fell_back() {
            eprintln!(
                "jals-lsp: the formatter could not vouch for its output; the document was left \
                 unchanged"
            );
            return Formatted {
                edits: Vec::new(),
                fell_back: true,
            };
        }
        let formatted = out.formatted;
        let edits = if formatted == *doc.text {
            Vec::new()
        } else {
            vec![TextEdit {
                range: Range {
                    start: Position::new(0, 0),
                    end: LspHost::position(doc, doc.text.len()),
                },
                new_text: formatted,
            }]
        };
        Formatted {
            edits,
            fell_back: false,
        }
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
            let out = api::formatting_edits(&doc, &Config::default(), FeatureSet::default()).await;
            assert!(out.edits.is_empty());
            // The distinction the actor turns into a `window/showMessage`: nothing to do is not the
            // same answer as a refusal, even though both produce no edits.
            assert!(!out.fell_back);
        });
    }
}
