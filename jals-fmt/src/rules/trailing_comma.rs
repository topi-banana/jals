//! `trailing-comma`: the trailing comma of an array initializer's last item.
//!
//! Welded to [`crate::lower`]'s delimited-list lowering (it consumes the per-row comma tokens),
//! so this is a plain function with a single call site rather than a `dyn` rule — there is no list
//! to iterate. Kept under `rules/` so every opt-in transformation lives together.

use jals_syntax::SyntaxToken;

use crate::config::TrailingComma;
use crate::doc::{Doc, if_break, nil, text};
use crate::lower::{Ctx, tok};

/// The document for the trailing comma of a delimited list's last item, per the [`TrailingComma`]
/// policy. `comma` is the source's trailing comma token when it had one.
///
/// A source comma that carries a comment is always kept verbatim — even when the policy would drop
/// it — so no comment is lost. Under [`Vertical`](TrailingComma::Vertical) the comma is an
/// [`if_break`]: it materializes only when the enclosing list breaks across lines.
pub fn doc(policy: TrailingComma, comma: Option<&SyntaxToken>, ctx: &Ctx<'_>) -> Doc {
    // A commented comma can't be conditionally dropped without losing the comment; preserve it.
    if let Some(t) = comma
        && ctx.comments.has_comments(t)
        && matches!(policy, TrailingComma::Never | TrailingComma::Vertical)
    {
        return tok(t, ctx);
    }
    match policy {
        TrailingComma::Preserve => comma.map_or_else(nil, |t| tok(t, ctx)),
        TrailingComma::Always => comma.map_or_else(|| text(","), |t| tok(t, ctx)),
        TrailingComma::Never => nil(),
        // The comma exists only in the broken layout. Any source comma reaching here is
        // comment-free (the early return handled commented ones), so a plain `,` reproduces it.
        TrailingComma::Vertical => if_break(text(","), nil()),
    }
}
