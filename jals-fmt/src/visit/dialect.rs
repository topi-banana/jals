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
//! The comma's *comments* are still emitted, so no comment is lost — but the **token is**, and that
//! is what [`TokenBudget`](crate::passes::TokenBudget) checks. It is therefore a declared row in
//! [`token_license`](crate::passes::token_license), gated on nothing; before it was declared the
//! fail-safe rejected the drop and handed back the whole file unformatted.
//!
//! Which comma is "the trailing one" is [`License::is_group_trailing_comma`], not a predicate of
//! this module's own: the pass that drops it and the check that licenses it have to agree, and two
//! implementations of that question are how they came apart.

use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::passes::token_license::License;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// A grouped import's `.{ A, B }`, emitted in the canonical compact form `.{A, B}`.
    pub(super) async fn visit_import_group(&mut self, node: &SyntaxNode) {
        for child in Self::children(node) {
            if let Some(tok) = child.as_token()
                && License::is_group_trailing_comma(tok)
            {
                self.emit_comments_without_token(tok);
                continue;
            }
            self.visit_element(&child).await;
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
