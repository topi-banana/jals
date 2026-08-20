//! `collapsible-if`: an `if` whose entire body is another `if`, where the two conditions could be
//! joined with `&&`.
//!
//! Ports `clippy::collapsible_if`. The nesting is reported only when it carries no information the
//! joined form would lose, which rules out four shapes:
//!
//! - an `else` on either `if` — the branches are not the same branch;
//! - a statement beside the inner `if` — the outer body does more than guard;
//! - a comment in the outer block outside the inner `if` — it is attached to the nesting, and
//!   collapsing would leave it explaining a condition that no longer exists on its own.
//!
//! An `else if` **is** eligible as the outer `if`: `else if (b) { if (c) … }` collapses to
//! `else if (b && c)` exactly as a free-standing one does. What an `else if` chain rules out is the
//! `if` *above* it, and that is already covered — an outer `if` with an `else` has two branches.
//!
//! The rule points at the outer `if`'s condition rather than the whole statement: that is the
//! expression the fix changes, and the whole statement is usually the rest of the screen.

use alloc::vec::Vec;

use jals_config::Category;
use jals_config::lint::Config;
use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::ast::{AstNode, IfStmt, Stmt};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::rules::significant;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "collapsible-if",
    category: Category::Complexity,
    level: |config| config.complexity.collapsible_if.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(api::check),
};

/// The `collapsible-if` rule.
mod api {
    use super::{
        AstNode, Config, Finding, IfStmt, LocalBoxFuture, Stmt, SyntaxElement, SyntaxKind,
        SyntaxNode, Vec, Yielder, significant,
    };

    const MESSAGE: &str = "this `if` only guards another `if`; join the two conditions with `&&`";

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        root: &'a SyntaxNode,
        _config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(root))
    }

    async fn check_impl(root: &SyntaxNode) -> Vec<Finding> {
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            let Some(outer) = IfStmt::cast(node) else {
                continue;
            };
            if !collapses(&outer) {
                continue;
            }
            // The condition, not the statement: it is what the fix rewrites.
            out.extend(
                outer
                    .condition()
                    .map(|cond| Finding::at_node(cond.syntax(), MESSAGE)),
            );
        }
        out
    }

    /// Whether `outer`'s body is a lone `if` that could join it.
    fn collapses(outer: &IfStmt) -> bool {
        let mut branches = outer.branches();
        let Some(body) = branches.next() else {
            return false;
        };
        if branches.next().is_some() {
            return false; // an `else` on the outer `if`
        }
        let inner = match body {
            // `if (a) { if (b) … }` — the block must hold the inner `if` and nothing else, not
            // even a comment.
            Stmt::Block(block) => {
                let mut stmts = block.stmts();
                let Some(Stmt::If(inner)) = stmts.next() else {
                    return false;
                };
                if stmts.next().is_some() {
                    return false;
                }
                if has_orphaned_comment(block.syntax(), inner.syntax()) {
                    return false;
                }
                inner
            }
            // `if (a) if (b) …` — already brace-less, and still collapsible.
            Stmt::If(inner) => inner,
            _ => return false,
        };
        inner.branches().count() == 1
    }

    /// Whether `block` holds a comment that collapsing would orphan: one written outside `inner`'s
    /// own significant span, which is where a comment about the *nesting* goes.
    ///
    /// The span is [`significant::range`] rather than `inner`'s node range, because rowan parks the
    /// leading trivia — the comment in question — inside the node that follows it, so a comment
    /// written before `if` is already part of the inner statement's range.
    fn has_orphaned_comment(block: &SyntaxNode, inner: &SyntaxNode) -> bool {
        let Some(inner) = significant::range(inner) else {
            return false;
        };
        block
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| {
                matches!(
                    token.kind(),
                    SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
                )
            })
            .any(|token| {
                let range = token.text_range();
                usize::from(range.end()) <= inner.start || usize::from(range.start()) >= inner.end
            })
    }
}
