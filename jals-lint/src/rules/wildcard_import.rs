//! `wildcard-import`: flag star imports such as `import java.util.*;`.

use jals_syntax::SyntaxKind;
use jals_syntax::ast::{AstNode, ImportDecl};

use crate::diagnostic::Severity;
use crate::rules::{Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "wildcard-import",
    default: Severity::Warn,
    check,
};

fn check(root: &jals_syntax::SyntaxNode) -> Vec<Finding> {
    let mut out = Vec::new();
    for node in root.descendants() {
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
