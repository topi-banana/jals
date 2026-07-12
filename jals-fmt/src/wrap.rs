//! Comment reflow — the `wrap-comments` / `comment-width` feature.
//!
//! Re-wraps standalone comments so no line exceeds `comment-width` (counting the leading
//! indentation). Two properties are non-negotiable:
//!
//! - **Prose is preserved.** Only inter-word whitespace and continuation markers (`//`,
//!   ` * `) change; no word is ever added, dropped, reordered, or split.
//! - **Idempotent.** The output is always in a canonical shape, and re-running the reflow
//!   over canonical output reproduces it exactly.
//!
//! To stay idempotent and avoid destroying intentional structure, lines are wrapped
//! independently — short lines are never merged into longer ones — and preformatted
//! regions (`<pre>`, fenced code) inside block comments are emitted verbatim.
//!
//! Each function returns the rendered comment whose *first* line carries no indentation
//! (the renderer has already positioned the cursor at the comment's column); every
//! continuation line begins with `newline` followed by `indent_str`.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use unicode_width::UnicodeWidthStr;

/// Namespace for the `wrap-comments` reflow helpers.
pub(crate) struct Wrap;

impl Wrap {
    /// Reflow a `// ...` line comment.
    ///
    /// `indent_str` is emitted before each continuation line and `indent_cols` is its display
    /// width (a tab counts as one indentation level wide). A comment that already fits is
    /// returned verbatim, so well-formed comments are never re-spaced.
    pub(crate) fn reflow_line(
        text: &str,
        indent_str: &str,
        indent_cols: usize,
        newline: &str,
        comment_width: usize,
    ) -> String {
        if indent_cols + UnicodeWidthStr::width(text) <= comment_width {
            return text.to_owned();
        }
        let words: Vec<&str> = text
            .strip_prefix("//")
            .unwrap_or(text)
            .split_whitespace()
            .collect();
        if words.is_empty() {
            return text.to_owned();
        }
        let prefix = "// ";
        let avail = comment_width
            .saturating_sub(indent_cols + prefix.len())
            .max(1);
        let mut out = String::new();
        for (i, line) in Self::pack(&words, avail).iter().enumerate() {
            if i > 0 {
                out.push_str(newline);
                out.push_str(indent_str);
            }
            out.push_str(prefix);
            out.push_str(line);
        }
        out
    }

    /// Reflow a `/* ... */` or `/** ... */` block / documentation comment.
    ///
    /// Multi-line comments keep their line structure (each line wrapped independently, never
    /// merged) and are re-emitted in canonical form: the opener alone, ` * ` margins, and a
    /// ` */` closer alone. A single-line comment is kept on one line unless it overflows, in
    /// which case it expands to the same canonical multi-line form. Anything that is not a
    /// cleanly delimited `/* ... */` (e.g. an unterminated comment from error recovery) is
    /// returned verbatim.
    pub(crate) fn reflow_block(
        text: &str,
        is_doc: bool,
        indent_str: &str,
        indent_cols: usize,
        newline: &str,
        comment_width: usize,
    ) -> String {
        let opener = if is_doc { "/**" } else { "/*" };
        let Some(inner) = text.strip_prefix(opener).and_then(|s| s.strip_suffix("*/")) else {
            return text.to_owned();
        };

        let margin_cols = indent_cols + " * ".len();
        let avail = comment_width.saturating_sub(margin_cols).max(1);

        if !text.contains('\n') {
            // Single line: keep it unless it overflows, then expand to a multi-line block.
            if indent_cols + UnicodeWidthStr::width(text) <= comment_width {
                return text.to_owned();
            }
            let words: Vec<&str> = inner.split_whitespace().collect();
            if words.is_empty() {
                return text.to_owned();
            }
            return Self::emit_block(opener, &Self::pack(&words, avail), indent_str, newline);
        }

        // Multi-line: strip each line's star margin to recover its content, wrapping the ones
        // that overflow. Preformatted regions are passed through untouched.
        let mut content: Vec<String> = Vec::new();
        let mut in_pre = false;
        for raw_line in inner.split('\n') {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let stripped = Self::strip_star_margin(line);
            let lower = stripped.to_ascii_lowercase();
            let opens = lower.contains("<pre>");
            let closes = lower.contains("</pre>");
            let fence = stripped.trim_start().starts_with("```");
            if in_pre || opens || closes || fence {
                content.push(stripped.to_owned());
                if fence {
                    in_pre = !in_pre;
                } else {
                    in_pre = (in_pre || opens) && !closes;
                }
                continue;
            }
            let width = UnicodeWidthStr::width(stripped);
            if width == 0 {
                content.push(String::new());
            } else if margin_cols + width <= comment_width {
                content.push(stripped.to_owned());
            } else {
                content.extend(Self::pack(
                    &stripped.split_whitespace().collect::<Vec<_>>(),
                    avail,
                ));
            }
        }

        // The opener and closer sit on their own lines, so the first and last content lines are
        // blank artifacts; drop them (but keep interior blank lines as paragraph breaks).
        while content.first().is_some_and(String::is_empty) {
            content.remove(0);
        }
        while content.last().is_some_and(String::is_empty) {
            content.pop();
        }
        if content.is_empty() {
            return format!("{opener} */");
        }
        Self::emit_block(opener, &content, indent_str, newline)
    }

    /// Assemble a canonical multi-line block comment from its content lines.
    fn emit_block(opener: &str, content: &[String], indent_str: &str, newline: &str) -> String {
        let mut out = String::from(opener);
        for line in content {
            out.push_str(newline);
            out.push_str(indent_str);
            if line.is_empty() {
                out.push_str(" *");
            } else {
                out.push_str(" * ");
                out.push_str(line);
            }
        }
        out.push_str(newline);
        out.push_str(indent_str);
        out.push_str(" */");
        out
    }

    /// Strip a line's leading whitespace, an optional `*`, and a single following space,
    /// recovering the line's content. `strip_star_margin` composed with the ` * ` margin that
    /// [`Wrap::emit_block`] adds is the identity, which is what makes reflow idempotent.
    fn strip_star_margin(line: &str) -> &str {
        let trimmed = line.trim_start();
        trimmed
            .strip_prefix('*')
            .map_or(trimmed, |rest| rest.strip_prefix(' ').unwrap_or(rest))
    }

    /// Greedily pack words into lines no wider than `avail`. A word longer than `avail` lands
    /// on its own line rather than being split, so reflowing the result is a fixed point.
    fn pack(words: &[&str], avail: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_width = 0;
        for &word in words {
            let word_width = UnicodeWidthStr::width(word);
            if cur.is_empty() {
                cur.push_str(word);
                cur_width = word_width;
            } else if cur_width + 1 + word_width <= avail {
                cur.push(' ');
                cur.push_str(word);
                cur_width += 1 + word_width;
            } else {
                lines.push(core::mem::take(&mut cur));
                cur.push_str(word);
                cur_width = word_width;
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, indent_cols: usize, width: usize) -> String {
        let indent = " ".repeat(indent_cols);
        Wrap::reflow_line(text, &indent, indent_cols, "\n", width)
    }

    fn block(text: &str, is_doc: bool, indent_cols: usize, width: usize) -> String {
        let indent = " ".repeat(indent_cols);
        Wrap::reflow_block(text, is_doc, &indent, indent_cols, "\n", width)
    }

    #[test]
    fn line_that_fits_is_verbatim() {
        assert_eq!(line("//  keep   spacing", 0, 80), "//  keep   spacing");
    }

    #[test]
    fn long_line_wraps_into_marked_lines() {
        assert_eq!(
            line("// alpha beta gamma delta epsilon", 0, 18),
            "// alpha beta\n// gamma delta\n// epsilon"
        );
    }

    #[test]
    fn wrapped_line_respects_indentation() {
        // indent of 4 columns eats into the budget; continuation lines carry the indent.
        assert_eq!(
            line("// alpha beta gamma", 4, 16),
            "// alpha\n    // beta\n    // gamma"
        );
    }

    #[test]
    fn overlong_word_stays_on_its_own_line() {
        assert_eq!(
            line("// short superduperlongtokenthatcannotbesplit end", 0, 12),
            "// short\n// superduperlongtokenthatcannotbesplit\n// end"
        );
    }

    #[test]
    fn short_block_is_verbatim() {
        assert_eq!(block("/* hi */", false, 0, 80), "/* hi */");
        assert_eq!(block("/** hi */", true, 0, 80), "/** hi */");
    }

    #[test]
    fn single_line_block_expands_when_too_long() {
        assert_eq!(
            block("/** alpha beta gamma delta */", true, 0, 16),
            "/**\n * alpha beta\n * gamma delta\n */"
        );
    }

    #[test]
    fn multiline_javadoc_wraps_each_line() {
        let src = "/**\n * Summary that is rather too wide to fit.\n * @param x value\n */";
        assert_eq!(
            block(src, true, 0, 20),
            "/**\n * Summary that is\n * rather too wide\n * to fit.\n * @param x value\n */"
        );
    }

    #[test]
    fn interior_blank_lines_are_kept() {
        let src = "/**\n * One.\n *\n * Two.\n */";
        assert_eq!(block(src, true, 0, 80), "/**\n * One.\n *\n * Two.\n */");
    }

    #[test]
    fn preformatted_regions_are_not_wrapped() {
        let src = "/**\n * <pre>\n * a b c d e f g h i j k l m n o p\n * </pre>\n */";
        assert_eq!(
            block(src, true, 0, 12),
            "/**\n * <pre>\n * a b c d e f g h i j k l m n o p\n * </pre>\n */"
        );
    }

    #[test]
    fn unterminated_block_is_verbatim() {
        // Error recovery can produce a `/*` with no closer; never touch it.
        assert_eq!(
            block("/* dangling no closer", false, 0, 4),
            "/* dangling no closer"
        );
    }

    #[test]
    fn reflow_is_idempotent() {
        let inputs = [
            ("/** alpha beta gamma delta epsilon zeta */", true),
            (
                "/**\n * Summary that is rather too wide to fit.\n * @param x value\n */",
                true,
            ),
            ("/* a b c d e f g h i j */", false),
        ];
        for (src, is_doc) in inputs {
            let once = block(src, is_doc, 4, 24);
            // Re-reflowing each line of the output reproduces it (the renderer re-parses
            // wrapped comments line-by-line, so per-line stability implies idempotence).
            let twice = block(&once, is_doc, 4, 24);
            assert_eq!(once, twice, "block reflow must be idempotent: {src:?}");
        }
        let long_line = "// alpha beta gamma delta epsilon zeta eta theta";
        let once = line(long_line, 4, 24);
        for produced in once.split('\n') {
            assert_eq!(line(produced.trim_start(), 4, 24), produced.trim_start());
        }
    }
}
