//! Expressions with canonical operator spacing.
//!
//! Binary, ternary, and unary expressions get canonical operator spacing and breakable layout.
//! A same-precedence binary run is one group breaking at every operator (placement per
//! `binop-separator`); a ternary breaks at its `?` / `:` against `single-line-if-else-max-width`;
//! a unary stays tight (`-x`), spacing only inserted to avoid operator fusion (`- -x`). Malformed
//! shapes from error recovery fall back to inline emission, byte-for-byte unchanged.

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::{BinopLayout, BinopSeparator};
use crate::doc::Doc;
use crate::lower::Ctx;

/// One wrapped step of a flattened binary run: the operator-token run and its right operand.
type BinopStep = (Vec<SyntaxToken>, SyntaxNode);

impl Ctx<'_> {
    /// The binding power of a binary operator, given its (1–3 adjacent) operator tokens.
    /// Mirrors `peek_bin_op` in `jals-syntax`'s grammar — only the `>` family is multi-token
    /// (`>=` is `GT EQ`, `>>` is `GT GT`, `>>>` is `GT GT GT`; `<=` and `<<` are single
    /// tokens). `None` for a token run that is not a known binary operator (error recovery).
    fn binop_bp(ops: &[SyntaxToken]) -> Option<u8> {
        use S::{
            AMP, AMP_AMP, BANG_EQ, CARET, EQ, EQ_EQ, GT, INSTANCEOF_KW, LSHIFT, LT, LT_EQ, MINUS,
            PERCENT, PIPE, PIPE_PIPE, PLUS, SLASH, STAR,
        };
        let kinds: Vec<S> = ops.iter().map(SyntaxToken::kind).collect();
        Some(match kinds.as_slice() {
            [PIPE_PIPE] => 1,
            [AMP_AMP] => 2,
            [PIPE] => 3,
            [CARET] => 4,
            [AMP] => 5,
            [EQ_EQ | BANG_EQ] => 6,
            [LT | LT_EQ | GT | INSTANCEOF_KW] | [GT, EQ] => 7,
            [LSHIFT] | [GT, GT] | [GT, GT, GT] => 8,
            [PLUS | MINUS] => 9,
            [STAR | SLASH | PERCENT] => 10,
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

    /// Flatten the left spine of same-precedence nested `BINARY_EXPR`s into the first operand
    /// and the `(operator run, rhs)` steps in source order. A left child of a *different*
    /// precedence stays a unit (its own group), so `a == b && c == d` breaks at `&&` before
    /// either `==` does. All Java binary operators are left-associative, so the same-precedence
    /// run is always a pure left spine.
    fn flatten_binary(node: &SyntaxNode) -> Option<(SyntaxNode, Vec<BinopStep>)> {
        let (lhs, ops, rhs) = Self::binary_parts(node)?;
        let bp = Self::binop_bp(&ops);
        let mut steps = vec![(ops, rhs)];
        let mut first = lhs;
        while first.kind() == S::BINARY_EXPR && bp.is_some() {
            let Some((lhs2, ops2, rhs2)) = Self::binary_parts(&first) else {
                break;
            };
            if Self::binop_bp(&ops2) != bp {
                break;
            }
            steps.push((ops2, rhs2));
            first = lhs2;
        }
        steps.reverse();
        Some((first, steps))
    }

    /// Lower a binary expression. The same-precedence run wraps with a break point at every
    /// operator, the operator leading the continuation line (`binop-separator = front`, the
    /// default) or trailing the broken line (`back`). A run of operator tokens (e.g. the two `>` of
    /// `>>`) is joined tightly so operator fusion is preserved. `binop-layout` chooses how it wraps:
    /// [`Tall`](BinopLayout::Tall) is one group — flat `a op b op c`, else *every* step on its own
    /// line; [`Compressed`](BinopLayout::Compressed) is a fill that packs as many operands per line
    /// as fit `max-width` (google-java-format's layout).
    pub(crate) async fn lower_binary(&self, node: &SyntaxNode) -> Doc {
        let Some((first, steps)) = Self::flatten_binary(node) else {
            // Error recovery produced something other than `lhs op rhs`; emit inline with
            // canonical spacing so every token is preserved verbatim.
            return self.lower_binary_inline(node).await;
        };
        match self.cfg.binop_layout {
            BinopLayout::Tall => {
                // All-or-nothing: one group breaking at every operator together.
                let mut tail: Vec<Doc> = Vec::new();
                for (ops, rhs) in &steps {
                    let op = Doc::concat(ops.iter().map(|t| self.tok(t)).collect());
                    match self.cfg.binop_separator {
                        BinopSeparator::Front => {
                            tail.extend([Doc::line(), op, Doc::text(" "), self.lower(rhs).await]);
                        }
                        BinopSeparator::Back => {
                            tail.extend([Doc::text(" "), op, Doc::line(), self.lower(rhs).await]);
                        }
                    }
                }
                Doc::group(Doc::concat(vec![
                    self.lower(&first).await,
                    Doc::continuation_indent(Doc::concat(tail)),
                ]))
            }
            BinopLayout::Compressed => {
                // Fill: each operand (with its glued operator) is a fill item; `fill` inserts a
                // breakable `line` between items, so the renderer packs as many per line as fit.
                // `front` glues the operator to the *following* operand (it leads the next line);
                // `back` glues it to the *preceding* one (it trails the broken line).
                let mut items: Vec<Doc> = Vec::with_capacity(steps.len() + 1);
                match self.cfg.binop_separator {
                    BinopSeparator::Front => {
                        items.push(self.lower(&first).await);
                        for (ops, rhs) in &steps {
                            let op = Doc::concat(ops.iter().map(|t| self.tok(t)).collect());
                            items.push(Doc::concat(vec![
                                op,
                                Doc::text(" "),
                                self.lower(rhs).await,
                            ]));
                        }
                    }
                    BinopSeparator::Back => {
                        let mut operand = self.lower(&first).await;
                        for (ops, rhs) in &steps {
                            let op = Doc::concat(ops.iter().map(|t| self.tok(t)).collect());
                            items.push(Doc::concat(vec![operand, Doc::text(" "), op]));
                            operand = self.lower(rhs).await;
                        }
                        items.push(operand);
                    }
                }
                Doc::continuation_indent(Doc::fill(items))
            }
        }
    }

    /// Lower a malformed binary expression inline as `lhs op rhs`, joining a run of operator
    /// tokens tightly and surrounding the whole operator with single spaces. Fallback for
    /// error-recovery shapes [`Ctx::flatten_binary`] cannot handle (e.g. a missing operand).
    async fn lower_binary_inline(&self, node: &SyntaxNode) -> Doc {
        let mut parts: Vec<Doc> = Vec::new();
        let mut pending_op: Vec<Doc> = Vec::new();

        for el in node.children_with_tokens() {
            if let Some(child) = el.as_node() {
                Self::flush_operator(&mut parts, &mut pending_op);
                parts.push(self.lower(child).await);
            } else if let Some(t) = el.as_token()
                && !t.kind().is_trivia()
            {
                pending_op.push(self.tok(t));
            }
        }
        Self::flush_operator(&mut parts, &mut pending_op);
        Doc::concat(parts)
    }

    fn flush_operator(parts: &mut Vec<Doc>, pending_op: &mut Vec<Doc>) {
        if pending_op.is_empty() {
            return;
        }
        parts.push(Doc::text(" "));
        parts.push(Doc::concat(core::mem::take(pending_op)));
        parts.push(Doc::text(" "));
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
    pub(crate) async fn lower_ternary(&self, node: &SyntaxNode) -> Doc {
        let Some((cond, q, then, colon, els)) = Self::ternary_parts(node) else {
            return self.lower_generic(node).await;
        };
        // A break point whose flat form is a space (`line`) or nothing (`softline`); both break to
        // a newline. The `?` is always space-surrounded; the `:` follows the colon-spacing config.
        let break_at = |flat_space: bool| {
            if flat_space {
                Doc::line()
            } else {
                Doc::softline()
            }
        };
        let colon_space = |on: bool| if on { Doc::text(" ") } else { Doc::nil() };
        // The ternary is an operator colon, so `space-around-operator-colon` also spaces it
        // (additive over `space-before-colon`), matching the enhanced-`for` / `assert` colons in
        // `want_space`. No `_` exception is needed here (unlike the for-each colon): a ternary's
        // then-branch operand is never a bare unnamed `_`.
        let sbc = self.cfg.space_before_colon || self.cfg.space_around_operator_colon;
        let sac = self.cfg.space_after_colon;
        let tail = match self.cfg.binop_separator {
            BinopSeparator::Front => Doc::concat(vec![
                break_at(true),
                self.tok(&q),
                Doc::text(" "),
                self.lower(&then).await,
                break_at(sbc),
                self.tok(&colon),
                colon_space(sac),
                self.lower(&els).await,
            ]),
            BinopSeparator::Back => Doc::concat(vec![
                Doc::text(" "),
                self.tok(&q),
                break_at(true),
                self.lower(&then).await,
                colon_space(sbc),
                self.tok(&colon),
                break_at(sac),
                self.lower(&els).await,
            ]),
        };
        let doc = Doc::concat(vec![
            self.lower(&cond).await,
            Doc::continuation_indent(tail),
        ]);
        Doc::group_within(doc, self.cfg.single_line_if_else_max_width)
    }

    /// Lower a unary expression tight (`-x`), inserting a space only when the operator and
    /// operand would otherwise fuse (`- -x`, `+ +x`).
    pub(crate) async fn lower_unary(&self, node: &SyntaxNode) -> Doc {
        let mut parts: Vec<Doc> = Vec::new();
        let mut prev: Option<SyntaxToken> = None;
        for el in node.children_with_tokens() {
            if let Some(child) = el.as_node() {
                if let Some(first) = Self::first_sig_token(child) {
                    parts.push(Self::tight_sep(prev.as_ref(), &first).await);
                }
                parts.push(self.lower(child).await);
                if let Some(last) = Self::last_sig_token(child) {
                    prev = Some(last);
                }
            } else if let Some(t) = el.as_token()
                && !t.kind().is_trivia()
            {
                parts.push(Self::tight_sep(prev.as_ref(), t).await);
                parts.push(self.tok(t));
                prev = Some(t.clone());
            }
        }
        Doc::concat(parts)
    }
}
