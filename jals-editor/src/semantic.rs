//! Protocol-neutral semantic-token classification over the lossless CST, refined by name
//! resolution.
//!
//! Identifiers are classified first from `jals-hir`'s file-local resolution
//! ([`jals_hir::Resolved::resolve_node`]): a resolved reference takes the kind of the binding it
//! names (a field is [`Property`](SemanticTokenKind::Property), a parameter
//! [`Parameter`](SemanticTokenKind::Parameter), a sibling type
//! [`Class`](SemanticTokenKind::Class)/[`Enum`](SemanticTokenKind::Enum)/…), and a declaring name
//! takes its own kind plus the `declaration` flag. A type name the file-local pass could not
//! place — an imported or same-package sibling declared in another file — is resolved against
//! the project index when one is supplied, so it too is classified by its declared kind rather
//! than the generic [`Type`](SemanticTokenKind::Type). Everything still unplaced — an external
//! (JDK) type, an inherited member, a member-access right-hand name, a qualified/annotation
//! name — falls back to a purely syntactic classification from the token's [`SyntaxKind`] and its
//! parent (sometimes grandparent) node. The fallback is what classified *every* identifier before
//! resolution was wired in, so it never regresses; resolution only sharpens what it can place.
//!
//! Hosts own only the wire format: the LSP's legend indices, UTF-16 delta encoding, and
//! multi-line splitting (or Monaco's equivalents).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Range;

use jals_hir::{DefKind, FileId, Namespace, ProjectIndex, TypeResolution};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// What a classified token *is* — the protocol-neutral vocabulary hosts map to their legend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticTokenKind {
    /// A package/module segment of a dotted name.
    Namespace,
    /// A type reference resolution could not sharpen.
    Type,
    /// A class or record.
    Class,
    /// An enum type.
    Enum,
    /// An interface or annotation type.
    Interface,
    /// A type parameter (`<T>`).
    TypeParameter,
    /// A method/constructor/lambda parameter.
    Parameter,
    /// A local variable (or catch/resource/pattern binding).
    Variable,
    /// A field.
    Property,
    /// An enum constant.
    EnumMember,
    /// A method or constructor.
    Method,
    /// A keyword (reserved, literal, or contextual).
    Keyword,
    /// A line/block/doc comment.
    Comment,
    /// A string, text block, or char literal.
    String,
    /// An integer or float literal.
    Number,
    /// An annotation name (after `@`).
    Decorator,
}

/// One classified token: its byte range, kind, and whether it declares the entity it names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticToken {
    /// The token's byte range (may span lines for a block comment / text block — hosts whose
    /// protocol forbids multi-line tokens split it).
    pub range: Range<usize>,
    /// The classification.
    pub kind: SemanticTokenKind,
    /// Whether this token is the declaring name of the entity (the LSP `declaration` modifier).
    pub declaration: bool,
}

/// Classifies a file's significant tokens.
pub struct SemanticTokens;

impl SemanticTokens {
    /// Classify every significant token under `root`, in document order. Whitespace, operators,
    /// delimiters, and unclassifiable identifiers are skipped. Async because the resolution
    /// pass ([`jals_hir::Resolved::resolve_node`]) yields cooperatively.
    pub async fn classify(
        root: &SyntaxNode,
        project: Option<(&ProjectIndex, FileId)>,
    ) -> Vec<SemanticToken> {
        let by_start = Self::resolution_classes(root, project).await;
        root.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter_map(|token| {
                let (kind, declaration) = Self::classify_token(&token, &by_start)?;
                Some(SemanticToken {
                    range: crate::byte_range(token.text_range()),
                    kind,
                    declaration,
                })
            })
            .collect()
    }

    /// A map from a token's start byte offset to its resolution-derived `(kind, declaration)`,
    /// built once per request from `jals-hir`'s file-local name resolution.
    ///
    /// Equivalent to calling [`jals_hir::Resolved::symbol_at`] for every identifier, but
    /// precomputed in one pass (O(references + defs)) so each token is an O(log n) lookup instead
    /// of a linear scan. A resolved reference maps to the kind of the binding it names (no
    /// `declaration` flag); a declaring name maps to its own kind plus the flag. References are
    /// inserted first and declarations only fill gaps (`symbol_at` treats a covering reference as
    /// authoritative), though in practice a reference and a declaring name never share a start
    /// offset. A reference the file-local pass left unresolved is placed only when `project`
    /// binds it to a cross-file type (by its declared kind); any other unresolved reference is
    /// omitted, leaving its token to the syntactic fallback.
    async fn resolution_classes(
        root: &SyntaxNode,
        project: Option<(&ProjectIndex, FileId)>,
    ) -> BTreeMap<usize, (SemanticTokenKind, bool)> {
        let resolved = jals_hir::Resolved::resolve_node(root).await;
        let mut by_start: BTreeMap<usize, (SemanticTokenKind, bool)> = BTreeMap::new();
        for reference in &resolved.references {
            if let Some(id) = reference.resolution.def_id() {
                by_start.insert(
                    reference.range.start,
                    (Self::kind_for(resolved.def(id).kind), false),
                );
            } else if let Some((index, file)) = project
                && reference.namespace == Namespace::Type
                && let TypeResolution::Project(item) = index.resolve_reference(file, reference)
            {
                // A cross-file type the file-local pass could not place: classify by the indexed
                // declaration's kind, sharper than the syntactic fallback's generic `Type`.
                by_start.insert(
                    reference.range.start,
                    (Self::kind_for(index.item(item).kind), false),
                );
            }
        }
        for def in &resolved.defs {
            by_start
                .entry(def.name_range.start)
                .or_insert_with(|| (Self::kind_for(def.kind), true));
        }
        by_start
    }

    /// The token kind for a resolved binding's [`DefKind`]. Mirrors the declaration-site mapping
    /// in [`Self::classify_ident_syntactic`], so a declaration classifies the same whether it is
    /// placed by resolution or syntax.
    const fn kind_for(kind: DefKind) -> SemanticTokenKind {
        match kind {
            DefKind::Class | DefKind::Record => SemanticTokenKind::Class,
            DefKind::Interface | DefKind::AnnotationType => SemanticTokenKind::Interface,
            DefKind::Enum => SemanticTokenKind::Enum,
            DefKind::TypeParam => SemanticTokenKind::TypeParameter,
            DefKind::Param | DefKind::LambdaParam => SemanticTokenKind::Parameter,
            DefKind::Field => SemanticTokenKind::Property,
            DefKind::EnumConstant => SemanticTokenKind::EnumMember,
            DefKind::Method | DefKind::Constructor => SemanticTokenKind::Method,
            DefKind::Local | DefKind::CatchParam | DefKind::Resource | DefKind::PatternVar => {
                SemanticTokenKind::Variable
            }
        }
    }

    /// Classify a single token into a `(kind, declaration)` pair, or `None` to skip it
    /// (whitespace/newlines, operators, delimiters, and unclassifiable identifiers).
    ///
    /// An identifier is taken from `by_start` (name resolution) when present, otherwise
    /// classified syntactically.
    fn classify_token(
        token: &SyntaxToken,
        by_start: &BTreeMap<usize, (SemanticTokenKind, bool)>,
    ) -> Option<(SemanticTokenKind, bool)> {
        use SyntaxKind::{
            BLOCK_COMMENT, CHAR_LITERAL, DOC_COMMENT, FLOAT_LITERAL, IDENT, INT_LITERAL,
            LINE_COMMENT, NEWLINE, STRING_LITERAL, TEXT_BLOCK, UNDERSCORE, WHITESPACE,
        };
        match token.kind() {
            WHITESPACE | NEWLINE => None,
            LINE_COMMENT | BLOCK_COMMENT | DOC_COMMENT => Some((SemanticTokenKind::Comment, false)),
            STRING_LITERAL | TEXT_BLOCK | CHAR_LITERAL => Some((SemanticTokenKind::String, false)),
            INT_LITERAL | FLOAT_LITERAL => Some((SemanticTokenKind::Number, false)),
            IDENT | UNDERSCORE => {
                let start = usize::from(token.text_range().start());
                by_start
                    .get(&start)
                    .copied()
                    .or_else(|| Self::classify_ident_syntactic(token))
            }
            k if Self::is_keyword(k) => Some((SemanticTokenKind::Keyword, false)),
            _ => None,
        }
    }

    /// Classify an identifier from the kind of its parent node, falling back to grandparent
    /// context to distinguish a method call from a plain name/field access. The syntactic
    /// fallback for identifiers name resolution cannot place.
    fn classify_ident_syntactic(token: &SyntaxToken) -> Option<(SemanticTokenKind, bool)> {
        use SyntaxKind::{
            ANNOTATION, ANNOTATION_PAIR, ANNOTATION_TYPE_DECL, CALL_EXPR, CATCH_CLAUSE, CLASS_DECL,
            CONSTRUCTOR_DECL, ENUM_CONSTANT, ENUM_DECL, FIELD_ACCESS, FIELD_DECL, INTERFACE_DECL,
            LOCAL_VAR_DECL, METHOD_DECL, METHOD_REF_EXPR, NAME_REF, NON_SEALED_KW, PARAM,
            QUALIFIED_NAME, RECORD_COMPONENT, RECORD_DECL, RESOURCE, TYPE, TYPE_PARAM,
            TYPE_PATTERN,
        };
        let parent = token.parent()?;
        let grandparent = || parent.parent().map(|n| n.kind());
        // Each arm names a distinct syntactic context; keeping the equal-bodied arms separate
        // (rather than merging them) is what documents which context maps to which token kind.
        #[allow(clippy::match_same_arms)]
        let classified = match parent.kind() {
            // Declaration sites: the identifier names the entity being declared.
            CLASS_DECL | RECORD_DECL => (SemanticTokenKind::Class, true),
            INTERFACE_DECL | ANNOTATION_TYPE_DECL => (SemanticTokenKind::Interface, true),
            ENUM_DECL => (SemanticTokenKind::Enum, true),
            METHOD_DECL | CONSTRUCTOR_DECL => (SemanticTokenKind::Method, true),
            TYPE_PARAM => (SemanticTokenKind::TypeParameter, true),
            PARAM | RECORD_COMPONENT => (SemanticTokenKind::Parameter, true),
            ENUM_CONSTANT => (SemanticTokenKind::EnumMember, true),
            FIELD_DECL => (SemanticTokenKind::Property, true),
            // Binding sites that introduce a local: `T x`, `var x = ..`, `catch (E e)`,
            // `o instanceof T p`, `case T p ->`.
            LOCAL_VAR_DECL | RESOURCE | CATCH_CLAUSE | TYPE_PATTERN => {
                (SemanticTokenKind::Variable, true)
            }
            // Reference sites.
            TYPE => (SemanticTokenKind::Type, false),
            METHOD_REF_EXPR => (SemanticTokenKind::Method, false),
            ANNOTATION_PAIR => (SemanticTokenKind::Property, false),
            // `non-sealed` is re-joined into a node wrapping `non` `-` `sealed`.
            NON_SEALED_KW => (SemanticTokenKind::Keyword, false),
            // A name / field is a method when it is the callee of a call expression.
            NAME_REF if grandparent() == Some(CALL_EXPR) => (SemanticTokenKind::Method, false),
            NAME_REF => (SemanticTokenKind::Variable, false),
            FIELD_ACCESS if grandparent() == Some(CALL_EXPR) => (SemanticTokenKind::Method, false),
            FIELD_ACCESS => (SemanticTokenKind::Property, false),
            // A dotted name: the annotation type after `@`, otherwise a package/module name.
            QUALIFIED_NAME if grandparent() == Some(ANNOTATION) => {
                (SemanticTokenKind::Decorator, false)
            }
            QUALIFIED_NAME => (SemanticTokenKind::Namespace, false),
            _ => return None,
        };
        Some(classified)
    }

    /// Whether `kind` is a keyword token — the reserved 50, the literal keywords
    /// (`true`/`false`/`null`), and the contextual keywords the parser promotes from `IDENT`.
    const fn is_keyword(kind: SyntaxKind) -> bool {
        use SyntaxKind::{
            ABSTRACT_KW, ASSERT_KW, BOOLEAN_KW, BREAK_KW, BYTE_KW, CASE_KW, CATCH_KW, CHAR_KW,
            CLASS_KW, CONST_KW, CONTINUE_KW, DEFAULT_KW, DO_KW, DOUBLE_KW, ELSE_KW, ENUM_KW,
            EXPORTS_KW, EXTENDS_KW, FALSE_KW, FINAL_KW, FINALLY_KW, FLOAT_KW, FOR_KW, GOTO_KW,
            IF_KW, IMPLEMENTS_KW, IMPORT_KW, INSTANCEOF_KW, INT_KW, INTERFACE_KW, LONG_KW,
            MODULE_KW, NATIVE_KW, NEW_KW, NULL_KW, OPEN_KW, OPENS_KW, PACKAGE_KW, PERMITS_KW,
            PRIVATE_KW, PROTECTED_KW, PROVIDES_KW, PUBLIC_KW, RECORD_KW, REQUIRES_KW, RETURN_KW,
            SEALED_KW, SHORT_KW, STATIC_KW, STRICTFP_KW, SUPER_KW, SWITCH_KW, SYNCHRONIZED_KW,
            THIS_KW, THROW_KW, THROWS_KW, TO_KW, TRANSIENT_KW, TRANSITIVE_KW, TRUE_KW, TRY_KW,
            USES_KW, VAR_KW, VOID_KW, VOLATILE_KW, WHEN_KW, WHILE_KW, WITH_KW, YIELD_KW,
        };
        matches!(
            kind,
            ABSTRACT_KW
                | ASSERT_KW
                | BOOLEAN_KW
                | BREAK_KW
                | BYTE_KW
                | CASE_KW
                | CATCH_KW
                | CHAR_KW
                | CLASS_KW
                | CONST_KW
                | CONTINUE_KW
                | DEFAULT_KW
                | DO_KW
                | DOUBLE_KW
                | ELSE_KW
                | ENUM_KW
                | EXTENDS_KW
                | FINAL_KW
                | FINALLY_KW
                | FLOAT_KW
                | FOR_KW
                | GOTO_KW
                | IF_KW
                | IMPLEMENTS_KW
                | IMPORT_KW
                | INSTANCEOF_KW
                | INT_KW
                | INTERFACE_KW
                | LONG_KW
                | NATIVE_KW
                | NEW_KW
                | PACKAGE_KW
                | PRIVATE_KW
                | PROTECTED_KW
                | PUBLIC_KW
                | RETURN_KW
                | SHORT_KW
                | STATIC_KW
                | STRICTFP_KW
                | SUPER_KW
                | SWITCH_KW
                | SYNCHRONIZED_KW
                | THIS_KW
                | THROW_KW
                | THROWS_KW
                | TRANSIENT_KW
                | TRY_KW
                | VOID_KW
                | VOLATILE_KW
                | WHILE_KW
                | TRUE_KW
                | FALSE_KW
                | NULL_KW
                | VAR_KW
                | YIELD_KW
                | RECORD_KW
                | SEALED_KW
                | PERMITS_KW
                | WHEN_KW
                | MODULE_KW
                | OPEN_KW
                | OPENS_KW
                | REQUIRES_KW
                | TRANSITIVE_KW
                | EXPORTS_KW
                | TO_KW
                | PROVIDES_KW
                | USES_KW
                | WITH_KW
        )
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;

    use super::*;

    /// Classify `text` with no project index.
    fn classify(text: &str) -> Vec<SemanticToken> {
        block_on_inline(async {
            SemanticTokens::classify(&jals_syntax::Parse::parse(text).await.syntax(), None).await
        })
    }

    /// The token starting at `needle`'s first occurrence.
    fn at(text: &str, needle: &str) -> SemanticToken {
        let start = text.find(needle).expect("needle present");
        classify(text)
            .into_iter()
            .find(|t| t.range.start == start)
            .unwrap_or_else(|| panic!("no token at offset {start} for {needle:?}"))
    }

    /// The token starting at `needle`'s *last* occurrence — a *use* of a name whose declaration
    /// appears earlier.
    fn at_last(text: &str, needle: &str) -> SemanticToken {
        let start = text.rfind(needle).expect("needle present");
        classify(text)
            .into_iter()
            .find(|t| t.range.start == start)
            .unwrap_or_else(|| panic!("no token at offset {start} for {needle:?}"))
    }

    #[test]
    fn declarations_are_classified() {
        let src = "class C<T> { int field; void m(int p) {} }";
        assert_eq!(at(src, "C").kind, SemanticTokenKind::Class);
        assert!(at(src, "C").declaration);
        assert_eq!(at(src, "T").kind, SemanticTokenKind::TypeParameter);
        assert_eq!(at(src, "field").kind, SemanticTokenKind::Property);
        assert_eq!(at(src, "m").kind, SemanticTokenKind::Method);
        assert_eq!(at(src, "p").kind, SemanticTokenKind::Parameter);
    }

    #[test]
    fn types_and_keywords() {
        let src = "class C { List x; }";
        assert_eq!(at(src, "class").kind, SemanticTokenKind::Keyword);
        assert_eq!(at(src, "List").kind, SemanticTokenKind::Type);
        // `int` is a primitive keyword, not a `Type`.
        assert_eq!(
            at("class C { int x; }", "int").kind,
            SemanticTokenKind::Keyword
        );
    }

    #[test]
    fn calls_vs_references_and_fields() {
        let src = "class C { void m() { foo(); obj.bar(); var x = obj.field; } }";
        assert_eq!(at(src, "foo").kind, SemanticTokenKind::Method);
        assert_eq!(at(src, "obj").kind, SemanticTokenKind::Variable);
        assert_eq!(at(src, "bar").kind, SemanticTokenKind::Method);
        assert_eq!(at(src, "field").kind, SemanticTokenKind::Property);
    }

    #[test]
    fn name_references_resolve_to_their_kind() {
        // A bare name reference is classified by what it resolves to, not a flat `Variable`.
        // Each `at_last` targets the *use*, not the earlier declaration of the same name.
        let field = at_last("class C { int f; int g() { return f; } }", "f");
        assert_eq!(field.kind, SemanticTokenKind::Property);
        assert!(
            !field.declaration,
            "a use must not carry the declaration flag"
        );

        assert_eq!(
            at_last("class C { int m(int p) { return p; } }", "p").kind,
            SemanticTokenKind::Parameter
        );
        assert_eq!(
            at_last("class C { int g() { int x = 0; return x; } }", "x").kind,
            SemanticTokenKind::Variable
        );
    }

    #[test]
    fn type_reference_to_a_sibling_is_precise() {
        // A type reference to a file-local sibling resolves to its declared kind (`Enum`), where
        // the syntactic fallback could only say `Type`.
        assert_eq!(
            at_last("enum E {} class C { E e; }", "E").kind,
            SemanticTokenKind::Enum
        );
    }

    #[test]
    fn unresolved_names_keep_the_syntactic_fallback() {
        // An external/unresolved type and a member access have no file-local binding, so they
        // fall back to syntax: `List` stays `Type`, the member `field` stays `Property`.
        assert_eq!(
            at("class C { List x; }", "List").kind,
            SemanticTokenKind::Type
        );
        assert_eq!(
            at("class C { void m() { var v = obj.field; } }", "field").kind,
            SemanticTokenKind::Property
        );
    }

    #[test]
    fn annotations_and_enum_members() {
        assert_eq!(
            at("@Override class D {}", "Override").kind,
            SemanticTokenKind::Decorator
        );
        let en = "enum E { A, B }";
        assert_eq!(at(en, "A").kind, SemanticTokenKind::EnumMember);
        assert_eq!(at(en, "B").kind, SemanticTokenKind::EnumMember);
    }

    #[test]
    fn imports_are_namespaces() {
        let src = "import java.util.List;";
        assert_eq!(at(src, "java").kind, SemanticTokenKind::Namespace);
        assert_eq!(at(src, "util").kind, SemanticTokenKind::Namespace);
    }

    #[test]
    fn literals_and_strings() {
        let src = "class C { int a = 42; String s = \"hi\"; char c = 'x'; }";
        assert_eq!(at(src, "42").kind, SemanticTokenKind::Number);
        assert_eq!(at(src, "\"hi\"").kind, SemanticTokenKind::String);
        assert_eq!(at(src, "'x'").kind, SemanticTokenKind::String);
    }

    #[test]
    fn multi_line_tokens_keep_their_full_byte_range() {
        // A block comment spans lines; the neutral token carries the whole range (hosts whose
        // protocol forbids multi-line tokens split it themselves).
        let src = "/* a\n   b */ class C {}";
        let comment = at(src, "/*");
        assert_eq!(comment.kind, SemanticTokenKind::Comment);
        assert_eq!(&src[comment.range], "/* a\n   b */");
    }

    #[test]
    fn cross_file_type_is_classified_by_its_kind() {
        use jals_hir::{FileId, ProjectIndex};

        block_on_inline(async {
            // `Color` is declared as an `enum` in another file. The file-local pass leaves the
            // reference unresolved (a generic `Type`); the project index sharpens it to `Enum`.
            let other = "package a; enum Color { RED }";
            let main = "package a; class C { Color c; }";
            let nodes = [
                (FileId(0), jals_syntax::Parse::parse(other).await.syntax()),
                (FileId(1), jals_syntax::Parse::parse(main).await.syntax()),
            ];
            let index = ProjectIndex::builder(&nodes).build().await;
            let start = main.find("Color").unwrap();
            let root = jals_syntax::Parse::parse(main).await.syntax();
            let kind_at = async |project| {
                SemanticTokens::classify(&root, project)
                    .await
                    .into_iter()
                    .find(|t| t.range.start == start)
                    .map(|t| t.kind)
                    .expect("no token at the `Color` reference")
            };
            assert_eq!(kind_at(None).await, SemanticTokenKind::Type);
            assert_eq!(
                kind_at(Some((&index, FileId(1)))).await,
                SemanticTokenKind::Enum
            );
        });
    }

    #[test]
    fn does_not_panic_on_garbage_or_empty() {
        // Invariant: analysis never panics, even on broken / arbitrary input.
        for text in ["", "class", "@#$%^ <<< class {", "класс 类 😀 \0 /*"] {
            let _ = classify(text);
        }
    }
}
