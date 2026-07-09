//! Builds LSP selection ranges from the lossless CST (`textDocument/selectionRange`).
//!
//! For each requested cursor position, this walks from the smallest CST element covering the
//! position up to the file root, yielding a chain of nested ranges (each parent fully contains
//! its child). Syntax-only: no name resolution. Editors use the chain to expand and shrink the
//! selection along syntactic boundaries.

use async_lsp::lsp_types::{Position, SelectionRange};
use jals_syntax::{Parse, SyntaxElement, SyntaxNode};
use text_size::TextRange;

use crate::line_index::LineIndex;

/// Build a selection-range chain for each requested position, in request order.
pub(crate) fn selection_ranges(
    parse: &Parse,
    text: &str,
    line_index: &LineIndex,
    positions: &[Position],
) -> Vec<SelectionRange> {
    let root = parse.syntax();
    positions
        .iter()
        .map(|&pos| range_at(&root, text, line_index, pos))
        .collect()
}

/// The nested-range chain at a single position: the covering token (if any), then each of its
/// ancestor nodes out to the root, collapsed so equal ranges don't repeat.
fn range_at(root: &SyntaxNode, text: &str, idx: &LineIndex, pos: Position) -> SelectionRange {
    let offset = idx.offset(text, pos);
    // Deepest element covering the empty range at the cursor. `offset` is clamped into
    // `[0, len]`, so the precondition (range contained in the root) holds.
    let elem = root.covering_element(TextRange::new(offset, offset));

    // Byte ranges, innermost first: the covering token, then every ancestor node.
    let mut ranges: Vec<TextRange> = Vec::new();
    let mut node = match elem {
        SyntaxElement::Token(token) => {
            ranges.push(token.text_range());
            token.parent()
        }
        SyntaxElement::Node(n) => Some(n),
    };
    while let Some(n) = node {
        ranges.push(n.text_range());
        node = n.parent();
    }
    // A node wrapping a single child shares its range; collapse so the chain strictly nests.
    ranges.dedup();

    // Link outermost -> innermost, so each inner range's `parent` points one step out.
    let mut selection: Option<SelectionRange> = None;
    for range in ranges.iter().rev() {
        selection = Some(SelectionRange {
            range: idx.range(text, *range),
            parent: selection.map(Box::new),
        });
    }
    // `ranges` always holds at least the root, but stay total just in case.
    selection.unwrap_or_else(|| SelectionRange {
        range: idx.range(text, TextRange::new(offset, offset)),
        parent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `outer` fully contains `inner`, both as `(start_line, start_char, end_line, end_char)`.
    fn contains(outer: (u32, u32, u32, u32), inner: (u32, u32, u32, u32)) -> bool {
        (outer.0, outer.1) <= (inner.0, inner.1) && (inner.2, inner.3) <= (outer.2, outer.3)
    }

    /// Follow the `parent` chain at one position; each range as
    /// `(start_line, start_char, end_line, end_char)`, innermost first.
    fn chain_at(text: &str, line: u32, character: u32) -> Vec<(u32, u32, u32, u32)> {
        let idx = LineIndex::new(text);
        let got = selection_ranges(
            &jals_syntax::parse(text),
            text,
            &idx,
            &[Position { line, character }],
        );
        assert_eq!(got.len(), 1);
        let mut chain = Vec::new();
        let mut cur = Some(&got[0]);
        while let Some(sr) = cur {
            let r = sr.range;
            chain.push((r.start.line, r.start.character, r.end.line, r.end.character));
            cur = sr.parent.as_deref();
        }
        chain
    }

    #[test]
    fn expands_from_token_to_file() {
        // `class Cls { int xy; }` — cursor inside the field name `xy` (char 17, on 'y').
        let text = "class Cls { int xy; }";
        let chain = chain_at(text, 0, 17);
        // Innermost selection is the `xy` token.
        assert_eq!(chain[0], (0, 16, 0, 18), "innermost = `xy`: {chain:?}");
        // Outermost selection is the whole file.
        assert_eq!(
            *chain.last().unwrap(),
            (0, 0, 0, 21),
            "outermost: {chain:?}"
        );
        // Each step strictly nests inside the next: no repeats, each contains the previous.
        for w in chain.windows(2) {
            assert!(
                contains(w[1], w[0]) && w[1] != w[0],
                "not strictly nesting: {chain:?}"
            );
        }
    }

    #[test]
    fn one_chain_per_position_in_order() {
        let text = "class Cls { int xy; }";
        let idx = LineIndex::new(text);
        let got = selection_ranges(
            &jals_syntax::parse(text),
            text,
            &idx,
            &[
                Position {
                    line: 0,
                    character: 7,
                }, // inside the type name `Cls`
                Position {
                    line: 0,
                    character: 17,
                }, // inside the field name `xy`
            ],
        );
        assert_eq!(got.len(), 2);
        // First chain's innermost is `Cls` (chars 6..9); second is `xy` (chars 16..18).
        assert_eq!(
            (got[0].range.start.character, got[0].range.end.character),
            (6, 9),
            "first innermost = `Cls`"
        );
        assert_eq!(
            (got[1].range.start.character, got[1].range.end.character),
            (16, 18),
            "second innermost = `xy`"
        );
    }

    #[test]
    fn never_panics_on_broken_or_out_of_range() {
        for text in ["", "class", "class C {", "/* unterminated", "{\n}\n", "@"] {
            let idx = LineIndex::new(text);
            let positions = [
                Position {
                    line: 0,
                    character: 0,
                },
                Position {
                    line: 999,
                    character: 999,
                },
                Position {
                    line: 0,
                    character: 999,
                },
            ];
            let got = selection_ranges(&jals_syntax::parse(text), text, &idx, &positions);
            assert_eq!(got.len(), positions.len());
        }
    }
}
