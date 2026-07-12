//! `constant-condition`: flag an `if` statement whose condition always evaluates to the same
//! value, making one of its branches dead.
//!
//! This is the lint-side adapter over [`jals_hir::DeadIf::collect`], which does the whole analysis
//! (see there): it folds boolean / integer literals, parentheses, `!`, the short-circuit operators,
//! integer comparisons, and same-file `final` constant variables — and is conservative, so a
//! condition it cannot prove constant is never reported. The dead branch is carried as the
//! finding's [`unnecessary_range`](Finding::unnecessary_range), which the LSP renders as faded
//! code.

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::Resolved;
use jals_syntax::SyntaxNode;

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "constant-condition",
    default: Severity::Warn,
    check: Checker::Resolved(ConstantCondition::check),
};

/// The `constant-condition` rule.
struct ConstantCondition;

impl ConstantCondition {
    fn check(root: &SyntaxNode, resolved: &Resolved) -> Vec<Finding> {
        jals_hir::DeadIf::collect(root, resolved)
            .into_iter()
            .map(|d| Finding {
                message: d.message(),
                range: d.condition_range,
                unnecessary_range: d
                    .dead_range
                    .map(|r| (r, String::from("this code is never executed"))),
                ..Finding::default()
            })
            .collect()
    }
}
