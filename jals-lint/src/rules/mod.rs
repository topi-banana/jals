//! The lint rules and the registry that drives them.
//!
//! Each rule is a pure function `fn(&SyntaxNode) -> Vec<Finding>` paired with metadata
//! ([`RuleMeta`]): a kebab-case name and a built-in default [`Severity`]. The library walks the
//! parsed CST, runs every enabled rule, and stamps each [`Finding`] with the rule name and the
//! severity resolved from configuration. Rules never mutate the tree and never panic.

use std::ops::Range;

use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::diagnostic::Severity;

mod empty_catch;
mod missing_braces;
mod naming;
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

/// A rule: its identity and its checker.
pub(crate) struct RuleMeta {
    /// Stable kebab-case name, used as the config key and shown in diagnostics.
    pub name: &'static str,
    /// Severity used when the rule is not configured.
    pub default: Severity,
    /// The checker: given the CST root, return every finding.
    pub check: fn(&SyntaxNode) -> Vec<Finding>,
}

/// Every rule, in a stable order.
pub(crate) const RULES: &[RuleMeta] = &[
    naming::RULE,
    wildcard_import::RULE,
    empty_catch::RULE,
    missing_braces::RULE,
];
