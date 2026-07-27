//! `formatter-tags` — regions the formatter must leave byte-identical.
//!
//! Eclipse's `@formatter:off` / `@formatter:on`, IntelliJ's `FORMATTER_OFF_TAG`, and Spotless's `toggleOffOn`.
//!
//! A region marked this way
//! `toggleOffOn()`. A region marked this way has to survive **every** stage untouched: L0 must
//! not reorder anything inside it, L2 must not re-space it, L1 must not re-wrap it, and L4 must
//! not trim it.
//!
//! # Why it is not a pipeline stage
//!
//! Splicing the text out before formatting and back in afterwards is the obvious implementation
//! and the wrong one: the significant-token check would no longer see the region, so a bug that
//! dropped tokens *around* the splice point would go unnoticed. Instead the region rides through
//! the document as a single verbatim [`Doc::Tok`](crate::ir::Doc::Tok). It contains newlines, so
//! its width is infinite and the enclosing level can never render flat — the engine walks past it
//! without a decision to make. The visitor emits it in place of the tokens it covers and
//! suppresses everything else it would have emitted for them.

use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxNode};
use text_size::TextRange;

use crate::comments::CommentMap;
use crate::style::Style;

/// Locates formatter-disabled regions.
pub(crate) struct OffOn;

impl OffOn {
    /// The disabled regions of `root`, in source order and never overlapping.
    ///
    /// A region opens at the comment holding `formatter-off-tag` and closes at the end of the
    /// next comment holding `formatter-on-tag`; an unclosed region runs to the end of the file,
    /// which is what every native formatter does. Returns empty when `[layout] formatter-tags`
    /// is off, so the whole mechanism costs one boolean on the default path.
    pub(crate) fn scan(root: &SyntaxNode, style: &Style) -> Vec<TextRange> {
        if !style.cfg.layout.formatter_tags {
            return Vec::new();
        }
        let off = style.cfg.layout.formatter_off_tag.as_str();
        let on = style.cfg.layout.formatter_on_tag.as_str();
        if off.is_empty() || on.is_empty() {
            return Vec::new();
        }

        let mut regions = Vec::new();
        let mut start = None;
        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            if !CommentMap::is_comment(tok.kind()) {
                continue;
            }
            let text = tok.text();
            match start {
                None if text.contains(off) => start = Some(tok.text_range().start()),
                Some(from) if text.contains(on) => {
                    regions.push(TextRange::new(from, tok.text_range().end()));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            regions.push(TextRange::new(from, root.text_range().end()));
        }
        regions
    }

    /// The index of the region containing `offset`, if any.
    ///
    /// Regions are disjoint and sorted, so this is a binary search; the common case is an empty
    /// list, which returns immediately.
    pub(crate) fn region_at(regions: &[TextRange], offset: text_size::TextSize) -> Option<usize> {
        if regions.is_empty() {
            return None;
        }
        regions
            .binary_search_by(|region| {
                if region.end() <= offset {
                    core::cmp::Ordering::Less
                } else if region.start() > offset {
                    core::cmp::Ordering::Greater
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .ok()
    }
}
