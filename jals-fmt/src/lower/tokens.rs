//! Token emission and spacing.
//!
//! The leaves of the lowering: turn a significant token into a [`Doc`] (re-attaching its
//! comments), and decide the separator between two adjacent tokens. [`Ctx::want_space`] is the
//! aesthetic rule; [`Ctx::sep`] wraps it in a fusion-safety net so the output never changes which
//! operators lex together.

use alloc::borrow::ToOwned;
use alloc::format;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::TypePunctuationDensity;
use crate::doc::Doc;
use crate::lower::Ctx;

impl Ctx<'_> {
    /// The bare text of a significant token. String literals and text blocks are emitted as raw
    /// (verbatim) text so their multi-line content is never reindented. An integer / float literal
    /// is run through the active literal rules ([`crate::rules::LiteralRegistry`], built once per
    /// format from the config): each rewrites a disjoint part of the literal — a decimal float's
    /// trailing zero, a hex literal's digit case, the trailing `l` / `f` / `d` type suffix — and
    /// they compose in turn. With the default (all-`Preserve`) config the registry is empty, so the
    /// token's text is emitted verbatim.
    fn token_text(&self, tok: &SyntaxToken) -> Doc {
        match tok.kind() {
            S::STRING_LITERAL | S::TEXT_BLOCK => Doc::raw(tok.text().to_owned()),
            S::INT_LITERAL | S::FLOAT_LITERAL => {
                let original = tok.text();
                Doc::text(
                    self.rules
                        .literals()
                        .apply(original, tok.kind())
                        .unwrap_or_else(|| original.into()),
                )
            }
            _ => Doc::text(tok.text().to_owned()),
        }
    }

    /// A significant token with its attached comments.
    pub(crate) fn tok(&self, tok: &SyntaxToken) -> Doc {
        self.comments.token(tok, self.token_text(tok))
    }

    /// The first non-trivia token contained in `node`, if any.
    pub(crate) fn first_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| !t.kind().is_trivia())
    }

    /// The last non-trivia token contained in `node`, if any.
    pub(crate) fn last_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|t| !t.kind().is_trivia())
            .last()
    }

    /// The aesthetic spacing rule between two significant tokens (before the fusion-safety
    /// net in [`Ctx::sep`] is applied). `next_parent` is the parent node kind of the `next` token,
    /// used only to disambiguate colon contexts (see [`Ctx::space_before_colon`]); callers pass it
    /// solely when `next` is a `COLON`.
    fn want_space(&self, prev: S, next: S, next_parent: Option<S>) -> bool {
        use S::{
            AMP, AT, BANG, COLON, COLON_COLON, COMMA, DOT, ELLIPSIS, GT, IDENT, LBRACK, LPAREN, LT,
            MINUS_MINUS, NEW_KW, PLUS_PLUS, RBRACK, RPAREN, SEMICOLON, SUPER_KW, THIS_KW, TILDE,
        };
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
                // An array type / dimension / subscript `[` hugs the token before it: `int[]`,
                // `String[][]`, `a[0]`, `foo()[0]`, `List<String>[]`.
                | LBRACK
                | LT
                | GT
                | PLUS_PLUS
                | MINUS_MINUS
        ) {
            return false;
        }
        // Colon spacing (ternary `?:`, enhanced-`for`, labels, `assert`, `case` / `default`) is
        // configurable. `space-before-colon` applies uniformly to every colon context;
        // `space-around-operator-colon` additionally spaces the *operator* colons (enhanced-`for`,
        // `assert` — the ternary is composed in `lower_ternary`), see [`Ctx::space_before_colon`].
        // `::` is a distinct token (`COLON_COLON`, handled above as a no-space operator) and is
        // never affected. The structural no-space rules above take precedence, so a colon abutting
        // `)` / `,` / `;` (only reachable through error recovery) never gains a stray space.
        if next == COLON {
            return self.space_before_colon(prev, next_parent);
        }
        if prev == COLON {
            return self.cfg.space_after_colon;
        }
        // Intersection-type `&` density (a type-parameter bound `<T extends A & B>` or a cast
        // intersection `(A & B) x`) is configurable. The bitwise-AND operator `&` is a
        // `BINARY_EXPR`, lowered by `lower_binary` with hardcoded spacing, so it never reaches here.
        if prev == AMP || next == AMP {
            return self.cfg.type_punctuation_density == TypePunctuationDensity::Wide;
        }
        // `(` hugs a preceding callee/array; keywords get a space before it.
        if next == LPAREN {
            return !matches!(prev, IDENT | RPAREN | RBRACK | SUPER_KW | THIS_KW | GT);
        }
        true
    }

    /// Whether a space precedes a `:`, given the colon's parent node kind. The *label* colons (a
    /// labeled statement, a `switch` `case` / `default`) follow `space_before_colon` alone. The
    /// *operator* colons — those that separate two operands: an enhanced `for` (`for (T x : xs)`)
    /// and an `assert` message (`assert c : m`) — additionally honor `space_around_operator_colon`
    /// (the ternary `:` is composed in `lower_ternary`, never reaching here). One exception
    /// preserves Google Java Format fidelity: an unnamed `_` for-each variable hugs its colon
    /// (`for (T _: xs)`), so the space is suppressed when the preceding token is the `_`
    /// (`UNDERSCORE`).
    fn space_before_colon(&self, prev: S, parent: Option<S>) -> bool {
        let operator_colon = matches!(parent, Some(S::FOR_EACH_STMT | S::ASSERT_STMT));
        self.cfg.space_before_colon
            || (operator_colon && self.cfg.space_around_operator_colon && prev != S::UNDERSCORE)
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
        let toks = jals_syntax::Lexer::tokenize(&joined);
        !(toks.len() == 2 && toks[0].text == a && toks[1].text == b)
    }

    /// The separator document between `prev` (if any) and the token `next`. Applies the
    /// aesthetic rule, then a fusion-safety net so the output never changes operator fusion.
    pub(crate) fn sep(&self, prev: Option<&SyntaxToken>, next: &SyntaxToken) -> Doc {
        let Some(p) = prev else {
            return Doc::nil();
        };
        let (pk, nk) = (p.kind(), next.kind());
        // Preserve `>>`, `>>>`, `>=`, `>>=` fusion exactly as the source had it.
        if pk == S::GT && (nk == S::GT || nk == S::EQ) {
            return if Self::adjacent(p, next) {
                Doc::nil()
            } else {
                Doc::text(" ")
            };
        }
        // The colon's parent node kind disambiguates colon contexts (operator vs label); only a
        // colon needs it, so the lookup is skipped for every other token pair.
        let next_parent = if nk == S::COLON {
            next.parent().map(|n| n.kind())
        } else {
            None
        };
        let space = self.want_space(pk, nk, next_parent) || Self::would_fuse(p.text(), next.text());
        if space { Doc::text(" ") } else { Doc::nil() }
    }

    /// A separator that keeps two tokens tight unless they would fuse (used for unary
    /// operators, e.g. `-x` but `- -x`).
    pub(crate) fn tight_sep(prev: Option<&SyntaxToken>, next: &SyntaxToken) -> Doc {
        match prev {
            Some(p) if Self::would_fuse(p.text(), next.text()) => Doc::text(" "),
            _ => Doc::nil(),
        }
    }
}
