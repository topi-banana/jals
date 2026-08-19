//! `wildcard-import`: flag star imports such as `import java.util.*;`, including the ones a jals
//! grouped import spells as a member (`import java.util.{concurrent.*};`).
//!
//! `import static a.B.*;` is reported too by default, and
//! [`static_imports`](jals_config::lint::WildcardImport::static_imports) exempts it: a static
//! wildcard is how a test file pulls in an assertion DSL, which is a project policy rather than an
//! oversight. It is one key and not a second rule because the two answers are exclusive — a static
//! wildcard is either reported or it is not, and two rules would let a config ask for both.

use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::SyntaxKind;
use jals_syntax::ast::{AstNode, ImportDecl, QualifiedName};

use jals_config::Category;
use jals_config::lint::{Config, StaticWildcard};

use crate::rules::{Checker, Finding, RuleMeta, Significant};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "wildcard-import",
    category: Category::Style,
    level: |config| config.style.wildcard_import.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(WildcardImport::check),
};

/// The `wildcard-import` rule.
struct WildcardImport;

impl WildcardImport {
    const MESSAGE: &'static str = "avoid wildcard imports; import the specific types you use";

    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(
        root: &'a jals_syntax::SyntaxNode,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root, config))
    }

    async fn check_impl(root: &jals_syntax::SyntaxNode, config: &Config) -> Vec<Finding> {
        let exempt_static =
            config.style.wildcard_import.options.static_imports == StaticWildcard::Allow;
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
            // `import static a.B.*;` — the whole declaration is static, so the exemption covers
            // its grouped members too.
            if exempt_static && import.is_static() {
                continue;
            }
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
                        Significant::range(member.syntax())
                            .map(|range| Finding::at_range(range, Self::MESSAGE)),
                    );
                }
            }
        }
        out
    }
}
