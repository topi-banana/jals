//! The jals dialect: grouped imports and attributes.
//!
//! These are jals's own syntax, so no vendor rule governs them and the formatter simply picks a
//! canonical form.
//!
//! # The one unconditional token change
//!
//! A grouped import's **trailing comma is dropped** (`import a.{B,};` → `import a.{B};`). This is
//! the crate's only token change that is not behind a config key, and it is confined to this node
//! kind, where the comma separates nothing: a group is always laid out flat, so there is no
//! vertical form for a trailing comma to serve, and the dialect desugaring ignores it either way.
//! The comma's *comments* are still emitted, so nothing is lost.

use jals_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A grouped import's `.{ A, B }`, emitted in the canonical compact form `.{A, B}`.
    pub(super) async fn visit_import_group(&mut self, node: &SyntaxNode) {
        let children = Self::children(node);
        // The comma past the last member separates nothing — that is the trailing comma. Found
        // by position rather than by "the next token is `}`", so error-recovery debris between
        // the two cannot disguise it as a separator.
        let last_member = children.iter().rposition(|child| {
            child
                .as_node()
                .is_some_and(|n| n.kind() == S::QUALIFIED_NAME)
        });

        for (nth, child) in children.iter().enumerate() {
            if let Some(tok) = child.as_token()
                && tok.kind() == S::COMMA
                && last_member.is_none_or(|last| nth > last)
            {
                self.emit_comments_without_token(tok);
                continue;
            }
            self.visit_element(child).await;
        }
    }

    /// A jals attribute, `#[cfg(feature = "x")]`.
    pub(super) async fn visit_attribute(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// Emit a token's comments while dropping the token's own text.
    fn emit_comments_without_token(&mut self, tok: &SyntaxToken) {
        for comment in self.comments.leading(tok).to_vec() {
            self.forced_break(Indent::ZERO);
            self.emit_comment(&comment);
        }
        for comment in self.comments.leading_inline(tok).to_vec() {
            self.space();
            self.emit_comment(&comment);
        }
        for comment in self.comments.trailing(tok).to_vec() {
            self.space();
            self.emit_comment(&comment);
        }
        for comment in self.comments.trailing_below(tok).to_vec() {
            self.forced_break(Indent::ZERO);
            self.emit_comment(&comment);
        }
    }
}
