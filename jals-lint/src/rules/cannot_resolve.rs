//! `cannot-resolve`: flag a type name that resolves to nothing — neither file-locally nor anywhere
//! in the project index.
//!
//! This is the lint-side adapter over [`jals_hir::ProjectIndex::unresolved_types`], which does the
//! whole analysis (see there): it walks the file's type-name references and keeps only the ones the
//! file-local resolver *and* the project layer both refuse. A name that might be provided from
//! outside the indexed sources — an import target, a `java.lang` name, anything reachable through
//! an on-demand import — resolves to [`External`](jals_hir::TypeResolution::External) and is never
//! reported, so the rule does not produce false positives.
//!
//! It is index-aware: with no project index (the file-local path) it reports nothing, since deciding
//! that a name is nameable from nowhere needs the whole project's type table.
//!
//! It is the one rule that defaults to [`LintLevel::Error`](jals_config::LintLevel::Error): an
//! unresolvable name is not a style question. It is a `[correctness]` key like any other, so a
//! project that indexes only part of its sources can still set it to `allow`.

use alloc::vec::Vec;

use alloc::format;

use jals_exec::LocalBoxFuture;
use jals_hir::{FileAnalysis, FileSemantics};

use jals_config::Category;
use jals_config::lint::Config;

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "cannot-resolve",
    category: Category::Correctness,
    level: |config| config.correctness.cannot_resolve.level,
    needs_clean_parse: false,
    check: Checker::Semantic(api::check),
};

/// The `cannot-resolve` rule.
mod api {
    use super::{Config, FileAnalysis, FileSemantics, Finding, LocalBoxFuture, Vec, format};

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        analysis: &'a FileAnalysis,
        project: Option<&'a FileSemantics<'a>>,
        _config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(analysis, project))
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
            .unresolved_types()
            .await
            .into_iter()
            .map(|u| Finding {
                message: format!("cannot resolve symbol `{}`", u.name),
                range: u.range,
                ..Finding::default()
            })
            .collect()
    }
}
