//! A typed AST layer over the untyped `rowan` CST.
//!
//! Each grammar node gets a newtype wrapper that implements [`AstNode`]. The wrappers are
//! zero-cost views into the green tree: casting is a kind check, and accessors walk children
//! lazily via `rowan`'s `support` helpers. `jals-fmt` / `jals-lint` / `jals-lsp` read the tree
//! through this layer instead of matching on raw [`SyntaxKind`]s.
//!
//! The layer is **generated**: `jals-syntax/java.ungram` describes the grammar, and
//! `cargo run -p xtask -- codegen` renders `generated.rs` from it (the file is committed and
//! CI verifies it is up to date). Only labeled grammar elements produce accessors; bespoke
//! accessors that don't fit the generated forms are hand-written in `ext.rs`. Accessors are
//! intentionally permissive — they return `Option`/iterators and never panic — because the
//! parser is error-resilient and may produce incomplete nodes.
//!
//! Three flavors of wrapper appear here:
//! - **Node wrappers** (e.g. [`ClassDecl`]): one struct per `SyntaxKind` node.
//! - **Enums** (e.g. [`Decl`], [`Stmt`], [`Expr`]): a sum over related node kinds, so a
//!   caller can match on "any statement" without knowing the concrete kind.
//! - **Tokens** ([`NameRef`], modifier/operator queries): typed access to significant tokens.
//!
//! [`SyntaxKind`]: crate::syntax_kind::SyntaxKind

mod ext;
mod generated;

use alloc::borrow::ToOwned;
use alloc::string::String;

pub use rowan::ast::{AstChildren, AstNode, AstPtr, SyntaxNodePtr};

pub use generated::*;

use crate::language::{SyntaxNode, SyntaxToken};
use crate::syntax_kind::SyntaxKind::IDENT;

// ===== Shared accessor helpers =====

/// Namespace for the shared typed-AST accessor helpers that walk a node's tokens.
struct AstSupport;

impl AstSupport {
    /// Returns the first significant token (non-trivia) of `node`, if any.
    fn first_sig_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| !t.kind().is_trivia())
    }

    /// Concatenates the text of all non-trivia tokens beneath `node` (drops whitespace/comments).
    fn non_trivia_text(node: &SyntaxNode) -> String {
        node.descendants_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| !t.kind().is_trivia())
            .map(|t| t.text().to_owned())
            .collect()
    }

    /// Returns the name (`IDENT`) declared directly under `node` (e.g. the type/method name).
    fn name_text(node: &SyntaxNode) -> Option<String> {
        node.children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == IDENT)
            .map(|t| t.text().to_owned())
    }

    /// The directly-declared name tokens (`IDENT` children) of `node`, in source order. The type of a
    /// declaration is a nested `TYPE` node, so its identifiers are not direct children; an unnamed `_`
    /// binding is an `UNDERSCORE` token and is likewise excluded.
    fn ident_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
        node.children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| t.kind() == IDENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parse;
    use crate::syntax_kind::SyntaxKind::{FINAL_KW, PUBLIC_KW};

    /// Casts the parsed root to a [`SourceFile`].
    fn source_file(src: &str) -> SourceFile {
        SourceFile::cast(jals_exec::block_on_inline(Parse::parse(src)).syntax())
            .expect("root is SOURCE_FILE")
    }

    /// Parses `class C { void m() { <body> } }` and returns the statements of `m`.
    fn method_stmts(body: &str) -> Vec<Stmt> {
        let src = format!("class C {{ void m(Object o) {{ {body} }} }}");
        let file = source_file(&src);
        let Decl::Class(class) = file.decls().next().unwrap() else {
            panic!("expected class");
        };
        let Member::Method(method) = class.body().unwrap().members().next().unwrap() else {
            panic!("expected method");
        };
        method.body().unwrap().stmts().collect()
    }

    /// Returns the single expression of the first expression statement in `body`.
    fn first_expr(body: &str) -> Expr {
        match method_stmts(body).into_iter().next().unwrap() {
            Stmt::Expr(es) => es.expr().unwrap(),
            other => panic!("expected expression statement, got {other:?}"),
        }
    }

    #[test]
    fn package_and_imports() {
        let file = source_file(
            "package a.b.c;\nimport java.util.List;\nimport static a.B.c;\nimport a.b.*;\n",
        );
        assert_eq!(file.package().unwrap().name().unwrap().text(), "a.b.c");

        let imports: Vec<_> = file.imports().collect();
        assert_eq!(imports.len(), 3);
        assert!(!imports[0].is_static());
        assert_eq!(imports[0].name().unwrap().text(), "java.util.List");
        assert!(imports[1].is_static());
        assert!(imports[2].name().unwrap().is_wildcard());
        assert_eq!(imports[2].name().unwrap().text(), "a.b.*");
    }

    #[test]
    fn class_shape() {
        let file = source_file(
            "public final class Foo<T> extends Bar implements I, J { private int x = 1; void m(int a) { return; } }",
        );
        let decl = file.decls().next().unwrap();
        let Decl::Class(class) = decl else {
            panic!("expected class");
        };
        assert_eq!(class.name().as_deref(), Some("Foo"));

        let mods = class.modifiers().unwrap();
        assert!(mods.has(PUBLIC_KW));
        assert!(mods.has(FINAL_KW));
        assert!(!mods.is_sealed());

        let tps: Vec<_> = class.type_params().unwrap().params().collect();
        assert_eq!(tps.len(), 1);
        assert_eq!(tps[0].name().as_deref(), Some("T"));

        assert_eq!(class.extends_clause().unwrap().types().count(), 1);
        assert_eq!(class.implements_clause().unwrap().types().count(), 2);

        let members: Vec<_> = class.body().unwrap().members().collect();
        assert_eq!(members.len(), 2);
        let Member::Field(field) = &members[0] else {
            panic!("expected field");
        };
        assert_eq!(field.name().as_deref(), Some("x"));
        assert_eq!(field.ty().unwrap().text(), "int");
        let Member::Method(method) = &members[1] else {
            panic!("expected method");
        };
        assert_eq!(method.name().as_deref(), Some("m"));
        assert_eq!(method.params().unwrap().params().count(), 1);
        assert!(method.body().is_some());
    }

    #[test]
    fn sealed_modifiers() {
        let file = source_file("public sealed interface S permits A, B { }");
        let Decl::Interface(iface) = file.decls().next().unwrap() else {
            panic!("expected interface");
        };
        assert!(iface.modifiers().unwrap().is_sealed());
        assert_eq!(iface.permits_clause().unwrap().types().count(), 2);

        let file2 = source_file("non-sealed class C { }");
        let Decl::Class(class) = file2.decls().next().unwrap() else {
            panic!("expected class");
        };
        assert!(class.modifiers().unwrap().is_non_sealed());
    }

    #[test]
    fn record_shape() {
        let file = source_file("record Point(int x, int y) implements Shape { }");
        let Decl::Record(record) = file.decls().next().unwrap() else {
            panic!("expected record");
        };
        assert_eq!(record.name().as_deref(), Some("Point"));
        let comps: Vec<_> = record.header().unwrap().components().collect();
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].name().as_deref(), Some("x"));
        assert_eq!(comps[1].name().as_deref(), Some("y"));
        assert_eq!(record.implements_clause().unwrap().types().count(), 1);
    }

    #[test]
    fn enum_shape() {
        let file = source_file("enum E { A, B(1), C; int x; }");
        let Decl::Enum(en) = file.decls().next().unwrap() else {
            panic!("expected enum");
        };
        let body = en.body().unwrap();
        let constants: Vec<_> = body.constants().collect();
        assert_eq!(constants.len(), 3);
        assert_eq!(constants[0].name().as_deref(), Some("A"));
        assert!(constants[1].args().is_some());
        assert_eq!(body.members().count(), 1);
    }

    #[test]
    fn statements_and_exprs() {
        let file =
            source_file("class C { void m() { int a = 1 + 2; if (a) return; while (a) m(); } }");
        let Decl::Class(class) = file.decls().next().unwrap() else {
            panic!("expected class");
        };
        let Member::Method(method) = class.body().unwrap().members().next().unwrap() else {
            panic!("expected method");
        };
        let stmts: Vec<_> = method.body().unwrap().stmts().collect();
        assert_eq!(stmts.len(), 3);

        let Stmt::LocalVar(local) = &stmts[0] else {
            panic!("expected local var");
        };
        assert_eq!(local.name().as_deref(), Some("a"));
        assert_eq!(local.ty().unwrap().text(), "int");
        let Some(Expr::Binary(bin)) = local.value() else {
            panic!("expected binary initializer");
        };
        assert!(matches!(bin.lhs(), Some(Expr::Literal(_))));
        assert!(matches!(bin.rhs(), Some(Expr::Literal(_))));

        assert!(matches!(&stmts[1], Stmt::If(_)));
        assert!(matches!(&stmts[2], Stmt::While(_)));
    }

    #[test]
    fn switch_and_patterns() {
        let file = source_file(
            "class C { void m(Object o) { switch (o) { case Integer i when i > 0 -> f(); default -> g(); } } }",
        );
        let Decl::Class(class) = file.decls().next().unwrap() else {
            panic!("expected class");
        };
        let Member::Method(method) = class.body().unwrap().members().next().unwrap() else {
            panic!("expected method");
        };
        let Stmt::Switch(switch) = method.body().unwrap().stmts().next().unwrap() else {
            panic!("expected switch");
        };
        assert!(matches!(switch.selector(), Some(Expr::NameRef(_))));
        let rules: Vec<_> = switch.body().unwrap().rules().collect();
        assert_eq!(rules.len(), 2);
        let first_label = rules[0].label().unwrap();
        assert!(!first_label.is_default());
        assert!(rules[1].label().unwrap().is_default());
    }

    #[test]
    fn if_branches_and_condition() {
        let Stmt::If(if_stmt) = method_stmts("if (o) f(); else g();")
            .into_iter()
            .next()
            .unwrap()
        else {
            panic!("expected if");
        };
        assert!(matches!(if_stmt.condition(), Some(Expr::NameRef(_))));
        // Both the then- and else-statements are `Stmt` children.
        assert_eq!(if_stmt.branches().count(), 2);
    }

    #[test]
    fn for_each_separates_iterable_and_body() {
        let Stmt::ForEach(fe) = method_stmts("for (String s : list) use(s);")
            .into_iter()
            .next()
            .unwrap()
        else {
            panic!("expected for-each");
        };
        assert_eq!(fe.ty().unwrap().text(), "String");
        assert_eq!(fe.name().as_deref(), Some("s"));
        // The iterable is the `Expr` child; the body is a `Stmt` child — they must not collide.
        assert!(matches!(fe.iterable(), Some(Expr::NameRef(_))));
        assert!(matches!(fe.body(), Some(Stmt::Expr(_))));
    }

    #[test]
    fn cast_splits_type_and_operand() {
        let Expr::Assignment(assign) = first_expr("x = (String) o;") else {
            panic!("expected assignment");
        };
        let Some(Expr::Cast(cast)) = assign.value() else {
            panic!("expected cast value");
        };
        assert_eq!(cast.ty().unwrap().text(), "String");
        assert!(matches!(cast.expr(), Some(Expr::NameRef(_))));
    }

    #[test]
    fn call_callee_and_args() {
        let Expr::Call(call) = first_expr("f(a, b, c);") else {
            panic!("expected call");
        };
        assert!(matches!(call.callee(), Some(Expr::NameRef(_))));
        assert_eq!(call.args().unwrap().args().count(), 3);
    }

    #[test]
    fn lambda_body_forms() {
        // Expression-bodied lambda.
        let Expr::Call(call) = first_expr("f(x -> x);") else {
            panic!("expected call");
        };
        let Some(Expr::Lambda(lambda)) = call.args().unwrap().args().next() else {
            panic!("expected lambda arg");
        };
        assert_eq!(lambda.params().unwrap().params().count(), 1);
        assert!(lambda.expr_body().is_some());
        assert!(lambda.block_body().is_none());

        // Block-bodied lambda.
        let Expr::Call(call2) = first_expr("g(() -> { return 0; });") else {
            panic!("expected call");
        };
        let Some(Expr::Lambda(lambda2)) = call2.args().unwrap().args().next() else {
            panic!("expected lambda arg");
        };
        assert!(lambda2.expr_body().is_none());
        assert!(lambda2.block_body().is_some());
    }

    #[test]
    fn try_with_resources_parts() {
        let Stmt::Try(try_stmt) =
            method_stmts("try (var r = open()) { use(r); } catch (E e) { } finally { }")
                .into_iter()
                .next()
                .unwrap()
        else {
            panic!("expected try");
        };
        assert_eq!(try_stmt.resources().unwrap().resources().count(), 1);
        assert!(try_stmt.block().is_some());
        assert_eq!(try_stmt.catches().count(), 1);
        assert!(try_stmt.finally().is_some());
        assert_eq!(try_stmt.catches().next().unwrap().ty().unwrap().text(), "E");
    }

    #[test]
    fn field_access_chain() {
        let Expr::FieldAccess(fa) = first_expr("a.b.c;") else {
            panic!("expected field access");
        };
        // The outermost access is `.c`; its receiver is `a.b`.
        assert_eq!(fa.field().as_deref(), Some("c"));
        assert!(matches!(fa.receiver(), Some(Expr::FieldAccess(_))));
    }

    #[test]
    fn ast_is_lossless_view() {
        // The typed layer is a pure view: the underlying node text still equals the source.
        let src = "class C<T> { List<T> xs; void m() { for (var x : xs) sum += x; } }";
        let file = source_file(src);
        assert_eq!(file.syntax().text().to_string(), src);
    }
}
