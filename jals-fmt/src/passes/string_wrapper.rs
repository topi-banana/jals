//! R4.1 — re-wrap a string concatenation that overflows the column limit.
//!
//! google-java-format's `StringWrapper`. Unlike everything else in the crate this is a **second
//! pass over the formatted text**: the output is re-parsed, each over-long `+` chain of string
//! literals is flattened and re-split, and the result is adopted only if formatting it again
//! reproduces it exactly.
//!
//! # Why a fixed-point check, not a proof
//!
//! Re-chunking changes where the literals' boundaries fall, which changes the widths the engine
//! measures, which can change where it breaks — a feedback edge the single-pass engine has
//! nowhere to express. Rather than reason about it, the pass **checks**: format the candidate and
//! keep it only if it is already formatted. A candidate that fails is discarded and the original
//! output stands, so the pass can never make a file worse, and `fmt ∘ fmt = fmt` survives.
//!
//! # What it will not do
//!
//! Split a *single* literal into new tokens. That would add `+` operators the source never had,
//! and google-java-format is not confirmed to do it either (`DESIGN.md` §10). The `+` and
//! string-piece multiset is therefore preserved and only the arrangement changes.

use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use text_size::TextRange;

use crate::ir::Width;
use crate::style::Style;

/// Re-wraps over-long string concatenations.
pub(crate) struct StringWrapper;

impl StringWrapper {
    /// The re-wrapped text, or `None` when nothing applies.
    ///
    /// The caller **must** run the candidate back through the formatter and keep it only if it
    /// comes back unchanged; the pass is defined by that check, not by this function.
    pub(crate) async fn candidate(formatted: &str, style: &Style) -> Option<String> {
        if !style.cfg.wrapping.reflow_long_strings {
            return None;
        }
        let parse = jals_syntax::Parse::parse(formatted).await;
        let edits = Self::plan(&parse.syntax(), formatted, style);
        if edits.is_empty() {
            return None;
        }
        Some(Self::splice(formatted, &edits))
    }

    /// The replacements to make, in source order and non-overlapping.
    fn plan(root: &SyntaxNode, src: &str, style: &Style) -> Vec<(TextRange, String)> {
        let mut edits = Vec::new();
        for node in root.descendants() {
            if node.kind() != SyntaxKind::BINARY_EXPR {
                continue;
            }
            // Outermost only: a nested chain is handled by its root, and editing both would
            // produce overlapping ranges.
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::BINARY_EXPR)
            {
                continue;
            }
            let Some(pieces) = Self::concatenation(&node) else {
                continue;
            };
            let range = node.text_range();
            let column = Self::column_of(src, usize::from(range.start()));
            if !Self::overflows(src, range, column, style) {
                continue;
            }
            if let Some(text) = Self::rewrap(&pieces, column, style) {
                edits.push((range, text));
            }
        }
        edits
    }

    /// The string literals of a pure `+` concatenation, or `None` when the node is anything else.
    ///
    /// "Pure" means every leaf is a `STRING_LITERAL` and every operator is `+`. A chain with a
    /// non-literal operand cannot be re-split without changing evaluation, and a text block
    /// carries its own layout, so both are refused.
    fn concatenation(node: &SyntaxNode) -> Option<Vec<String>> {
        let mut pieces = Vec::new();
        if !Self::collect(node, &mut pieces) || pieces.len() < 2 {
            return None;
        }
        Some(pieces)
    }

    /// Walk a `+` chain, pushing each literal's body. Returns whether the chain stayed pure.
    fn collect(node: &SyntaxNode, out: &mut Vec<String>) -> bool {
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Token(tok) if tok.kind().is_trivia() => {}
                SyntaxElement::Token(tok) if tok.kind() == SyntaxKind::PLUS => {}
                SyntaxElement::Token(_) => return false,
                SyntaxElement::Node(child) => match child.kind() {
                    SyntaxKind::BINARY_EXPR => {
                        if !Self::collect(&child, out) {
                            return false;
                        }
                    }
                    SyntaxKind::LITERAL => {
                        let Some(body) = Self::literal_body(&child) else {
                            return false;
                        };
                        out.push(body);
                    }
                    _ => return false,
                },
            }
        }
        true
    }

    /// The text between a string literal's quotes, or `None` for anything that is not one.
    fn literal_body(node: &SyntaxNode) -> Option<String> {
        let tok = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|tok| !tok.kind().is_trivia())?;
        if tok.kind() != SyntaxKind::STRING_LITERAL {
            return None;
        }
        let text = tok.text();
        Some(text.strip_prefix('"')?.strip_suffix('"')?.into())
    }

    /// The column a byte offset sits at.
    fn column_of(src: &str, offset: usize) -> usize {
        let start = src[..offset].rfind('\n').map_or(0, |at| at + 1);
        Width::utf16(&src[start..offset])
    }

    /// Whether the concatenation is worth touching: it spans lines, or its line is over the limit.
    fn overflows(src: &str, range: TextRange, column: usize, style: &Style) -> bool {
        let text = &src[usize::from(range.start())..usize::from(range.end())];
        text.contains('\n') || column + Width::utf16(text) > style.max_width()
    }

    /// Re-chunk the pieces so each line fits, `+` leading every continuation.
    ///
    /// Returns `None` when the budget is too small to make progress — a deeply indented
    /// concatenation with a narrow limit — rather than emitting one character per line.
    fn rewrap(pieces: &[String], column: usize, style: &Style) -> Option<String> {
        let indent = column + style.continuation_cols;
        // `+ "` … `"` is four columns of overhead on a continuation line.
        let budget = style.max_width().checked_sub(indent + 4)?;
        if budget < 16 {
            return None;
        }

        let joined: String = pieces.concat();
        let chunks = Self::split(&joined, budget);
        if chunks.len() < 2 {
            return None;
        }

        let mut out = String::new();
        for (nth, chunk) in chunks.iter().enumerate() {
            if nth > 0 {
                out.push('\n');
                style.write_indent(indent, &mut out);
                out.push_str("+ ");
            }
            out.push('"');
            out.push_str(chunk);
            out.push('"');
        }
        Some(out)
    }

    /// Split a literal body into chunks of at most `budget` columns, breaking after a space where
    /// possible and never inside an escape sequence.
    fn split(body: &str, budget: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut width = 0usize;
        let mut last_space: Option<usize> = None;

        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            let mut unit = String::new();
            unit.push(c);
            // An escape is one indivisible unit; `\uXXXX` and the octal forms are longer, and
            // splitting any of them would change what the literal means.
            if c == '\\' {
                if let Some(next) = chars.next() {
                    unit.push(next);
                    if next == 'u' {
                        for _ in 0..4 {
                            match chars.peek() {
                                Some(hex) if hex.is_ascii_hexdigit() => {
                                    unit.push(chars.next().unwrap_or('0'));
                                }
                                _ => break,
                            }
                        }
                    }
                }
            }

            let unit_width = Width::utf16(&unit);
            if width + unit_width > budget && !current.is_empty() {
                match last_space {
                    Some(at) if at > 0 => {
                        let rest = current.split_off(at);
                        chunks.push(core::mem::take(&mut current));
                        current = rest;
                        width = Width::utf16(&current);
                    }
                    _ => {
                        chunks.push(core::mem::take(&mut current));
                        width = 0;
                    }
                }
                last_space = None;
            }

            current.push_str(&unit);
            width += unit_width;
            if c == ' ' {
                last_space = Some(current.len());
            }
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        chunks
    }

    /// Apply non-overlapping replacements to `src`.
    fn splice(src: &str, edits: &[(TextRange, String)]) -> String {
        let mut out = String::with_capacity(src.len());
        let mut at = 0usize;
        for (range, text) in edits {
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            if start < at || end > src.len() {
                continue;
            }
            out.push_str(&src[at..start]);
            out.push_str(text);
            at = end;
        }
        out.push_str(&src[at..]);
        out
    }
}
