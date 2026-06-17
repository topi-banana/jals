//! Delimited lists (params, args, array initializers).
//!
//! A comma-separated, delimited list wraps all-or-nothing: items are separated by a soft line
//! that is a space when flat and a break when wrapped. Argument lists and array initializers also
//! break against their own width budgets (`fn-call-width` / `array-width`); a parameter list
//! follows `fn-params-layout`. With `overflow-delimited-expr` a call / annotation argument list
//! whose final item is a delimited expression instead hangs that item past the call line. The
//! trailing comma is preserved by default; for an array initializer it follows `trailing-comma`.

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{FnParamsLayout, TrailingComma};
use crate::doc::{
    Doc, concat, continuation_indent, fill, group, group_always_break, group_overflow,
    group_within, line, nil, softline,
};
use crate::lower::{Ctx, first_sig_token, last_sig_token, lower, sep, tok};

/// The node forming the entire final item of a paren-delimited list — `(…, <node>)` with the
/// node directly between the last comma (or the open paren) and the close paren — plus that
/// close paren. `None` for any other shape: a trailing comma, stray or missing tokens, or an
/// empty recovery node; the caller then keeps the all-or-nothing layout.
fn sole_last_item(list: &SyntaxNode) -> Option<(SyntaxNode, SyntaxToken)> {
    let sig: Vec<SyntaxElement> = list
        .children_with_tokens()
        .filter(|el| match el {
            SyntaxElement::Node(n) => first_sig_token(n).is_some(),
            SyntaxElement::Token(t) => !t.kind().is_trivia(),
        })
        .collect();
    let [.., prev, cand, close] = sig.as_slice() else {
        return None;
    };
    let close = close.as_token().filter(|t| t.kind() == S::RPAREN)?.clone();
    let node = cand.as_node()?.clone();
    match prev.as_token().map(SyntaxToken::kind)? {
        S::COMMA | S::LPAREN => Some((node, close)),
        _ => None,
    }
}

/// Whether `node` is a delimited expression that `overflow-delimited-expr` may hang: a
/// block-bodied lambda, an anonymous-class / array-creating `new`, a bare array initializer,
/// or — in an annotation argument list — an `ANNOTATION_PAIR` whose value is one of those.
/// Expression-bodied lambdas are excluded so they keep the all-or-nothing layout exactly.
fn is_overflowable_expr(node: &SyntaxNode, in_annotation: bool) -> bool {
    match node.kind() {
        S::LAMBDA_EXPR => node.children().any(|c| c.kind() == S::BLOCK),
        S::ARRAY_INIT => true,
        S::NEW_EXPR => node
            .children()
            .any(|c| matches!(c.kind(), S::CLASS_BODY | S::ARRAY_INIT)),
        S::ANNOTATION_PAIR if in_annotation => node
            .children()
            .last()
            .is_some_and(|v| is_overflowable_expr(&v, false)),
        _ => false,
    }
}

/// Whether `overflow-delimited-expr` applies to this list: the option is on, the list is a
/// call / annotation argument list, its final item is exactly one overflowable node, and
/// neither that node's first token nor the close paren carries leading comments — those need
/// their own line above their anchor, which the vertical layout provides.
fn overflows_last_item(node: &SyntaxNode, ctx: &Ctx<'_>) -> bool {
    if !ctx.cfg.overflow_delimited_expr
        || !matches!(node.kind(), S::ARG_LIST | S::ANNOTATION_ARG_LIST)
    {
        return false;
    }
    let Some((cand, close)) = sole_last_item(node) else {
        return false;
    };
    is_overflowable_expr(&cand, node.kind() == S::ANNOTATION_ARG_LIST)
        && first_sig_token(&cand).is_some_and(|t| !ctx.comments.has_leading(&t))
        && !ctx.comments.has_leading(&close)
}

/// Lower a comma-separated, delimited list that wraps all-or-nothing. Items are separated by a
/// soft line that becomes a space when flat and a break when wrapped. An argument list
/// (`ARG_LIST`) additionally breaks when its flat width exceeds `fn-call-width`, and an array
/// initializer (`ARRAY_INIT`) when it exceeds `array-width`.
///
/// With `overflow-delimited-expr` enabled, a call or annotation argument list whose final
/// item is a delimited expression (see [`is_overflowable_expr`]) instead hangs that item:
/// laid out flat, the earlier items stay on the line and only the item's body breaks
/// (`f(a, () -> {` … `});`); laid out broken, the result is identical to the all-or-nothing
/// layout.
///
/// Inter-item commas are emitted verbatim. The final item's trailing comma is preserved by
/// default; for an array initializer it instead follows the `trailing-comma` policy (see
/// [`crate::rules::trailing_comma::doc`]) — the only Java list where adding or dropping it is legal.
pub(crate) fn lower_delimited(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    // Never synthesize a delimiter that the source lacks (error recovery): start empty
    // and fill from the real tokens so the significant-token sequence is preserved.
    let mut open_doc = nil();
    let mut close_doc = nil();
    // Whether a closing delimiter token was actually present. A list left open by error
    // recovery has none; synthesizing a trailing comma there is unsafe (see `policy` below).
    let mut has_close = false;
    // Each row is one item's content plus the comma token that follows it (if any). The comma
    // of the final row is the list's (optional) trailing comma.
    let mut rows: Vec<(Doc, Option<SyntaxToken>)> = Vec::new();
    let mut current: Vec<Doc> = Vec::new();
    let mut cur_prev: Option<SyntaxToken> = None;

    for el in node.children_with_tokens() {
        if let Some(child) = el.as_node() {
            if let Some(first) = first_sig_token(child) {
                current.push(sep(cur_prev.as_ref(), &first, ctx.cfg));
            }
            current.push(lower(child, ctx));
            cur_prev = last_sig_token(child);
        } else if let Some(t) = el.as_token() {
            let kind = t.kind();
            match kind {
                S::LPAREN | S::LBRACE | S::LBRACK => open_doc = tok(t, ctx),
                S::RPAREN | S::RBRACE | S::RBRACK => {
                    close_doc = tok(t, ctx);
                    has_close = true;
                }
                S::COMMA => {
                    // The comma ends the current item; keep it so the trailing one can follow
                    // the `trailing-comma` policy while inter-item commas stay verbatim.
                    rows.push((concat(std::mem::take(&mut current)), Some(t.clone())));
                    cur_prev = None;
                }
                _ if kind.is_trivia() => {}
                _ => {
                    current.push(sep(cur_prev.as_ref(), t, ctx.cfg));
                    current.push(tok(t, ctx));
                    cur_prev = Some(t.clone());
                }
            }
        }
    }
    if !current.is_empty() {
        rows.push((concat(std::mem::take(&mut current)), None));
    }

    if rows.is_empty() {
        return concat(vec![open_doc, close_doc]);
    }

    // Only an array initializer honors `trailing-comma`; every other delimited list preserves
    // the source exactly (a trailing comma elsewhere is invalid Java, reachable only via error
    // recovery, so dropping/adding one is never appropriate). An initializer left unclosed by
    // error recovery (no `}`) is also preserved: with no closing brace a synthesized trailing
    // comma is not actually trailing — on a re-parse it reads as an item separator and pulls the
    // following token into the list, which would break idempotency.
    let policy = if node.kind() == S::ARRAY_INIT && has_close {
        ctx.cfg.trailing_comma
    } else {
        TrailingComma::Preserve
    };

    let last = rows.len() - 1;
    let mut items: Vec<Doc> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (content, comma))| {
            let comma_doc = if i == last {
                crate::rules::trailing_comma::doc(policy, comma.as_ref(), ctx)
            } else {
                // An inter-item comma is required; emit it verbatim. A missing one (malformed
                // input) is never synthesized.
                comma.map_or_else(nil, |t| tok(&t, ctx))
            };
            concat(vec![content, comma_doc])
        })
        .collect();

    // `overflow-delimited-expr`: hang the final delimited item past the call line. Laid out
    // flat, the earlier items stay on the line and only the item's body breaks; laid out
    // broken, the result is identical to the all-or-nothing layout below, so this structure
    // strictly generalizes it.
    if overflows_last_item(node, ctx)
        && let Some(last_item) = items.pop()
    {
        let mut head_inner = vec![softline()];
        if !items.is_empty() {
            head_inner.push(crate::doc::join(line(), items));
            head_inner.push(line());
        }
        let head = concat(vec![open_doc, continuation_indent(concat(head_inner))]);
        let tail = concat(vec![softline(), close_doc]);
        let budget = (node.kind() == S::ARG_LIST).then_some(ctx.cfg.fn_call_width);
        return group_overflow(head, last_item, tail, budget);
    }

    // A `Compressed` parameter list packs as many parameters per line as fit (a `Fill`);
    // every other list joins its items with a plain break, wrapping all-or-nothing.
    let compressed_params =
        node.kind() == S::PARAM_LIST && ctx.cfg.fn_params_layout == FnParamsLayout::Compressed;
    let inner = if compressed_params {
        fill(items)
    } else {
        crate::doc::join(line(), items)
    };
    let doc = concat(vec![
        open_doc,
        continuation_indent(concat(vec![softline(), inner])),
        softline(),
        close_doc,
    ]);
    // A call's argument list (`ARG_LIST`) honors `fn-call-width` and an array initializer
    // (`ARRAY_INIT`) honors `array-width`. A parameter list (`PARAM_LIST`) follows
    // `fn-params-layout`: `Vertical` forces one parameter per line, while `Tall` / `Compressed`
    // break against `max-width` like every other list. The rest only break against `max-width`.
    match node.kind() {
        S::ARG_LIST => group_within(doc, ctx.cfg.fn_call_width),
        S::ARRAY_INIT => group_within(doc, ctx.cfg.array_width),
        S::PARAM_LIST if ctx.cfg.fn_params_layout == FnParamsLayout::Vertical => {
            group_always_break(doc)
        }
        _ => group(doc),
    }
}
