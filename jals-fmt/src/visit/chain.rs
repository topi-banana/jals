//! Method chains: `a.b().c().d()`.
//!
//! In the CST a chain is a left-leaning spine of `CALL_EXPR` / `FIELD_ACCESS` / `METHOD_REF_EXPR`
//! nodes, so visiting it naively would open one level per link and let the innermost receiver
//! break on its own — the ragged output that "break at the highest level first" exists to
//! prevent. The chain is therefore **flattened at its outermost node**: one level, one break
//! before each `.`, so the whole chain either fits or goes one call per line.
//!
//! # What this deliberately does not do
//!
//! Palantir keeps the *prefix* of an over-long chain on the receiver's line and breaks only the
//! tail (`PartialInlineability`, driven by its backtracking search). That is a different
//! resolution algorithm, not a different rule, so the single engine cannot express it and does
//! not try — it is difference **D3** in `DESIGN.md` §18.2. `wrap-first-method-in-chain` moves the
//! first break to before the first call, which is the closest this vocabulary comes.

use alloc::vec::Vec;

use jals_config::fmt::WrapPolicy;
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A selector chain, flattened at its outermost node.
    pub(super) async fn visit_chain(&mut self, node: &SyntaxNode) {
        // A link inside a larger chain is emitted by the chain's root, never on its own.
        if Self::is_chain_link(node) {
            self.emit_chain_spine(node).await;
            return;
        }

        let policy = self.style.cfg.wrapping.method_chain;
        let before_dot = self.style.cfg.wrapping.before_method_chain_dot;
        let dots = Self::count_dots(node);
        // A single selector is not a chain; wrapping it would only push `.foo` onto its own line.
        if dots < 2 || policy == WrapPolicy::Never || !before_dot {
            self.emit_chain_spine(node).await;
            return;
        }

        let continuation = self.style.continuation();
        self.open(continuation.clone());
        self.emit_chain_spine(node).await;
        self.close_indent(&continuation);
    }

    /// Whether this node is a link inside an enclosing chain rather than its root.
    fn is_chain_link(node: &SyntaxNode) -> bool {
        node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                S::CALL_EXPR | S::FIELD_ACCESS | S::METHOD_REF_EXPR
            )
        })
    }

    /// The chain's spine, outermost first.
    fn spine(node: &SyntaxNode) -> Vec<SyntaxNode> {
        let mut spine = Vec::new();
        let mut cursor = Some(node.clone());
        while let Some(current) = cursor {
            if !matches!(
                current.kind(),
                S::CALL_EXPR | S::FIELD_ACCESS | S::METHOD_REF_EXPR
            ) {
                break;
            }
            cursor = current.first_child();
            spine.push(current);
        }
        spine
    }

    /// The selector dots a link owns.
    fn dots_of(node: &SyntaxNode) -> usize {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| matches!(tok.kind(), S::DOT | S::COLON_COLON))
            .count()
    }

    /// Whether the leading field selects of this chain are one unit rather than links.
    ///
    /// google-java-format: *"if there's only one invocation, treat leading field accesses as a
    /// single unit"* — with no second call to align under, `myField.foo()` reads better whole
    /// than split. With two or more the alignment matters and every dot is a link.
    fn glues_prefix(node: &SyntaxNode) -> bool {
        Self::spine(node)
            .iter()
            .filter(|link| matches!(link.kind(), S::CALL_EXPR | S::METHOD_REF_EXPR))
            .count()
            == 1
    }

    /// How many selector dots the chain rooted here holds, counting a glued prefix as none.
    fn count_dots(node: &SyntaxNode) -> usize {
        let spine = Self::spine(node);
        let glue = Self::glues_prefix(node);
        let mut dots = 0usize;
        let mut seen_invocation = false;
        for link in spine.iter().rev() {
            if matches!(link.kind(), S::CALL_EXPR | S::METHOD_REF_EXPR) {
                seen_invocation = true;
            }
            if glue && !seen_invocation {
                continue;
            }
            dots += Self::dots_of(link);
        }
        dots
    }

    /// Emit one link, breaking before its selector when the chain is wrapping.
    async fn emit_chain_spine(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.method_chain;
        let before_dot = self.style.cfg.wrapping.before_method_chain_dot;
        let wrap_first = self.style.cfg.wrapping.wrap_first_method_in_chain;
        let breakable = policy != WrapPolicy::Never
            && Self::count_dots(&Self::chain_root(node)) >= 2
            && before_dot;

        let glue = Self::glues_prefix(&Self::chain_root(node));
        let children: Vec<SyntaxElement> = Self::children(node);
        for (nth, child) in children.iter().enumerate() {
            let is_selector = child
                .as_token()
                .is_some_and(|tok| matches!(tok.kind(), S::DOT | S::COLON_COLON));
            // A dot inside a glued prefix is not a break point — see [`Ctx::glues_prefix`].
            let glued = glue && !Self::invocation_at_or_below(node);
            if is_selector && breakable && !glued {
                // The receiver's own call keeps its dot on the receiver's line unless
                // `wrap-first-method-in-chain` asks otherwise.
                let first_link = nth == 1 && !Self::is_chain_link_receiver(node);
                if wrap_first || !first_link {
                    self.list_break_tight(policy, Indent::ZERO);
                }
            }
            self.visit_element(child).await;
        }
    }

    /// Whether an invocation appears at `node` or anywhere down its receiver spine.
    fn invocation_at_or_below(node: &SyntaxNode) -> bool {
        Self::spine(node)
            .iter()
            .any(|link| matches!(link.kind(), S::CALL_EXPR | S::METHOD_REF_EXPR))
    }

    /// The outermost node of the chain `node` belongs to.
    fn chain_root(node: &SyntaxNode) -> SyntaxNode {
        let mut root = node.clone();
        while let Some(parent) = root.parent() {
            if !matches!(
                parent.kind(),
                S::CALL_EXPR | S::FIELD_ACCESS | S::METHOD_REF_EXPR
            ) {
                break;
            }
            root = parent;
        }
        root
    }

    /// Whether this link's receiver is itself a chain link — i.e. it is not the chain's start.
    fn is_chain_link_receiver(node: &SyntaxNode) -> bool {
        node.first_child().is_some_and(|child| {
            matches!(
                child.kind(),
                S::CALL_EXPR | S::FIELD_ACCESS | S::METHOD_REF_EXPR
            )
        })
    }
}
