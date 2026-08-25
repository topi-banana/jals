//! `cannot-resolve`: flag a name that resolves to nothing — neither file-locally nor anywhere in
//! the project index.
//!
//! This is javac's "cannot find symbol", and it covers all three name-spaces through two facts
//! `jals-hir` answers, each of which does the whole analysis (see there):
//!
//! - [`unresolved_types`](jals_hir::FileSemantics::unresolved_types) — a **type** name the
//!   file-local resolver *and* the project layer both refuse. A name that might be provided from
//!   outside the indexed sources (an import target, a `java.lang` name, anything reachable through
//!   an on-demand import) resolves to [`External`](jals_hir::TypeResolution::External) and is
//!   never reported.
//! - [`unresolved_names`](jals_hir::FileSemantics::unresolved_names) — a **variable** or **method**
//!   name that no scope, no supertype, and no static import binds. It stands down wherever a
//!   negative answer would need something the index does not carry: a type that inherits from
//!   outside the project, a partial standard-library stub, an on-demand static import whose owner
//!   is either, and the two positions where a name binds against something other than a scope (the
//!   ambiguous-name qualifier of JLS §6.5.2, and a `case` label's constant).
//!
//! Neither fact produces a false positive, which is what lets one `error`-level rule carry both.
//!
//! It is index-aware: with no project index (the file-local path) it reports nothing, since
//! deciding that a name is nameable from nowhere needs the whole project's table.
//!
//! It is the one rule that defaults to [`LintLevel::Error`](jals_config::LintLevel::Error): an
//! unresolvable name is not a style question. It is a `[correctness]` key like any other, so a
//! project that indexes only part of its sources can still set it to `allow`.

use alloc::vec::Vec;

use alloc::format;

use jals_exec::LocalBoxFuture;
use jals_hir::{FileAnalysis, FileSemantics, Namespace, UnresolvedName};

use jals_config::Category;
use jals_config::lint::Config;

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "cannot-resolve",
    category: Category::Correctness,
    level: |config| config.correctness.cannot_resolve.level,
    needs_clean_parse: false,
    check: Checker::Semantic(CannotResolve::check),
};

/// The `cannot-resolve` rule.
struct CannotResolve;

impl CannotResolve {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        analysis: &'a FileAnalysis,
        project: Option<&'a FileSemantics<'a>>,
        _config: &'a Config,
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
        let mut out: Vec<Finding> = semantics
            .unresolved_types()
            .await
            .into_iter()
            .map(|u| Finding {
                message: format!("cannot resolve symbol `{}`", u.name),
                range: u.range,
                ..Finding::default()
            })
            .collect();
        out.extend(
            semantics
                .unresolved_names()
                .await
                .into_iter()
                .map(Self::name_finding),
        );
        out
    }

    /// One unresolved value/method name as a finding.
    ///
    /// The two name-spaces are worded apart because they read apart — javac's detail line says
    /// `variable x` against `method x()`, and a reader chasing a missing name wants to know which
    /// one was looked for. The lead stays `cannot resolve symbol` in both, which is the wording the
    /// type half already uses.
    fn name_finding(unresolved: UnresolvedName) -> Finding {
        let message = match unresolved.namespace {
            Namespace::Method => format!("cannot resolve method `{}`", unresolved.name),
            _ => format!("cannot resolve symbol `{}`", unresolved.name),
        };
        Finding {
            message,
            range: unresolved.range,
            ..Finding::default()
        }
    }
}
