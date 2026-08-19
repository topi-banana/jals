//! `unused-imports`: flag every import declaration whose name the file never spells.
//!
//! This rule consumes `jals-hir`'s file-local analysis —
//! [`unused_imports`](FileAnalysis::unused_imports) — and does no searching of its own. What it
//! owns is the *wording*; the bindings that same analysis reports are `unused-variables`' and
//! `dead-code`'s.
//!
//! One file settles the question outright: an import is scoped to the compilation unit that writes
//! it, so a file that never spells the name has seen every use there can be. There is no opt-out
//! either — a leading `_` says nothing about an import, because the name is not this file's to
//! choose.

use alloc::format;
use alloc::vec::Vec;

use jals_exec::LocalBoxFuture;
use jals_hir::FileAnalysis;

use jals_config::Category;
use jals_config::lint::Config;

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unused-imports",
    category: Category::Unused,
    level: |config| config.unused.unused_imports.level,
    needs_clean_parse: false,
    check: Checker::Analyzed(UnusedImports::check),
};

/// The `unused-imports` rule.
struct UnusedImports;

impl UnusedImports {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        analysis: &'a FileAnalysis,
        _config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis))
    }

    async fn check_impl(analysis: &FileAnalysis) -> Vec<Finding> {
        let mut out = Vec::new();
        for import in analysis.unused_imports().await {
            let modifier = if import.is_static { "static " } else { "" };
            out.push(Finding::unnecessary_at(
                import.range,
                format!("unused {modifier}import `{}`", import.name),
            ));
        }
        out
    }
}
