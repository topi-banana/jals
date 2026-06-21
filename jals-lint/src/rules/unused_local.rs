//! `unused-local`: flag local variables and parameters that are never referenced.
//!
//! This rule consumes `jals-hir`'s file-local name resolution: a binding that no reference
//! resolves to is unused. `jals-hir` already classifies bindings, so the rule simply flags unused
//! [`DefKind::Local`]s and [`DefKind::Param`]s. The kinds it skips are deliberate: abstract /
//! interface parameters are never registered (they can't be referenced), and lambda parameters are
//! a separate kind (their non-use is routinely intentional). `@Override` / interface-implementation
//! parameters remain a known source of false positives — suppress the rule via `jalslint.toml`
//! where that matters.

use jals_hir::DefKind;
use jals_syntax::SyntaxNode;

use crate::diagnostic::Severity;
use crate::rules::{Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unused-local",
    default: Severity::Warn,
    check,
};

fn check(root: &SyntaxNode) -> Vec<Finding> {
    let resolved = jals_hir::resolve_node(root);
    let mut out = Vec::new();
    for def in resolved.unused_defs() {
        let what = match def.kind {
            DefKind::Local => "local variable",
            DefKind::Param => "parameter",
            _ => continue,
        };
        out.push(Finding {
            range: def.name_range.clone(),
            message: format!("unused {what} `{}`", def.name),
        });
    }
    out
}
