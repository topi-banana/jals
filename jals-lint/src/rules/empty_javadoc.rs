//! `empty-javadoc`: a `/** … */` whose content is only whitespace and leading asterisks.
//!
//! Ports `clippy::empty_docs`. An empty doc comment is strictly worse than no doc comment: tooling
//! reads it as documentation that exists, a reader reads it as a promise someone meant to keep,
//! and `javadoc` renders a blank entry for it. Deleting it or filling it are both improvements;
//! leaving it is the only thing that is not.
//!
//! Only `DOC_COMMENT` tokens are examined, so an ordinary `/* */` block comment — which documents
//! nothing by definition — is never reported. `/**/` is not a doc comment at all (the lexer reads
//! it as a block comment), so the shortest reportable spelling is `/***/`.

use alloc::vec::Vec;

use jals_config::Category;
use jals_config::lint::Config;
use jals_exec::{LocalBoxFuture, Yielder};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "empty-javadoc",
    category: Category::Documentation,
    level: |config| config.documentation.empty_javadoc.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(EmptyJavadoc::check),
};

/// The `empty-javadoc` rule.
struct EmptyJavadoc;

impl EmptyJavadoc {
    const MESSAGE: &'static str =
        "empty Javadoc comment; document the declaration or remove the comment";

    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(root: &'a SyntaxNode, _config: &'a Config) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root))
    }

    async fn check_impl(root: &SyntaxNode) -> Vec<Finding> {
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for token in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            yielder.tick().await;
            if token.kind() == SyntaxKind::DOC_COMMENT && Self::is_empty(token.text()) {
                out.push(Finding::at_token(&token, Self::MESSAGE));
            }
        }
        out
    }

    /// Whether a Javadoc token's body says nothing: strip the `/**` and `*/` delimiters, then the
    /// per-line asterisks that are decoration rather than content.
    fn is_empty(text: &str) -> bool {
        text.trim_start_matches("/**")
            .trim_end_matches("*/")
            .chars()
            .all(|c| c == '*' || c.is_whitespace())
    }
}
