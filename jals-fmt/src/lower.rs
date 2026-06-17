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
//! layout, including import reordering/grouping, lives in [`crate::imports`]; modifier
//! reordering (`reorder-modifiers`) lives in [`crate::modifiers`].

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::comments::{self, CommentMap};
use crate::config::{
    BinopSeparator, BraceStyle, Config, ControlBraceStyle, FloatLiteralTrailingZero,
    FnParamsLayout, HexLiteralCase, LiteralSuffixCase, TrailingComma, TypePunctuationDensity,
};
use crate::doc::{
    Doc, blank_line, concat, continuation_indent, fill, group, group_always_break, group_overflow,
    group_within, hardline, if_break, indent, line, nil, raw, softline, text,
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
        S::TERNARY_EXPR => lower_ternary(node, ctx),
        S::UNARY_EXPR => lower_unary(node, ctx),
        S::CALL_EXPR | S::FIELD_ACCESS => lower_chain(node, ctx),
        S::MODIFIERS => crate::modifiers::lower_modifiers(node, ctx),
        S::NON_SEALED_KW => lower_non_sealed(node, ctx),
        _ => lower_generic(node, ctx),
    }
}

/// Lower the `non-sealed` modifier. Its three tokens (`non` `-` `sealed`) form one keyword, so
/// they are emitted tight (no spaces) — the generic path would insert spaces and produce the
/// non-keyword `non - sealed`. Comments attached to any of the tokens are preserved.
fn lower_non_sealed(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let parts: Vec<Doc> = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| tok(&t, ctx))
        .collect();
    concat(parts)
}

// ---------------------------------------------------------------------------
// Token emission and spacing
// ---------------------------------------------------------------------------

/// The bare text of a significant token. String literals and text blocks are emitted as
/// raw (verbatim) text so their multi-line content is never reindented. An integer / float
/// literal is normalized by the literal rules, each applied in turn: [`FloatLiteralTrailingZero`]
/// adjusts a decimal float's trailing zero, [`HexLiteralCase`] normalizes a hex literal's digit
/// case, and [`LiteralSuffixCase`] normalizes the trailing `l` / `f` / `d` type suffix (each a
/// no-op under its default `Preserve`). The three touch disjoint parts of a literal — the
/// fraction, the hex mantissa, and the final suffix letter respectively — so they compose without
/// interference and the order is immaterial.
fn token_text(tok: &SyntaxToken, cfg: &Config) -> Doc {
    match tok.kind() {
        S::STRING_LITERAL | S::TEXT_BLOCK => raw(tok.text().to_string()),
        S::INT_LITERAL | S::FLOAT_LITERAL => {
            let original = tok.text();
            let mut current = original.to_string();
            if let Some(s) = map_float_trailing_zero(&current, cfg.float_literal_trailing_zero) {
                current = s;
            }
            if let Some(s) = map_hex_case(&current, cfg.hex_literal_case) {
                current = s;
            }
            if let Some(s) = map_literal_suffix(&current, tok.kind(), cfg.literal_suffix_case) {
                current = s;
            }
            text(current)
        }
        _ => text(tok.text().to_string()),
    }
}

/// Normalize the trailing zero of a **decimal** floating-point literal `lit` per `policy`,
/// returning the rewritten text — or `None` when nothing should change (the policy is
/// [`Preserve`](FloatLiteralTrailingZero::Preserve), or `lit` is out of scope).
///
/// Out of scope (always `None`): a hex literal (`0x…`), a literal with no `.` (a dotless float
/// `1e10` / `100f`, or any integer), and — for [`Never`](FloatLiteralTrailingZero::Never) — a
/// leading-dot float (`.5` / `.0`, whose fraction can never be stripped without producing the
/// illegal bare `.`). The literal's text is pure ASCII, so byte indices are char indices.
///
/// Let `frac` be the digit/underscore run right after the `.` (it stops at an `e` / `E` exponent
/// marker or an `f` / `F` / `d` / `D` suffix). [`Always`](FloatLiteralTrailingZero::Always) inserts
/// a single `0` when `frac` is empty; [`Never`](FloatLiteralTrailingZero::Never) removes `frac`
/// when it is non-empty and consists solely of `0`s (so `1.50` and underscore fractions like
/// `1.0_0` are left intact). Both transforms preserve the numeric value, the suffix, and the
/// exponent, and are idempotent in a single pass.
fn map_float_trailing_zero(lit: &str, policy: FloatLiteralTrailingZero) -> Option<String> {
    if policy == FloatLiteralTrailingZero::Preserve {
        return None;
    }
    let bytes = lit.as_bytes();
    // Hex literals (`0x` / `0X`) are out of scope — left to `hex-literal-case`.
    if bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X') {
        return None;
    }
    // A dotless float (`1e10`, `100f`) or any integer literal has no fraction to normalize.
    let dot = bytes.iter().position(|&b| b == b'.')?;
    // The fraction is the digit/underscore run after the `.`; it ends at the exponent or suffix.
    let mut frac_end = dot + 1;
    while frac_end < bytes.len() && (bytes[frac_end].is_ascii_digit() || bytes[frac_end] == b'_') {
        frac_end += 1;
    }
    match policy {
        FloatLiteralTrailingZero::Always if frac_end == dot + 1 => {
            // Empty fraction: insert a single `0` right after the `.` (`1.` → `1.0`).
            Some(format!("{}0{}", &lit[..dot + 1], &lit[dot + 1..]))
        }
        FloatLiteralTrailingZero::Never
            // Non-empty integer part (so the bare `.` is never produced) and an all-zero fraction.
            if dot > 0
                && frac_end > dot + 1
                && bytes[dot + 1..frac_end].iter().all(|&b| b == b'0') =>
        {
            // Strip the whole zero run at once (`1.0` / `1.00` → `1.`) — required for idempotency.
            Some(format!("{}{}", &lit[..dot + 1], &lit[frac_end..]))
        }
        _ => None,
    }
}

/// Normalize the case of the hexadecimal digit letters (`a`–`f` / `A`–`F`) of `lit` per
/// `case`, returning the rewritten text — or `None` when nothing should change (the policy is
/// [`Preserve`](HexLiteralCase::Preserve), or `lit` is not a hex literal).
///
/// Only the hex *mantissa* digits are remapped. The `0x` / `0X` prefix is kept verbatim; for a
/// hex float the mantissa stops at the `p` / `P` exponent marker (the marker, its sign and
/// decimal digits, and any `f` / `F` / `d` / `D` suffix follow unchanged); for a hex integer it
/// stops before a trailing `l` / `L` suffix. The mantissa of a well-formed literal holds only
/// hex digits, `.`, and `_`, so an ASCII case map touches exactly the `a`–`f` letters.
fn map_hex_case(lit: &str, case: HexLiteralCase) -> Option<String> {
    if case == HexLiteralCase::Preserve {
        return None;
    }
    let bytes = lit.as_bytes();
    // A hex literal: `0x` / `0X` prefix. (The lexer only emits such a token with at least one
    // hex digit after the prefix, and a numeric token's text is pure ASCII.)
    if bytes.len() < 2 || bytes[0] != b'0' || !matches!(bytes[1], b'x' | b'X') {
        return None;
    }
    let mantissa_end = match bytes[2..].iter().position(|b| matches!(b, b'p' | b'P')) {
        // Hex float: the mantissa ends at the `p` / `P` exponent marker.
        Some(i) => i + 2,
        // Hex integer: the mantissa ends before a trailing `l` / `L` suffix, if any.
        None if matches!(bytes.last(), Some(b'l' | b'L')) => lit.len() - 1,
        None => lit.len(),
    };
    let mantissa = &lit[2..mantissa_end];
    let mapped = match case {
        HexLiteralCase::Upper => mantissa.to_ascii_uppercase(),
        HexLiteralCase::Lower => mantissa.to_ascii_lowercase(),
        HexLiteralCase::Preserve => unreachable!("handled above"),
    };
    Some(format!("{}{}{}", &lit[..2], mapped, &lit[mantissa_end..]))
}

/// Normalize the case of the trailing type suffix of the numeric literal `lit` (whose token is
/// `kind`) per `case`, returning the rewritten text — or `None` when nothing should change (the
/// policy is [`Preserve`](LiteralSuffixCase::Preserve), the literal carries no suffix, or it is
/// already in the requested case).
///
/// The suffix is always the literal's final character: the `l` / `L` `long` suffix of an
/// [`INT_LITERAL`](S::INT_LITERAL), or the `f` / `F` / `d` / `D` `float` / `double` suffix of a
/// [`FLOAT_LITERAL`](S::FLOAT_LITERAL). The token kind disambiguates the otherwise ambiguous
/// trailing letters: a final `f` / `d` on an integer literal is a hex digit (`0xabcdef`), not a
/// suffix, and a float literal never ends in `l` / `L`. Only that one letter is remapped; the
/// value, radix prefix, mantissa, and exponent are kept verbatim. The literal's text is pure
/// ASCII, so the final byte is the final character.
fn map_literal_suffix(lit: &str, kind: S, case: LiteralSuffixCase) -> Option<String> {
    if case == LiteralSuffixCase::Preserve {
        return None;
    }
    let last = *lit.as_bytes().last()?;
    let is_suffix = match kind {
        S::INT_LITERAL => matches!(last, b'l' | b'L'),
        S::FLOAT_LITERAL => matches!(last, b'f' | b'F' | b'd' | b'D'),
        _ => false,
    };
    if !is_suffix {
        return None;
    }
    let mapped = match case {
        LiteralSuffixCase::Upper => last.to_ascii_uppercase(),
        LiteralSuffixCase::Lower => last.to_ascii_lowercase(),
        LiteralSuffixCase::Preserve => unreachable!("handled above"),
    };
    if mapped == last {
        return None;
    }
    Some(format!("{}{}", &lit[..lit.len() - 1], mapped as char))
}

/// A significant token with its attached comments.
pub(crate) fn tok(tok: &SyntaxToken, ctx: &Ctx<'_>) -> Doc {
    ctx.comments.token(tok, token_text(tok, ctx.cfg))
}

/// The first non-trivia token contained in `node`, if any.
pub(crate) fn first_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| !t.kind().is_trivia())
}

/// The last non-trivia token contained in `node`, if any.
pub(crate) fn last_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .last()
}

/// The aesthetic spacing rule between two significant tokens (before the fusion-safety
/// net in [`sep`] is applied).
fn want_space(prev: S, next: S, cfg: &Config) -> bool {
    use S::*;
    // A constructor-call type witness `new <Integer>Foo()` keeps a space after `new`; the
    // generic no-space-before-`<` rule below (for `Foo<T>`) must not glue `new` to its `<`.
    if prev == NEW_KW && next == LT {
        return true;
    }
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
    // Intersection-type `&` density (a type-parameter bound `<T extends A & B>` or a cast
    // intersection `(A & B) x`) is configurable. The bitwise-AND operator `&` is a `BINARY_EXPR`,
    // lowered by `lower_binary` with hardcoded spacing, so it never reaches here.
    if prev == AMP || next == AMP {
        return cfg.type_punctuation_density == TypePunctuationDensity::Wide;
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
pub(crate) fn sep(prev: Option<&SyntaxToken>, next: &SyntaxToken, cfg: &Config) -> Doc {
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
pub(crate) fn lower_generic(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
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
            // `modifiers::emitted_boundary_tokens`); every other node emits in tree order.
            let (emitted_first, emitted_last) = if child.kind() == S::MODIFIERS {
                crate::modifiers::emitted_boundary_tokens(child, ctx.cfg)
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

    let doc = concat(vec![
        concat(head_line),
        continuation_indent(concat(wrapped)),
    ]);
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

/// Whether an empty `node` is governed by [`empty_item_single_line`](Config::empty_item_single_line)
/// — a declaration body: a type body (`CLASS_BODY`) or a method / constructor / initializer block.
/// Control-flow blocks, `switch` blocks, lambda bodies, and bare blocks are never governed (they
/// always keep `{}`), matching rustfmt's item-only scoping.
fn governs_empty_single_line(node: &SyntaxNode) -> bool {
    matches!(node.kind(), S::CLASS_BODY) || (node.kind() == S::BLOCK && is_declaration_body(node))
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
    group(concat(vec![
        lower(&first, ctx),
        continuation_indent(concat(tail)),
    ]))
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
fn lower_ternary(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
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

#[cfg(test)]
mod tests {
    use jals_syntax::SyntaxKind;

    use super::{map_float_trailing_zero, map_hex_case, map_literal_suffix};
    use crate::config::FloatLiteralTrailingZero::{self, Always, Never};
    use crate::config::HexLiteralCase::{Lower, Preserve, Upper};
    use crate::config::LiteralSuffixCase;

    #[test]
    fn preserve_is_a_no_op() {
        assert_eq!(map_hex_case("0xFf", Preserve), None);
    }

    #[test]
    fn non_hex_literals_are_untouched() {
        // Decimal, octal, and binary literals have no hex digits and no `0x` prefix.
        for lit in ["123", "0", "0777", "0b1010", "1_000L", "3.14f", "1e10"] {
            assert_eq!(map_hex_case(lit, Upper), None, "{lit}");
            assert_eq!(map_hex_case(lit, Lower), None, "{lit}");
        }
    }

    #[test]
    fn maps_hex_integer_digits() {
        assert_eq!(map_hex_case("0xff", Upper).as_deref(), Some("0xFF"));
        assert_eq!(map_hex_case("0xFF", Lower).as_deref(), Some("0xff"));
        assert_eq!(
            map_hex_case("0xCafeBabe", Upper).as_deref(),
            Some("0xCAFEBABE")
        );
        assert_eq!(
            map_hex_case("0xCafeBabe", Lower).as_deref(),
            Some("0xcafebabe")
        );
    }

    #[test]
    fn keeps_the_radix_prefix_case() {
        // The `0x` / `0X` prefix is never rewritten — only the digits after it.
        assert_eq!(map_hex_case("0Xff", Upper).as_deref(), Some("0XFF"));
        assert_eq!(map_hex_case("0XFF", Lower).as_deref(), Some("0Xff"));
    }

    #[test]
    fn keeps_the_integer_suffix_case() {
        // The `l` / `L` suffix is outside the mantissa and keeps its case.
        assert_eq!(map_hex_case("0xabl", Upper).as_deref(), Some("0xABl"));
        assert_eq!(map_hex_case("0xABL", Lower).as_deref(), Some("0xabL"));
        // `_` separators in the mantissa are preserved.
        assert_eq!(
            map_hex_case("0xDEAD_beefL", Lower).as_deref(),
            Some("0xdead_beefL")
        );
    }

    #[test]
    fn maps_hex_float_mantissa_only() {
        // The mantissa (before `p`) is mapped; the `p` exponent, its decimal digits, and any
        // `f` / `d` suffix keep their case.
        assert_eq!(map_hex_case("0xA.Bp1f", Lower).as_deref(), Some("0xa.bp1f"));
        assert_eq!(map_hex_case("0xa.bP1F", Upper).as_deref(), Some("0xA.BP1F"));
        // `f` / `d` are valid hex digits in the mantissa, but a suffix after the exponent is not.
        assert_eq!(map_hex_case("0xFp2d", Lower).as_deref(), Some("0xfp2d"));
        assert_eq!(map_hex_case("0X1P-2D", Lower).as_deref(), Some("0X1P-2D"));
    }

    #[test]
    fn float_trailing_zero_preserve_is_a_no_op() {
        for lit in ["1.0", "1.", "1.50", "0x1.0p3"] {
            assert_eq!(
                map_float_trailing_zero(lit, FloatLiteralTrailingZero::Preserve),
                None,
                "{lit}"
            );
        }
    }

    #[test]
    fn float_trailing_zero_skips_dotless_and_non_float_literals() {
        // No `.` to normalize: dotless floats and every integer literal are untouched.
        for lit in ["1e10", "100f", "1f", "123", "0xCafe", "0b1010", "0777"] {
            assert_eq!(map_float_trailing_zero(lit, Always), None, "{lit}");
            assert_eq!(map_float_trailing_zero(lit, Never), None, "{lit}");
        }
    }

    #[test]
    fn float_trailing_zero_skips_hex_floats() {
        // Hex floats are left to `hex-literal-case`; the `0x` prefix opts them out of both modes.
        for lit in ["0x1.0p3", "0x1.p3", "0X1.0P-2f"] {
            assert_eq!(map_float_trailing_zero(lit, Always), None, "{lit}");
            assert_eq!(map_float_trailing_zero(lit, Never), None, "{lit}");
        }
    }

    #[test]
    fn always_inserts_a_single_trailing_zero() {
        assert_eq!(
            map_float_trailing_zero("1.", Always).as_deref(),
            Some("1.0")
        );
        assert_eq!(
            map_float_trailing_zero("1.f", Always).as_deref(),
            Some("1.0f")
        );
        assert_eq!(
            map_float_trailing_zero("1.e10", Always).as_deref(),
            Some("1.0e10")
        );
        // A fraction that already has a digit (even another zero) is left as written.
        for lit in ["1.0", "1.00", "1.5", ".5", ".0"] {
            assert_eq!(map_float_trailing_zero(lit, Always), None, "{lit}");
        }
    }

    #[test]
    fn never_strips_an_all_zero_fraction() {
        assert_eq!(map_float_trailing_zero("1.0", Never).as_deref(), Some("1."));
        assert_eq!(
            map_float_trailing_zero("1.00", Never).as_deref(),
            Some("1.")
        );
        assert_eq!(map_float_trailing_zero("0.0", Never).as_deref(), Some("0."));
        assert_eq!(
            map_float_trailing_zero("1.0f", Never).as_deref(),
            Some("1.f")
        );
        assert_eq!(
            map_float_trailing_zero("1.0e10", Never).as_deref(),
            Some("1.e10")
        );
    }

    #[test]
    fn never_keeps_nonzero_underscore_empty_and_leading_dot_fractions() {
        // A non-zero digit, an underscore-grouped fraction, an already-empty fraction, and a
        // leading-dot float (stripping which would yield the illegal bare `.`) are all untouched.
        for lit in ["1.5", "1.50", "1.05", "1.0_0", "1.", ".0", ".5"] {
            assert_eq!(map_float_trailing_zero(lit, Never), None, "{lit}");
        }
    }

    use LiteralSuffixCase::{Lower as SufLower, Preserve as SufPreserve, Upper as SufUpper};
    const INT: SyntaxKind = SyntaxKind::INT_LITERAL;
    const FLOAT: SyntaxKind = SyntaxKind::FLOAT_LITERAL;

    #[test]
    fn literal_suffix_preserve_is_a_no_op() {
        for (lit, kind) in [("123l", INT), ("1.5f", FLOAT), ("2.0D", FLOAT)] {
            assert_eq!(map_literal_suffix(lit, kind, SufPreserve), None, "{lit}");
        }
    }

    #[test]
    fn literal_suffix_maps_the_integer_long_suffix() {
        assert_eq!(
            map_literal_suffix("123l", INT, SufUpper).as_deref(),
            Some("123L")
        );
        assert_eq!(
            map_literal_suffix("123L", INT, SufLower).as_deref(),
            Some("123l")
        );
        // The suffix is the only thing touched — hex digits, `_` separators, and the radix prefix
        // are all preserved.
        assert_eq!(
            map_literal_suffix("0xCAFEl", INT, SufUpper).as_deref(),
            Some("0xCAFEL")
        );
        assert_eq!(
            map_literal_suffix("0b1010L", INT, SufLower).as_deref(),
            Some("0b1010l")
        );
    }

    #[test]
    fn literal_suffix_maps_the_float_and_double_suffix() {
        for (lit, want) in [("1.5f", "1.5F"), ("1.5d", "1.5D"), ("1e10f", "1e10F")] {
            assert_eq!(
                map_literal_suffix(lit, FLOAT, SufUpper).as_deref(),
                Some(want)
            );
        }
        for (lit, want) in [("1.5F", "1.5f"), ("2.0D", "2.0d")] {
            assert_eq!(
                map_literal_suffix(lit, FLOAT, SufLower).as_deref(),
                Some(want)
            );
        }
    }

    #[test]
    fn literal_suffix_leaves_an_integer_literals_trailing_hex_digit_alone() {
        // A final `f` / `d` on an *integer* literal is a hex digit, never a suffix, so it must
        // never be rewritten — the token kind is what tells the two apart.
        for lit in ["0xabcdef", "0xff", "0xFD", "0xabcd"] {
            assert_eq!(map_literal_suffix(lit, INT, SufUpper), None, "{lit}");
            assert_eq!(map_literal_suffix(lit, INT, SufLower), None, "{lit}");
        }
    }

    #[test]
    fn literal_suffix_maps_only_the_hex_floats_final_letter() {
        // The `f` inside the hex-float mantissa is left alone; only the trailing `d` suffix flips.
        assert_eq!(
            map_literal_suffix("0x1.fp3d", FLOAT, SufUpper).as_deref(),
            Some("0x1.fp3D")
        );
        assert_eq!(
            map_literal_suffix("0x1p3f", FLOAT, SufUpper).as_deref(),
            Some("0x1p3F")
        );
    }

    #[test]
    fn literal_suffix_skips_unsuffixed_literals_and_already_correct_case() {
        // No trailing suffix letter to normalize.
        for (lit, kind) in [("123", INT), ("1.5", FLOAT), ("1e10", FLOAT), ("0xff", INT)] {
            assert_eq!(map_literal_suffix(lit, kind, SufUpper), None, "{lit}");
            assert_eq!(map_literal_suffix(lit, kind, SufLower), None, "{lit}");
        }
        // Already in the requested case: no change (returns `None`).
        assert_eq!(map_literal_suffix("123L", INT, SufUpper), None);
        assert_eq!(map_literal_suffix("1.5f", FLOAT, SufLower), None);
    }
}
