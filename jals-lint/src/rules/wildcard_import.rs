//! `wildcard-import`: flag star imports such as `import java.util.*;`, including the ones a jals
//! grouped import spells as a member (`import java.util.{concurrent.*};`).

use alloc::vec::Vec;
use core::ops::Range;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::SyntaxKind;
use jals_syntax::ast::{AstNode, ImportDecl, QualifiedName};

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
    const MESSAGE: &'static str = "avoid wildcard imports; import the specific types you use";

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
                out.push(Finding::at_node(import.syntax(), Self::MESSAGE));
            }
            // A jals grouped import hides its wildcards one level down: in
            // `import java.util.{concurrent.*};` the declaration's own name is the shared prefix
            // `java.util`, so the check above sees no star. Each on-demand member is the same
            // wildcard import spelled differently — `jals-hir` already records it as one — so it
            // is flagged too, pointing at the member rather than the whole declaration, since the
            // named members beside it are not the problem.
            if let Some(group) = import.group() {
                for member in group.members().filter(QualifiedName::is_wildcard) {
                    out.extend(
                        Self::significant_range(member.syntax())
                            .map(|range| Finding::at_range(range, Self::MESSAGE)),
                    );
                }
            }
        }
        out
    }

    /// A node's byte range without the trivia rowan parks inside it. A grouped import's member
    /// node starts at the space after the preceding comma (`{HashMap,·concurrent.*}`), and a
    /// diagnostic that began one column early would underline that space instead of the name.
    /// `None` for a node holding no significant token at all (error recovery).
    fn significant_range(node: &jals_syntax::SyntaxNode) -> Option<Range<usize>> {
        let mut tokens = node
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia())
            .map(|token| token.text_range());
        let first = tokens.next()?;
        let last = tokens.last().unwrap_or(first);
        Some(usize::from(first.start())..usize::from(last.end()))
    }
}
