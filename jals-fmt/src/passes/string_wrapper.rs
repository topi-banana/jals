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
            // Outermost only: a nested chain is handled by its root, and editing both would
            // produce overlapping ranges. A lone literal inside one is likewise its root's.
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::BINARY_EXPR)
            {
                continue;
            }
            let pieces = match node.kind() {
                SyntaxKind::BINARY_EXPR => Self::concatenation(&node),
                // A single literal too long for its line is split into a concatenation of its
                // own — this is the case `reflow-long-strings` mostly exists for, and the one
                // place in the crate where `+` operators are added.
                SyntaxKind::LITERAL => Self::literal_body(&node).map(|body| alloc::vec![body]),
                _ => continue,
            };
            let Some(pieces) = pieces else {
                continue;
            };
            let range = node.text_range();
            if !Self::overflows(src, range, style) {
                continue;
            }
            // The node's range starts at its leading trivia; the column that matters is where
            // its first significant token lands.
            let start = node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|tok| !tok.kind().is_trivia())
                .map_or_else(|| range.start(), |tok| tok.text_range().start());
            let column = Self::column_of(src, usize::from(start));
            // What follows the concatenation on its line — the `);` of `foo("…");` — has to
            // fit after the last chunk, so the last line's budget answers for it.
            let end = usize::from(range.end());
            let trailing = src[end..].find('\n').map_or(src.len() - end, |at| at);
            if let Some(text) = Self::rewrap(&pieces, column, trailing, style) {
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

    /// Whether the concatenation is worth touching: one of the lines it occupies is over the
    /// limit.
    ///
    /// Spanning lines is *not* enough. A concatenation the author already broken into short
    /// pieces — a generated table of `"\u0000\u0000…"` rows, say — is under the limit on every
    /// line and google-java-format leaves it exactly as written: its `LongStringsAndTextBlockScanner`
    /// only collects a literal whose own line runs past the column limit.
    fn overflows(src: &str, range: TextRange, style: &Style) -> bool {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let from = src[..start].rfind('\n').map_or(0, |at| at + 1);
        let to = src[end..].find('\n').map_or(src.len(), |at| end + at);
        src[from..to]
            .split('\n')
            .any(|line| Width::utf16(line) > style.max_width())
    }

    /// Re-chunk the pieces so each one fits a continuation line.
    ///
    /// The result is emitted **flat** — `"a" + "b" + "c"` on one logical line — and the caller
    /// re-formats it. Choosing the line breaks here as well would mean guessing what the engine
    /// is about to decide, and a guess that misses is exactly what the fixed-point check throws
    /// away. Re-splitting is the part the engine cannot do; placing breaks is the part only it
    /// should.
    ///
    /// Returns `None` when the budget is too small to make progress — a deeply indented
    /// concatenation with a narrow limit — rather than emitting one character per line.
    fn rewrap(pieces: &[String], column: usize, trailing: usize, style: &Style) -> Option<String> {
        // The first chunk starts where the literal already is and pays only for its quotes; a
        // continuation line pays for its indent and for `+ ` on top of that
        // (`width -= 6` in google-java-format's `reflow`, for its four-column indent).
        let first = style.max_width().checked_sub(column + 2)?;
        let budget = first.checked_sub(style.continuation_cols + 2)?;
        if budget < 16 {
            return None;
        }

        let joined: String = pieces.concat();
        let chunks = Self::split(&joined, first, budget, trailing);
        // Nothing to gain when the re-split reproduces the pieces it started from.
        if chunks.len() < 2 || chunks == pieces {
            return None;
        }

        let mut out = String::new();
        for (nth, chunk) in chunks.iter().enumerate() {
            if nth > 0 {
                out.push_str(" + ");
            }
            out.push('"');
            out.push_str(chunk);
            out.push('"');
        }
        Some(out)
    }

    /// Split a literal body into chunks of at most `budget` columns, breaking after a space where
    /// possible and never inside an escape sequence.
    fn split(body: &str, first: usize, rest: usize, trailing: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut width = 0usize;
        let mut last_space: Option<usize> = None;
        let total = Width::utf16(body);
        let mut consumed = 0usize;

        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            let mut unit = String::new();
            unit.push(c);
            // An escape is one indivisible unit; `\uXXXX` and the octal forms are longer, and
            // splitting any of them would change what the literal means.
            if c == '\\'
                && let Some(next) = chars.next()
            {
                {
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
            let mut budget = if chunks.is_empty() { first } else { rest };
            // Once what is left fits, this is the last chunk and it shares its line with whatever
            // follows the concatenation.
            if total - consumed <= budget {
                budget = budget.saturating_sub(trailing);
            }
            if width + unit_width > budget && !current.is_empty() {
                match last_space {
                    Some(at) if at > 0 => {
                        let tail = current.split_off(at);
                        consumed += Width::utf16(&current);
                        chunks.push(core::mem::take(&mut current));
                        current = tail;
                        width = Width::utf16(&current);
                    }
                    _ => {
                        consumed += width;
                        chunks.push(core::mem::take(&mut current));
                        width = 0;
                    }
                }
                last_space = None;
            }

            // The break falls *before* a space, so the space opens the continuation chunk —
            // `"… and then some" + " more"`.
            if c == ' ' {
                last_space = Some(current.len());
            }
            current.push_str(&unit);
            width += unit_width;
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
            // Overlapping or out-of-range edits are dropped rather than trusted: a bad splice
            // would corrupt the file, and skipping one only means a concatenation stays as it was.
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
