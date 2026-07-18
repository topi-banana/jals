//! `wildcard-import`: flag star imports such as `import java.util.*;`.

use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::SyntaxKind;
use jals_syntax::ast::{AstNode, ImportDecl};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "wildcard-import",
    default: Severity::Warn,
    check: Checker::Syntactic(WildcardImport::check),
};

/// The `wildcard-import` rule.
struct WildcardImport;

impl WildcardImport {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check(root: &jals_syntax::SyntaxNode) -> LocalBoxFuture<'_, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root))
    }

    async fn check_impl(root: &jals_syntax::SyntaxNode) -> Vec<Finding> {
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            if node.kind() != SyntaxKind::IMPORT_DECL {
                continue;
            }
            let Some(import) = ImportDecl::cast(node) else {
                continue;
            };
            if let Some(name) = import.name()
                && name.is_wildcard()
            {
                out.push(Finding::at_node(
                    import.syntax(),
                    "avoid wildcard imports; import the specific types you use",
                ));
            }
        }
        out
    }
}
