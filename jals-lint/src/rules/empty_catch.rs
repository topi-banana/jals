//! `empty-catch`: flag a `catch` clause whose block is empty and carries no comment.
//!
//! A catch that swallows an exception silently is a common bug. A block with an explanatory
//! comment (`catch (E e) { /* ignored: ... */ }`) is treated as intentional and not flagged.

use alloc::vec::Vec;

use jals_syntax::ast::{AstNode, CatchClause};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "empty-catch",
    default: Severity::Warn,
    check: Checker::Syntactic(EmptyCatch::check),
};

/// The `empty-catch` rule.
struct EmptyCatch;

impl EmptyCatch {
    fn check(root: &SyntaxNode) -> Vec<Finding> {
        let mut out = Vec::new();
        for node in root.descendants() {
            if node.kind() != SyntaxKind::CATCH_CLAUSE {
                continue;
            }
            let Some(catch) = CatchClause::cast(node.clone()) else {
                continue;
            };
            let Some(block) = catch.block() else {
                continue;
            };
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
            if !has_stmt && !has_comment {
                out.push(Finding::at_node(
                    &node,
                    "empty catch block swallows the exception; handle it or add a comment explaining why",
                ));
            }
        }
        out
    }
}
