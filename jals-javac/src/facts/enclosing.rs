//! Which class holds an enclosing instance, and which type encloses it.
//!
//! Two halves of one question, and getting either wrong is a class that is well-formed and wrong.
//! [`Facts::in_static_context`](super::Facts::in_static_context) already says it in its own words:
//! give a class an enclosing instance it should not have and its constructor takes an argument the
//! `new` has nothing to pass; take one away from a class in an instance context and every uplevel
//! access it makes reads another instance's fields.
//!
//! Only one backend ever asked. The JVM lowering answered it in three arms — a nested class, a
//! local class, an anonymous class body — while the wasm one asked
//! [`is_inner_class`](super::Facts::is_inner_class), which is the first arm alone. Nothing
//! miscompiled *by that route*, because the layout and the creation site stayed consistent with
//! each other and simply never gave a local or anonymous class one. What it cost was elsewhere: an
//! uplevel field read from such a class reported the name as unresolved, and an uplevel *call*
//! pushed local 0 without checking whose `this` it was and produced a module the validator rejects.
//!
//! The type the answer names is an `ItemId` and nothing more. Each backend needs a name for it —
//! the JVM an internal name, wasm a struct index — and those are answers about a target.

use jals_hir::{DefKind, FileId, ItemId, ProjectIndex};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxKind, SyntaxNode};

use super::{FactError, Facts, Result};

impl Facts<'_> {
    /// Whether `node` declares a class that holds a reference to a lexically enclosing instance.
    ///
    /// Three arms, because Java writes the answer three different ways:
    ///
    /// 1. A class declared directly in another type's body says it with the `static` modifier,
    ///    written or not written on the declaration itself.
    /// 2. A **local** class, declared in a method body, cannot write `static` at all, so what
    ///    decides is where it sits.
    /// 3. An **anonymous** class body is decided the same way, with one extra condition: a
    ///    *qualified* creation (`outer.new Inner() {}`) already hands the class an enclosing
    ///    instance — the one its supertype's constructor needs — and there is a single such
    ///    parameter, so it cannot also carry the lexically enclosing one.
    ///
    /// The `DefKind::Class` test is the caller's precondition folded in rather than left outside:
    /// an interface, `@interface`, `enum`, and `record` are implicitly `static`, so only a class
    /// can hold one at all, and a caller that forgot would get arm 2's answer for a nested `enum`.
    pub(crate) fn holds_enclosing_instance(
        node: &SyntaxNode,
        item: ItemId,
        index: &ProjectIndex,
    ) -> bool {
        if !matches!(index.item(item).kind, DefKind::Class) {
            return false;
        }
        match node.kind() {
            SyntaxKind::CLASS_DECL if Self::is_nested(node) => Self::is_inner_class(node),
            SyntaxKind::CLASS_DECL => !Self::in_static_context(node),
            SyntaxKind::NEW_EXPR | SyntaxKind::ENUM_CONSTANT => {
                !Self::in_static_context(node)
                    && ast::NewExpr::cast(node.clone())
                        .is_none_or(|new| Self::new_qualifier(&new).is_none())
            }
            _ => false,
        }
    }

    /// Whether `node` is an anonymous class declaration — a body written at a creation site.
    ///
    /// Both forms. A `new` carrying a class body is the familiar one; an `enum` constant carrying
    /// one is the other, and JLS §8.9.3 says so in as many words: the constant is an instance of an
    /// anonymous subclass of the `enum`. Neither has a name, which is why the index keys both on a
    /// *position* rather than on an identifier.
    ///
    /// The two backends spelled this three ways between them — wasm counted the `enum` constant,
    /// the JVM's named predicate did not, and the JVM's enclosing-type walk listed the constant
    /// separately beside that predicate so that it covered both after all. Three spellings agreeing
    /// by construction is what this replaces. The keys coincide too, and not by luck: an `enum`
    /// constant's node begins at its own name token, so keying it by position and keying it by name
    /// name the same offset.
    pub(crate) fn is_anonymous_body(node: &SyntaxNode) -> bool {
        matches!(
            node.kind(),
            SyntaxKind::NEW_EXPR | SyntaxKind::ENUM_CONSTANT
        ) && node
            .children()
            .any(|child| child.kind() == SyntaxKind::CLASS_BODY)
    }

    /// The indexed type that lexically encloses `node` — the nearest one, not the parent's parent.
    ///
    /// A local class's parent chain runs through a block and a method; an anonymous class's runs
    /// through whatever expression created it. Only for a class declared directly in another's body
    /// do the two agree, which is exactly the case the shorter walk was written against.
    ///
    /// An anonymous body is *in* the search rather than walked past. A class declared inside
    /// `new Object() { class Local {} }` is `Local` of the anonymous class, not of the file's outer
    /// one; walking past it names the outer type, so `Local`'s constructor takes an `Outer` while
    /// every `new Local()` inside the body pushes the anonymous `this` — a class file the JVM
    /// refuses at the first creation. An `enum` constant's body is in it for the same reason.
    ///
    /// The item is found by *position* for a body with no name and by the name token otherwise,
    /// which is how each was indexed.
    pub(crate) fn enclosing_type_of(
        node: &SyntaxNode,
        file: FileId,
        index: &ProjectIndex,
    ) -> Result<ItemId> {
        let declaration = node
            .ancestors()
            .skip(1)
            .find(|ancestor| {
                matches!(
                    ancestor.kind(),
                    SyntaxKind::CLASS_DECL
                        | SyntaxKind::INTERFACE_DECL
                        | SyntaxKind::ENUM_DECL
                        | SyntaxKind::ANNOTATION_TYPE_DECL
                        | SyntaxKind::RECORD_DECL
                        | SyntaxKind::ENUM_CONSTANT
                ) || Self::is_anonymous_body(ancestor)
            })
            .ok_or(FactError::Unsupported(
                "an inner class with no enclosing type",
            ))?;
        // Keyed by position for a `new`'s body, which has no name to key on.
        //
        // **Not** for an `enum` constant's, even though [`is_anonymous_body`](Self::is_anonymous_body)
        // counts that as one too. The two questions genuinely differ here, and the difference was
        // measured rather than assumed: a constant's body *is* an anonymous subclass and the index
        // does hold an item at its position, but nothing downstream names that item as an enclosing
        // type. Letting the positional key through compiles `enum E { A { class Inner {} }; }` into
        // a class called `E$Inner` where javac writes `E$1$Inner` — output the JVM loads and runs,
        // and which says the wrong thing about what encloses what. So it falls through to the name
        // branch, where `ast::Decl` has no `EnumConstant` variant and the report says exactly that.
        if declaration.kind() == SyntaxKind::NEW_EXPR && Self::is_anonymous_body(&declaration) {
            return index
                .item_by_decl(file, usize::from(declaration.text_range().start()))
                .ok_or(FactError::Unsupported(
                    "an anonymous enclosing class that is not indexed",
                ));
        }
        let name = ast::Decl::name_token_of(&declaration)
            .ok_or(FactError::Unsupported("an enclosing type with no name"))?;
        index
            .item_by_decl(file, usize::from(name.text_range().start()))
            .ok_or_else(|| FactError::Unresolved(name.text().into()))
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned as _;
    use alloc::string::{String, ToString as _};
    use alloc::vec::Vec;

    use jals_exec::block_on_inline;
    use jals_hir::{FileAnalysis, FileId, ProjectIndex};
    use jals_syntax::SyntaxKind;
    use jals_syntax::ast;

    use super::Facts;

    /// Every class-shaped declaration in `source`, with whether it holds an enclosing instance and
    /// which type encloses it.
    fn declarations(source: &str) -> Vec<(String, bool, String)> {
        let root = block_on_inline(jals_syntax::Parse::parse(source)).syntax();
        let index = block_on_inline(
            ProjectIndex::builder(&[(FileId(0), root.clone())])
                .with_stdlib()
                .build(),
        );
        let mut out = Vec::new();
        for node in root.descendants() {
            let keyed_at = match node.kind() {
                SyntaxKind::CLASS_DECL => ast::Decl::name_token_of(&node)
                    .map(|name| usize::from(name.text_range().start())),
                _ if Facts::is_anonymous_body(&node) => {
                    Some(usize::from(node.text_range().start()))
                }
                _ => None,
            };
            let Some(item) = keyed_at.and_then(|at| index.item_by_decl(FileId(0), at)) else {
                continue;
            };
            let enclosing = Facts::enclosing_type_of(&node, FileId(0), &index)
                .map_or_else(|_| "<none>".to_owned(), |id| index.item(id).fqn.to_string());
            out.push((
                index.item(item).fqn.to_string(),
                Facts::holds_enclosing_instance(&node, item, &index),
                enclosing,
            ));
        }
        // The analysis is not needed for any of this, and building it would only slow the test.
        let _ = block_on_inline(FileAnalysis::of(&root));
        out
    }

    /// Whether a class holds an enclosing instance is decided three different ways, and only the
    /// first is the `static` modifier.
    ///
    /// A nested class says it by writing `static` or not. A local class cannot write it, so where
    /// it sits decides. An anonymous body is the same, minus the case where a qualified creation
    /// has already spoken for the single enclosing-instance parameter.
    ///
    /// One backend implemented the first arm only. Getting arm 2 or 3 wrong in the *other*
    /// direction — handing an enclosing instance to a class in a `static` context — is the failure
    /// `in_static_context`'s own doc describes: a constructor with a parameter the `new` has
    /// nothing to pass.
    #[test]
    fn an_enclosing_instance_is_decided_three_ways_and_only_one_is_the_static_modifier() {
        let seen = declarations(
            "class Outer {
                 class Inner {}                          // nested, not static
                 static class Nested {}                  // nested, static
                 void instanceMethod() {
                     class LocalInInstance {}            // local, instance context
                     Object anon = new Object() {};      // anonymous, instance context
                 }
                 static void staticMethod() {
                     class LocalInStatic {}              // local, static context
                     Object anon = new Object() {};      // anonymous, static context
                 }
             }",
        );
        let holds: Vec<(String, bool)> = seen
            .iter()
            .map(|(name, holds, _)| (name.clone(), *holds))
            .collect();
        assert_eq!(
            holds,
            [
                ("Outer".to_owned(), false),
                ("Outer.Inner".to_owned(), true),
                ("Outer.Nested".to_owned(), false),
                ("Outer.LocalInInstance".to_owned(), true),
                ("Outer.1".to_owned(), true),
                ("Outer.LocalInStatic".to_owned(), false),
                ("Outer.2".to_owned(), false),
            ],
            "actual: {seen:#?}"
        );
    }

    /// The enclosing type is the *nearest* one, and an anonymous body is one of them.
    ///
    /// A class declared inside `new Object() { class Local {} }` is `Local` of the anonymous class.
    /// A walk that looked at the parent's parent, or that skipped anonymous bodies, names the outer
    /// class instead — and then `Local`'s constructor takes an `Outer` while every `new Local()`
    /// inside the body pushes the anonymous `this`.
    #[test]
    fn the_enclosing_type_is_the_nearest_one_and_an_anonymous_body_counts() {
        let seen = declarations(
            "class Outer {
                 void m() {
                     Object o = new Object() {
                         class Local {}
                     };
                 }
             }",
        );
        let inside: Vec<&(String, bool, String)> = seen
            .iter()
            .filter(|(name, _, _)| name.ends_with("Local"))
            .collect();
        assert_eq!(inside.len(), 1, "the local class is indexed: {seen:#?}");
        assert!(
            inside[0].2.ends_with(".1"),
            "the anonymous body encloses it, not `Outer`: {:?}",
            inside[0]
        );
    }
}
