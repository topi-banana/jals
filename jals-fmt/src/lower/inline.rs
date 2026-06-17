//! Generic (inline) lowering — the universal fallback.
//!
//! Lays a node out on one logical line with normalized spacing: child nodes recurse through
//! [`lower`], tokens are separated per [`sep`]. [`lower_elements`] is the shared element loop,
//! reused both for a whole node's children ([`lower_inline`]) and for chain-selector emission
//! (`crate::lower::chains`). Control-flow statements route through here too, with `control_flow`
//! set so `control-brace-style = next-line` can push a `}`-anchored continuation to its own line.

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::ControlBraceStyle;
use crate::doc::{Doc, concat, hardline, nil};
use crate::lower::{Ctx, first_sig_token, last_sig_token, lower, sep, tok};

/// Lay a node out inline: child nodes are recursed, tokens are separated by single
/// spaces per [`crate::lower::want_space`]. Whitespace, newlines, and comment trivia are skipped
/// here (comments are injected via [`tok`]).
pub(crate) fn lower_generic(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    lower_inline(node, ctx, false)
}

/// Lay out a control-flow statement (`if` / `try` / `do-while`) inline, honoring
/// [`ControlBraceStyle`]: under `next-line`, a continuation that directly follows a closing
/// brace (`} else`, `} catch`, `} finally`, `} while`) moves onto its own line. (The opening
/// brace of each block is handled separately, by `lower_braced` via `opens_on_next_line`.)
/// With the default `same-line` it is byte-for-byte identical to [`lower_generic`].
pub(crate) fn lower_control_flow(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    lower_inline(node, ctx, true)
}

/// Shared core of [`lower_generic`] and [`lower_control_flow`]. When `control_flow` is set,
/// the separator before a `}`-anchored continuation becomes a forced break under
/// `control-brace-style = next-line` (see [`flow_sep`]).
pub(crate) fn lower_inline(node: &SyntaxNode, ctx: &Ctx<'_>, control_flow: bool) -> Doc {
    lower_elements(node.children_with_tokens(), ctx, control_flow)
}

/// Lay out an arbitrary run of CST elements inline. The element loop is shared between
/// [`lower_inline`] (a whole node's children) and chain-selector emission, which feeds it a
/// `FIELD_ACCESS`'s children minus the receiver (see `lower_after_first_node`); routing both
/// through here keeps the type-witness hug below in one place.
pub(crate) fn lower_elements(
    els: impl Iterator<Item = SyntaxElement>,
    ctx: &Ctx<'_>,
    control_flow: bool,
) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut prev: Option<SyntaxToken> = None;
    // A `.<T>` / `::<T>` explicit type witness hugs the method name that follows it
    // (`List.<String>of()`, `Foo::<String>bar`), unlike a type's own `<...>` (`List<T> x`).
    let mut hug_witness = false;

    for el in els {
        if let Some(child) = el.as_node() {
            // A reordered `MODIFIERS` node emits its tokens in a different order than the tree,
            // so the separators around it must use the *emitted* boundary tokens (see
            // `rules::modifiers::emitted_boundary_tokens`); every other node emits in tree order.
            let (emitted_first, emitted_last) = if child.kind() == S::MODIFIERS {
                crate::rules::modifiers::emitted_boundary_tokens(child, ctx.cfg)
            } else {
                (first_sig_token(child), last_sig_token(child))
            };
            if let Some(first) = emitted_first.as_ref() {
                let s = if hug_witness {
                    nil()
                } else {
                    flow_sep(ctx, control_flow, prev.as_ref(), child.kind(), first)
                };
                parts.push(s);
            }
            hug_witness = child.kind() == S::TYPE_ARGS
                && matches!(
                    prev.as_ref().map(|t| t.kind()),
                    Some(S::DOT | S::COLON_COLON)
                );
            parts.push(lower(child, ctx));
            if let Some(last) = emitted_last {
                prev = Some(last);
            }
        } else if let Some(t) = el.as_token() {
            if t.kind().is_trivia() {
                continue;
            }
            let s = if hug_witness {
                nil()
            } else {
                flow_sep(ctx, control_flow, prev.as_ref(), t.kind(), t)
            };
            parts.push(s);
            hug_witness = false;
            parts.push(tok(t, ctx));
            prev = Some(t.clone());
        }
    }
    concat(parts)
}

/// Whether `kind` identifies a control-flow continuation that `control-brace-style` may push
/// to the next line: the `else` / `while` token of an `if` / `do-while`, or a `catch` /
/// `finally` clause node of a `try`.
fn is_continuation(kind: S) -> bool {
    matches!(
        kind,
        S::ELSE_KW | S::WHILE_KW | S::CATCH_CLAUSE | S::FINALLY_CLAUSE
    )
}

/// The separator before a child element (`next`, of kind `next_kind`). When `control_flow`
/// is set, `control-brace-style = next-line` forces a break before a continuation that
/// directly follows a closing brace; otherwise the normal token spacing from [`sep`].
fn flow_sep(
    ctx: &Ctx<'_>,
    control_flow: bool,
    prev: Option<&SyntaxToken>,
    next_kind: S,
    next: &SyntaxToken,
) -> Doc {
    if control_flow
        && ctx.cfg.control_brace_style == ControlBraceStyle::NextLine
        && is_continuation(next_kind)
        && prev.map(|p| p.kind()) == Some(S::RBRACE)
    {
        return hardline();
    }
    sep(prev, next, ctx.cfg)
}
