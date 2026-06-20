//! Sequences of statements / members.
//!
//! Lowers a `{ ... }` body (block, class body, switch body) with one indentation level, plus the
//! item sequences inside it. Brace placement follows [`BraceStyle`] / [`ControlBraceStyle`], and a
//! few options (`empty-item-single-line`, `fn-single-line`, `force-multiline-blocks`) tune the
//! empty / single-statement cases. Blank lines from the source are preserved (clamped by the
//! renderer).

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{BraceStyle, Config, ControlBraceStyle, SwitchCaseBody};
use crate::doc::{
    Doc, blank_line, concat, continuation_indent, group, hardline, if_break, indent, line, nil,
    text,
};
use crate::lower::{
    Ctx, first_sig_token, last_sig_token, lower, lower_elements, lower_generic, sep, tok,
};

/// Lower a `{ ... }` node (block, class body, switch body) with one indentation level.
pub(crate) fn lower_braced(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let tokens: Vec<SyntaxToken> = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .collect();
    let lbrace = tokens.iter().find(|t| t.kind() == S::LBRACE);
    let rbrace = tokens.iter().rfind(|t| t.kind() == S::RBRACE);

    // Malformed (a brace is missing from error recovery): never synthesize a brace —
    // fall back to inline emission so the significant-token sequence is preserved.
    let (Some(lbrace), Some(rbrace)) = (lbrace, rbrace) else {
        return lower_generic(node, ctx);
    };

    let (inner, any) = lower_items(node, ctx);
    let open = tok(lbrace, ctx);
    let has_dangling = ctx.comments.has_dangling(rbrace);
    let dangling = ctx.comments.dangling(rbrace);
    let close = concat(vec![text("}"), ctx.comments.trailing_doc(rbrace)]);

    // An empty body. Under `empty_item_single_line` (the default) it collapses to `{}` on the
    // header's line regardless of brace style, so `next-line` never strands a lone `{}`. With
    // the option off, an empty *declaration* body (a type body, or a method / constructor /
    // initializer block) instead expands to a two-line `{ <newline> }`, opening on its own
    // line under `brace-style = next-line`; control-flow / `switch` / lambda / bare blocks are
    // never governed and always keep `{}` (matching rustfmt's item-only scoping).
    if !any && !has_dangling {
        // `force_multiline_blocks` expands every empty block to a two-line `{ <newline> }`,
        // overriding `empty_item_single_line` and extending past it to control-flow / `switch` /
        // lambda / bare blocks (which the latter never governs).
        if !ctx.cfg.force_multiline_blocks
            && (ctx.cfg.empty_item_single_line || !governs_empty_single_line(node))
        {
            return concat(vec![open, close]);
        }
        let lead = if opens_on_next_line(node, ctx.cfg) {
            hardline()
        } else {
            nil()
        };
        return concat(vec![lead, open, hardline(), close]);
    }

    // `fn-single-line`: a declaration body holding exactly one statement and no comments
    // collapses onto the header's line when it fits `max-width`. The grouped layout renders
    // flat (`header { stmt }`) when it fits and falls back to the standard multi-line body
    // otherwise; the `if_break` lead keeps the brace on its own line in the broken case under
    // `brace-style = next-line` (and is not a forced break, so the flat form stays available).
    if ctx.cfg.fn_single_line
        && !ctx.cfg.force_multiline_blocks
        && !has_dangling
        && is_declaration_body(node)
        && single_statement_no_comments(node, ctx)
        && !header_has_trailing_comment(node, ctx)
    {
        let lead = if opens_on_next_line(node, ctx.cfg) {
            if_break(hardline(), nil())
        } else {
            nil()
        };
        return group(concat(vec![
            lead,
            open,
            indent(concat(vec![line(), inner])),
            line(),
            close,
        ]));
    }

    // The break after the opening `{`. Normally a plain `hardline`, but with
    // `blank_line_at_block_start` a leading blank line in the source is preserved (clamped by the
    // renderer), so the body's first item keeps its blank line just like the inter-item breaks do.
    let lead_break = match (ctx.cfg.blank_line_at_block_start, first_item_token(node)) {
        (true, Some(t)) => break_before(&t, ctx),
        _ => hardline(),
    };
    let mut body: Vec<Doc> = vec![lead_break];
    if any {
        body.push(inner);
    }
    if has_dangling {
        if any {
            body.push(hardline());
        }
        body.push(dangling);
    }

    // Under a `next-line` style a (non-empty) body opens its brace on its own line. The
    // leading break renders at the header's indentation; the separating space the parent
    // emitted before the brace is then trimmed away by the renderer.
    let lead = if opens_on_next_line(node, ctx.cfg) {
        hardline()
    } else {
        nil()
    };

    concat(vec![lead, open, indent(concat(body)), hardline(), close])
}

/// Whether the opening brace of braced `node` should sit on its own line. Declaration bodies
/// — every type body (`CLASS_BODY`), a module body (`MODULE_BODY`), and the block of a method,
/// constructor, or initializer — follow [`BraceStyle`]; control-flow blocks, `switch` blocks,
/// lambda bodies, and bare statement blocks follow [`ControlBraceStyle`].
fn opens_on_next_line(node: &SyntaxNode, cfg: &Config) -> bool {
    match node.kind() {
        S::CLASS_BODY | S::MODULE_BODY => cfg.brace_style == BraceStyle::NextLine,
        S::BLOCK if is_declaration_body(node) => cfg.brace_style == BraceStyle::NextLine,
        S::BLOCK | S::SWITCH_BLOCK => cfg.control_brace_style == ControlBraceStyle::NextLine,
        _ => false,
    }
}

/// Whether `node` (a `BLOCK`) is a declaration body — the block of a method, constructor, or
/// initializer — as opposed to a control-flow block, lambda body, or bare block.
fn is_declaration_body(node: &SyntaxNode) -> bool {
    matches!(
        node.parent().map(|p| p.kind()),
        Some(S::METHOD_DECL | S::CONSTRUCTOR_DECL | S::INITIALIZER)
    )
}

/// Whether an empty `node` is governed by [`empty_item_single_line`](Config::empty_item_single_line)
/// — a declaration body: a type body (`CLASS_BODY`), a module body (`MODULE_BODY`), or a method /
/// constructor / initializer block. Control-flow blocks, `switch` blocks, lambda bodies, and bare
/// blocks are never governed (they always keep `{}`), matching rustfmt's item-only scoping.
fn governs_empty_single_line(node: &SyntaxNode) -> bool {
    matches!(node.kind(), S::CLASS_BODY | S::MODULE_BODY)
        || (node.kind() == S::BLOCK && is_declaration_body(node))
}

/// Whether `node` (a braced body) holds exactly one statement and carries no comment anywhere
/// inside the braces — the precondition for [`fn_single_line`](Config::fn_single_line) to
/// collapse it onto one line. A comment (which must never be dropped or moved off its anchor)
/// or a second statement keeps the body multi-line.
fn single_statement_no_comments(node: &SyntaxNode, ctx: &Ctx<'_>) -> bool {
    let mut stmts = node.children().filter(|c| first_sig_token(c).is_some());
    if stmts.next().is_none() || stmts.next().is_some() {
        return false; // zero or more than one statement
    }
    !has_comments_in_subtree(node, ctx)
}

/// Whether any significant token of `node`'s parent declaration that precedes the body (`node`)
/// carries a trailing comment. Such a comment renders as a line suffix that flushes at the body's
/// first newline; collapsing the body onto one line under [`fn_single_line`](Config::fn_single_line)
/// would relocate it past the closing brace, re-anchoring it on the next parse and breaking
/// idempotency — so a header trailing comment keeps the body multi-line. (A comment *inside* the
/// braces is already caught by [`single_statement_no_comments`].)
fn header_has_trailing_comment(node: &SyntaxNode, ctx: &Ctx<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let body_start = node.text_range().start();
    parent
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia() && t.text_range().end() <= body_start)
        .any(|t| ctx.comments.has_trailing(&t))
}

/// Build the inner document for a sequence of item nodes. Returns the content and whether
/// any item was emitted. Braces are skipped (a brace wrapper adds them); blank lines from
/// the source are preserved (clamped by the renderer).
pub(crate) fn lower_items(node: &SyntaxNode, ctx: &Ctx<'_>) -> (Doc, bool) {
    let mut parts: Vec<Doc> = Vec::new();
    let mut saw = false;

    for el in node.children_with_tokens() {
        if let Some(child) = el.as_node() {
            // Skip empty nodes (e.g. an empty `MODIFIERS` produced by error recovery):
            // they carry no tokens, and emitting a separator for them would introduce
            // spurious blank lines that grow on re-formatting.
            if first_sig_token(child).is_none() {
                continue;
            }
            if saw {
                parts.push(item_separator(child, ctx));
            }
            parts.push(lower(child, ctx));
            saw = true;
        } else if let Some(t) = el.as_token() {
            let kind = t.kind();
            if kind == S::LBRACE || kind == S::RBRACE || kind.is_trivia() {
                continue;
            }
            // Stray significant token (e.g. a lone `;`); keep it, space-separated.
            if saw {
                parts.push(text(" "));
            }
            parts.push(tok(t, ctx));
            saw = true;
        }
    }
    (concat(parts), saw)
}

/// The token a leading blank line before the body's first item should anchor on: the first item
/// node's first significant token (or a leading stray significant token), skipping the opening
/// brace and trivia. Mirrors how [`lower_items`] picks the first item, so the blank-line run before
/// it is counted exactly as an inter-item break would count it.
fn first_item_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    for el in node.children_with_tokens() {
        match el {
            SyntaxElement::Node(child) => {
                if let Some(t) = first_sig_token(&child) {
                    return Some(t);
                }
            }
            SyntaxElement::Token(t) => {
                let kind = t.kind();
                if kind == S::LBRACE || kind == S::RBRACE || kind.is_trivia() {
                    continue;
                }
                return Some(t);
            }
        }
    }
    None
}

/// The line break before an item node: the source's blank-line run (clamped to
/// `max_blank_lines` by the renderer) when it had one, else a plain line break.
pub(crate) fn item_separator(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    match first_sig_token(node) {
        Some(t) => break_before(&t, ctx),
        None => hardline(),
    }
}

/// The line break before a row anchored at significant token `t`: the source's blank-line run
/// (clamped to `max_blank_lines` by the renderer) when one preceded it, else a plain line break.
/// The token-anchored core of [`item_separator`], shared with the enum-body lowering (which also
/// anchors on bare `;` tokens that have no containing item node).
pub(crate) fn break_before(t: &SyntaxToken, ctx: &Ctx<'_>) -> Doc {
    let blanks = if ctx.comments.has_leading(t) {
        ctx.comments.blank_lines_before_first(t)
    } else {
        blank_lines_before(t)
    };
    if blanks > 0 {
        blank_line(blanks)
    } else {
        hardline()
    }
}

/// The number of blank lines preceding `tok` in the source (0 when it is on the next line,
/// or on the same line as the previous token). A run of `n` consecutive newlines is `n - 1`
/// blank lines. A comment between stops the run, so a lone comment line is not a blank line.
pub(crate) fn blank_lines_before(tok: &SyntaxToken) -> usize {
    let mut newlines = 0usize;
    let mut cur = tok.prev_token();
    while let Some(t) = cur {
        match t.kind() {
            S::NEWLINE => newlines += 1,
            S::WHITESPACE => {}
            _ => break,
        }
        cur = t.prev_token();
    }
    newlines.saturating_sub(1)
}

/// Lower a *legacy* (colon-form) `switch` group — `(SwitchLabel ':')+ Stmt*` — per
/// [`SwitchCaseBody`]. The arrow form is a `SWITCH_RULE` node and never reaches here.
///
/// `SameLine` keeps the generic inline layout wholesale (`case X: stmt; stmt;`). `Always` (and a
/// `SingleLine` group that is not eligible to stay inline) puts each label on its own line and
/// breaks every body statement onto its own line, indented one level — google-java-format's
/// layout. `SingleLine` keeps a lone label with a single, comment-free statement inline. A
/// malformed group (error recovery — a label without a colon, a stray significant token) falls
/// back to the inline path, so every significant token is still emitted exactly once.
pub(crate) fn lower_switch_group(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    if ctx.cfg.switch_case_body == SwitchCaseBody::SameLine {
        return lower_generic(node, ctx);
    }
    let Some((labels, stmts)) = split_switch_group(node) else {
        return lower_generic(node, ctx);
    };
    // `single-line`: a single label with a single statement and no comments stays on the colon
    // line. A comment forces the broken, indented form so it renders correctly and idempotently.
    if ctx.cfg.switch_case_body == SwitchCaseBody::SingleLine
        && labels.len() == 1
        && stmts.len() == 1
        && !has_comments_in_subtree(node, ctx)
    {
        return lower_generic(node, ctx);
    }

    let mut parts: Vec<Doc> = Vec::new();
    for (i, (label, colon)) in labels.iter().enumerate() {
        // Each fall-through label takes its own line; `item_separator` preserves a leading
        // comment / blank line on the label, whose tokens are emitted by `lower_elements`.
        if i > 0 {
            parts.push(item_separator(label, ctx));
        }
        let els = [
            SyntaxElement::Node(label.clone()),
            SyntaxElement::Token(colon.clone()),
        ];
        parts.push(lower_elements(els.into_iter(), ctx, false));
    }
    // The body statements break onto their own lines, one indent level deeper than the labels;
    // the first statement's `item_separator` is the break from the last label's colon line.
    let mut body: Vec<Doc> = Vec::new();
    for stmt in &stmts {
        body.push(item_separator(stmt, ctx));
        body.push(lower(stmt, ctx));
    }
    if !body.is_empty() {
        parts.push(indent(concat(body)));
    }
    concat(parts)
}

/// Lower an *arrow-form* `switch` rule — `SwitchLabel '->' (Block | ThrowStmt | Expr ';')`.
///
/// When a comment forces a break right after `->` — a trailing comment on `->`, or a leading
/// comment on the body's first token — the `->` and the body hang at one *continuation* indent
/// past the label (the continuation-indent counterpart to the legacy colon group's block-indented
/// body, [`lower_switch_group`]). Gating on a forced break keeps every comment-free body
/// byte-for-byte unchanged (a long body still wraps on the arrow line, a `{ … }` block still aligns
/// its `}` with the label); and because the wrap only fires when the body is already on its own
/// line, shifting the *whole* body to the continuation level is correct even for an
/// anonymous-class / lambda body (its `}` stays aligned with the hung body). A `{ … }` body is
/// excluded (blocks never take a continuation indent), and a malformed rule with no `->` falls
/// back to the inline path, so every significant token is still emitted exactly once.
pub(crate) fn lower_switch_rule(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    // Children (grammar): `SwitchLabel '->' (Block | ThrowStmt | Expr ';')`. The label is the only
    // significant content before the `->`, so the rule splits cleanly into the label and a tail.
    let label = node.children().find(|n| n.kind() == S::SWITCH_LABEL);
    let arrow = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == S::ARROW);
    let (Some(label), Some(arrow)) = (label, arrow) else {
        return lower_generic(node, ctx); // malformed: missing the label or the `->`
    };

    // A `{ … }` block body keeps the generic layout — its `{` rides on the arrow line and it aligns
    // its own `}` with the label, so it is never hung at a continuation indent.
    let body = node.children().find(|n| n.kind() != S::SWITCH_LABEL);
    if body.as_ref().map(|n| n.kind()) == Some(S::BLOCK) {
        return lower_generic(node, ctx);
    }

    // The body lands on its own line only when a comment forces a break right after `->` — a
    // trailing comment on `->`, or a leading comment on the body's first significant token.
    let forces_break = ctx.comments.has_trailing(&arrow)
        || body
            .as_ref()
            .and_then(first_sig_token)
            .is_some_and(|t| ctx.comments.has_leading(&t));
    if !forces_break {
        return lower_generic(node, ctx);
    }

    // The label stays at the rule's level; the `->` and the body hang at one continuation indent,
    // so the comment-forced body line sits one level past the label. The `->` is inside the wrap
    // so the break its trailing comment carries — and any leading-comment break on the body — is
    // requested at `base + continuation`, governing where the body's first line lands.
    let arrow_sep = sep(last_sig_token(&label).as_ref(), &arrow, ctx.cfg);
    let tail = node
        .children_with_tokens()
        .skip_while(|e| e.as_token().map(|t| t.kind()) != Some(S::ARROW));
    concat(vec![
        lower(&label, ctx),
        arrow_sep,
        continuation_indent(lower_elements(tail, ctx, false)),
    ])
}

/// A switch label paired with its terminating `:` token.
type SwitchLabelPair = (SyntaxNode, SyntaxToken);

/// Split a `SWITCH_GROUP` into its `(label, ':')` pairs and its body statements. Returns `None`
/// for a malformed group (a label without a following colon, a statement before a colon, a stray
/// colon / significant token, or no labels at all) so the caller can fall back to inline emission
/// and preserve every token. Empty (token-less) statement nodes from error recovery are skipped.
fn split_switch_group(node: &SyntaxNode) -> Option<(Vec<SwitchLabelPair>, Vec<SyntaxNode>)> {
    let mut labels: Vec<SwitchLabelPair> = Vec::new();
    let mut stmts: Vec<SyntaxNode> = Vec::new();
    let mut pending: Option<SyntaxNode> = None;
    for el in node.children_with_tokens() {
        if let Some(n) = el.as_node() {
            if n.kind() == S::SWITCH_LABEL {
                // A label must precede every statement and follow a colon-terminated label.
                if pending.is_some() || !stmts.is_empty() {
                    return None;
                }
                pending = Some(n.clone());
            } else if first_sig_token(n).is_some() {
                if pending.is_some() {
                    return None; // a statement before its label's colon
                }
                stmts.push(n.clone());
            }
        } else if let Some(t) = el.as_token() {
            let kind = t.kind();
            if kind == S::COLON {
                match pending.take() {
                    Some(label) => labels.push((label, t.clone())),
                    None => return None, // a stray colon
                }
            } else if !kind.is_trivia() {
                return None; // a stray significant token
            }
        }
    }
    if pending.is_some() || labels.is_empty() {
        return None;
    }
    Some((labels, stmts))
}

/// Whether any significant token in `node`'s subtree carries a comment. Used to keep a body that
/// holds a comment (which must never be dropped or moved off its anchor) out of a one-line layout.
fn has_comments_in_subtree(node: &SyntaxNode, ctx: &Ctx<'_>) -> bool {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .any(|t| ctx.comments.has_comments(&t))
}
