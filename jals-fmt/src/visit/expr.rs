//! Expressions: binary runs, ternaries, assignments, lambdas, casts, and `new`.
//!
//! Two placement conventions do most of the work, and both are google-java-format's:
//!
//! - a **binary or ternary operator starts the continuation line** (`before-binary-operator`,
//!   `before-ternary-operator`), so a reader scanning the left margin sees what joins the pieces;
//! - an **assignment breaks after `=`** (`before-assignment-operator` off), so the target stays
//!   visible on the first line.
//!
//! A same-precedence run is one level, which is what makes it break as a unit: `a + b + c` either
//! fits or splits at every operator under `if-long-per-item`, and packs under `if-long`.

use jals_config::fmt::WrapPolicy;
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::visit::{Ctx, Spacing};

impl Ctx<'_> {
    /// A binary operator run.
    pub(super) async fn visit_binary(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.binary_operation;
        let before = self.style.cfg.wrapping.before_binary_operator;
        // Operators of the *same* precedence are one run, laid out as one level: `a + b + c`
        // wraps all-or-nothing rather than letting the inner half fit while the outer half does
        // not. A sub-expression that binds tighter is a different run, and opens a level of its
        // own — which is what indents `x * y` one step further than the `+` it hangs off, and
        // what makes the lowest-precedence operator the first to break.
        let root = Self::run_root(node);
        let nested = node.parent().is_some_and(|parent| {
            parent.kind() == S::BINARY_EXPR && Self::precedence(&parent) == Self::precedence(node)
        });
        // Same rule as an argument list: a run whose operands are all short fills, and one long
        // operand makes that packing arbitrary, so the run goes one operator per line. The
        // question is asked of the **run**, not of this node: `("a" + b) + long` is one run, and
        // deciding its inner half separately would fill there and break here.
        let policy = if policy == WrapPolicy::IfLong && !self.run_operands_are_short(&root) {
            WrapPolicy::IfLongPerItem
        } else {
            policy
        };
        let continuation = self.style.continuation();
        if !nested {
            self.open(continuation.clone());
        }
        self.emit_operator_run(node, policy, before).await;
        if !nested {
            self.close_indent(&continuation);
        }
    }

    /// The outermost node of the same-precedence operator run `node` belongs to.
    fn run_root(node: &SyntaxNode) -> SyntaxNode {
        let precedence = Self::precedence(node);
        let mut root = node.clone();
        while let Some(parent) = root.parent() {
            if parent.kind() != S::BINARY_EXPR || Self::precedence(&parent) != precedence {
                break;
            }
            root = parent;
        }
        root
    }

    /// The binding strength of the operator a `BINARY_EXPR` is built around, higher binding
    /// tighter. `0` for a node with no operator token (error recovery).
    fn precedence(node: &SyntaxNode) -> u8 {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find_map(|tok| match tok.kind() {
                S::PIPE_PIPE => Some(1),
                S::AMP_AMP => Some(2),
                S::PIPE => Some(3),
                S::CARET => Some(4),
                S::AMP => Some(5),
                S::EQ_EQ | S::BANG_EQ => Some(6),
                S::LT | S::GT | S::LT_EQ | S::INSTANCEOF_KW => Some(7),
                S::LSHIFT => Some(8),
                S::PLUS | S::MINUS => Some(9),
                S::STAR | S::SLASH | S::PERCENT => Some(10),
                _ => None,
            })
            .unwrap_or(0)
    }

    /// Whether every operand of the same-precedence run rooted at `node` is under
    /// `[wrapping] fill-item-width` columns of source.
    fn run_operands_are_short(&self, node: &SyntaxNode) -> bool {
        let limit = self.style.cfg.wrapping.fill_item_width;
        if limit == 0 {
            return true;
        }
        let precedence = Self::precedence(node);
        // Iterative: an operator run is a left-leaning spine as deep as the expression is long.
        let mut pending = alloc::vec![node.clone()];
        while let Some(current) = pending.pop() {
            for child in current.children() {
                if child.kind() == S::BINARY_EXPR && Self::precedence(&child) == precedence {
                    pending.push(child);
                } else if Self::source_width(&child) >= limit {
                    return false;
                }
            }
        }
        true
    }

    /// Emit a node's children, placing a break on the chosen side of each operator token.
    async fn emit_operator_run(&mut self, node: &SyntaxNode, policy: WrapPolicy, before: bool) {
        let children = Self::children(node);
        // The `!before` break waits until the operator is fully spelled. `>>`, `>=`, and `>>>=`
        // arrive as several adjacent tokens (`spacing`'s module header), so a break placed after
        // the first of them lands *inside* the operator: `total >> 1` came out `total > > 1`, and
        // `x >= 1` came out `x > = 1`. That is a different token stream, which the fail-safe
        // answers by returning the whole file unformatted.
        let mut deferred: Option<&'static str> = None;
        for (nth, child) in children.iter().enumerate() {
            let is_operator = child
                .as_token()
                .is_some_and(|tok| Self::is_binary_operator(tok.kind()));
            // A fused `>>` arrives as several `GT` tokens; only the first one takes the break.
            let fused_tail = is_operator && Self::fuses_with_previous(&children, nth);
            if is_operator && !fused_tail {
                let flat = self.operator_flat(child);
                if before {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                } else {
                    deferred = Some(flat);
                }
                self.visit_element(child).await;
                continue;
            }
            // The first child that is not another piece of the operator: the deferred break falls
            // here, in front of the right operand, which is the gap it always stood for — so the
            // spacing it carries is that gap's, not the one in front of the operator.
            if !Self::fuses_with_previous(&children, nth) && deferred.take().is_some() {
                let flat = self.gap_flat(child);
                self.list_break_flat(policy, flat, Indent::ZERO);
            }
            self.visit_element(child).await;
        }
        // An operator with no right operand — error recovery — still gets its break, so the
        // deferred one is never simply dropped.
        if let Some(flat) = deferred {
            self.list_break_flat(policy, flat, Indent::ZERO);
        }
    }

    /// Whether `children[nth]` is another token of the fused operator `children[nth - 1]` opens.
    ///
    /// [`Spacing::fused`] is the single definition of "these two tokens spell one operator" — it
    /// is what keeps them emitted tight — so the break placement asks it rather than re-deriving
    /// the answer from `GT` alone. Two questions about one operator, answered once.
    fn fuses_with_previous(children: &[SyntaxElement], nth: usize) -> bool {
        let (Some(previous), Some(current)) = (
            nth.checked_sub(1).and_then(|at| children[at].as_token()),
            children[nth].as_token(),
        ) else {
            return false;
        };
        Spacing::fused(previous, current)
    }

    /// The flat rendering of the break placed against an operator token.
    ///
    /// A break stands where a space would otherwise be decided, so the operator's own `[spacing]`
    /// rule has to travel with it — otherwise `space-around-additive-operators = false` would be
    /// honored on an expression that fits and ignored on one that wraps.
    ///
    /// The pair asked about is the *emitted* one, [`Ctx::previous`](crate::visit::Ctx), not
    /// `prev_token()`: the break replaces exactly the space
    /// [`token`](crate::visit::Ctx::token) would have emitted for that pair, and the source's own
    /// whitespace sits between them in the tree. Handing `Spacing` a `WHITESPACE` token instead
    /// looks harmless while every rule here answers from the operator alone — but it silently
    /// withholds the left operand from the guards that need it, and gluing `x` to `instanceof`
    /// spells `xinstanceof`.
    fn operator_flat(&self, child: &SyntaxElement) -> &'static str {
        let space = child.as_token().is_some_and(|tok| {
            self.previous
                .as_ref()
                .is_none_or(|previous| Spacing::between(previous, tok, self.style))
        });
        Self::flat_space(space)
    }

    /// The flat rendering of a break placed *in front of* `next`, rather than in front of an
    /// operator.
    ///
    /// [`operator_flat`](Self::operator_flat) answers for the pair (previous token, operator),
    /// which is the gap a `before` break stands in. An `after` break stands in the other gap —
    /// (operator, right operand) — and on a **fused** operator the two are not interchangeable:
    /// `x >>= 2` spells its operator as `GT GT EQ`, so the pair in front of the `=` is *inside*
    /// the operator and [`Spacing::fused`] answers "tight". Asking about the gap the break is
    /// actually placed in renders `x >>= 2`, where asking about the operator rendered `x >>=2`.
    fn gap_flat(&self, next: &SyntaxElement) -> &'static str {
        let space = Self::leading_token(next).is_some_and(|tok| {
            self.previous
                .as_ref()
                .is_none_or(|previous| Spacing::between(previous, &tok, self.style))
        });
        Self::flat_space(space)
    }

    /// The first significant token `element` emits — the one a break in front of it precedes.
    fn leading_token(element: &SyntaxElement) -> Option<SyntaxToken> {
        match element {
            SyntaxElement::Token(tok) => Some(tok.clone()),
            SyntaxElement::Node(node) => node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|tok| !tok.kind().is_trivia()),
        }
    }

    /// Whether a token kind is an infix operator that may carry a break.
    const fn is_binary_operator(kind: S) -> bool {
        matches!(
            kind,
            S::PLUS
                | S::MINUS
                | S::STAR
                | S::SLASH
                | S::PERCENT
                | S::AMP
                | S::PIPE
                | S::CARET
                | S::AMP_AMP
                | S::PIPE_PIPE
                | S::EQ_EQ
                | S::BANG_EQ
                | S::LT
                | S::GT
                | S::LT_EQ
                | S::LSHIFT
                | S::INSTANCEOF_KW
        )
    }

    /// An assignment expression: the break falls after the operator by default.
    pub(super) async fn visit_assignment(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.assignment;
        let before = self.style.cfg.wrapping.before_assignment_operator;
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let is_operator = child
                .as_token()
                .is_some_and(|tok| Self::is_assignment_operator(tok.kind()));
            if is_operator {
                // `>>=` is spelled `GT GT EQ`, and only its `EQ` is an assignment operator — so
                // a `before` break placed in front of the token that matched lands *inside* the
                // operator, which re-lexes it and costs the whole file its formatting. The same
                // guard [`Self::emit_operator_run`] carries, and the same consequence: the run
                // takes no break here, since its first token is not one this method sees.
                if before && !Self::fuses_with_previous(&children, nth) {
                    let flat = self.operator_flat(child);
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                self.visit_element(child).await;
                if !before {
                    // The gap this break stands in is (operator, right-hand side), and on `>>=`
                    // — `GT GT EQ` — that is not the gap in front of the `=`. See
                    // [`Ctx::gap_flat`].
                    let flat = children
                        .get(nth + 1)
                        .map_or(" ", |next| self.gap_flat(next));
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                continue;
            }
            self.visit_element(child).await;
        }
        self.close_indent(&continuation);
    }

    /// Whether a token kind is an assignment operator.
    const fn is_assignment_operator(kind: S) -> bool {
        matches!(
            kind,
            S::EQ
                | S::PLUS_EQ
                | S::MINUS_EQ
                | S::STAR_EQ
                | S::SLASH_EQ
                | S::PERCENT_EQ
                | S::AMP_EQ
                | S::PIPE_EQ
                | S::CARET_EQ
                | S::LSHIFT_EQ
        )
    }

    /// `cond ? a : b` — both `?` and `:` start their continuation lines.
    pub(super) async fn visit_ternary(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.ternary;
        let before = self.style.cfg.wrapping.before_ternary_operator;
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        for child in Self::children(node) {
            let is_operator = child
                .as_token()
                .is_some_and(|tok| matches!(tok.kind(), S::QUESTION | S::COLON));
            if is_operator {
                let flat = self.operator_flat(&child);
                if before {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                self.visit_element(&child).await;
                if !before {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                continue;
            }
            self.visit_element(&child).await;
        }
        self.close_indent(&continuation);
    }

    /// A lambda. Only a single expression body may break right after the `->`; a block body
    /// follows the brace rules instead.
    pub(super) async fn visit_lambda(&mut self, node: &SyntaxNode) {
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        let mut past_arrow = false;
        for child in Self::children(node) {
            // `visitLambdaExpression` opens the continuation *once*, around the parameter list;
            // the list itself adds nothing, or a parameter that wrapped would land two steps in.
            if child
                .as_node()
                .is_some_and(|child| child.kind() == S::LAMBDA_PARAMS)
            {
                self.list_indent = Some(Indent::ZERO);
            }
            if child.as_token().is_some_and(|tok| tok.kind() == S::ARROW) {
                self.visit_element(&child).await;
                past_arrow = true;
                continue;
            }
            if past_arrow {
                past_arrow = false;
                match child.as_node().map(SyntaxNode::kind) {
                    Some(S::BLOCK) => {
                        self.close_indent(&continuation);
                        self.brace_before(self.style.cfg.braces.lambda_body);
                        self.visit_element(&child).await;
                        return;
                    }
                    _ => self.break_op(Indent::ZERO),
                }
            }
            self.visit_element(&child).await;
        }
        self.close_indent(&continuation);
    }

    /// A cast: `(Type) value`. The space after the `)` is `[spacing] after-type-cast`.
    pub(super) async fn visit_cast(&mut self, node: &SyntaxNode) {
        // `visitTypeCast` opens a level and puts a break between the `)` and the value, so a cast
        // whose value does not fit moves the *value* to the next line rather than breaking inside
        // it: `(Foo)\n    bar(a, b)`, not `(Foo) bar(\n    a, b)`.
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        let children = Self::children(node);
        for child in &children {
            if child.as_token().is_some_and(|tok| tok.kind() == S::RPAREN) {
                self.visit_element(child).await;
                let flat = Self::flat_space(self.style.cfg.spacing.after_type_cast);
                self.list_break_flat(self.style.cfg.wrapping.binary_operation, flat, Indent::ZERO);
                continue;
            }
            self.visit_element(child).await;
        }
        self.close_indent(&continuation);
    }

    /// `new Type(args)`, `new Type[…]`, `new Type() { … }`.
    pub(super) async fn visit_new(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if child
                .as_node()
                .is_some_and(|child| child.kind() == S::CLASS_BODY)
            {
                self.brace_before(self.style.cfg.braces.type_declaration);
            }
            self.visit_element(&child).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use jals_config::fmt::Config;

    /// Format with the break placed *after* the operator.
    fn breaking_after(src: &str) -> crate::FormatOutput {
        let mut cfg = Config::default();
        cfg.wrapping.before_binary_operator = false;
        jals_exec::block_on_inline(crate::FormatOutput::format_source(src, &cfg))
    }

    #[test]
    fn a_fused_operator_never_takes_a_break_inside_itself() {
        // `>>`, `>=`, and `>>>=` are several adjacent `GT`-family tokens. The run takes one break,
        // and on this side it belongs after the *last* of them: a break after the first re-lexes
        // the operator, which the fail-safe answers by returning the whole file unformatted — so
        // before this fix the rule was inert on every source containing one.
        for (src, spelled) in [
            ("class Z { void m() { int a = t >> 1; } }\n", ">> 1"),
            ("class Z { void m() { int a = t >>> 1; } }\n", ">>> 1"),
            ("class Z { void m() { boolean a = t >= 1; } }\n", ">= 1"),
        ] {
            let out = breaking_after(src);
            assert!(
                !out.fell_back(),
                "the fail-safe refused {src:?}, so nothing was formatted",
            );
            assert!(
                out.formatted.contains(spelled),
                "the operator was split apart in:\n{}",
                out.formatted,
            );
        }
    }

    #[test]
    fn a_compound_shift_assignment_survives_its_own_break() {
        // `>>=` and `>>>=` are the same multi-token shape one method over, in `visit_assignment`,
        // which places its break the same way — and under the *default* config, since
        // `before-assignment-operator` is off. The operator has to come out spelled as one piece,
        // and separated from its right operand: the break stands in the gap *after* the operator,
        // so it carries that gap's spacing rather than the one in front of the `=`, which on a
        // fused operator is inside it and answers "tight". Asking the wrong pair rendered
        // `x >>=2`.
        let out = jals_exec::block_on_inline(crate::FormatOutput::format_source(
            "class Z { void m() { int x = 1; x >>= 2; x >>>= 3; } }\n",
            &Config::default(),
        ));
        assert!(!out.fell_back(), "the fail-safe refused the output");
        assert!(out.formatted.contains("x >>= 2;"), "{}", out.formatted);
        assert!(out.formatted.contains("x >>>= 3;"), "{}", out.formatted);
    }

    #[test]
    fn a_compound_shift_assignment_survives_a_break_placed_before_it() {
        // The other side of the same method, under `before-assignment-operator` — what Eclipse's
        // `wrap_before_assignment_operator` lowers to. `>>=` is `GT GT EQ` and only its `EQ` is an
        // assignment operator, so a break in front of *that* token falls inside the operator: the
        // file came back unformatted, and one `>>=` cost every Eclipse-derived profile the whole
        // file. The run therefore takes **no** break here — its first token is not one
        // `visit_assignment` sees — which is why the `+=` control below is what proves the rule
        // is still doing something.
        let mut cfg = Config::default();
        cfg.wrapping.before_assignment_operator = true;
        cfg.layout.max_width = 24;
        let format =
            |src: &str| jals_exec::block_on_inline(crate::FormatOutput::format_source(src, &cfg));

        let shifted = format(
            "class Z { void m() { int xxxxxxxxxxxxxxxx = 1; xxxxxxxxxxxxxxxx >>= 22222222222; } }\n",
        );
        assert!(
            !shifted.fell_back(),
            "the fail-safe refused the file, so nothing in it was formatted:\n{}",
            shifted.formatted,
        );
        assert!(
            shifted.formatted.contains(">>= 22222222222"),
            "the operator was split apart:\n{}",
            shifted.formatted,
        );

        let plain = format(
            "class Z { void m() { int xxxxxxxxxxxxxxxx = 1; xxxxxxxxxxxxxxxx += 22222222222; } }\n",
        );
        assert!(!plain.fell_back());
        assert!(
            plain.formatted.contains('\n') && plain.formatted.contains("+="),
            "{}",
            plain.formatted,
        );
    }

    #[test]
    fn the_break_still_falls_after_an_unfused_operator() {
        // The control: deferring the break must cost the fused runs and nothing else, or "no break
        // is misplaced" would also be satisfied by placing none at all.
        let out = breaking_after("class Z { void m() { int a = t << 1; } }\n");
        assert!(!out.fell_back());
        assert!(out.formatted.contains("t << 1"), "{}", out.formatted);
    }
}
