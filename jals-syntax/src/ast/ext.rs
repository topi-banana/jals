//! Hand-written accessors that don't fit the generated forms.
//!
//! `generated.rs` covers the four mechanical accessor shapes driven by
//! `java.ungram` labels; everything that needs bespoke tree-walking (token
//! text, positional selection among same-typed children, parameterized
//! queries) lives here. Both halves together form the public `ast` API.

use rowan::ast::support;

use super::{
    AssignmentExpr, AstNode, BinaryExpr, ExportsDirective, Expr, FieldAccess, Literal, Modifiers,
    NameRef, OpensDirective, ProvidesDirective, QualifiedName, Type, first_sig_token,
    non_trivia_text,
};
use crate::language::SyntaxToken;
use crate::syntax_kind::SyntaxKind::{self, *};

impl QualifiedName {
    /// The full dotted text as written (without surrounding trivia), e.g. `a.b.c` or `a.b.*`.
    pub fn text(&self) -> String {
        non_trivia_text(&self.syntax)
    }
}

impl ExportsDirective {
    /// The target modules of a qualified `exports ... to ...`, if any.
    pub fn to_modules(&self) -> impl Iterator<Item = QualifiedName> {
        self.syntax
            .children()
            .filter_map(QualifiedName::cast)
            .skip(1)
    }
}

impl OpensDirective {
    /// The target modules of a qualified `opens ... to ...`, if any.
    pub fn to_modules(&self) -> impl Iterator<Item = QualifiedName> {
        self.syntax
            .children()
            .filter_map(QualifiedName::cast)
            .skip(1)
    }
}

impl ProvidesDirective {
    /// The implementation types listed after `with`.
    pub fn providers(&self) -> impl Iterator<Item = QualifiedName> {
        self.syntax
            .children()
            .filter_map(QualifiedName::cast)
            .skip(1)
    }
}

impl Modifiers {
    /// Whether a plain keyword modifier `kind` (e.g. `PUBLIC_KW`) is present.
    pub fn has(&self, kind: SyntaxKind) -> bool {
        support::token(&self.syntax, kind).is_some()
    }

    /// Whether the `non-sealed` modifier is present.
    pub fn is_non_sealed(&self) -> bool {
        self.syntax.children().any(|n| n.kind() == NON_SEALED_KW)
    }
}

impl Type {
    /// The type text with surrounding/interleaved trivia removed (e.g. `List<T>`).
    ///
    /// Use [`AstNode::syntax`]`().text()` if you need the verbatim slice including trivia.
    pub fn text(&self) -> String {
        non_trivia_text(&self.syntax)
    }
}

impl Literal {
    /// The literal token.
    pub fn token(&self) -> Option<SyntaxToken> {
        first_sig_token(&self.syntax)
    }

    /// The literal text as written.
    pub fn text(&self) -> Option<String> {
        self.token().map(|t| t.text().to_string())
    }
}

impl NameRef {
    /// The referenced name text.
    pub fn text(&self) -> Option<String> {
        first_sig_token(&self.syntax).map(|t| t.text().to_string())
    }
}

impl BinaryExpr {
    /// The left-hand operand.
    pub fn lhs(&self) -> Option<Expr> {
        self.operands().next()
    }

    /// The right-hand operand (absent for `instanceof`, whose RHS is a type/pattern).
    pub fn rhs(&self) -> Option<Expr> {
        self.operands().nth(1)
    }
}

impl FieldAccess {
    /// The accessed field/member name (the `IDENT` after the dot).
    pub fn field(&self) -> Option<String> {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .filter(|t| t.kind() == IDENT)
            .last()
            .map(|t| t.text().to_string())
    }
}

impl AssignmentExpr {
    /// The assignment target (the first operand).
    pub fn target(&self) -> Option<Expr> {
        self.parts().next()
    }

    /// The assigned value (the second operand).
    pub fn value(&self) -> Option<Expr> {
        self.parts().nth(1)
    }
}
