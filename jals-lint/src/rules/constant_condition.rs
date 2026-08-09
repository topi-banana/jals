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

use alloc::format;

use jals_exec::LocalBoxFuture;
use jals_hir::FileAnalysis;

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "constant-condition",
    default: Severity::Warn,
    needs_clean_parse: false,
    check: Checker::Analyzed(ConstantCondition::check),
};

/// The `constant-condition` rule.
struct ConstantCondition;

impl ConstantCondition {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check(analysis: &FileAnalysis) -> LocalBoxFuture<'_, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis))
    }

    async fn check_impl(analysis: &FileAnalysis) -> Vec<Finding> {
        analysis
            .dead_ifs()
            .await
            .into_iter()
            .map(|d| Finding {
                message: format!(
                    "`if` condition is always {}",
                    if d.value { "true" } else { "false" }
                ),
                range: d.condition_range,
                unnecessary_range: d
                    .dead_range
                    .map(|r| (r, String::from("this code is never executed"))),
                ..Finding::default()
            })
            .collect()
    }
}
