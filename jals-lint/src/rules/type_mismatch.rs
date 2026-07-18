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

use jals_exec::LocalBoxFuture;
use jals_hir::Resolved;
use jals_syntax::SyntaxNode;

use crate::IndexCtx;
use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: crate::TYPE_MISMATCH_RULE,
    default: Severity::Warn,
    check: Checker::Indexed(TypeMismatch::check),
};

/// The `type-mismatch` rule.
struct TypeMismatch;

impl TypeMismatch {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        root: &'a SyntaxNode,
        resolved: &'a Resolved,
        index: Option<IndexCtx<'a>>,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root, resolved, index))
    }

    async fn check_impl(
        root: &SyntaxNode,
        resolved: &Resolved,
        index: Option<IndexCtx<'_>>,
    ) -> Vec<Finding> {
        jals_hir::TypeInference::type_mismatches(root, resolved, index)
            .await
            .into_iter()
            .map(|m| Finding {
                message: m.message(),
                range: m.range,
                ..Finding::default()
            })
            .collect()
    }
}
