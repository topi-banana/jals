//! `unused-local`: flag local variables and parameters that are never referenced.
//!
//! This rule consumes `jals-hir`'s file-local name resolution: a binding that no reference
//! resolves to is unused. `jals-hir` already classifies bindings, so the rule simply flags unused
//! [`DefKind::Local`]s and [`DefKind::Param`]s. The kinds it skips are deliberate: abstract /
//! interface parameters are never registered (they can't be referenced), and lambda parameters are
//! a separate kind (their non-use is routinely intentional). `@Override` / interface-implementation
//! parameters remain a known source of false positives — suppress the rule via `jalslint.toml`
//! where that matters.

use alloc::format;
use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{DefKind, FileAnalysis};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unused-local",
    default: Severity::Warn,
    needs_clean_parse: false,
    check: Checker::Analyzed(UnusedLocal::check),
};

/// The `unused-local` rule.
struct UnusedLocal;

impl UnusedLocal {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check(analysis: &FileAnalysis) -> LocalBoxFuture<'_, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis))
    }

    async fn check_impl(analysis: &FileAnalysis) -> Vec<Finding> {
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for def in analysis.unused_defs() {
            yielder.tick().await;
            let what = match def.kind {
                DefKind::Local => "local variable",
                DefKind::Param => "parameter",
                _ => continue,
            };
            out.push(Finding {
                range: def.name_range.clone(),
                message: format!("unused {what} `{}`", def.name),
                // The binding itself is the unnecessary code — consumers fade it in place.
                unnecessary: true,
                ..Finding::default()
            });
        }
        out
    }
}
