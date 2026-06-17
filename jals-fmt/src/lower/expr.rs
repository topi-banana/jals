//! Expressions with canonical operator spacing.
//!
//! Binary, ternary, and unary expressions get canonical operator spacing and breakable layout.
//! A same-precedence binary run is one group breaking at every operator (placement per
//! `binop-separator`); a ternary breaks at its `?` / `:` against `single-line-if-else-max-width`;
//! a unary stays tight (`-x`), spacing only inserted to avoid operator fusion (`- -x`). Malformed
//! shapes from error recovery fall back to inline emission, byte-for-byte unchanged.

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{BinopLayout, BinopSeparator};
use crate::doc::{
    Doc, concat, continuation_indent, fill, group, group_within, line, nil, softline, text,
};
use crate::lower::{Ctx, first_sig_token, last_sig_token, lower, lower_generic, tight_sep, tok};

/// The binding power of a binary operator, given its (1–3 adjacent) operator tokens.
/// Mirrors `peek_bin_op` in `jals-syntax`'s grammar — only the `>` family is multi-token
/// (`>=` is `GT EQ`, `>>` is `GT GT`, `>>>` is `GT GT GT`; `<=` and `<<` are single
/// tokens). `None` for a token run that is not a known binary operator (error recovery).
fn binop_bp(ops: &[SyntaxToken]) -> Option<u8> {
    use S::*;
    let kinds: Vec<S> = ops.iter().map(SyntaxToken::kind).collect();
    Some(match kinds.as_slice() {
        [PIPE_PIPE] => 1,
        [AMP_AMP] => 2,
        [PIPE] => 3,
        [CARET] => 4,
        [AMP] => 5,
        [EQ_EQ] | [BANG_EQ] => 6,
        [LT] | [LT_EQ] | [GT] | [GT, EQ] | [INSTANCEOF_KW] => 7,
        [LSHIFT] | [GT, GT] | [GT, GT, GT] => 8,
        [PLUS] | [MINUS] => 9,
        [STAR] | [SLASH] | [PERCENT] => 10,
        _ => return None,
    })
}

/// Split a `BINARY_EXPR` into its canonical `lhs op rhs` shape: exactly two child nodes
/// with the operator-token run between them. `None` for any other shape (error recovery),
/// in which case the caller falls back to inline emission.
fn binary_parts(node: &SyntaxNode) -> Option<(SyntaxNode, Vec<SyntaxToken>, SyntaxNode)> {
    let mut nodes = node.children();
    let (lhs, rhs) = (nodes.next()?, nodes.next()?);
    if nodes.next().is_some() {
        return None;
    }
    let ops: Vec<SyntaxToken> = node
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|t| !t.kind().is_trivia())
        .collect();
    // The operator must sit between the operands; anything else is malformed.
    if ops.is_empty()
        || ops.first()?.text_range().start() < lhs.text_range().end()
        || ops.last()?.text_range().end() > rhs.text_range().start()
    {
        return None;
    }
    Some((lhs, ops, rhs))
}

/// One wrapped step of a flattened binary run: the operator-token run and its right operand.
type BinopStep = (Vec<SyntaxToken>, SyntaxNode);

/// Flatten the left spine of same-precedence nested `BINARY_EXPR`s into the first operand
/// and the `(operator run, rhs)` steps in source order. A left child of a *different*
/// precedence stays a unit (its own group), so `a == b && c == d` breaks at `&&` before
/// either `==` does. All Java binary operators are left-associative, so the same-precedence
/// run is always a pure left spine.
fn flatten_binary(node: &SyntaxNode) -> Option<(SyntaxNode, Vec<BinopStep>)> {
    let (lhs, ops, rhs) = binary_parts(node)?;
    let bp = binop_bp(&ops);
    let mut steps = vec![(ops, rhs)];
    let mut first = lhs;
    while first.kind() == S::BINARY_EXPR && bp.is_some() {
        let Some((lhs2, ops2, rhs2)) = binary_parts(&first) else {
            break;
        };
        if binop_bp(&ops2) != bp {
            break;
        }
        steps.push((ops2, rhs2));
        first = lhs2;
    }
    steps.reverse();
    Some((first, steps))
}

/// Lower a binary expression. The same-precedence run wraps with a break point at every
/// operator, the operator leading the continuation line (`binop-separator = front`, the default)
/// or trailing the broken line (`back`). A run of operator tokens (e.g. the two `>` of `>>`) is
/// joined tightly so operator fusion is preserved. `binop-layout` chooses how it wraps:
/// [`Tall`](BinopLayout::Tall) is one group — flat `a op b op c`, else *every* step on its own
/// line; [`Compressed`](BinopLayout::Compressed) is a fill that packs as many operands per line
/// as fit `max-width` (google-java-format's layout).
pub(crate) fn lower_binary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let Some((first, steps)) = flatten_binary(node) else {
        // Error recovery produced something other than `lhs op rhs`; emit inline with
        // canonical spacing so every token is preserved verbatim.
        return lower_binary_inline(node, ctx);
    };
    match ctx.cfg.binop_layout {
        BinopLayout::Tall => {
            // All-or-nothing: one group breaking at every operator together.
            let mut tail: Vec<Doc> = Vec::new();
            for (ops, rhs) in &steps {
                let op = concat(ops.iter().map(|t| tok(t, ctx)).collect());
                match ctx.cfg.binop_separator {
                    BinopSeparator::Front => tail.extend([line(), op, text(" "), lower(rhs, ctx)]),
                    BinopSeparator::Back => tail.extend([text(" "), op, line(), lower(rhs, ctx)]),
                }
            }
            group(concat(vec![
                lower(&first, ctx),
                continuation_indent(concat(tail)),
            ]))
        }
        BinopLayout::Compressed => {
            // Fill: each operand (with its glued operator) is a fill item; `fill` inserts a
            // breakable `line` between items, so the renderer packs as many per line as fit.
            // `front` glues the operator to the *following* operand (it leads the next line);
            // `back` glues it to the *preceding* one (it trails the broken line).
            let mut items: Vec<Doc> = Vec::with_capacity(steps.len() + 1);
            match ctx.cfg.binop_separator {
                BinopSeparator::Front => {
                    items.push(lower(&first, ctx));
                    for (ops, rhs) in &steps {
                        let op = concat(ops.iter().map(|t| tok(t, ctx)).collect());
                        items.push(concat(vec![op, text(" "), lower(rhs, ctx)]));
                    }
                }
                BinopSeparator::Back => {
                    let mut operand = lower(&first, ctx);
                    for (ops, rhs) in &steps {
                        let op = concat(ops.iter().map(|t| tok(t, ctx)).collect());
                        items.push(concat(vec![operand, text(" "), op]));
                        operand = lower(rhs, ctx);
                    }
                    items.push(operand);
                }
            }
            continuation_indent(fill(items))
        }
    }
}

/// Lower a malformed binary expression inline as `lhs op rhs`, joining a run of operator
/// tokens tightly and surrounding the whole operator with single spaces. Fallback for
/// error-recovery shapes [`flatten_binary`] cannot handle (e.g. a missing operand).
fn lower_binary_inline(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut pending_op: Vec<Doc> = Vec::new();

    for el in node.children_with_tokens() {
        if let Some(child) = el.as_node() {
            flush_operator(&mut parts, &mut pending_op);
            parts.push(lower(child, ctx));
        } else if let Some(t) = el.as_token()
            && !t.kind().is_trivia()
        {
            pending_op.push(tok(t, ctx));
        }
    }
    flush_operator(&mut parts, &mut pending_op);
    concat(parts)
}

fn flush_operator(parts: &mut Vec<Doc>, pending_op: &mut Vec<Doc>) {
    if pending_op.is_empty() {
        return;
    }
    parts.push(text(" "));
    parts.push(concat(std::mem::take(pending_op)));
    parts.push(text(" "));
}

/// Split a `TERNARY_EXPR` into its canonical `cond ? then : els` shape: exactly three child
/// nodes with the `?` token between the first two and the `:` token between the last two.
/// `None` for any other shape (error recovery), in which case the caller falls back to inline
/// emission.
fn ternary_parts(
    node: &SyntaxNode,
) -> Option<(SyntaxNode, SyntaxToken, SyntaxNode, SyntaxToken, SyntaxNode)> {
    let mut nodes = node.children();
    let (cond, then, els) = (nodes.next()?, nodes.next()?, nodes.next()?);
    if nodes.next().is_some() {
        return None;
    }
    let toks: Vec<SyntaxToken> = node
        .children_with_tokens()
        .filter_map(SyntaxElement::into_token)
        .filter(|t| !t.kind().is_trivia())
        .collect();
    let [q, colon] = toks.as_slice() else {
        return None;
    };
    // The `?` must sit between `cond` and `then`, the `:` between `then` and `els`.
    if q.kind() != S::QUESTION
        || colon.kind() != S::COLON
        || q.text_range().start() < cond.text_range().end()
        || then.text_range().start() < q.text_range().end()
        || colon.text_range().start() < then.text_range().end()
        || els.text_range().start() < colon.text_range().end()
    {
        return None;
    }
    Some((cond, q.clone(), then, colon.clone(), els))
}

/// Lower a ternary conditional `cond ? then : els`. It is one group with a break point before
/// (or, under `binop-separator = back`, after) each operator: flat it is `cond ? then: els`
/// (the `:` spacing follows `space-before-colon` / `space-after-colon`, byte-identical to
/// inline emission); when its flat width exceeds `single-line-if-else-max-width` — or it would
/// overflow `max-width` — it wraps, the `?` and `:` leading the continuation lines
/// (`binop-separator = front`, the default) or trailing the broken lines (`back`). A value of
/// `0` for the width wraps every ternary. A malformed ternary (error recovery) falls back to
/// inline emission, byte-for-byte unchanged.
pub(crate) fn lower_ternary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let Some((cond, q, then, colon, els)) = ternary_parts(node) else {
        return lower_generic(node, ctx);
    };
    // A break point whose flat form is a space (`line`) or nothing (`softline`); both break to
    // a newline. The `?` is always space-surrounded; the `:` follows the colon-spacing config.
    let break_at = |flat_space: bool| if flat_space { line() } else { softline() };
    let colon_space = |on: bool| if on { text(" ") } else { nil() };
    let sbc = ctx.cfg.space_before_colon;
    let sac = ctx.cfg.space_after_colon;
    let tail = match ctx.cfg.binop_separator {
        BinopSeparator::Front => concat(vec![
            break_at(true),
            tok(&q, ctx),
            text(" "),
            lower(&then, ctx),
            break_at(sbc),
            tok(&colon, ctx),
            colon_space(sac),
            lower(&els, ctx),
        ]),
        BinopSeparator::Back => concat(vec![
            text(" "),
            tok(&q, ctx),
            break_at(true),
            lower(&then, ctx),
            colon_space(sbc),
            tok(&colon, ctx),
            break_at(sac),
            lower(&els, ctx),
        ]),
    };
    let doc = concat(vec![lower(&cond, ctx), continuation_indent(tail)]);
    group_within(doc, ctx.cfg.single_line_if_else_max_width)
}

/// Lower a unary expression tight (`-x`), inserting a space only when the operator and
/// operand would otherwise fuse (`- -x`, `+ +x`).
pub(crate) fn lower_unary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut prev: Option<SyntaxToken> = None;
    for el in node.children_with_tokens() {
        if let Some(child) = el.as_node() {
            if let Some(first) = first_sig_token(child) {
                parts.push(tight_sep(prev.as_ref(), &first));
            }
            parts.push(lower(child, ctx));
            if let Some(last) = last_sig_token(child) {
                prev = Some(last);
            }
        } else if let Some(t) = el.as_token()
            && !t.kind().is_trivia()
        {
            parts.push(tight_sep(prev.as_ref(), t));
            parts.push(tok(t, ctx));
            prev = Some(t.clone());
        }
    }
    concat(parts)
}
