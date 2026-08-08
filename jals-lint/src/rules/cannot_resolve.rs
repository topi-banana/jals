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
//! Unlike every other rule here this one defaults to [`Severity::Error`]: an unresolvable name is
//! not a style question. It is a configuration key like any other, so a project that indexes only
//! part of its sources can still set it to `allow`.

use alloc::vec::Vec;

use jals_exec::LocalBoxFuture;
use jals_hir::Resolved;
use jals_syntax::SyntaxNode;

use crate::IndexCtx;
use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "cannot-resolve",
    default: Severity::Error,
    needs_clean_parse: false,
    check: Checker::Indexed(CannotResolve::check),
};

/// The `cannot-resolve` rule.
struct CannotResolve;

impl CannotResolve {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        root: &'a SyntaxNode,
        resolved: &'a Resolved,
        index: Option<IndexCtx<'a>>,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root, resolved, index))
    }

    async fn check_impl(
        _root: &SyntaxNode,
        resolved: &Resolved,
        index: Option<IndexCtx<'_>>,
    ) -> Vec<Finding> {
        // Returning nothing without an index is the `Checker::Indexed` contract the driver leans on
        // to silence this rule on a broken parse without naming it.
        let Some((index, file)) = index else {
            return Vec::new();
        };

        index
            .unresolved_types(file, resolved)
            .await
            .into_iter()
            .map(|u| Finding {
                message: u.message(),
                range: u.range,
                ..Finding::default()
            })
            .collect()
    }
}
