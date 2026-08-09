//! `unreported-exception`: flag a checked exception a method / constructor can raise but neither
//! declares in its `throws` clause nor handles with an enclosing `try` / `catch`.
//!
//! This is the lint-side adapter over [`jals_hir::UnreportedException::collect`], which does the
//! whole analysis (see there): it classifies each raised type as a checked exception via the project's
//! `Throwable` hierarchy, subtracts the ones the enclosing declaration declares or an enclosing
//! `try`/`catch` catches, and is conservative — a raise it cannot fully prove is never reported.
//!
//! It is index-aware: with no project index (the file-local path) it reports nothing, since checked /
//! unchecked classification and cross-file `throws` lookup both need the index.

use alloc::vec::Vec;

use alloc::format;

use jals_exec::LocalBoxFuture;
use jals_hir::{FileAnalysis, FileSemantics};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unreported-exception",
    default: Severity::Warn,
    needs_clean_parse: false,
    check: Checker::Semantic(UnreportedExceptionRule::check),
};

/// The `unreported-exception` rule (named with a `Rule` suffix to avoid clashing with the
/// [`jals_hir::UnreportedException`] analysis type it delegates to).
struct UnreportedExceptionRule;

impl UnreportedExceptionRule {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        analysis: &'a FileAnalysis,
        project: Option<&'a FileSemantics<'a>>,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis, project))
    }

    async fn check_impl(
        _analysis: &FileAnalysis,
        project: Option<&FileSemantics<'_>>,
    ) -> Vec<Finding> {
        // Reporting nothing without a project is the `Checker::Semantic` contract the driver leans
        // on to silence this rule on a broken parse without naming it.
        let Some(semantics) = project else {
            return Vec::new();
        };
        semantics
            .unreported_exceptions()
            .await
            .into_iter()
            .map(|e| Finding {
                message: format!(
                    "unreported exception {}; must be caught or declared to be thrown",
                    e.name
                ),
                range: e.range,
                ..Finding::default()
            })
            .collect()
    }
}
