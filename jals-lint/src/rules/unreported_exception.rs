//! `unreported-exception`: flag a checked exception a method / constructor can raise but neither
//! declares in its `throws` clause nor handles with an enclosing `try` / `catch`.
//!
//! This is the lint-side adapter over [`jals_hir::unreported_exceptions`], which does the whole
//! analysis (see there): it classifies each raised type as a checked exception via the project's
//! `Throwable` hierarchy, subtracts the ones the enclosing declaration declares or an enclosing
//! `try`/`catch` catches, and is conservative — a raise it cannot fully prove is never reported.
//!
//! It is index-aware: with no project index (the file-local path) it reports nothing, since checked /
//! unchecked classification and cross-file `throws` lookup both need the index.

use alloc::vec::Vec;

use jals_hir::Resolved;
use jals_syntax::SyntaxNode;

use crate::IndexCtx;
use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub const RULE: RuleMeta = RuleMeta {
    name: "unreported-exception",
    default: Severity::Warn,
    check: Checker::Indexed(check),
};

fn check(root: &SyntaxNode, resolved: &Resolved, index: Option<IndexCtx>) -> Vec<Finding> {
    jals_hir::unreported_exceptions(root, resolved, index)
        .into_iter()
        .map(|e| Finding {
            message: e.message(),
            range: e.range,
            ..Finding::default()
        })
        .collect()
}
