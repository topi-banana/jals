//! Lower the CST into a [`Doc`].
//!
//! Every token is a direct child of exactly one node, so as long as each rule emits all
//! of its direct-child significant tokens (and recurses into child nodes via [`lower`]),
//! every token is emitted exactly once — guaranteeing the significant-token invariant
//! regardless of how much bespoke coverage exists. Comments are attached to tokens by a
//! pre-pass ([`crate::comments`]) and injected when those tokens are emitted, so they are
//! never dropped or duplicated either.
//!
//! Structural nodes (source file, bodies, blocks) get multi-line layout; delimited lists
//! (params, args, array initializers) wrap all-or-nothing; binary/unary expressions get
//! canonical operator spacing; everything else falls back to [`lower_generic`], which lays
//! a node out inline with normalized spacing.

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::comments::{self, CommentMap};
use crate::config::{BraceStyle, Config, ControlBraceStyle};
use crate::doc::{
    Doc, blank_line, concat, group, group_within, hardline, indent, line, nil, raw, softline, text,
};

/// Lowering context shared (immutably) across the walk.
struct Ctx<'a> {
    comments: CommentMap,
    cfg: &'a Config,
}

/// Lower the whole tree.
pub(crate) fn lower_root(root: &SyntaxNode, cfg: &Config) -> Doc {
    let ctx = Ctx {
        comments: comments::build(root),
        cfg,
    };
    let body = lower(root, &ctx);
    // Append any orphan comments (a file containing only comments has no token to anchor
    // them to). `orphan_doc` is empty unless the file has no significant tokens.
    concat(vec![body, ctx.comments.orphan_doc()])
}

/// Lower a node, dispatching on its kind.
fn lower(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    match node.kind() {
        S::SOURCE_FILE => lower_items(node, ctx).0,
        S::CLASS_BODY | S::BLOCK | S::SWITCH_BLOCK => lower_braced(node, ctx),
        S::PARAM_LIST | S::ARG_LIST | S::RECORD_HEADER | S::ANNOTATION_ARG_LIST | S::ARRAY_INIT => {
            lower_delimited(node, ctx)
        }
        S::IF_STMT | S::TRY_STMT | S::DO_WHILE_STMT => lower_control_flow(node, ctx),
        S::BINARY_EXPR => lower_binary(node, ctx),
        S::UNARY_EXPR => lower_unary(node, ctx),
        S::CALL_EXPR | S::FIELD_ACCESS => lower_chain(node, ctx),
        _ => lower_generic(node, ctx),
    }
}

// ---------------------------------------------------------------------------
// Token emission and spacing
// ---------------------------------------------------------------------------

/// The bare text of a significant token. String literals and text blocks are emitted as
/// raw (verbatim) text so their multi-line content is never reindented.
fn token_text(tok: &SyntaxToken) -> Doc {
    match tok.kind() {
        S::STRING_LITERAL | S::TEXT_BLOCK => raw(tok.text().to_string()),
        _ => text(tok.text().to_string()),
    }
}

/// A significant token with its attached comments.
fn tok(tok: &SyntaxToken, ctx: &Ctx<'_>) -> Doc {
    ctx.comments.token(tok, token_text(tok))
}

/// The first non-trivia token contained in `node`, if any.
fn first_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| !t.kind().is_trivia())
}

/// The last non-trivia token contained in `node`, if any.
fn last_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .last()
}

/// The aesthetic spacing rule between two significant tokens (before the fusion-safety
/// net in [`sep`] is applied).
fn want_space(prev: S, next: S) -> bool {
    use S::*;
    // No space after these.
    if matches!(
        prev,
        LPAREN | LBRACK | DOT | COLON_COLON | AT | BANG | TILDE | PLUS_PLUS | MINUS_MINUS | LT
    ) {
        return false;
    }
    // No space before these.
    if matches!(
        next,
        COMMA
            | SEMICOLON
            | DOT
            | COLON_COLON
            | ELLIPSIS
            | RPAREN
            | RBRACK
            | LT
            | GT
            | PLUS_PLUS
            | MINUS_MINUS
    ) {
        return false;
    }
    // `(` hugs a preceding callee/array; keywords get a space before it.
    if next == LPAREN {
        return !matches!(prev, IDENT | RPAREN | RBRACK | SUPER_KW | THIS_KW | GT);
    }
    true
}

/// Are `prev` and `next` adjacent in the source (no trivia between them)?
fn adjacent(prev: &SyntaxToken, next: &SyntaxToken) -> bool {
    usize::from(prev.text_range().end()) == usize::from(next.text_range().start())
}

/// Whether concatenating `a` and `b` would lex as anything other than the two tokens
/// `a` then `b` — i.e. they must not be placed adjacent (e.g. `-` and `>` form `->`).
fn would_fuse(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let joined = format!("{a}{b}");
    let toks = jals_syntax::tokenize(&joined);
    !(toks.len() == 2 && toks[0].text == a && toks[1].text == b)
}

/// The separator document between `prev` (if any) and the token `next`. Applies the
/// aesthetic rule, then a fusion-safety net so the output never changes operator fusion.
fn sep(prev: Option<&SyntaxToken>, next: &SyntaxToken) -> Doc {
    let Some(p) = prev else {
        return nil();
    };
    let (pk, nk) = (p.kind(), next.kind());
    // Preserve `>>`, `>>>`, `>=`, `>>=` fusion exactly as the source had it.
    if pk == S::GT && (nk == S::GT || nk == S::EQ) {
        return if adjacent(p, next) { nil() } else { text(" ") };
    }
    let space = want_space(pk, nk) || would_fuse(p.text(), next.text());
    if space { text(" ") } else { nil() }
}

/// A separator that keeps two tokens tight unless they would fuse (used for unary
/// operators, e.g. `-x` but `- -x`).
fn tight_sep(prev: Option<&SyntaxToken>, next: &SyntaxToken) -> Doc {
    match prev {
        Some(p) if would_fuse(p.text(), next.text()) => text(" "),
        _ => nil(),
    }
}

// ---------------------------------------------------------------------------
// Generic (inline) lowering — the universal fallback
// ---------------------------------------------------------------------------

/// Lay a node out inline: child nodes are recursed, tokens are separated by single
/// spaces per [`want_space`]. Whitespace, newlines, and comment trivia are skipped here
/// (comments are injected via [`tok`]).
fn lower_generic(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    lower_inline(node, ctx, false)
}

/// Lay out a control-flow statement (`if` / `try` / `do-while`) inline, honoring
/// [`ControlBraceStyle`]: under `next-line`, a continuation that directly follows a closing
/// brace (`} else`, `} catch`, `} finally`, `} while`) moves onto its own line. (The opening
/// brace of each block is handled separately, by [`lower_braced`] via [`opens_on_next_line`].)
/// With the default `same-line` it is byte-for-byte identical to [`lower_generic`].
fn lower_control_flow(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    lower_inline(node, ctx, true)
}

/// Shared core of [`lower_generic`] and [`lower_control_flow`]. When `control_flow` is set,
/// the separator before a `}`-anchored continuation becomes a forced break under
/// `control-brace-style = next-line` (see [`flow_sep`]).
fn lower_inline(node: &SyntaxNode, ctx: &Ctx<'_>, control_flow: bool) -> Doc {
    lower_elements(node.children_with_tokens(), ctx, control_flow)
}

/// Lay out an arbitrary run of CST elements inline. The element loop is shared between
/// [`lower_inline`] (a whole node's children) and chain-selector emission, which feeds it a
/// `FIELD_ACCESS`'s children minus the receiver (see [`lower_after_first_node`]); routing both
/// through here keeps the type-witness hug below in one place.
fn lower_elements(
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
            if let Some(first) = first_sig_token(child) {
                let s = if hug_witness {
                    nil()
                } else {
                    flow_sep(ctx, control_flow, prev.as_ref(), child.kind(), &first)
                };
                parts.push(s);
            }
            hug_witness = child.kind() == S::TYPE_ARGS
                && matches!(
                    prev.as_ref().map(|t| t.kind()),
                    Some(S::DOT | S::COLON_COLON)
                );
            parts.push(lower(child, ctx));
            if let Some(last) = last_sig_token(child) {
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
    sep(prev, next)
}

// ---------------------------------------------------------------------------
// Method chains (`a.b().c().d()`)
// ---------------------------------------------------------------------------

/// One `.selector` step of a method chain. `callee` is the `FIELD_ACCESS` carrying the dot,
/// optional type witness, and member name; `call` is the enclosing `CALL_EXPR` when the step
/// is a method invocation (it holds the `ARG_LIST`), or `None` for a plain field access.
struct ChainLink {
    callee: SyntaxNode,
    call: Option<SyntaxNode>,
}

impl ChainLink {
    fn is_call(&self) -> bool {
        self.call.is_some()
    }
}

/// Lower a `FIELD_ACCESS` / `CALL_EXPR`. A chain with at least two method calls is laid out
/// breakable: the receiver and any leading field accesses stay on the first line, the first
/// call hugs them, and each later `.call()` / `.field` wraps onto its own indented line when
/// the chain does not fit `max-width` or its flat width exceeds `chain-width`. Anything else
/// (a lone call, a pure field path `a.b.c`, a malformed node) falls back to inline emission,
/// byte-for-byte unchanged.
fn lower_chain(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let Some((head, links)) = flatten_chain(node) else {
        return lower_inline(node, ctx, false);
    };
    // Count every method invocation in the chain — the head itself if it is a call/`new`, plus
    // each call link — so `a.b.c` (no calls) and `foo.bar()` (one) stay inline.
    let calls = links.iter().filter(|l| l.is_call()).count()
        + usize::from(matches!(head.kind(), S::CALL_EXPR | S::NEW_EXPR));
    if calls < 2 {
        return lower_inline(node, ctx, false);
    }

    // Leading field accesses (before the first call) ride on the head's line, so
    // `this.config.foo().bar()` keeps `this.config` together instead of breaking every dot.
    let first_call = links.iter().position(ChainLink::is_call).unwrap_or(0);
    let (lead, rest) = links.split_at(first_call);

    let mut head_line = vec![lower(&head, ctx)];
    for link in lead {
        head_line.push(lower_link(link, ctx));
    }
    // The first call hugs the head; subsequent steps wrap one per line.
    let mut rest = rest.iter();
    if let Some(first) = rest.next() {
        head_line.push(lower_link(first, ctx));
    }
    let mut wrapped: Vec<Doc> = Vec::new();
    for link in rest {
        wrapped.push(softline());
        wrapped.push(lower_link(link, ctx));
    }

    let doc = concat(vec![concat(head_line), indent(concat(wrapped))]);
    group_within(doc, ctx.cfg.chain_width)
}

/// Lower one chain step: its `.selector`, plus the argument list when it is a call.
fn lower_link(link: &ChainLink, ctx: &Ctx<'_>) -> Doc {
    let selector = lower_after_first_node(&link.callee, ctx);
    match &link.call {
        Some(call) => concat(vec![selector, lower_after_first_node(call, ctx)]),
        None => selector,
    }
}

/// Flatten a left-nested chain into its head (base) expression and the `.selector` steps in
/// source order. Returns `None` when `node` applies no `.`-selector to a receiver (so it is
/// not a chain), letting the caller fall back to inline emission.
fn flatten_chain(node: &SyntaxNode) -> Option<(SyntaxNode, Vec<ChainLink>)> {
    let mut links: Vec<ChainLink> = Vec::new();
    let mut cur = node.clone();
    let head = loop {
        match cur.kind() {
            S::FIELD_ACCESS => {
                let recv = first_child_node(&cur)?;
                links.push(ChainLink {
                    callee: cur.clone(),
                    call: None,
                });
                cur = recv;
            }
            S::CALL_EXPR => {
                let callee = first_child_node(&cur)?;
                // `foo(...)` (callee is a bare name, not `recv.method`) is the chain head.
                if callee.kind() != S::FIELD_ACCESS {
                    break cur;
                }
                let recv = first_child_node(&callee)?;
                links.push(ChainLink {
                    callee,
                    call: Some(cur.clone()),
                });
                cur = recv;
            }
            _ => break cur,
        }
    };
    if links.is_empty() {
        return None;
    }
    links.reverse();
    Some((head, links))
}

/// The first child node (skipping tokens) of `node`.
fn first_child_node(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.children().next()
}

/// Lower every child of `node` except its first child node, reusing the inline element loop.
/// For a chain step the dropped child is the receiver / callee (the spine continuation, lowered
/// separately), so the emitted part is exactly this step's `.`, type witness, name, and — for a
/// `CALL_EXPR` — its argument list. Every token is still emitted exactly once.
fn lower_after_first_node(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let mut dropped = false;
    let els = node.children_with_tokens().filter(move |el| {
        if !dropped && el.as_node().is_some() {
            dropped = true;
            return false;
        }
        true
    });
    lower_elements(els, ctx, false)
}

// ---------------------------------------------------------------------------
// Sequences of statements / members
// ---------------------------------------------------------------------------

/// Lower a `{ ... }` node (block, class body, switch body) with one indentation level.
fn lower_braced(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
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

    // An empty body collapses to `{}` on the header's line regardless of brace style
    // (cf. rustfmt's `empty_item_single_line`), so `next-line` never strands a lone `{}`.
    if !any && !has_dangling {
        return concat(vec![open, close]);
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
/// — every type body (`CLASS_BODY`) and the block of a method, constructor, or initializer —
/// follow [`BraceStyle`]; control-flow blocks, `switch` blocks, lambda bodies, and bare
/// statement blocks follow [`ControlBraceStyle`].
fn opens_on_next_line(node: &SyntaxNode, cfg: &Config) -> bool {
    match node.kind() {
        S::CLASS_BODY => cfg.brace_style == BraceStyle::NextLine,
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

/// Build the inner document for a sequence of item nodes. Returns the content and whether
/// any item was emitted. Braces are skipped (a brace wrapper adds them); blank lines from
/// the source are preserved (clamped by the renderer).
fn lower_items(node: &SyntaxNode, ctx: &Ctx<'_>) -> (Doc, bool) {
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
fn item_separator(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    match first_sig_token(node) {
        Some(t) => {
            let blanks = if ctx.comments.has_leading(&t) {
                ctx.comments.blank_lines_before_first(&t)
            } else {
                blank_lines_before(&t)
            };
            if blanks > 0 {
                blank_line(blanks)
            } else {
                hardline()
            }
        }
        None => hardline(),
    }
}

/// The number of blank lines preceding `tok` in the source (0 when it is on the next line,
/// or on the same line as the previous token). A run of `n` consecutive newlines is `n - 1`
/// blank lines. A comment between stops the run, so a lone comment line is not a blank line.
fn blank_lines_before(tok: &SyntaxToken) -> usize {
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

// ---------------------------------------------------------------------------
// Delimited lists (params, args, array initializers)
// ---------------------------------------------------------------------------

/// Lower a comma-separated, delimited list that wraps all-or-nothing. Each item carries
/// its own trailing comma (so a trailing comma in the source is preserved), and items are
/// separated by a soft line that becomes a space when flat and a break when wrapped.
fn lower_delimited(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    // Never synthesize a delimiter that the source lacks (error recovery): start empty
    // and fill from the real tokens so the significant-token sequence is preserved.
    let mut open_doc = nil();
    let mut close_doc = nil();
    let mut items: Vec<Doc> = Vec::new();
    let mut current: Vec<Doc> = Vec::new();
    let mut cur_prev: Option<SyntaxToken> = None;

    for el in node.children_with_tokens() {
        if let Some(child) = el.as_node() {
            if let Some(first) = first_sig_token(child) {
                current.push(sep(cur_prev.as_ref(), &first));
            }
            current.push(lower(child, ctx));
            cur_prev = last_sig_token(child);
        } else if let Some(t) = el.as_token() {
            let kind = t.kind();
            match kind {
                S::LPAREN | S::LBRACE | S::LBRACK => open_doc = tok(t, ctx),
                S::RPAREN | S::RBRACE | S::RBRACK => close_doc = tok(t, ctx),
                S::COMMA => {
                    // The comma ends the current item; items are joined by a soft line.
                    current.push(tok(t, ctx));
                    items.push(concat(std::mem::take(&mut current)));
                    cur_prev = None;
                }
                _ if kind.is_trivia() => {}
                _ => {
                    current.push(sep(cur_prev.as_ref(), t));
                    current.push(tok(t, ctx));
                    cur_prev = Some(t.clone());
                }
            }
        }
    }
    if !current.is_empty() {
        items.push(concat(std::mem::take(&mut current)));
    }

    if items.is_empty() {
        return concat(vec![open_doc, close_doc]);
    }

    group(concat(vec![
        open_doc,
        indent(concat(vec![softline(), crate::doc::join(line(), items)])),
        softline(),
        close_doc,
    ]))
}

// ---------------------------------------------------------------------------
// Expressions with canonical operator spacing
// ---------------------------------------------------------------------------

/// Lower a binary expression as `lhs op rhs`, joining a run of operator tokens (e.g. the
/// two `>` of `>>`) tightly and surrounding the whole operator with single spaces.
fn lower_binary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
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

/// Lower a unary expression tight (`-x`), inserting a space only when the operator and
/// operand would otherwise fuse (`- -x`, `+ +x`).
fn lower_unary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
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
