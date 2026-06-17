//! Sequences of statements / members.
//!
//! Lowers a `{ ... }` body (block, class body, switch body) with one indentation level, plus the
//! item sequences inside it. Brace placement follows [`BraceStyle`] / [`ControlBraceStyle`], and a
//! few options (`empty-item-single-line`, `fn-single-line`, `force-multiline-blocks`) tune the
//! empty / single-statement cases. Blank lines from the source are preserved (clamped by the
//! renderer).

use jals_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{BraceStyle, Config, ControlBraceStyle};
use crate::doc::{Doc, blank_line, concat, group, hardline, if_break, indent, line, nil, text};
use crate::lower::{Ctx, first_sig_token, lower, lower_generic, tok};

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
    let has_dangling = ctx.comments.has_leading(rbrace);
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

    let mut body: Vec<Doc> = vec![hardline()];
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
    !node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .any(|t| ctx.comments.has_comments(&t))
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
