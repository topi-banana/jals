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
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::ir::Indent;
use crate::visit::{Ctx, Spacing};

impl Ctx<'_> {
    /// A binary operator run.
    pub(super) async fn visit_binary(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.binary_operation;
        let before = self.style.cfg.wrapping.before_binary_operator;
        // A nested run of the same precedence is already inside its parent's level; opening a new
        // one there would let the inner half fit while the outer half did not, which is exactly
        // the ragged output "break at the highest level first" exists to avoid.
        let nested = node
            .parent()
            .is_some_and(|parent| parent.kind() == S::BINARY_EXPR);
        let continuation = self.style.continuation();
        if !nested {
            self.open(continuation.clone());
        }
        self.emit_operator_run(node, policy, before).await;
        if !nested {
            self.close_indent(&continuation);
        }
    }

    /// Emit a node's children, placing a break on the chosen side of each operator token.
    async fn emit_operator_run(&mut self, node: &SyntaxNode, policy: WrapPolicy, before: bool) {
        let children = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let is_operator = child
                .as_token()
                .is_some_and(|tok| Self::is_binary_operator(tok.kind()));
            // A fused `>>` arrives as several `GT` tokens; only the first one takes the break.
            let fused_tail = is_operator
                && nth > 0
                && children[nth - 1]
                    .as_token()
                    .is_some_and(|previous| previous.kind() == S::GT);
            if is_operator && !fused_tail {
                let flat = self.operator_flat(child);
                if before {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                self.visit_element(child).await;
                if !before {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                continue;
            }
            self.visit_element(child).await;
        }
    }

    /// The flat rendering of the break placed against an operator token.
    ///
    /// A break stands where a space would otherwise be decided, so the operator's own `[spacing]`
    /// rule has to travel with it — otherwise `space-around-additive-operators = false` would be
    /// honored on an expression that fits and ignored on one that wraps.
    fn operator_flat(&self, child: &SyntaxElement) -> &'static str {
        let space = child.as_token().is_some_and(|tok| {
            let previous = tok.prev_token();
            previous.is_none_or(|previous| Spacing::between(&previous, tok, self.style))
        });
        Self::flat_space(space)
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
        for child in &children {
            let is_operator = child
                .as_token()
                .is_some_and(|tok| Self::is_assignment_operator(tok.kind()));
            if is_operator {
                let flat = self.operator_flat(child);
                if before {
                    self.list_break_flat(policy, flat, Indent::ZERO);
                }
                self.visit_element(child).await;
                if !before {
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
        self.visit_children(node).await;
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
