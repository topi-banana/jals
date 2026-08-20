//! `type-mismatch`: flag assignment-context type incompatibilities.
//!
//! This rule consumes `jals-hir` type inference: a variable initializer or a simple `=` assignment
//! whose value type is not assignable to its slot type is reported. With no project index it is the
//! file-local half — reference types resolve only by spelling, so it catches primitive narrowing
//! (`int x = 1.0;`), `boolean`/numeric confusion, `null` to a primitive, and array element
//! mismatches. When the caller supplies a [`ProjectIndex`] (the CLI over a multi-file run, the
//! language server) it additionally catches project-internal subtyping mismatches (a `Sub`/`Base`
//! confusion) and bad call arguments resolved across files.
//!
//! Conservative by construction (see [`jals_hir::Ty::is_assignable_to`]): an `Unknown` type, an
//! external / boxing pair, and a numeric constant that narrowing could rescue (`byte b = 1;`) are
//! never flagged, so the rule does not produce false positives.

use alloc::vec::Vec;

use alloc::format;
use alloc::string::ToString;

use jals_exec::LocalBoxFuture;
use jals_hir::{FileAnalysis, FileSemantics, MismatchKind};

use jals_config::Category;
use jals_config::lint::Config;

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "type-mismatch",
    category: Category::Correctness,
    level: |config| config.correctness.type_mismatch.level,
    // Every finding here comes from type inference; see `RuleMeta::needs_clean_parse`.
    needs_clean_parse: true,
    check: Checker::Semantic(api::check),
};

/// The `type-mismatch` rule.
mod api {
    use super::{
        Config, FileAnalysis, FileSemantics, Finding, LocalBoxFuture, MismatchKind, ToString, Vec,
        format,
    };

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        analysis: &'a FileAnalysis,
        project: Option<&'a FileSemantics<'a>>,
        _config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(analysis, project))
    }

    pub(crate) async fn check_impl(
        analysis: &FileAnalysis,
        project: Option<&FileSemantics<'_>>,
    ) -> Vec<Finding> {
        // The only rule with a real answer either way: with a project it also sees cross-file
        // subtyping and call arguments, without one it still catches the primitive half.
        let mismatches = match project {
            Some(semantics) => semantics.type_mismatches().await,
            None => analysis.type_mismatches().await,
        };
        mismatches
            .into_iter()
            .map(|m| Finding {
                message: match m.kind() {
                    MismatchKind::Assignment { expected, found } => {
                        format!("incompatible types: `{found}` cannot be assigned to `{expected}`")
                    }
                    MismatchKind::NoOverload { name, args } => {
                        let list = args
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("no overload of `{name}` accepts the argument types ({list})")
                    }
                },
                range: m.range,
                ..Finding::default()
            })
            .collect()
    }
}
