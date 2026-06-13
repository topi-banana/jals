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
//! (params, args, array initializers) wrap all-or-nothing (except that
//! `overflow-delimited-expr` may hang a call's trailing lambda / anonymous class / array
//! initializer past the call line); binary/unary expressions get
//! canonical operator spacing, and a binary expression that overflows `max-width` wraps at
//! its operators (placement per `binop-separator`); everything else falls back to
//! [`lower_generic`], which lays a node out inline with normalized spacing. Source-file
//! layout, including import reordering/grouping, lives in [`crate::imports`].

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::comments::{self, CommentMap};
use crate::config::{
    BinopSeparator, BraceStyle, Config, ControlBraceStyle, FnParamsLayout, TrailingComma,
};
use crate::doc::{
    Doc, blank_line, concat, fill, group, group_always_break, group_overflow, group_within,
    hardline, if_break, indent, line, nil, raw, softline, text,
};

/// Lowering context shared (immutably) across the walk.
pub(crate) struct Ctx<'a> {
    pub(crate) comments: CommentMap,
    pub(crate) cfg: &'a Config,
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
pub(crate) fn lower(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    match node.kind() {
        S::SOURCE_FILE => crate::imports::lower_source_file(node, ctx),
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
pub(crate) fn tok(tok: &SyntaxToken, ctx: &Ctx<'_>) -> Doc {
    ctx.comments.token(tok, token_text(tok))
}

/// The first non-trivia token contained in `node`, if any.
pub(crate) fn first_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
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
fn want_space(prev: S, next: S, cfg: &Config) -> bool {
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
    // Colon spacing (ternary `?:`, enhanced-`for`, labels, `assert`, `case` / `default`) is
    // configurable and applies uniformly to every colon context. `::` is a distinct token
    // (`COLON_COLON`, handled above as a no-space operator) and is never affected. The
    // structural no-space rules above take precedence, so a colon abutting `)` / `,` / `;`
    // (only reachable through error recovery) never gains a stray space.
    if next == COLON {
        return cfg.space_before_colon;
    }
    if prev == COLON {
        return cfg.space_after_colon;
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
fn sep(prev: Option<&SyntaxToken>, next: &SyntaxToken, cfg: &Config) -> Doc {
    let Some(p) = prev else {
        return nil();
    };
    let (pk, nk) = (p.kind(), next.kind());
    // Preserve `>>`, `>>>`, `>=`, `>>=` fusion exactly as the source had it.
    if pk == S::GT && (nk == S::GT || nk == S::EQ) {
        return if adjacent(p, next) { nil() } else { text(" ") };
    }
    let space = want_space(pk, nk, cfg) || would_fuse(p.text(), next.text());
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
    sep(prev, next, ctx.cfg)
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

// ---------------------------------------------------------------------------
// Delimited lists (params, args, array initializers)
// ---------------------------------------------------------------------------

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
/// [`trailing_comma_doc`]) — the only Java list where adding or dropping it is legal.
fn lower_delimited(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    // Never synthesize a delimiter that the source lacks (error recovery): start empty
    // and fill from the real tokens so the significant-token sequence is preserved.
    let mut open_doc = nil();
    let mut close_doc = nil();
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
                S::RPAREN | S::RBRACE | S::RBRACK => close_doc = tok(t, ctx),
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
    // recovery, so dropping/adding one is never appropriate).
    let policy = if node.kind() == S::ARRAY_INIT {
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
                trailing_comma_doc(policy, comma.as_ref(), ctx)
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
        let head = concat(vec![open_doc, indent(concat(head_inner))]);
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
        indent(concat(vec![softline(), inner])),
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

/// The document for the trailing comma of a delimited list's last item, per the
/// [`TrailingComma`] policy. `comma` is the source's trailing comma token when it had one.
///
/// A source comma that carries a comment is always kept verbatim — even when the policy would
/// drop it — so no comment is lost. Under [`Vertical`](TrailingComma::Vertical) the comma is an
/// [`if_break`]: it materializes only when the enclosing list breaks across lines.
fn trailing_comma_doc(policy: TrailingComma, comma: Option<&SyntaxToken>, ctx: &Ctx<'_>) -> Doc {
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

// ---------------------------------------------------------------------------
// Expressions with canonical operator spacing
// ---------------------------------------------------------------------------

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

/// Lower a binary expression. The same-precedence run is one group with a break point at
/// every operator: flat it is `a op b op c`; when it overflows `max-width` every step wraps,
/// the operator leading the continuation line (`binop-separator = front`, the default) or
/// trailing the broken line (`back`). A run of operator tokens (e.g. the two `>` of `>>`)
/// is joined tightly so operator fusion is preserved.
fn lower_binary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let Some((first, steps)) = flatten_binary(node) else {
        // Error recovery produced something other than `lhs op rhs`; emit inline with
        // canonical spacing so every token is preserved verbatim.
        return lower_binary_inline(node, ctx);
    };
    let mut tail: Vec<Doc> = Vec::new();
    for (ops, rhs) in &steps {
        let op = concat(ops.iter().map(|t| tok(t, ctx)).collect());
        match ctx.cfg.binop_separator {
            BinopSeparator::Front => tail.extend([line(), op, text(" "), lower(rhs, ctx)]),
            BinopSeparator::Back => tail.extend([text(" "), op, line(), lower(rhs, ctx)]),
        }
    }
    group(concat(vec![lower(&first, ctx), indent(concat(tail))]))
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
