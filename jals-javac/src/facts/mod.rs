//! The source facts both lowerings read.
//!
//! A *fact* here is an answer about the program as written — the span the inference memo is keyed
//! on, the definition a name binds to, the locals a class captures, the constant a `case` label
//! names, whether one method overrides another. Every one is derived from the CST plus [`jals_hir`]
//! and nothing else. **This module knows no instruction.** It names no `Descriptor`, no
//! `Assembler`, no `ValType`, and no local-slot numbering, because those are answers about a
//! *target* rather than about the source.
//!
//! # Why it exists
//!
//! The two lowerings genuinely differ — one emits a `goto` stream, the other structured
//! `block` / `loop` / `br` — and the crate's own documentation says so. But the layer *underneath*
//! that difference was written twice, and the copies drifted: `span` was byte-identical, `def_at`
//! differed only in which field it reached through, and the `switch` arm reader was duplicated down
//! to its explanatory comment. Where they were not identical they were wrong in different ways — a
//! `case ~5:` was rejected by one backend and silently compiled as `5` by the other, and a
//! `Type::name` method reference was selected by name *and arity* on one side and by name alone on
//! the other.
//!
//! One fact, one answer, one place to test it. What stays behind in each backend is what actually
//! differs: control flow, the wasm `Layout`, the JVM's `Slots`, and erasure.
//!
//! # Being the only entry is the point
//!
//! For a while it was one entry among several. The `case`-label evaluator read its literals here
//! while both *expression* paths read theirs from the JVM backend's own module — which wasm reached
//! across the backend seam to call. `Facts::declarator_initialiser` existed to stop names and
//! initialisers being paired by index, and four of the five sites that pair them went on doing it,
//! which is a wrong `static` field on wasm and a class the JVM rejects at load. The modifier scan
//! answered two different questions in the two backends, and "the superclass" had three rules.
//!
//! None of that named the other backend, so a rule against reaching across the seam
//! (`no-wasm-into-jvm-lowering`) would have stayed green throughout. What makes a fact single-sourced
//! is that there is one place to ask and it has a test; the rules only stop the cheapest way to get
//! a second one.
//!
//! A fact both backends need goes here. One that names an instruction does not — and
//! `facts-names-no-instruction` is what keeps that sentence from going back to being prose.
//!
//! # What it deliberately does not do
//!
//! It does not check. A fact this layer cannot establish is *reported* ([`FactError`]) rather than
//! guessed at, for the same reason [`crate::desc`] refuses a type it cannot name: a wrong answer
//! here becomes a class file that verifies and then does the wrong thing.
//!
//! It also holds no memo. One would need interior mutability, and that would cost the `Copy` that
//! lets a [`Facts`] be passed exactly like the [`TypedFile`] it wraps.

mod constant;
mod inherit;
mod literal;
mod method_ref;
mod switch;

pub(crate) use constant::CaseKey;
pub(crate) use inherit::{Hierarchy, Overrides};
pub(crate) use literal::Literal;
pub(crate) use method_ref::RefReceiver;

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::{DefId, DefKind, FileId, MemberId, ProjectIndex, TypedFile};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Why a source fact could not be established.
///
/// Two variants, and both are also spelled by `LowerError` and `WasmError` — which is what lets
/// each backend absorb one without inventing a third vocabulary. The `&'static str` of an
/// `Unsupported` travels *verbatim* through those conversions: several are pinned by name in the
/// integration tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FactError {
    /// A construct this layer does not evaluate.
    Unsupported(&'static str),
    /// A name the index did not resolve.
    Unresolved(String),
}

impl core::fmt::Display for FactError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "{what} is not evaluated here"),
            Self::Unresolved(name) => write!(f, "`{name}` did not resolve"),
        }
    }
}

/// What asking for a source fact answers.
pub(crate) type Result<T> = core::result::Result<T, FactError>;

/// The source facts of one analysed file.
///
/// A `Copy` handle over [`TypedFile`], which already carries the analysis, the index, and the file
/// id — so this borrows nothing beyond it and travels by value wherever the file is in scope.
#[derive(Clone, Copy)]
pub(crate) struct Facts<'a> {
    typed: TypedFile<'a>,
}

impl<'a> Facts<'a> {
    /// The facts of `typed`.
    pub(crate) const fn of(typed: TypedFile<'a>) -> Self {
        Self { typed }
    }

    /// The file with its inference already run.
    const fn typed(self) -> TypedFile<'a> {
        self.typed
    }

    /// The project index the file was bound against.
    pub(crate) const fn index(self) -> &'a ProjectIndex {
        self.typed.index()
    }

    /// Which file this is, in the index's numbering.
    pub(crate) const fn file(self) -> FileId {
        self.typed.file()
    }

    /// The index-only facts, for a question this file's contents do not bear on.
    const fn hierarchy(self) -> Hierarchy<'a> {
        Hierarchy::of(self.typed.index())
    }

    /// The file's syntax tree.
    const fn root(self) -> &'a SyntaxNode {
        self.typed.root()
    }

    // --- pure CST -------------------------------------------------------------------------------

    /// A node's byte span, keyed the way the inference memo is: the node's own range, leading
    /// trivia included, because that is what the analysis recorded against.
    ///
    /// Every `type_of_expr` / `call_target_of` / `field_target_of` lookup goes through here, so it
    /// is the one place the key is spelled and the two backends cannot disagree about it.
    pub(crate) fn span(node: &SyntaxNode) -> core::ops::Range<usize> {
        let range = node.text_range();
        usize::from(range.start())..usize::from(range.end())
    }

    /// A node's operator, as the run of its own non-trivia tokens.
    ///
    /// A run rather than one kind because the lexer never joins a `>` to what follows, so that
    /// `List<List<T>>` still closes as two of them: `>>` is `[GT, GT]`, `>>>` is `[GT, GT, GT]`,
    /// and `>=` is `[GT, EQ]`. Both backends decode operators from this, and each used to carry its
    /// own copy of the rule together with the comment explaining it.
    ///
    /// Matching the run *exactly* is also what keeps `--5` from reading as a negation: `--` is its
    /// own `MINUS_MINUS` kind, so a rule that merely asks whether a `MINUS` is present answers
    /// wrongly, which is how one backend compiled `case --5:` as `5`.
    pub(crate) fn operator(node: &SyntaxNode) -> Vec<SyntaxKind> {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect()
    }

    /// Whether one of `node`'s own tokens is `keyword`. Direct children only: a nested expression's
    /// keyword is not this node's.
    fn has_keyword(node: &SyntaxNode, keyword: SyntaxKind) -> bool {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| token.kind() == keyword)
    }

    /// Whether a node is the bare `this`.
    ///
    /// It carries no identifier token, so nothing resolves it as a name; its keyword is the only
    /// thing that identifies it.
    pub(crate) fn is_this(node: &SyntaxNode) -> bool {
        Self::has_keyword(node, SyntaxKind::THIS_KW)
    }

    /// Whether a declaration's modifiers carry `keyword`.
    ///
    /// Distinct from [`has_keyword`](Self::has_keyword), which reads the node's *own* tokens: a
    /// declaration's modifiers live in a `MODIFIERS` child, so `static` on an initialiser block is
    /// inside one rather than on the block.
    ///
    /// Every `MODIFIERS` child is read, not the first. The JVM backend used `.find` and the wasm one
    /// `.filter(…).flat_map(…)`, so the two answered differently for a declaration the parser gave
    /// more than one — and reading all of them can only ever be the superset.
    pub(crate) fn has_modifier(node: &SyntaxNode, keyword: SyntaxKind) -> bool {
        node.children()
            .filter(|child| child.kind() == SyntaxKind::MODIFIERS)
            .flat_map(|modifiers| modifiers.children_with_tokens())
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| token.kind() == keyword)
    }

    /// Whether a type declaration sits directly inside another type's body.
    pub(crate) fn is_nested(node: &SyntaxNode) -> bool {
        node.parent()
            .is_some_and(|parent| parent.kind() == SyntaxKind::CLASS_BODY)
    }

    /// Whether a class declaration is a non-`static` nested one — an *inner* class.
    ///
    /// A nested interface, `@interface`, `enum`, and `record` are implicitly `static` and hold no
    /// enclosing instance, so only a nested *class* can be one. An inner class holds its enclosing
    /// instance in a synthetic field and every constructor takes it as an extra first parameter, so
    /// a backend that answers this differently from the other emits constructors one parameter
    /// short — a `NoSuchMethodError` at the first `new`, not a missing convenience.
    ///
    /// Pure CST, deliberately: the JVM backend additionally asks the index whether the item is a
    /// `DefKind::Class`, and folding an index lookup in here would make one caller's extra
    /// precondition part of the shared answer.
    pub(crate) fn is_inner_class(node: &SyntaxNode) -> bool {
        node.kind() == SyntaxKind::CLASS_DECL
            && Self::is_nested(node)
            && !Self::has_modifier(node, SyntaxKind::STATIC_KW)
    }

    /// A body's explicit constructor invocation — a bare `this(…)` or `super(…)`.
    ///
    /// JLS §8.8.7 puts it first or nowhere, so only the first statement is examined. Only the bare
    /// forms count: `this.method()` and `super.method()` are qualified calls whose callee is a
    /// field access rather than a name reference.
    pub(crate) fn explicit_constructor_invocation(body: &ast::Block) -> Option<ast::CallExpr> {
        let ast::Stmt::Expr(first) = body.stmts().next()? else {
            return None;
        };
        let ast::Expr::Call(call) = first.expr()? else {
            return None;
        };
        let ast::Expr::NameRef(name) = call.callee()? else {
            return None;
        };
        (Self::has_keyword(name.syntax(), SyntaxKind::THIS_KW)
            || Self::has_keyword(name.syntax(), SyntaxKind::SUPER_KW))
        .then_some(call)
    }

    /// Whether an explicit constructor invocation names `keyword` — `THIS_KW` for the `this(…)`
    /// form, `SUPER_KW` for `super(…)`.
    pub(crate) fn delegates_to(call: &ast::CallExpr, keyword: SyntaxKind) -> bool {
        matches!(call.callee(), Some(ast::Expr::NameRef(name))
            if Self::has_keyword(name.syntax(), keyword))
    }

    /// Whether a constructor body begins with an explicit invocation naming `keyword`.
    pub(crate) fn body_delegates_to(body: &ast::Block, keyword: SyntaxKind) -> bool {
        Self::explicit_constructor_invocation(body)
            .is_some_and(|call| Self::delegates_to(&call, keyword))
    }

    /// The primitive a `TYPE` node's keyword names.
    ///
    /// The JVM backend carried a verbatim second copy of this, down to the keyword list, because the
    /// one here was private and the erasure path needed it.
    pub(crate) fn primitive_of(node: &ast::Type) -> Option<jals_hir::Primitive> {
        use jals_hir::Primitive;
        use jals_syntax::SyntaxKind::{
            BOOLEAN_KW, BYTE_KW, CHAR_KW, DOUBLE_KW, FLOAT_KW, INT_KW, LONG_KW, SHORT_KW,
        };
        node.syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find_map(|token| {
                Some(match token.kind() {
                    BOOLEAN_KW => Primitive::Boolean,
                    BYTE_KW => Primitive::Byte,
                    SHORT_KW => Primitive::Short,
                    CHAR_KW => Primitive::Char,
                    INT_KW => Primitive::Int,
                    LONG_KW => Primitive::Long,
                    FLOAT_KW => Primitive::Float,
                    DOUBLE_KW => Primitive::Double,
                    _ => return None,
                })
            })
    }

    /// Each declarator of a flat declaration, paired with the initialiser written after its own `=`.
    ///
    /// The CST is flat: `int a = 1, b = 2;` is **one** declaration whose names and expressions are
    /// siblings. Pairing them *by index* is right only when every declarator has an initialiser —
    /// and four of the five lowering sites did exactly that. `int a, b = 2;` has one expression and
    /// two names, so index pairing handed `2` to `a` and left `b` unset: as a field that printed
    /// `2 0` where Java prints `0 2`, as a JVM local it read a slot the frame never defined, and as
    /// a wasm local it read that local's zero default.
    ///
    /// So the tokens are walked in order instead: the most recent name owns the next expression, and
    /// a `COMMA` closes it. The names are the declaration's *direct* `IDENT` token children — the
    /// same tokens [`ast::LocalVarDecl::names`] and [`ast::FieldDecl::names`] read, because a
    /// declaration's type is a nested `TYPE` node whose identifiers are not direct children. That
    /// agreement is what lets a caller walk one and index the other without going out of step.
    ///
    /// An unnamed `_` binding is an `UNDERSCORE` token, which neither this nor `names` reports. Its
    /// initialiser is *dropped* rather than handed to the declarator before it — the value is
    /// evaluated for its effect and bound to nothing, and giving it to the previous name would be
    /// the same misalignment one declarator further along.
    pub(crate) fn declarators(decl: &SyntaxNode) -> Vec<(SyntaxToken, Option<ast::Expr>)> {
        let mut out: Vec<(SyntaxToken, Option<ast::Expr>)> = Vec::new();
        // Whether the declarator now open is one this reports — false before the first name, after
        // a `COMMA`, and for an unnamed `_`.
        let mut named = false;
        let mut assigned = false;
        for element in decl.children_with_tokens() {
            match element {
                jals_syntax::SyntaxElement::Token(token) => match token.kind() {
                    SyntaxKind::IDENT => {
                        out.push((token, None));
                        named = true;
                        assigned = false;
                    }
                    SyntaxKind::EQ => assigned = true,
                    // Both close whatever declarator was open without opening a reportable one: a
                    // `COMMA` because the declarator ended, an `UNDERSCORE` because the one it
                    // opens has no name. Either way the next expression belongs to neither, and it
                    // is dropped rather than handed backwards.
                    SyntaxKind::UNDERSCORE | SyntaxKind::COMMA => {
                        named = false;
                        assigned = false;
                    }
                    _ => {}
                },
                jals_syntax::SyntaxElement::Node(node) => {
                    let Some(expr) = ast::Expr::cast(node) else {
                        continue;
                    };
                    if assigned {
                        if named && let Some(last) = out.last_mut() {
                            last.1 = Some(expr);
                        }
                        assigned = false;
                    }
                }
            }
        }
        out
    }

    /// The initialiser the declarator naming `name_start` was given.
    ///
    /// The by-offset shape of [`declarators`](Self::declarators), for the constant evaluator, which
    /// starts from a definition's name range rather than from the declaration.
    fn declarator_initialiser(decl: &SyntaxNode, name_start: usize) -> Option<ast::Expr> {
        Self::declarators(decl)
            .into_iter()
            .find(|(name, _)| usize::from(name.text_range().start()) == name_start)
            .and_then(|(_, value)| value)
    }

    /// Whether a `switch` group's statements leave the arm — a `yield`, a `throw`, or a `return`.
    ///
    /// The colon form falls from one group into the next, so a `switch` *expression* written that
    /// way must not be able to fall out of its last one: JLS §14.22 requires every arm of a value
    /// switch to yield or throw. The JVM backend asks its assembler whether the fall-out point is
    /// still reachable; wasm has no such tracker, so the question is asked of the source instead —
    /// which is the same question, one step earlier.
    ///
    /// Conservative on purpose: an arm that ends in an `if` both of whose branches yield is
    /// reported rather than accepted. Reporting a program that could have compiled is a gap;
    /// accepting one whose last arm yields nothing produced a module that trapped at run time.
    pub(crate) fn arm_leaves(group: &ast::SwitchGroup) -> bool {
        group.stmts().last().is_some_and(|stmt| {
            matches!(
                stmt,
                ast::Stmt::Yield(_) | ast::Stmt::Throw(_) | ast::Stmt::Return(_)
            )
        })
    }

    // --- resolution -----------------------------------------------------------------------------

    /// The definition a name-reference node binds to.
    ///
    /// Keyed by the identifier *token*, not the node: a `NAME_REF` carries its leading trivia and
    /// the resolver indexes references by where the name itself starts.
    ///
    /// The `symbol_at` fallback is load-bearing — it makes this resolve *declaration* sites too, so
    /// a `PARAM` node answers with the definition it declares. Slot allocation depends on it, and
    /// dropping it makes every parameter silently unresolvable.
    pub(crate) fn def_at(self, node: &SyntaxNode) -> Option<DefId> {
        let start = Self::first_name(node)?;
        self.typed
            .analysis()
            .reference_at(start)
            .and_then(|reference| reference.resolution.def_id())
            .or_else(|| self.typed.analysis().symbol_at(start))
    }

    /// A node's own first identifier token. Direct children only.
    ///
    /// The token, not the node's text: a `NAME_REF`'s text runs from the start of its leading
    /// trivia, so a comment on the line above is part of it. The token is the name and everything
    /// else is layout — which is why a lookup keyed on the text of the node found nothing whenever
    /// the name happened to be commented.
    pub(crate) fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == SyntaxKind::IDENT)
    }

    /// Where a node's own first identifier token starts. Direct children only.
    fn first_name(node: &SyntaxNode) -> Option<usize> {
        Self::name_token(node).map(|token| usize::from(token.text_range().start()))
    }

    /// The indexed member a file-local definition declares.
    ///
    /// A name that resolved to a definition but to no *local* is one of the enclosing type's own
    /// fields, written without the `this.` the JVM still requires. Reached by the **declaration's**
    /// offset rather than the reference's — which is the one thing the three hand-written copies
    /// each had to remember.
    pub(crate) fn member_of_def(self, id: DefId) -> Option<MemberId> {
        let declaration = self.typed.analysis().def(id);
        self.index()
            .member_by_decl(self.file(), declaration.name_range.start)
    }

    /// The indexed member the name token `token` declares.
    pub(crate) fn member_at(self, token: &SyntaxToken) -> Result<MemberId> {
        self.index()
            .member_by_decl(self.file(), usize::from(token.text_range().start()))
            .ok_or_else(|| FactError::Unresolved(token.text().into()))
    }

    /// The locals a class declared inside a block captures, in source order and without repeats.
    ///
    /// A capture is what it sounds like: a name inside the class that resolves to a definition
    /// *outside* it. Each becomes a field and a trailing constructor parameter, which is how a
    /// class outlives the frame the local lived in.
    ///
    /// Unlike [`def_at`](Self::def_at) this reads `reference_at` alone: a declaration *inside* the
    /// class is not a capture, and the `symbol_at` fallback would make every one of them look like
    /// one.
    pub(crate) fn captured_by(self, node: &SyntaxNode) -> Vec<DefId> {
        let mut out = Vec::new();
        let inside_block = node
            .ancestors()
            .skip(1)
            .any(|ancestor| ancestor.kind() == SyntaxKind::BLOCK);
        if !inside_block {
            return out;
        }
        let range = node.text_range();
        let analysis = self.typed.analysis();
        for token in node
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::IDENT)
        {
            let Some(id) = analysis
                .reference_at(usize::from(token.text_range().start()))
                .and_then(|reference| reference.resolution.def_id())
            else {
                continue;
            };
            let def = analysis.def(id);
            // Only a *local* is captured: a field of the enclosing class is reached through its
            // instance, and a type name is not a value at all.
            if !matches!(def.kind, DefKind::Local | DefKind::Param) {
                continue;
            }
            let Ok(start) = u32::try_from(def.name_range.start) else {
                continue;
            };
            if !range.contains(start.into()) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned as _;
    use alloc::string::{String, ToString as _};
    use alloc::vec::Vec;

    use jals_exec::block_on_inline;
    use jals_syntax::ast::{self, AstNode as _};
    use jals_syntax::{SyntaxKind, SyntaxNode};

    use super::Facts;

    /// The first declaration of `kind` in `source`.
    fn decl(source: &str, kind: SyntaxKind) -> SyntaxNode {
        block_on_inline(jals_syntax::Parse::parse(source))
            .syntax()
            .descendants()
            .find(|node| node.kind() == kind)
            .expect("the declaration is present")
    }

    /// Each declarator's name, with the initialiser's text.
    fn paired(node: &SyntaxNode) -> Vec<(String, Option<String>)> {
        Facts::declarators(node)
            .into_iter()
            .map(|(name, value)| {
                let written = value.map(|expr| expr.syntax().text().to_string().trim().to_owned());
                (name.text().to_owned(), written)
            })
            .collect()
    }

    /// `int a, b = 2;` gives `2` to **`b`**, and gives `a` nothing.
    ///
    /// The CST is flat — one declaration whose names and expressions are siblings — so pairing them
    /// *by index* is right only when every declarator has an initialiser. Four of the five lowering
    /// sites did exactly that: with one expression and two names the value landed on `a` and `b` was
    /// left unset. As a field that printed `2 0` where Java prints `0 2`; as a JVM local it left a
    /// slot the verifier refuses to read; as a wasm local it left the local's zero default, which is
    /// silent.
    ///
    /// A local and a field are asserted together because they share one grammar rule (`field_tail`),
    /// and the bug lived on the local half for as long as it did *because* the field half was fixed
    /// on its own.
    #[test]
    fn a_declarator_takes_the_value_written_after_its_own_equals() {
        let expected = [
            ("a".to_owned(), None),
            ("b".to_owned(), Some("2".to_owned())),
            ("c".to_owned(), Some("3".to_owned())),
            ("d".to_owned(), None),
        ];
        let local = decl(
            "class C { void m() { int a, b = 2, c = 3, d; } }",
            SyntaxKind::LOCAL_VAR_DECL,
        );
        assert_eq!(paired(&local), expected);

        let field = decl(
            "class C { int a, b = 2, c = 3, d; }",
            SyntaxKind::FIELD_DECL,
        );
        assert_eq!(paired(&field), expected);
    }

    /// The pairing and the `names` accessor read the same tokens, so a lowering that walks one and
    /// indexes the other cannot go out of step.
    ///
    /// Both take the declaration's *direct* `IDENT` children, which is why a declaration's type
    /// contributes no name: `var` and `String` live in a nested `TYPE` node.
    #[test]
    fn the_pairing_lines_up_with_the_names_accessor() {
        let node = decl(
            "class C { void m() { String a = x, b, c = y; } }",
            SyntaxKind::LOCAL_VAR_DECL,
        );
        let declared = ast::LocalVarDecl::cast(node.clone()).expect("a local declaration");
        let names: Vec<String> = declared.names().map(|t| t.text().to_owned()).collect();
        let walked: Vec<String> = paired(&node).into_iter().map(|(name, _)| name).collect();

        assert_eq!(names, ["a", "b", "c"]);
        assert_eq!(walked, names);
    }

    /// An unnamed `_` binding is a declarator neither this nor `names` reports, and its initialiser
    /// is dropped rather than handed to the declarator before it.
    ///
    /// Giving it to the previous name would be the same misalignment one declarator further along —
    /// `a` would take `f()`'s value and the `1` it was written with would be lost.
    #[test]
    fn an_unnamed_binding_takes_its_value_with_it() {
        let node = decl(
            "class C { void m() { var a = 1, _ = f(), b = 2; } }",
            SyntaxKind::LOCAL_VAR_DECL,
        );
        assert_eq!(
            paired(&node),
            [
                ("a".to_owned(), Some("1".to_owned())),
                ("b".to_owned(), Some("2".to_owned())),
            ]
        );
    }

    /// An array initialiser is the declarator's value like any other expression, and a declarator
    /// whose dimensions are written after the name still owns what follows its `=`.
    #[test]
    fn a_declarator_owns_whatever_its_equals_introduces() {
        let node = decl(
            "class C { void m() { int[] a = {1, 2}, b[] = null, c; } }",
            SyntaxKind::LOCAL_VAR_DECL,
        );
        assert_eq!(
            paired(&node),
            [
                ("a".to_owned(), Some("{1, 2}".to_owned())),
                ("b".to_owned(), Some("null".to_owned())),
                ("c".to_owned(), None),
            ]
        );
    }
}
