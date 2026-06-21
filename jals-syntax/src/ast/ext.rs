//! Hand-written accessors that don't fit the generated forms.
//!
//! `generated.rs` covers the four mechanical accessor shapes driven by
//! `java.ungram` labels; everything that needs bespoke tree-walking (token
//! text, positional selection among same-typed children, parameterized
//! queries) lives here. Both halves together form the public `ast` API.

use rowan::ast::support;

use super::{
    AssignmentExpr, AstNode, BinaryExpr, CatchClause, ExportsDirective, Expr, FieldAccess,
    FieldDecl, Literal, LocalVarDecl, Modifiers, NameRef, OpensDirective, ProvidesDirective,
    QualifiedName, Resource, Type, first_sig_token, non_trivia_text,
};
use crate::language::{SyntaxNode, SyntaxToken};
use crate::syntax_kind::SyntaxKind::{self, *};

/// The directly-declared name tokens (`IDENT` children) of `node`, in source order. The type of a
/// declaration is a nested `TYPE` node, so its identifiers are not direct children; an unnamed `_`
/// binding is an `UNDERSCORE` token and is likewise excluded.
fn ident_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|it| it.into_token())
        .filter(|t| t.kind() == IDENT)
}

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

impl LocalVarDecl {
    /// Every declared variable name token, in source order.
    ///
    /// A local declaration may bind several variables at once (`int a, b;`); the generated
    /// [`name`](LocalVarDecl::name) accessor only yields the first. Each name is a direct `IDENT`
    /// token child (the type is a nested `TYPE` node, so its identifiers are not included).
    /// An unnamed `_` binding is an `UNDERSCORE` token and is intentionally not reported here.
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> {
        ident_tokens(&self.syntax)
    }
}

impl FieldDecl {
    /// Every declared field name token, in source order (`int a, b;` binds two).
    ///
    /// Like [`LocalVarDecl::names`]: each name is a direct `IDENT` token child, and an unnamed
    /// `_` binding is not reported.
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> {
        ident_tokens(&self.syntax)
    }
}

impl CatchClause {
    /// The caught exception's binding name token (the `IDENT` after the type(s)), if named.
    ///
    /// The catch types are nested `TYPE` nodes, so the only direct `IDENT` token is the binding.
    /// Returns `None` for an unnamed `_` binding (an `UNDERSCORE` token).
    pub fn binding(&self) -> Option<SyntaxToken> {
        ident_tokens(&self.syntax).next()
    }
}

impl Resource {
    /// The resource variable's binding name token (the `IDENT` after the type), if this resource
    /// declares a new variable.
    ///
    /// Returns `None` when the resource is an existing variable used directly (`try (existing)`,
    /// where the resource is a reference node, not a declaration) or an unnamed `_` binding.
    pub fn binding(&self) -> Option<SyntaxToken> {
        ident_tokens(&self.syntax).next()
    }
}

#[cfg(test)]
mod tests {
    use super::AstNode;
    use crate::ast::{CatchClause, FieldDecl, LocalVarDecl, Resource};
    use crate::parser::parse;

    /// Returns the first descendant of `src` that casts to `T`.
    fn first<T: AstNode<Language = crate::language::JavaLanguage>>(src: &str) -> T {
        parse(src)
            .syntax()
            .descendants()
            .find_map(T::cast)
            .expect("node present")
    }

    fn names_of(decl: impl Iterator<Item = crate::language::SyntaxToken>) -> Vec<String> {
        decl.map(|t| t.text().to_string()).collect()
    }

    #[test]
    fn local_var_names_collects_every_declarator() {
        let local: LocalVarDecl = first("class C { void m() { int a, b = c, d; } }");
        assert_eq!(names_of(local.names()), ["a", "b", "d"]);
    }

    #[test]
    fn field_names_collects_every_declarator() {
        let field: FieldDecl = first("class C { int x, y; }");
        assert_eq!(names_of(field.names()), ["x", "y"]);
    }

    #[test]
    fn local_var_underscore_is_not_a_name() {
        // `var _ = ...` binds nothing referenceable.
        let local: LocalVarDecl = first("class C { void m() { var _ = f(); } }");
        assert_eq!(names_of(local.names()), Vec::<String>::new());
    }

    #[test]
    fn catch_binding_skips_the_types() {
        let catch: CatchClause = first("class C { void m() { try { } catch (A | B e) { } } }");
        assert_eq!(
            catch.binding().map(|t| t.text().to_string()).as_deref(),
            Some("e")
        );
    }

    #[test]
    fn catch_binding_underscore_is_none() {
        let catch: CatchClause = first("class C { void m() { try { } catch (E _) { } } }");
        assert!(catch.binding().is_none());
    }

    #[test]
    fn resource_binding_is_the_declared_variable() {
        let resource: Resource = first("class C { void m() { try (var r = open()) { } } }");
        assert_eq!(
            resource.binding().map(|t| t.text().to_string()).as_deref(),
            Some("r")
        );
    }
}
