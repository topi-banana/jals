//! Protocol-neutral selection-range chains from the lossless CST.
//!
//! For a cursor offset, this walks from the smallest CST element covering the offset up to the
//! file root, yielding a chain of nested byte ranges (each next range fully contains the
//! previous). Syntax-only: no name resolution. Editors use the chain to expand and shrink the
//! selection along syntactic boundaries; hosts only map the byte ranges to their protocol's
//! shape (the LSP's parent-linked `SelectionRange`, Monaco's range list).

use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::{SyntaxElement, SyntaxNode};
use text_size::{TextRange, TextSize};

/// Computes selection-expansion chains.
pub struct SelectionChains;

impl SelectionChains {
    /// The nested-range chain at `offset`, innermost first: the covering token (if any), then
    /// each of its ancestor nodes out to the root, collapsed so equal ranges don't repeat.
    /// `offset` past EOF is clamped. The result always holds at least the root's range.
    pub fn at(root: &SyntaxNode, offset: usize) -> Vec<Range<usize>> {
        let end = usize::from(root.text_range().end());
        let offset = TextSize::from(u32::try_from(offset.min(end)).unwrap_or(u32::MAX));
        // Deepest element covering the empty range at the cursor. `offset` is clamped into
        // `[0, len]`, so the precondition (range contained in the root) holds.
        let elem = root.covering_element(TextRange::new(offset, offset));

        // Byte ranges, innermost first: the covering token, then every ancestor node.
        let mut ranges: Vec<Range<usize>> = Vec::new();
        let mut node = match elem {
            SyntaxElement::Token(token) => {
                ranges.push(Self::byte_range(token.text_range()));
                token.parent()
            }
            SyntaxElement::Node(n) => Some(n),
        };
        while let Some(n) = node {
            ranges.push(Self::byte_range(n.text_range()));
            node = n.parent();
        }
        // A node wrapping a single child shares its range; collapse so the chain strictly nests.
        ranges.dedup();
        ranges
    }

    /// A `text_size::TextRange` as a plain byte range.
    fn byte_range(range: TextRange) -> Range<usize> {
        usize::from(range.start())..usize::from(range.end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_at(text: &str, offset: usize) -> Vec<Range<usize>> {
        let parse = jals_exec::block_on_inline(jals_syntax::Parse::parse(text));
        SelectionChains::at(&parse.syntax(), offset)
    }

    #[test]
    fn expands_from_token_to_file() {
        // `class Cls { int xy; }` — cursor inside the field name `xy` (byte 17, on 'y').
        let text = "class Cls { int xy; }";
        let chain = chain_at(text, 17);
        // Innermost selection is the `xy` token; outermost is the whole file.
        assert_eq!(chain[0], 16..18, "innermost = `xy`: {chain:?}");
        assert_eq!(
            *chain.last().unwrap(),
            0..text.len(),
            "outermost: {chain:?}"
        );
        // Each step strictly nests inside the next: no repeats, each contains the previous.
        for w in chain.windows(2) {
            assert!(
                w[1].start <= w[0].start && w[0].end <= w[1].end && w[1] != w[0],
                "not strictly nesting: {chain:?}"
            );
        }
    }

    #[test]
    fn always_holds_at_least_the_root() {
        for text in ["", "class", "class C {", "/* unterminated", "{\n}\n", "@"] {
            for offset in [0, 3, 999] {
                let chain = chain_at(text, offset);
                assert!(!chain.is_empty(), "{text:?} at {offset}");
                assert_eq!(*chain.last().unwrap(), 0..text.len());
            }
        }
    }
}
