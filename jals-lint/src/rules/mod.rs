//! The lint rules and the registry that drives them.
//!
//! Each rule is a pure function `fn(&SyntaxNode) -> Vec<Finding>` paired with metadata
//! ([`RuleMeta`]): a kebab-case name and a built-in default [`Severity`]. The library walks the
//! parsed CST, runs every enabled rule, and stamps each [`Finding`] with the rule name and the
//! severity resolved from configuration. Rules never mutate the tree and never panic.

use std::ops::Range;

use jals_hir::Resolved;
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::IndexCtx;

use crate::diagnostic::Severity;

mod empty_catch;
mod missing_braces;
mod naming;
mod type_mismatch;
mod unused_local;
mod wildcard_import;

/// A potential problem reported by a rule, before it is tagged with a rule name / severity.
pub(crate) struct Finding {
    /// Byte range in the original source.
    pub range: Range<usize>,
    /// Human-readable message.
    pub message: String,
}

impl Finding {
    /// A finding spanning `node`.
    pub(crate) fn at_node(node: &SyntaxNode, message: impl Into<String>) -> Finding {
        let range = node.text_range();
        Finding {
            range: usize::from(range.start())..usize::from(range.end()),
            message: message.into(),
        }
    }

    /// A finding spanning `token`.
    pub(crate) fn at_token(token: &SyntaxToken, message: impl Into<String>) -> Finding {
        let range = token.text_range();
        Finding {
            range: usize::from(range.start())..usize::from(range.end()),
            message: message.into(),
        }
    }
}

/// How a rule is invoked. Most rules need only the CST; resolution-based rules additionally take
/// the file-local name resolution, which the library computes at most once per lint (see
/// [`crate::lint_node`]) and shares across every [`Checker::Resolved`] / [`Checker::Indexed`] rule.
#[derive(Clone, Copy)]
pub(crate) enum Checker {
    /// A pure syntactic rule: given the CST root, return every finding.
    Syntactic(fn(&SyntaxNode) -> Vec<Finding>),
    /// A rule that also consumes `jals-hir` file-local name resolution.
    Resolved(fn(&SyntaxNode, &Resolved) -> Vec<Finding>),
    /// A rule that, in addition to name resolution, may resolve reference types against a
    /// project-wide symbol index when the caller supplies one ([`IndexCtx`]); with no index it
    /// falls back to the file-local behavior. The basis for cross-file type checking.
    Indexed(fn(&SyntaxNode, &Resolved, Option<IndexCtx>) -> Vec<Finding>),
}

/// A rule: its identity and its checker.
pub(crate) struct RuleMeta {
    /// Stable kebab-case name, used as the config key and shown in diagnostics.
    pub name: &'static str,
    /// Severity used when the rule is not configured.
    pub default: Severity,
    /// The checker, syntactic or resolution-based.
    pub check: Checker,
}

/// Every rule, in a stable order.
pub(crate) const RULES: &[RuleMeta] = &[
    naming::RULE,
    wildcard_import::RULE,
    empty_catch::RULE,
    missing_braces::RULE,
    unused_local::RULE,
    type_mismatch::RULE,
];
