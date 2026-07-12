//! `trailing-comma`: the trailing comma of an array initializer's last item.
//!
//! Welded to [`crate::lower`]'s delimited-list lowering (it consumes the per-row comma tokens),
//! so this is a plain function with a single call site rather than a `dyn` rule — there is no list
//! to iterate. Kept under `rules/` so every opt-in transformation lives together.

use jals_syntax::SyntaxToken;

use crate::config::TrailingComma;
use crate::doc::Doc;
use crate::lower::Ctx;

/// The `trailing-comma` rule. A zero-sized handle grouping the trailing-comma emission so it is
/// reached through the type rather than as a free function.
pub(crate) struct TrailingCommaRule;

impl TrailingCommaRule {
    /// The document for the trailing comma of a delimited list's last item, per the
    /// [`TrailingComma`] policy. `comma` is the source's trailing comma token when it had one.
    ///
    /// A source comma that carries a comment is always kept verbatim — even when the policy would
    /// drop it — so no comment is lost. Under [`Vertical`](TrailingComma::Vertical) the comma is a
    /// [`Doc::if_break`]: it materializes only when the enclosing list breaks across lines.
    pub(crate) fn doc(policy: TrailingComma, comma: Option<&SyntaxToken>, ctx: &Ctx<'_>) -> Doc {
        // A commented comma can't be conditionally dropped without losing the comment; preserve it.
        if let Some(t) = comma
            && ctx.comments.has_comments(t)
            && matches!(policy, TrailingComma::Never | TrailingComma::Vertical)
        {
            return ctx.tok(t);
        }
        match policy {
            TrailingComma::Preserve => comma.map_or_else(Doc::nil, |t| ctx.tok(t)),
            TrailingComma::Always => comma.map_or_else(|| Doc::text(","), |t| ctx.tok(t)),
            TrailingComma::Never => Doc::nil(),
            // The comma exists only in the broken layout. Any source comma reaching here is
            // comment-free (the early return handled commented ones), so a plain `,` reproduces it.
            TrailingComma::Vertical => Doc::if_break(Doc::text(","), Doc::nil()),
        }
    }
}
