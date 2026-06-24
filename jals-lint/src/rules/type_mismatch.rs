//! `type-mismatch`: flag assignment-context type incompatibilities.
//!
//! This rule consumes `jals-hir` type inference: a variable initializer or a simple `=` assignment
//! whose value type is not assignable to its slot type is reported. It is the file-local
//! (index-free) half — reference types resolve only by spelling, so it catches primitive narrowing
//! (`int x = 1.0;`), `boolean`/numeric confusion, `null` to a primitive, and array element
//! mismatches. The language server runs an index-aware variant that additionally catches
//! project-internal subtyping mismatches (a `Sub`/`Base` confusion).
//!
//! Conservative by construction (see [`jals_hir::Ty::is_assignable_to`]): an `Unknown` type, an
//! external / boxing pair, and a numeric constant that narrowing could rescue (`byte b = 1;`) are
//! never flagged, so the rule does not produce false positives.

use jals_syntax::SyntaxNode;

use crate::diagnostic::Severity;
use crate::rules::{Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: crate::TYPE_MISMATCH_RULE,
    default: Severity::Warn,
    check,
};

fn check(root: &SyntaxNode) -> Vec<Finding> {
    let resolved = jals_hir::resolve_node(root);
    jals_hir::type_mismatches(root, &resolved, None)
        .into_iter()
        .map(|m| Finding {
            message: m.message(),
            range: m.range,
        })
        .collect()
}
