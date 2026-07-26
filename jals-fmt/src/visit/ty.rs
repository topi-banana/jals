//! Types and patterns.
//!
//! Types are mostly punctuation decisions, which [`Spacing`](super::Spacing) already owns: `<>`
//! hugs, `[]` clings to the token before it, a type annotation is separated from the `[]` or
//! `...` that follows, `&` in an intersection and `|` in a multi-`catch` take spaces. What is
//! left here is the record deconstruction pattern, whose component list is a real delimited list
//! with its own `[wrapping]` rule.

use jals_syntax::SyntaxNode;

use crate::visit::Ctx;

impl Ctx<'_> {
    /// A record deconstruction pattern, `Point(int x, int y)`.
    pub(super) async fn visit_record_pattern(&mut self, node: &SyntaxNode) {
        self.visit_delimited(node).await;
    }
}
