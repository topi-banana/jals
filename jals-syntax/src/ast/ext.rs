//! Hand-written accessors that don't fit the generated forms.
//!
//! `generated.rs` covers the four mechanical accessor shapes driven by
//! `java.ungram` labels; everything that needs bespoke tree-walking (token
//! text, positional selection among same-typed children, parameterized
//! queries) lives here. Both halves together form the public `ast` API.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use rowan::WalkEvent;
use rowan::ast::support;

use super::{
    AssignmentExpr, AstNode, AstSupport, AttrMeta, Attribute, BinaryExpr, BreakStmt, CatchClause,
    ContinueStmt, Decl, Expr, FieldAccess, FieldDecl, ForStmt, Literal, LocalVarDecl, Modifiers,
    QualifiedName, Resource, Stmt, SwitchExpr, Type, YieldStmt,
};
use crate::language::{SyntaxNode, SyntaxToken};
use crate::syntax_kind::SyntaxKind::{
    self, DOT, EQ, FOR_KW, IDENT, RPAREN, SEMICOLON, SWITCH_EXPR, SWITCH_STMT, YIELD_STMT,
};
#[cfg(test)]
use crate::syntax_kind::SyntaxKind::{MODIFIERS, NON_SEALED_KW};

impl QualifiedName {
    /// The full dotted name, e.g. `a.b.c` or `a.b.*` — each segment with its JLS §3.3 escapes
    /// resolved.
    ///
    /// Composed from the decoded segments rather than from the node's text, because this is the name
    /// a package, an import, or a qualified type *is*: `import a.b.\u0043;` imports `a.b.C`, and
    /// matching it against the declaration of `C` needs both spelled the language's way. The
    /// wildcard is re-added because it is punctuation of the import, not a segment.
    pub fn text(&self) -> String {
        let mut out = self.segments().join(".");
        if self.is_wildcard() {
            if !out.is_empty() {
                out.push('.');
            }
            out.push('*');
        }
        out
    }

    /// The dotted segments in source order (`a.b.C` → `["a", "b", "C"]`), decoded. The trailing
    /// wildcard `*` of an on-demand import is not a segment.
    fn segments(&self) -> Vec<String> {
        AstSupport::ident_tokens(&self.syntax)
            .map(|t| crate::decoded_ident(&t).into_owned())
            .collect()
    }

    /// The last (simple) segment (`import a.b.Foo;` → `Foo`), decoded. `None` for a wildcard import
    /// (`a.b.*`), which names no single type.
    pub fn last_segment(&self) -> Option<String> {
        if self.is_wildcard() {
            return None;
        }
        AstSupport::ident_tokens(&self.syntax)
            .last()
            .map(|t| crate::decoded_ident(&t).into_owned())
    }

    /// The qualifier (package) part: everything before the simple name (`a.b.C` → `a.b`), or the
    /// full package of an on-demand import (`a.b.*` → `a.b`). `None` when there is no qualifier.
    pub fn qualifier(&self) -> Option<String> {
        let segs = self.segments();
        let take = if self.is_wildcard() {
            segs.len()
        } else {
            segs.len().saturating_sub(1)
        };
        if take == 0 {
            return None;
        }
        Some(segs[..take].join("."))
    }
}

impl Attribute {
    /// All jals attributes attached to `node`, wherever the parser placed them: leading
    /// direct children (statement position) plus those inside a direct `MODIFIERS` child
    /// (declarations parsed through `modifiers()`). A node never holds both in practice.
    #[cfg(test)]
    fn of(node: &SyntaxNode) -> impl Iterator<Item = Self> {
        node.children().filter_map(Self::cast).chain(
            node.children()
                .filter(|n| n.kind() == MODIFIERS)
                .flat_map(|m| m.children().filter_map(Self::cast)),
        )
    }
}

impl AttrMeta {
    /// The meta name text (`cfg` in `#[cfg(...)]`, `feature` in `feature = "x"`).
    pub(crate) fn name_text(&self) -> Option<String> {
        self.name().map(|n| n.text())
    }
}

impl Modifiers {
    /// Whether a plain keyword modifier `kind` (e.g. `PUBLIC_KW`) is present.
    pub fn has(&self, kind: SyntaxKind) -> bool {
        support::token(&self.syntax, kind).is_some()
    }

    /// Whether the `non-sealed` modifier is present.
    #[cfg(test)]
    pub(crate) fn is_non_sealed(&self) -> bool {
        self.syntax.children().any(|n| n.kind() == NON_SEALED_KW)
    }
}

impl Type {
    /// The type text with surrounding/interleaved trivia removed (e.g. `List<T>`).
    ///
    /// Use [`AstNode::syntax`]<code>().text()</code> if you need the verbatim slice including trivia.
    #[cfg(test)]
    pub(crate) fn text(&self) -> String {
        AstSupport::non_trivia_text(&self.syntax)
    }

    /// The simple-name identifier token of a reference type (the last top-level `IDENT`): `a.b.C`
    /// → the `C` token, `List<Foo>` → the `List` token. `None` for a primitive, `var`, or `void`
    /// (which have no identifier).
    ///
    /// Type arguments are nested `TYPE_ARGS` nodes, so the names inside `List<Foo>` are not direct
    /// `IDENT` tokens — only the outer `List` is considered here.
    pub fn simple_name_token(&self) -> Option<SyntaxToken> {
        AstSupport::ident_tokens(&self.syntax).last()
    }

    /// The text of [`simple_name_token`](Type::simple_name_token): `a.b.C` → `C`, with its JLS §3.3
    /// escapes resolved — this names a type rather than rendering one.
    pub fn simple_name(&self) -> Option<String> {
        self.simple_name_token()
            .map(|t| crate::decoded_ident(&t).into_owned())
    }

    /// Whether the type name is qualified, i.e. a dotted reference type (`a.b.C`).
    pub fn is_qualified(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .any(|t| t.kind() == DOT)
    }

    /// The qualified name text of a reference type, with type arguments and array dimensions
    /// removed (`java.util.List<String>[]` → `java.util.List`). `None` for a non-reference type.
    pub fn qualified_text(&self) -> Option<String> {
        // Decoded, like every other name accessor here: the dots are punctuation and the segments
        // are identifiers, so an escape inside one changes which type is named.
        let text: String = self
            .syntax
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| matches!(t.kind(), IDENT | DOT))
            .map(|t| crate::decoded_ident(&t).into_owned())
            .collect();
        (!text.is_empty()).then_some(text)
    }

    /// Whether this is a primitive, `var`, or `void` type — one with no reference name to resolve.
    /// Equivalently, a type with no top-level `IDENT` token (a reference type always has one).
    pub fn is_primitive_or_var(&self) -> bool {
        AstSupport::ident_tokens(&self.syntax).next().is_none()
    }

    /// The type-argument `Type` nodes written on this type, in order (`List<String>` → one `String`,
    /// `Map<K, V>` → `K`, `V`); empty for a raw or argument-free type. A bare wildcard (`?`) appears
    /// as a node with no reference name (see [`is_primitive_or_var`](Type::is_primitive_or_var)).
    pub fn type_arg_types(&self) -> impl Iterator<Item = Self> {
        self.type_args().into_iter().flat_map(|ta| ta.args())
    }
}

impl Literal {
    /// The literal token.
    pub fn token(&self) -> Option<SyntaxToken> {
        AstSupport::first_sig_token(&self.syntax)
    }

    /// The literal text as written.
    #[cfg(test)]
    fn text(&self) -> Option<String> {
        self.token().map(|t| t.text().to_owned())
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
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| t.kind() == IDENT)
            .last()
            .map(|t| t.text().to_owned())
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

    /// The operator tokens between the two operands, in order.
    ///
    /// A simple assignment is one `EQ`. A compound one is either a single fused token (`PLUS_EQ`,
    /// `STAR_EQ`, …) or — for the shift forms, whose `>` the lexer never joins to what follows so
    /// that `List<List<T>>` still closes — several: `>>=` arrives as `GT GT EQ`.
    fn operator(&self) -> impl Iterator<Item = SyntaxToken> {
        self.syntax
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .filter(|t| !t.kind().is_trivia())
    }

    /// Whether this is a plain `=` rather than a compound assignment.
    ///
    /// The distinction is invisible in the node kind — `x = 1` and `x += 1` are both
    /// `ASSIGNMENT_EXPR` — so a consumer that lowers only the simple form has to ask, or it will
    /// silently emit the wrong program.
    pub fn is_simple(&self) -> bool {
        let mut operator = self.operator();
        operator.next().is_some_and(|token| token.kind() == EQ) && operator.next().is_none()
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
        AstSupport::ident_tokens(&self.syntax)
    }
}

impl FieldDecl {
    /// Every declared field name token, in source order (`int a, b;` binds two).
    ///
    /// Like [`LocalVarDecl::names`]: each name is a direct `IDENT` token child, and an unnamed
    /// `_` binding is not reported.
    pub fn names(&self) -> impl Iterator<Item = SyntaxToken> {
        AstSupport::ident_tokens(&self.syntax)
    }
}

impl CatchClause {
    /// The caught exception's binding name token (the `IDENT` after the type(s)), if named.
    ///
    /// The catch types are nested `TYPE` nodes, so the only direct `IDENT` token is the binding.
    /// Returns `None` for an unnamed `_` binding (an `UNDERSCORE` token).
    pub fn binding(&self) -> Option<SyntaxToken> {
        AstSupport::ident_tokens(&self.syntax).next()
    }

    /// Every caught exception type, including each arm of a multi-catch (`catch (A | B e)`). The
    /// generated [`ty`](Self::ty) accessor returns only the first arm, so the `Type` children are
    /// walked directly.
    pub fn types(&self) -> impl Iterator<Item = Type> {
        self.syntax.children().filter_map(Type::cast)
    }
}

impl BreakStmt {
    /// The label this `break` names, if any.
    ///
    /// The grammar has no slot for it — there is nothing else a bare `IDENT` on a `break` could be —
    /// so it is the statement's own first identifier token, exactly as a `catch` binding is.
    pub fn label(&self) -> Option<SyntaxToken> {
        AstSupport::ident_tokens(&self.syntax).next()
    }
}

impl ContinueStmt {
    /// The label this `continue` names, if any. See [`BreakStmt::label`].
    pub fn label(&self) -> Option<SyntaxToken> {
        AstSupport::ident_tokens(&self.syntax).next()
    }
}

impl Resource {
    /// The resource variable's binding name token (the `IDENT` after the type), if this resource
    /// declares a new variable.
    ///
    /// Returns `None` when the resource is an existing variable used directly (`try (existing)`,
    /// where the resource is a reference node, not a declaration) or an unnamed `_` binding.
    pub fn binding(&self) -> Option<SyntaxToken> {
        AstSupport::ident_tokens(&self.syntax).next()
    }
}

impl Decl {
    /// The declared name token, whichever declaration form this is.
    ///
    /// Every `Decl` variant labels its name in the grammar, so each arm is that node's generated
    /// `name_token`. The dispatch exists because a caller usually reaches a declaration as a plain
    /// [`SyntaxNode`] — the seven forms are seven `SyntaxKind`s — and would otherwise re-find the
    /// `IDENT` by hand to get the offset `jals-hir`'s index is keyed on. Going through the typed
    /// accessors means a variant that later moves its name into a child node keeps working here.
    ///
    /// That is also why the dispatch is private: every caller arrives holding the node, so
    /// [`name_token_of`](Self::name_token_of) is the entry and this is its body.
    fn name_token(&self) -> Option<SyntaxToken> {
        match self {
            Self::Class(decl) => decl.name_token(),
            Self::Interface(decl) => decl.name_token(),
            Self::Enum(decl) => decl.name_token(),
            Self::Record(decl) => decl.name_token(),
            Self::AnnotationType(decl) => decl.name_token(),
            Self::Method(decl) => decl.name_token(),
            Self::Field(decl) => decl.name_token(),
        }
    }

    /// The declared name token of `node`, if it is a declaration at all.
    pub fn name_token_of(node: &SyntaxNode) -> Option<SyntaxToken> {
        Self::cast(node.clone()).and_then(|decl| decl.name_token())
    }
}

/// Which part of a C-style `for` a direct child node belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ForSection {
    Init,
    Condition,
    Update,
    Body,
}

impl ForSection {
    /// The section a header `;` moves into. The body's own `;` is an `EmptyStmt` node rather than a
    /// token child, so only the two separators reach this in well-formed input; the last arm is
    /// what keeps a stray one from re-sectioning the statement.
    const fn after_semicolon(self) -> Self {
        match self {
            Self::Init => Self::Condition,
            Self::Condition => Self::Update,
            Self::Update | Self::Body => self,
        }
    }
}

impl ForStmt {
    /// Pairs every direct child node with the section of the statement it sits in.
    ///
    /// A C-style `for` is flat in the CST: the two `;` and the `)` are direct token children, and
    /// each section is the run of sibling nodes between them. Sectioning therefore cannot be done
    /// by child type — all three header sections may hold an `Expr` — which is why the grammar
    /// leaves them unlabeled and this walk is the one place the shape is written down.
    ///
    /// The walk starts at `for` rather than at `(`: `attrs:Attribute*` precede the keyword, so
    /// counting from the top would file the attribute of `#[cfg(…)] for (…)` under the
    /// initialiser, and starting at `(` would drop the whole statement when the `(` is missing
    /// from malformed input. `for` is the *only* thing that opens the walk — neither separator can
    /// — so an attribute is outside it whatever the attribute happens to contain.
    fn sections(&self) -> impl Iterator<Item = (ForSection, SyntaxNode)> {
        let mut section = None;
        self.syntax
            .children_with_tokens()
            .filter_map(move |child| match child {
                rowan::NodeOrToken::Token(token) => {
                    match token.kind() {
                        FOR_KW => section = Some(ForSection::Init),
                        SEMICOLON => section = section.map(ForSection::after_semicolon),
                        RPAREN => section = section.map(|_| ForSection::Body),
                        _ => {}
                    }
                    None
                }
                rowan::NodeOrToken::Node(node) => section.map(|section| (section, node)),
            })
    }

    /// The direct child nodes sitting in `want`, in source order.
    fn nodes_in(&self, want: ForSection) -> impl Iterator<Item = SyntaxNode> {
        self.sections()
            .filter_map(move |(section, node)| (section == want).then_some(node))
    }

    /// The initialiser entries, in source order: either one `LocalVarDecl` (`for (int i = 0; …`) or
    /// a comma-separated run of `Expr`s (`for (i = 0, j = n; …`). Empty for `for (;;)`.
    ///
    /// Kept at [`SyntaxNode`] granularity rather than narrowed to a typed enum because a local
    /// *type* declaration is also legal here (see the parser's `for_init`), and because a caller
    /// that cannot lower an entry must be able to report it rather than have it silently dropped.
    pub fn init(&self) -> impl Iterator<Item = SyntaxNode> {
        self.nodes_in(ForSection::Init)
    }

    /// The loop condition, or `None` for `for (;;)`.
    pub fn condition(&self) -> Option<Expr> {
        self.nodes_in(ForSection::Condition)
            .next()
            .and_then(Expr::cast)
    }

    /// The update entries, in source order; empty when the clause is omitted. At [`SyntaxNode`]
    /// granularity for the same reason as [`init`](Self::init).
    pub fn update(&self) -> impl Iterator<Item = SyntaxNode> {
        self.nodes_in(ForSection::Update)
    }

    /// The loop body — whatever follows the `)`.
    pub fn body(&self) -> Option<Stmt> {
        self.nodes_in(ForSection::Body).next().and_then(Stmt::cast)
    }
}

impl SwitchExpr {
    /// The value-producing expressions of this switch expression: each arrow rule's
    /// [`expr`](super::SwitchRule::expr) body (`case X -> expr;`), plus every `yield`'s value —
    /// covering both arrow blocks and colon groups. A `throw` or otherwise value-less arm
    /// contributes nothing.
    ///
    /// A nested switch expression or statement is skipped as a whole subtree, so an inner
    /// switch's arms and yields are never misattributed to this one.
    pub fn result_exprs(&self) -> impl Iterator<Item = Expr> {
        let arrows = self
            .body()
            .into_iter()
            .flat_map(|b| b.rules())
            .filter_map(|r| r.expr());
        let mut walk = self.body().map(|b| b.syntax().preorder());
        let yields = core::iter::from_fn(move || {
            let walk = walk.as_mut()?;
            while let Some(event) = walk.next() {
                let WalkEvent::Enter(node) = event else {
                    continue;
                };
                match node.kind() {
                    SWITCH_EXPR | SWITCH_STMT => walk.skip_subtree(),
                    YIELD_STMT => {
                        if let Some(expr) = YieldStmt::cast(node).and_then(|y| y.expr()) {
                            return Some(expr);
                        }
                    }
                    _ => {}
                }
            }
            None
        });
        arrows.chain(yields)
    }
}

#[cfg(test)]
mod tests {
    use super::{AstNode, SyntaxNode};
    use crate::ast::{
        AttrArg, Attribute, BreakStmt, CatchClause, ClassDecl, ContinueStmt, Decl, ExprStmt,
        FieldDecl, ForStmt, ImportDecl, ImportGroup, LocalVarDecl, MethodDecl, QualifiedName,
        Resource, Stmt, SwitchExpr, Type,
    };
    use crate::parser::Parse;

    /// Returns the first descendant of `src` that casts to `T`.
    fn first<T: AstNode<Language = crate::language::JavaLanguage>>(src: &str) -> T {
        jals_exec::block_on_inline(Parse::parse(src))
            .syntax()
            .descendants()
            .find_map(T::cast)
            .expect("node present")
    }

    fn names_of(decl: impl Iterator<Item = crate::language::SyntaxToken>) -> Vec<String> {
        decl.map(|t| t.text().to_owned()).collect()
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

    /// `break l;` names a label and `break;` names none. The grammar has no slot for it, so both
    /// compiler backends walked the statement's tokens themselves and wrote the same rule twice.
    #[test]
    fn break_and_continue_name_their_label() {
        let with: BreakStmt = first("class C { void m() { l: while (true) { break l; } } }");
        assert_eq!(
            with.label().map(|t| t.text().to_owned()).as_deref(),
            Some("l")
        );

        let without: BreakStmt = first("class C { void m() { while (true) { break; } } }");
        assert_eq!(without.label(), None);

        let carry_on: ContinueStmt =
            first("class C { void m() { l: while (true) { continue l; } } }");
        assert_eq!(
            carry_on.label().map(|t| t.text().to_owned()).as_deref(),
            Some("l")
        );
    }

    #[test]
    fn catch_binding_skips_the_types() {
        let catch: CatchClause = first("class C { void m() { try { } catch (A | B e) { } } }");
        assert_eq!(
            catch.binding().map(|t| t.text().to_owned()).as_deref(),
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
            resource.binding().map(|t| t.text().to_owned()).as_deref(),
            Some("r")
        );
    }

    #[test]
    fn type_qualified_reference_splits_name_and_qualifier() {
        let ty: Type = first("class C { java.util.List<String> f; }");
        assert_eq!(ty.simple_name().as_deref(), Some("List"));
        assert!(ty.is_qualified());
        assert_eq!(ty.qualified_text().as_deref(), Some("java.util.List"));
        assert!(!ty.is_primitive_or_var());
    }

    #[test]
    fn type_generic_simple_name_ignores_args() {
        let ty: Type = first("class C { List<Foo> f; }");
        assert_eq!(ty.simple_name().as_deref(), Some("List"));
        assert!(!ty.is_qualified());
        assert_eq!(ty.qualified_text().as_deref(), Some("List"));
    }

    #[test]
    fn type_primitive_has_no_reference_name() {
        let ty: Type = first("class C { int x; }");
        assert_eq!(ty.simple_name(), None);
        assert_eq!(ty.qualified_text(), None);
        assert!(ty.is_primitive_or_var());
    }

    #[test]
    fn type_array_of_reference_keeps_name() {
        let ty: Type = first("class C { String[] xs; }");
        assert_eq!(ty.simple_name().as_deref(), Some("String"));
        assert!(!ty.is_primitive_or_var());
    }

    #[test]
    fn qualified_name_segments_and_parts() {
        let qn: QualifiedName = first("import a.b.Foo;");
        assert_eq!(qn.segments(), ["a", "b", "Foo"]);
        assert_eq!(qn.last_segment().as_deref(), Some("Foo"));
        assert_eq!(qn.qualifier().as_deref(), Some("a.b"));
        assert!(!qn.is_wildcard());
    }

    #[test]
    fn grouped_import_exposes_prefix_and_members() {
        // The prefix is `ImportDecl::name()` (a direct child); members live under the group.
        let decl: ImportDecl = first("import java.util.{HashMap, regex.Pattern, concurrent.*};");
        assert_eq!(decl.name().unwrap().text(), "java.util");
        assert!(!decl.is_static());
        let group: ImportGroup = decl.group().expect("grouped import has a group");
        let members: Vec<String> = group.members().map(|m| m.text()).collect();
        assert_eq!(members, ["HashMap", "regex.Pattern", "concurrent.*"]);
    }

    #[test]
    fn static_grouped_import_keeps_static_flag() {
        let decl: ImportDecl = first("import static java.lang.Math.{PI, E};");
        assert!(decl.is_static());
        assert_eq!(decl.name().unwrap().text(), "java.lang.Math");
        let members: Vec<String> = decl.group().unwrap().members().map(|m| m.text()).collect();
        assert_eq!(members, ["PI", "E"]);
    }

    #[test]
    fn ordinary_import_has_no_group() {
        let decl: ImportDecl = first("import java.util.List;");
        assert!(decl.group().is_none());
    }

    #[test]
    fn attribute_on_a_class_lives_in_its_modifiers() {
        let class: ClassDecl = first("#[cfg(feature = \"x\")]\npublic class C {}");
        let attrs: Vec<Attribute> = class.modifiers().unwrap().attrs().collect();
        assert_eq!(attrs.len(), 1);
        let meta = attrs[0].meta().unwrap();
        assert_eq!(meta.name_text().as_deref(), Some("cfg"));
        // The unified accessor sees it too.
        assert_eq!(Attribute::of(class.syntax()).count(), 1);
    }

    #[test]
    fn attribute_meta_exposes_nested_args_and_literals() {
        let class: ClassDecl =
            first("#[cfg(any(feature = \"a\", not(feature = \"b\")))] class C {}");
        let attr = Attribute::of(class.syntax()).next().unwrap();
        let cfg = attr.meta().unwrap();
        assert_eq!(cfg.name_text().as_deref(), Some("cfg"));
        let args: Vec<AttrArg> = cfg.args().unwrap().args().collect();
        let [AttrArg::AttrMeta(any)] = args.as_slice() else {
            panic!("cfg holds a single meta argument");
        };
        assert_eq!(any.name_text().as_deref(), Some("any"));
        let nested: Vec<AttrArg> = any.args().unwrap().args().collect();
        let [AttrArg::AttrMeta(feature), AttrArg::AttrMeta(not)] = nested.as_slice() else {
            panic!("any holds two meta arguments");
        };
        assert_eq!(feature.name_text().as_deref(), Some("feature"));
        assert_eq!(feature.value().unwrap().text().as_deref(), Some("\"a\""));
        assert_eq!(not.name_text().as_deref(), Some("not"));
    }

    #[test]
    fn attribute_on_a_statement_is_a_leading_child() {
        let stmt: ExprStmt = first("class C { void m() { #[cfg(feature = \"x\")] f(); } }");
        assert_eq!(stmt.attrs().count(), 1);
        assert_eq!(Attribute::of(stmt.syntax()).count(), 1);
    }

    #[test]
    fn attribute_on_an_import_is_a_leading_child() {
        let decl: ImportDecl = first("#[cfg(feature = \"x\")] import java.util.List;");
        assert_eq!(decl.attrs().count(), 1);
        assert_eq!(decl.name().unwrap().text(), "java.util.List");
    }

    #[test]
    fn qualified_name_wildcard_has_no_last_segment() {
        let qn: QualifiedName = first("import a.b.*;");
        assert_eq!(qn.segments(), ["a", "b"]);
        assert_eq!(qn.last_segment(), None);
        assert_eq!(qn.qualifier().as_deref(), Some("a.b"));
        assert!(qn.is_wildcard());
    }

    #[test]
    fn decl_name_token_carries_the_name_offset() {
        const SRC: &str = "class Foo { int bar; void baz() {} }";
        // The offset is the whole point of the token accessor: `jals-hir`'s index is keyed on
        // where a name starts (`item_by_decl` / `member_by_decl`), so the text cannot stand in.
        for (declared, expected) in [
            (Decl::name_token_of(first::<ClassDecl>(SRC).syntax()), "Foo"),
            (Decl::name_token_of(first::<FieldDecl>(SRC).syntax()), "bar"),
            (
                Decl::name_token_of(first::<MethodDecl>(SRC).syntax()),
                "baz",
            ),
        ] {
            let token = declared.unwrap_or_else(|| panic!("`{expected}` is a declared name"));
            assert_eq!(token.text(), expected);
            assert_eq!(
                usize::from(token.text_range().start()),
                SRC.find(expected).unwrap()
            );
        }
    }

    #[test]
    fn a_generated_name_and_its_token_cannot_disagree() {
        // `AstSupport::name_text` is written in terms of `name_token`; this pins that they keep
        // reporting the same token, since the two are generated from one label.
        let class: ClassDecl = first("class Foo {}");
        assert_eq!(class.name().as_deref(), Some("Foo"));
        assert_eq!(
            class.name_token().map(|t| t.text().to_owned()),
            class.name()
        );
    }

    #[test]
    fn a_non_declaration_declares_no_name() {
        // The old hand-rolled walk took the first `IDENT` of any node it was handed. Going through
        // `Decl` means a node that is not one of the seven declaration forms answers `None`
        // instead of a name that is really a reference.
        let stmt: ExprStmt = first("class C { void m() { f(); } }");
        assert!(Decl::name_token_of(stmt.syntax()).is_none());
    }

    /// Wraps `body` in a method so the `for` under test is the first one in the file.
    fn for_stmt(header_and_body: &str) -> ForStmt {
        first(&alloc::format!(
            "class C {{ void m() {{ {header_and_body} }} }}"
        ))
    }

    /// The source text of `node`, less the leading trivia a lossless CST attaches to it.
    fn trimmed(node: &SyntaxNode) -> String {
        node.text().to_string().trim().to_owned()
    }

    fn texts(nodes: impl Iterator<Item = SyntaxNode>) -> Vec<String> {
        nodes.map(|node| trimmed(&node)).collect()
    }

    #[test]
    fn for_sections_split_a_declaring_header() {
        let stmt = for_stmt("for (int i = 0; i < n; i++) { f(); }");
        assert_eq!(texts(stmt.init()), ["int i = 0"]);
        assert_eq!(
            stmt.condition().map(|c| trimmed(c.syntax())),
            Some("i < n".to_owned())
        );
        assert_eq!(texts(stmt.update()), ["i++"]);
        // The hazard the flat CST creates: `LOCAL_VAR_DECL` casts to `Stmt`, so a body accessor
        // that selected by child type alone would return the initialiser's declaration instead.
        let body = stmt.body().expect("the body is the statement after `)`");
        assert!(matches!(body, Stmt::Block(_)), "body was {body:?}");
        assert_eq!(trimmed(body.syntax()), "{ f(); }");
    }

    #[test]
    fn for_sections_are_empty_in_an_infinite_loop() {
        let stmt = for_stmt("for (;;) { f(); }");
        assert_eq!(texts(stmt.init()), Vec::<String>::new());
        assert!(stmt.condition().is_none());
        assert_eq!(texts(stmt.update()), Vec::<String>::new());
        assert!(stmt.body().is_some());
    }

    #[test]
    fn for_sections_keep_every_comma_separated_entry() {
        let stmt = for_stmt("for (i = 0, j = n; i < j; i++, j--) ;");
        assert_eq!(texts(stmt.init()), ["i = 0", "j = n"]);
        assert_eq!(texts(stmt.update()), ["i++", "j--"]);
        // The body's `;` is an `EMPTY_STMT` node, not a token child, so it never reaches the
        // sectioning walk and cannot advance a section past the header.
        assert!(matches!(stmt.body(), Some(Stmt::Empty(_))));
    }

    #[test]
    fn for_sections_survive_an_omitted_clause() {
        // The condition is present but both other clauses are gone; the update must not absorb it.
        let stmt = for_stmt("for (; i < n; ) { f(); }");
        assert_eq!(texts(stmt.init()), Vec::<String>::new());
        assert_eq!(
            stmt.condition().map(|c| trimmed(c.syntax())),
            Some("i < n".to_owned())
        );
        assert_eq!(texts(stmt.update()), Vec::<String>::new());
    }

    #[test]
    fn an_unclosed_for_header_degrades_where_it_broke() {
        // `expect(RPAREN)` records a diagnostic without inserting a token, so a header with no `)`
        // has nothing to end it and the body reads as one more update entry. That is what the two
        // hand-rolled walks did as well; what this pins is that the sections *before* the break are
        // still right and that nothing panics on the way — parsing is lossless and total, and
        // sectioning what it produced has to be too.
        let stmt = for_stmt("for (int i = 0; i < n; i++ { f(); }");
        assert_eq!(texts(stmt.init()), ["int i = 0"]);
        assert_eq!(
            stmt.condition().map(|c| trimmed(c.syntax())),
            Some("i < n".to_owned())
        );
        assert_eq!(texts(stmt.update()), ["i++", "{ f(); }"]);
        assert!(stmt.body().is_none());
    }

    #[test]
    fn a_for_attribute_is_not_an_initialiser() {
        // `attrs:Attribute*` precede the `for` keyword as direct children, so a walk that started
        // sectioning at the top of the node would file this attribute under the initialiser and
        // hand a consumer a node it cannot lower.
        let stmt = for_stmt("#[cfg(feature = \"x\")] for (int i = 0; i < n; i++) { f(); }");
        assert_eq!(stmt.attrs().count(), 1);
        assert_eq!(texts(stmt.init()), ["int i = 0"]);
    }

    #[test]
    fn switch_result_exprs_covers_every_arm_shape_and_skips_nested_switches() {
        // Arrow expr, arrow block (whose yield's value is itself a nested switch), throw arm,
        // and a colon group; the nested switch's own arm must not leak into the outer list.
        let switch: SwitchExpr = first(
            "class C { int m(int x) { return switch (x) { \
                 case 1 -> 10; \
                 case 2 -> { yield switch (x) { default -> 30; }; } \
                 case 3 -> throw new RuntimeException(); \
                 default: yield 40; \
             }; } }",
        );
        let texts: Vec<String> = switch
            .result_exprs()
            .map(|e| e.syntax().text().to_string().trim().to_owned())
            .collect();
        assert_eq!(texts, ["10", "switch (x) { default -> 30; }", "40"]);
    }
}
