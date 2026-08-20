//! `empty-catch`: flag a `catch` clause whose block is empty and carries no comment.
//!
//! A catch that swallows an exception silently is a common bug. The rule accepts two statements
//! of intent, both configurable: a comment inside the block
//! (`catch (E e) { /* ignored: ... */ }`, [`IgnoredCatch`]) and an exception parameter whose name
//! is on [`allowed_names`](jals_config::lint::EmptyCatch::allowed_names). The comment is accepted
//! out of the box because it is the conventional Java spelling; the name list is empty out of the
//! box, because a project that spells intent in the name has to say which names it uses.

use alloc::vec::Vec;

use jals_config::lint::{Config, IgnoredCatch};
use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::ast::{AstNode, CatchClause};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use jals_config::Category;

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "empty-catch",
    category: Category::Suspicious,
    level: |config| config.suspicious.empty_catch.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(api::check),
};

/// The `empty-catch` rule.
mod api {
    use super::{
        AstNode, CatchClause, Config, Finding, IgnoredCatch, LocalBoxFuture, SyntaxElement,
        SyntaxKind, SyntaxNode, Vec, Yielder,
    };

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        root: &'a SyntaxNode,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(root, config))
    }

    async fn check_impl(root: &SyntaxNode, config: &Config) -> Vec<Finding> {
        let options = &config.suspicious.empty_catch.options;
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            if node.kind() != SyntaxKind::CATCH_CLAUSE {
                continue;
            }
            let Some(catch) = CatchClause::cast(node.clone()) else {
                continue;
            };
            let Some(block) = catch.block() else {
                continue;
            };
            // A name the project has declared meaning-bearing (`ignored`, `expected`) says the
            // same thing a comment says, in the place the reader is already looking.
            if catch.binding().is_some_and(|name| {
                options
                    .allowed_names
                    .iter()
                    .any(|allowed| allowed == name.text())
            }) {
                continue;
            }
            let block = block.syntax();
            let has_stmt = block.children().next().is_some();
            let has_comment = block
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .any(|t| {
                    matches!(
                        t.kind(),
                        SyntaxKind::LINE_COMMENT
                            | SyntaxKind::BLOCK_COMMENT
                            | SyntaxKind::DOC_COMMENT
                    )
                });
            let explained = has_comment && options.commented == IgnoredCatch::Accept;
            if !has_stmt && !explained {
                out.push(Finding::at_node(
                    &node,
                    "empty catch block swallows the exception; handle it or add a comment explaining why",
                ));
            }
        }
        out
    }
}
