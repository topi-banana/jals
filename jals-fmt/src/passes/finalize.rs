//! L4 output finalization — the line-level shape of the file.
//!
//! Everything here is engine-independent text work, the same generic steps Spotless offers
//! (`trimTrailingWhitespace()`, `endWithNewline()`, line-ending normalization). The engine always
//! emits `\n`, so this is the single place the configured terminator is applied and the only
//! place that has to know about `\r`.

use alloc::string::String;

use crate::style::Style;

pub(crate) use api::apply;

/// Applies `[layout]`'s output-shape rules to the rendered text.
pub(crate) mod api {
    use super::{String, Style};

    /// Trim, normalize terminators, and settle the final newline.
    ///
    /// `src` is the text this run started from, and the only thing read off it is whether it
    /// ended with a newline. `insert-final-newline` **adds** one that is missing; it is not a
    /// licence to take away one that is there — Eclipse spells the very same option
    /// `insert_new_line_at_end_of_file_if_missing`, and Spotless's `endWithNewline()` likewise
    /// only appends. Treating the `false` side as "strip" made every file of a corpus formatted
    /// with a stock Eclipse profile differ on its last line.
    pub(crate) fn apply(text: &str, src: &str, style: &Style) -> String {
        let layout = &style.cfg.layout;
        let newline = style.newline;

        let body = text.trim_end_matches(['\n', '\r', ' ', '\t']);
        let mut out = String::with_capacity(text.len() + newline.len());

        for (nth, line) in body.split('\n').enumerate() {
            if nth > 0 {
                out.push_str(newline);
            }
            let line = line.trim_end_matches('\r');
            // Trimming wins over keeping an indent on an empty line when both are on: the two
            // settings contradict each other and every native formatter resolves it this way.
            let kept = if layout.trim_trailing_whitespace {
                line.trim_end_matches([' ', '\t'])
            } else if layout.indent_empty_lines {
                line
            } else if line.trim_matches([' ', '\t']).is_empty() {
                ""
            } else {
                line
            };
            out.push_str(kept);
        }

        if (layout.insert_final_newline || src.ends_with('\n')) && !out.is_empty() {
            out.push_str(newline);
        }
        out
    }
}
