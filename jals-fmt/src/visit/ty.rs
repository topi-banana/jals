//! Types and patterns.
//!
//! Types are mostly punctuation decisions, which [`Spacing`](super::Spacing) already owns: `<>`
//! hugs, `[]` clings to the token before it, a type annotation is separated from the `[]` or
//! `...` that follows, `&` in an intersection and `|` in a multi-`catch` take spaces. What is
//! left here is the record deconstruction pattern, whose component list is a real delimited list
//! with its own `[wrapping]` rule.

use jals_syntax::{SyntaxKind as S, SyntaxNode};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A record deconstruction pattern, `Point(int x, int y)`.
    /// A type, whose array dimensions may wrap between brackets.
    ///
    /// `maybeAddDims` puts a fill break in front of every `[`, inside a `plusFour` level, because
    /// a type like `int[][]…[]` can otherwise exceed the column limit with no legal break in it
    /// at all. Only a genuinely long run gets the treatment: `String[][]` reads as one token and
    /// splitting it would be noise.
    pub(super) async fn visit_type(&mut self, node: &SyntaxNode) {
        const LONG_RUN: usize = 3;
        let children = Self::children(node);
        let dims = children
            .iter()
            .filter(|child| child.as_token().is_some_and(|tok| tok.kind() == S::LBRACK))
            .count();
        if dims < LONG_RUN {
            self.visit_children(node).await;
            return;
        }
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        for child in &children {
            if child.as_token().is_some_and(|tok| tok.kind() == S::LBRACK) {
                self.ops
                    .brk(crate::ir::FillMode::Independent, "", Indent::ZERO, None);
                self.space_already_emitted();
            }
            self.visit_element(child).await;
        }
        self.close_indent(&continuation);
    }

    pub(super) async fn visit_record_pattern(&mut self, node: &SyntaxNode) {
        self.visit_delimited(node).await;
    }
}
