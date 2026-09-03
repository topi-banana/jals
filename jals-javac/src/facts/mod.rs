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
//! # What the ratchets could not see
//!
//! The three ast-grep rules around this layer stop a backend *naming* the other one, or naming a
//! target's vocabulary from here. None of them stops a backend re-implementing a fact **inline**,
//! and ten of them were, with all three rules green the whole time:
//!
//! - Whether an operand **denotes `null`**, answered by walking an operand's entire subtree for the
//!   keyword. `f(null) == y` therefore read as a null test on `y`, so `x == y` compiled as
//!   `y == null` — a module `wasm-tools` accepts and `wasmtime` runs, returning the wrong answer.
//! - Whether a class **holds an enclosing instance**, in three arms on one side and one on the
//!   other. The layout and the creation site stayed consistent, so nothing miscompiled by that
//!   route; what it cost was an uplevel field read reported as an unresolved *name* and an uplevel
//!   call that pushed the wrong `this` and failed in the validator.
//! - Which member a **field access** names, where one side bypassed the memo for a `this.`
//!   qualifier and re-resolved by name — the answer a `super.` qualifier would have got wrong.
//! - Whether a node is an **anonymous class body**, spelled three ways between the two backends and
//!   four ways within one of them.
//! - And six that agreed: a `super.`-qualified call, the operator a token run spells (eight
//!   decoders, with the sentence explaining the `>` split copied beside four of them), numeric
//!   promotion (JLS §5.6, written *three* times — twice in the backends and once privately in
//!   [`constant`]), `a.length`, a `for`-each's loop variable (one of eight declaration bindings
//!   that reached past this layer to spell the offset key by hand), and a qualified `new`'s
//!   qualifier, where the answer that was right lived in a backend and the one the syntax layer
//!   published was the fragile one.
//!
//! What makes a fact single-sourced is that there is one place to ask **and it has a test**. Every
//! one of the ten is asked here now, and every one is tested here — with no JDK and no engine in
//! reach, because this crate's tests run under `wasm32-wasip1` in CI, where there is neither.
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
mod enclosing;
mod inherit;
mod literal;
mod method_ref;
mod numeric;
mod operator;
mod switch;

pub(crate) use constant::CaseKey;
pub(crate) use inherit::{Hierarchy, Overrides};
pub(crate) use literal::Literal;
pub(crate) use method_ref::RefReceiver;
pub use numeric::Numeric;
pub(crate) use operator::{Operator, Unary};
pub(crate) use switch::ArmLabels;

use alloc::string::String;
use alloc::vec::Vec;

use jals_hir::{DefId, DefKind, FileId, ItemId, MemberId, ProjectIndex, Ty, TypedFile};
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
    /// and `>=` is `[GT, EQ]`.
    ///
    /// Private, and that is the point. Both backends used to read the run and decode it themselves —
    /// eight matches between them, with the sentence above copied beside four. The run now has
    /// exactly three readers, all in [`operator`](super::operator), and what leaves this module is
    /// the decoded [`Operator`] rather than the tokens.
    fn operator(node: &SyntaxNode) -> Vec<SyntaxKind> {
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

    /// Whether a node is the bare `super`.
    ///
    /// Like [`is_this`](Self::is_this) it carries no identifier token, so its keyword is the only
    /// thing that identifies it. What separates the two is what each *forces*: `this.f()` is an
    /// ordinary virtual call, and `super.f()` names one body in particular — the superclass's — so it
    /// is not dispatched at all.
    pub(crate) fn is_super(node: &SyntaxNode) -> bool {
        Self::has_keyword(node, SyntaxKind::SUPER_KW)
    }

    /// The type whose enclosing instance a *qualified* `this` or `super` names — `Outer` in both
    /// `Outer.this` and `Outer.super.f` (JLS §15.8.4, §15.11.2).
    ///
    /// Neither is a member access: the access carries the keyword where a field name would be, so
    /// there is no identifier for a member lookup to use and the ordinary path reports an *empty*
    /// name. Both push the same value — `Outer.super.f` is `Outer.this` with the field looked up one
    /// level higher, and a field is bound by where it is declared rather than by dispatch. What they
    /// do not share is a *call*: `Outer.super.m()` is not dispatched at all, which is why
    /// [`is_qualified_super_call`](Self::is_qualified_super_call) exists beside this.
    pub(crate) fn qualified_enclosing(self, access: &ast::FieldAccess) -> Option<ItemId> {
        access
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| matches!(token.kind(), SyntaxKind::THIS_KW | SyntaxKind::SUPER_KW))?;
        let receiver = access.receiver()?;
        self.ty_of_name(receiver.syntax()).ok()?.project_id()
    }

    /// Whether a call's receiver is a *qualified* `super` — `Outer.super.m()` (JLS §15.11.2) or
    /// `Iface.super.m()` (§15.12.1).
    ///
    /// Both name one body in particular and are not dispatched, and neither is reachable by the
    /// `invokespecial` the bare `super.` uses: JVMS §6.5 lets that name only the direct superclass
    /// or a direct superinterface, and `Outer`'s superclass is neither of the compiled class's.
    /// Reported rather than emitted as a virtual call on the enclosing instance, which is the same
    /// bytes calling the override the source wrote `super` to avoid.
    pub(crate) fn is_qualified_super_call(call: &ast::CallExpr) -> bool {
        let Some(ast::Expr::FieldAccess(callee)) = call.callee() else {
            return false;
        };
        let Some(ast::Expr::FieldAccess(receiver)) = callee.receiver() else {
            return false;
        };
        Self::has_keyword(receiver.syntax(), SyntaxKind::SUPER_KW)
    }

    /// Whether a call is `super.`-qualified: its callee is a field access whose receiver is the
    /// bare `super`.
    ///
    /// The composition, not just the leaf. [`is_super`](Self::is_super) was already shared and both
    /// backends still built this same five-line `matches!` on top of it — character for character,
    /// with the wasm copy carrying a comment saying the fact came "from the same place" as the JVM
    /// one. The atom was shared; the question was not.
    ///
    /// What it decides is whether the call is dispatched at all. `super.f()` names one body in
    /// particular — the superclass's — so selecting by the receiver's *runtime* type is how an
    /// override calling `super.f()` calls itself forever.
    ///
    /// Not `Iface.super.m()` (JLS §15.12.1): that receiver is a qualified name rather than the bare
    /// keyword, and neither backend emits it.
    pub(crate) fn is_super_call(call: &ast::CallExpr) -> bool {
        matches!(
            call.callee(),
            Some(ast::Expr::FieldAccess(ref access))
                if access.receiver().is_some_and(|receiver| Self::is_super(receiver.syntax()))
        )
    }

    /// The expression a qualified `new` names as its enclosing instance — the one written *before*
    /// the `new` keyword. `None` for the unqualified form.
    ///
    /// The position filter is the whole content. `NewExpr::qualifier()` is a generated accessor and
    /// takes the first child castable to an `Expr` with no filter at all, but the grammar puts an
    /// array creation's dimension expression directly under `NEW_EXPR` too — so `new int[n]` hands
    /// back `n` as a "qualifier". Nothing miscompiles today only because both callers of the
    /// generated accessor are guarded against reaching an array creation, and one of those guards
    /// is a `CLASS_BODY` test written for an unrelated reason.
    ///
    /// So the answer that was *right* lived in the wasm backend and the one the syntax layer
    /// published was the fragile one. This is the right answer, in the layer both backends ask.
    pub(crate) fn new_qualifier(new: &ast::NewExpr) -> Option<ast::Expr> {
        let keyword = new
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == SyntaxKind::NEW_KW)?;
        new.syntax()
            .children()
            .filter(|child| child.text_range().end() <= keyword.text_range().start())
            .find_map(ast::Expr::cast)
    }

    /// Whether a method reference names `new` rather than a method.
    ///
    /// `T::new` constructs; every other spelling selects an existing member. The wasm backend asked
    /// this of a collected declaration while [`method_ref`](Self::method_ref) asked it of the
    /// reference node, so the same keyword test was written in two places for one question.
    pub(crate) fn constructs(node: &SyntaxNode) -> bool {
        Self::has_keyword(node, SyntaxKind::NEW_KW)
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

    /// Whether a member declaration sits directly in an interface's or `@interface`'s body.
    ///
    /// Two of Java's implicit-`static` rules hang off this one shape, and both are invisible in the
    /// modifiers: JLS §9.3 makes an interface's field `public static final` however it was written,
    /// and JLS §9.5 makes *every* member type of an interface `static` — a member **class**
    /// included, which is the one case the `static` keyword otherwise decides on its own.
    pub(crate) fn is_interface_member(node: &SyntaxNode) -> bool {
        node.parent().is_some_and(|body| {
            body.kind() == SyntaxKind::CLASS_BODY
                && body.parent().is_some_and(|owner| {
                    matches!(
                        owner.kind(),
                        SyntaxKind::INTERFACE_DECL | SyntaxKind::ANNOTATION_TYPE_DECL
                    )
                })
        })
    }

    /// Whether a member type declaration is `static` — written, or implied by what declares it.
    ///
    /// Three sources, and only the first is a modifier a reader can see:
    ///
    /// 1. the `static` keyword on the declaration;
    /// 2. its *kind* — a member interface, `@interface`, `enum`, or `record` is implicitly `static`
    ///    (JLS §8.5.1, §9.5, §8.9, §8.10), so only a member **class** is ever in question;
    /// 3. its *owner* — every member type of an interface is `static`, class included.
    ///
    /// Two things read the answer and both are wrong without it. An `InnerClasses` entry is the
    /// only place `ACC_STATIC` can be recorded for a nested type (JVMS §4.7.6), so a reader of the
    /// class file is told what the source said only through this; and a class that is *not* static
    /// holds an enclosing instance, so answering `interface I { class C {} }` from the modifier
    /// alone gives `C` a constructor parameter every `new I.C()` in a `static` method has nothing
    /// to pass.
    pub(crate) fn is_static_member_type(node: &SyntaxNode) -> bool {
        node.kind() != SyntaxKind::CLASS_DECL
            || Self::has_modifier(node, SyntaxKind::STATIC_KW)
            || Self::is_interface_member(node)
    }

    /// Whether a class declaration is a non-`static` nested one — an *inner* class.
    ///
    /// A nested interface, `@interface`, `enum`, and `record` are implicitly `static` and hold no
    /// enclosing instance, so only a nested *class* can be one — and not even every one of those,
    /// since a member class of an interface is implicitly `static` too. An inner class holds its
    /// enclosing instance in a synthetic field and every constructor takes it as an extra first
    /// parameter, so a backend that answers this differently from the other emits constructors one
    /// parameter short — a `NoSuchMethodError` at the first `new`, not a missing convenience.
    ///
    /// Arm one of three. It is private because it is no longer the whole question anyone asks:
    /// [`holds_enclosing_instance`](Self::holds_enclosing_instance) composes it with the local- and
    /// anonymous-class arms, and asking this one alone is exactly what left a local class without an
    /// enclosing instance.
    fn is_inner_class(node: &SyntaxNode) -> bool {
        node.kind() == SyntaxKind::CLASS_DECL
            && Self::is_nested(node)
            && !Self::is_static_member_type(node)
    }

    /// Whether `node` sits where there is no `this` — a `static` method, a `static` initialiser, or
    /// a `static` field's initialiser.
    ///
    /// A local or anonymous class declared in such a place holds **no** enclosing instance: there is
    /// none to hand it. The distinction is not cosmetic on either side of the seam. Give such a class
    /// a `this$0` and its constructor takes an argument the `new` has nothing to pass; take one away
    /// from a class in an instance context and every uplevel access it makes is emitted against the
    /// wrong object — well-typed, and reading another instance's fields.
    ///
    /// The walk stops at the first member declaration because that is what owns the `static`-ness. A
    /// type declaration reached first means the node is not inside a member at all, which only a
    /// malformed tree produces; a walk that runs out of ancestors is at the file's top level, where
    /// there is likewise no instance.
    ///
    /// A constructor's **early construction context** counts as such a place even though a
    /// constructor is otherwise the most instance-bound context there is: everything up to and
    /// including its explicit `this(…)` / `super(…)` invocation runs before that invocation
    /// returns, so `this` does not exist yet (JLS §8.8.7.1, widened from the invocation's own
    /// arguments to the whole prologue by JEP 447). javac skips the enclosing instance for an
    /// anonymous class created there for exactly this reason (JDK-8166108), and a backend that
    /// passes one emits `super(this)` from a frame whose `this` is `UninitializedThis`.
    pub(crate) fn in_static_context(node: &SyntaxNode) -> bool {
        for ancestor in node.ancestors() {
            if Self::is_explicit_constructor_invocation(&ancestor) {
                return true;
            }
            match ancestor.kind() {
                // An interface's field is `static` however it was written (JLS §9.3), so its
                // initialiser runs in `<clinit>` and has no `this` — which is the whole of what
                // makes `interface I { Object o = new Object() {}; }` an anonymous class with no
                // enclosing instance rather than one whose `new` pushes an instance that does not
                // exist.
                SyntaxKind::FIELD_DECL => {
                    return Self::has_modifier(&ancestor, SyntaxKind::STATIC_KW)
                        || Self::is_interface_member(&ancestor);
                }
                SyntaxKind::METHOD_DECL | SyntaxKind::INITIALIZER => {
                    return Self::has_modifier(&ancestor, SyntaxKind::STATIC_KW);
                }
                // A constructor is an instance context by definition — there is no `static` one —
                // but only from its delegation onward.
                SyntaxKind::CONSTRUCTOR_DECL => return Self::before_delegation(node, &ancestor),
                _ => {}
            }
        }
        true
    }

    /// Whether `node` sits in `constructor`'s prologue — before its explicit `this(…)` / `super(…)`
    /// invocation, which is the half of an early construction context that is not the invocation
    /// itself.
    ///
    /// JEP 447 is what makes this a range rather than a point. Before it, an explicit invocation was
    /// the body's first statement and the only code that could precede `this` existing was the
    /// invocation's own arguments; now a constructor may run a whole prologue first, and every
    /// statement of it sees an object the verifier will not let anything read.
    fn before_delegation(node: &SyntaxNode, constructor: &SyntaxNode) -> bool {
        let Some(body) = constructor.children().find_map(ast::Block::cast) else {
            return false;
        };
        let Some((at, _)) = Self::explicit_constructor_invocation(&body) else {
            return false;
        };
        body.stmts().take(at).any(|statement| {
            statement
                .syntax()
                .text_range()
                .contains_range(node.text_range())
        })
    }

    /// Whether `node` is a bare `this(…)` or `super(…)` call.
    ///
    /// The shape [`explicit_constructor_invocation`](Self::explicit_constructor_invocation) looks for,
    /// asked of one node rather than found as a body's first statement — which is what an upward walk
    /// out of an argument needs.
    fn is_explicit_constructor_invocation(node: &SyntaxNode) -> bool {
        node.kind() == SyntaxKind::CALL_EXPR
            && ast::CallExpr::cast(node.clone())
                .and_then(|call| call.callee())
                .is_some_and(|callee| {
                    matches!(&callee, ast::Expr::NameRef(name)
                        if Self::has_keyword(name.syntax(), SyntaxKind::THIS_KW)
                            || Self::has_keyword(name.syntax(), SyntaxKind::SUPER_KW))
                })
    }

    /// A body's explicit constructor invocation — a bare `this(…)` or `super(…)` — and **which**
    /// top-level statement it is.
    ///
    /// It used to be the first statement or nowhere, and the position was therefore implicit. JEP
    /// 447 (JLS §8.8.7 as of Java 25) admits a *prologue* of statements before it, so the position
    /// is now part of the answer: everything before it runs while `this` is still
    /// `uninitializedThis`, and everything after it runs on a fully constructed object. A reader
    /// that looked only at the first statement did not merely miss the delegation — it emitted the
    /// implicit `super()` prologue **as well as** the explicit call the body still contains, so
    /// `Object.<init>` ran twice on one object.
    ///
    /// Only the bare forms count: `this.method()` and `super.method()` are qualified calls whose
    /// callee is a field access rather than a name reference. Only the *top level* of the body is
    /// scanned, since an invocation nested inside a block or a branch is not a Java program.
    pub(crate) fn explicit_constructor_invocation(
        body: &ast::Block,
    ) -> Option<(usize, ast::CallExpr)> {
        body.stmts().enumerate().find_map(|(at, statement)| {
            let ast::Stmt::Expr(statement) = statement else {
                return None;
            };
            let ast::Expr::Call(call) = statement.expr()? else {
                return None;
            };
            let ast::Expr::NameRef(name) = call.callee()? else {
                return None;
            };
            (Self::has_keyword(name.syntax(), SyntaxKind::THIS_KW)
                || Self::has_keyword(name.syntax(), SyntaxKind::SUPER_KW))
            .then_some((at, call))
        })
    }

    /// Whether an explicit constructor invocation names `keyword` — `THIS_KW` for the `this(…)`
    /// form, `SUPER_KW` for `super(…)`.
    pub(crate) fn delegates_to(call: &ast::CallExpr, keyword: SyntaxKind) -> bool {
        matches!(call.callee(), Some(ast::Expr::NameRef(name))
            if Self::has_keyword(name.syntax(), keyword))
    }

    /// Whether a constructor body carries an explicit invocation naming `keyword`.
    pub(crate) fn body_delegates_to(body: &ast::Block, keyword: SyntaxKind) -> bool {
        Self::explicit_constructor_invocation(body)
            .is_some_and(|(_, call)| Self::delegates_to(&call, keyword))
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
                    if !assigned {
                        continue;
                    }
                    assigned = false;
                    // `named` is set only where a pair was just pushed, so there is one to fill.
                    if named && let Some(last) = out.last_mut() {
                        last.1 = Some(expr);
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
        self.def_at_token(&Self::name_token(node)?)
    }

    /// The definition a name *token* binds to — [`def_at`](Self::def_at) for a name that has no
    /// node of its own.
    ///
    /// Four declarations are spelled that way, and every one of them is a bare `IDENT` with nothing
    /// to hand `def_at`: a local variable's declarator, a `for`-each's loop variable, a
    /// try-with-resources binding, and a `catch` parameter. Both backends therefore reached *past*
    /// this layer for all four and called `analysis().symbol_at(offset)` themselves — eight copies
    /// of one lookup, each spelling the offset key by hand. They agreed, which is the only reason
    /// it never showed; `Facts::span`'s whole reason for existing is that a key spelled twice is a
    /// key that can be spelled differently.
    ///
    /// The `reference_at`-then-`symbol_at` order is [`def_at`](Self::def_at)'s, and it is
    /// single-sourced here rather than restated — `def_at` now goes through this. A declaration site
    /// has no reference, so it falls through to `symbol_at` and all eight callers keep the answer
    /// they had.
    pub(crate) fn def_at_token(self, token: &SyntaxToken) -> Option<DefId> {
        let start = usize::from(token.text_range().start());
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

    /// Whether an expression *denotes* `null` — its only possible value is the null reference.
    ///
    /// Asked of the inference, not of the syntax, because that is the question a reference
    /// comparison needs answered: `x == <anything whose type is null>` is a null test, and
    /// `x == <anything else>` is an identity test. `Ty::Null` is the type of the null literal
    /// (JLS §4.1) and nothing else has it, so the memo is exactly this predicate.
    ///
    /// It is strictly better than either thing that was here before, and both were wrong in
    /// opposite directions:
    ///
    /// - A **subtree** walk for a `NULL_KW` token — what the wasm backend had — says `true` for any
    ///   operand that merely *contains* a `null` somewhere. `f(null) == y` took that branch, so the
    ///   left operand was dropped and `ref.is_null` was applied to `y`: `x == y` compiled as
    ///   `y == null`. It validated, it did not trap, and it returned the wrong answer.
    /// - A **own-token** test says `false` for `(null)` and for `(c ? null : null)`, both of which
    ///   denote null and neither of which has the keyword as a direct child.
    ///
    /// The own-token fallback is only for a node the inference recorded nothing against; it can
    /// only ever add the bare literal, never a subtree.
    ///
    /// Distinct from the `NULL_KW` arm in each backend's *literal* dispatch, which asks which of
    /// the seven literal kinds a token is on the way to emitting one. That is a question about a
    /// token, and this is a question about an expression.
    pub(crate) fn denotes_null(self, node: &SyntaxNode) -> bool {
        self.typed.type_of_expr(Self::span(node)).map_or_else(
            || Self::has_keyword(node, SyntaxKind::NULL_KW),
            |ty| matches!(ty, Ty::Null),
        )
    }

    /// The indexed member a field access names.
    ///
    /// The inference memo, unconditionally. It was worth measuring rather than assuming, because
    /// the two backends disagreed about whether it could be: the wasm copy carried a comment saying
    /// "`this` has no inferred type, so the analysis records no target for it" and re-resolved a
    /// `this.`-qualified access **by name** on the enclosing class instead. That comment is not
    /// true. The memo answers `this.x` in every shape there is — a field that hides an inherited
    /// one, an inherited one, an interface constant, and one reached from an inner, a `static`
    /// nested, or a local class.
    ///
    /// Where the two *do* differ, the memo is the one that is right. `super.x` names the hidden
    /// field and a name lookup on the enclosing class names the hiding one, because a field is
    /// hidden rather than overridden (JLS §15.11.2) and both targets name where the field is
    /// declared. The name lookup only ever agreed because nothing asked it about `super`.
    pub(crate) fn field_target(self, access: &ast::FieldAccess) -> Result<MemberId> {
        self.typed
            .field_target_of(Self::span(access.syntax()))
            .ok_or_else(|| {
                FactError::Unresolved(
                    Self::field_token(access).map_or_else(String::new, |token| token.text().into()),
                )
            })
    }

    /// A field access's own name token — the `IDENT` after the dot.
    ///
    /// The *last* one, matching `FieldAccess::field`: a receiver is a node rather than a token, so
    /// there is normally only one, and taking the last is what survives a tree where there is not.
    fn field_token(access: &ast::FieldAccess) -> Option<SyntaxToken> {
        access
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::IDENT)
            .last()
    }

    /// Whether a field access is really the array-length operator.
    ///
    /// `a.length` on an array is not a member access at all — both targets answer it with an
    /// instruction, so the index resolved no member and there is nothing for a field lookup to
    /// find. The *classification* is a source question and the two emissions are not, which is why
    /// only this half is shared.
    ///
    /// The name is compared decoded. `FieldAccess::field` hands back the token's raw text, so both
    /// backends' `== Some("length")` missed `a.length` — legal Java (JLS §3.3) that then took
    /// the member path and reported `length` as unresolved.
    pub(crate) fn is_array_length(self, access: &ast::FieldAccess) -> bool {
        let Some(token) = Self::field_token(access) else {
            return false;
        };
        if jals_syntax::decoded_ident(&token) != "length" {
            return false;
        }
        access.receiver().is_some_and(|receiver| {
            matches!(
                self.typed.type_of_expr(Self::span(receiver.syntax())),
                Some(Ty::Array(_))
            )
        })
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

    /// Whether each call in `source` is `super.`-qualified, in source order.
    fn super_calls(source: &str) -> Vec<bool> {
        block_on_inline(jals_syntax::Parse::parse(source))
            .syntax()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .map(|call| Facts::is_super_call(&call))
            .collect()
    }

    /// Only the bare `super` receiver counts — not `this.`, not a name that merely spells it.
    ///
    /// Both backends built this same `matches!` on top of the shared `Facts::is_super`, character
    /// for character. The atom was shared and the composition was not, which is the shape the three
    /// ast-grep ratchets cannot see: neither copy named the other backend.
    ///
    /// What it decides is whether the call is dispatched. Answer `true` for `this.f()` and an
    /// override stops being reachable; answer `false` for `super.f()` and an override that calls it
    /// calls itself forever.
    #[test]
    fn only_a_bare_super_receiver_makes_a_call_undispatched() {
        assert_eq!(
            super_calls(
                "class C extends B { void m(C sup) { super.f(); this.f(); f(); sup.f(); } }"
            ),
            [true, false, false, false]
        );
    }

    /// What each `new` in `source` names as its qualifier, as written.
    fn qualifiers(source: &str) -> Vec<Option<String>> {
        block_on_inline(jals_syntax::Parse::parse(source))
            .syntax()
            .descendants()
            .filter_map(ast::NewExpr::cast)
            .map(|new| {
                Facts::new_qualifier(&new)
                    .map(|expr| expr.syntax().text().to_string().trim().to_owned())
            })
            .collect()
    }

    /// The qualifier is what sits *before* the `new` keyword, and an array's dimension does not.
    ///
    /// `NewExpr::qualifier()` is generated as "the first child castable to an `Expr`" with no
    /// position filter, but the grammar puts an array creation's dimension expression directly under
    /// `NEW_EXPR` — so it answers `n` for `new int[n]`. Nothing miscompiles today only because both
    /// of its callers are guarded against reaching an array creation, one of them by a `CLASS_BODY`
    /// test written for an unrelated reason. The answer that was right lived in the wasm backend;
    /// this is that answer, in the layer both backends ask.
    #[test]
    fn a_qualifier_is_what_precedes_the_new_keyword() {
        assert_eq!(
            qualifiers(
                "class C { void m(C outer) { outer.new Inner(); new Inner(); new int[n]; } }"
            ),
            [Some("outer".to_owned()), None, None]
        );
    }

    /// `source` analysed and bound, ready to ask a resolution fact of.
    ///
    /// The chain is spelled out rather than hidden behind a helper returning a [`Facts`]: a
    /// `TypedFile` borrows the binding, which borrows the analysis *and* the index, so nothing
    /// shorter than the whole chain can be handed back. Each test therefore writes it, and the
    /// stdlib stubs are folded in because they are compile-time constants parsed in memory rather
    /// than a host read.
    macro_rules! bound {
        ($source:expr, $facts:ident, $root:ident => $body:block) => {{
            let $root = block_on_inline(jals_syntax::Parse::parse($source)).syntax();
            let analysis = block_on_inline(jals_hir::FileAnalysis::of(&$root));
            let index = block_on_inline(
                jals_hir::ProjectIndex::builder(&[(jals_hir::FileId(0), $root.clone())])
                    .with_stdlib()
                    .build(),
            );
            let semantics = analysis.in_project(&index, jals_hir::FileId(0));
            let $facts = Facts::of(block_on_inline(semantics.typed()));
            $body
        }};
    }

    /// A `for`-each's loop variable is a bare token, and it still binds.
    ///
    /// The grammar gives it an `IDENT` with no node of its own, so `def_at` has nothing to take.
    /// Both backends therefore called `analysis().symbol_at(offset)` directly — the one name binding
    /// in either of them that spelled its own key, written twice and agreeing only by luck.
    #[test]
    fn a_for_each_variable_binds_through_its_own_token() {
        bound!(
            "class C { int m(int[] xs) { int t = 0; for (int v : xs) { t += v; } return t; } }",
            facts,
            root => {
                let statement = root
                    .descendants()
                    .find_map(ast::ForEachStmt::cast)
                    .expect("the `for`-each is present");
                let name = statement.name_token().expect("the variable has a name");
                let id = facts
                    .def_at_token(&name)
                    .expect("the loop variable binds to its own declaration");
                assert_eq!(facts.typed().analysis().def(id).name, "v");
            }
        );
    }

    /// Whether each operand of each `==` in `source` denotes null, left then right, in order.
    fn null_operands(source: &str) -> Vec<(bool, bool)> {
        bound!(source, facts, root => {
            root.descendants()
                .filter_map(ast::BinaryExpr::cast)
                .filter(|binary| {
                    super::Operator::binary(binary.syntax()) == Some(super::Operator::Eq)
                })
                .map(|binary| {
                    let side = |expr: Option<ast::Expr>| {
                        expr.is_some_and(|expr| facts.denotes_null(expr.syntax()))
                    };
                    (side(binary.lhs()), side(binary.rhs()))
                })
                .collect()
        })
    }

    /// Denoting null is a question about the *expression*, not about whether a `null` is written
    /// anywhere inside it.
    ///
    /// This is the shape of a confirmed miscompile. The wasm backend asked it by walking the whole
    /// subtree for a `NULL_KW` token, so `pick(x, null) == y` answered `true` on the **left** — and
    /// `reference_equality` then dropped that operand and applied `ref.is_null` to `y`. `x == y`
    /// compiled as `y == null`: the module validated, `wasmtime` did not trap, and the answer was
    /// simply wrong. `pick(x, null) == x` returned 0 where Java returns 1.
    ///
    /// The opposite error is just as available: an own-token test misses `(null)` and
    /// `(c ? null : null)`, both of which denote null and neither of which carries the keyword as a
    /// direct child. Only the inference answers all five rows, which is why this asks the memo.
    ///
    /// Both operand orders are asserted because the lowering tests the right side first — a fix that
    /// only corrected one branch would still pass a test written in one order.
    #[test]
    fn denoting_null_is_about_the_expression_not_a_token_somewhere_inside_it() {
        assert_eq!(
            null_operands(
                "class N {
                     static N pick(N a, N b) { return a; }
                     static boolean m(N x, N y, boolean c) {
                         return x == null
                             || x == (null)
                             || x == (c ? null : null)
                             || pick(x, null) == y
                             || x == pick(y, null)
                             || x == y;
                     }
                 }"
            ),
            [
                (false, true),  // x == null
                (false, true),  // x == (null)          — an own-token test misses this
                (false, true),  // x == (c ? null : null) — and this
                (false, false), // pick(x, null) == y   — the subtree walk said `true` here
                (false, false), // x == pick(y, null)   — and here, in the mirror order
                (false, false), // x == y
            ]
        );
    }

    /// Which class declares the member each field access names, in source order.
    fn field_owners(source: &str) -> Vec<String> {
        bound!(source, facts, root => {
            root.descendants()
                .filter_map(ast::FieldAccess::cast)
                .map(|access| {
                    facts.field_target(&access).map_or_else(
                        |error| alloc::format!("{error}"),
                        |member| facts.index().item(facts.index().member(member).owner).fqn.to_string(),
                    )
                })
                .collect()
        })
    }

    /// `this.x` and `super.x` name *different* fields when one hides the other, and the memo is
    /// what knows which.
    ///
    /// A field is hidden rather than overridden (JLS §15.11.2), and both targets name the class
    /// where the field is declared — so getting this wrong reads the wrong object's slot, silently.
    ///
    /// One backend re-resolved a `this.`-qualified access **by name** on the enclosing class,
    /// behind a comment claiming the analysis records no target for `this`. It does. The name
    /// lookup happens to agree for `this.x`, and it answers `Sub` for `super.x` — the hiding field
    /// rather than the hidden one. It never showed because that branch tested for `this` and
    /// `super.x` fell through to the memo; a branch widened to cover both would have been a
    /// miscompile the day it was written.
    #[test]
    fn a_hidden_field_is_named_by_the_memo_and_not_by_a_name_lookup() {
        assert_eq!(
            field_owners(
                "class Base { int x = 1; int inherited = 7; }
                 class Sub extends Base {
                     int x = 2;
                     int a() { return this.x; }
                     int b() { return super.x; }
                     int c() { return this.inherited; }
                 }"
            ),
            ["Sub", "Base", "Base"]
        );
    }

    /// Whether each field access in `source` is the array-length operator, in source order.
    fn array_lengths(source: &str) -> Vec<bool> {
        bound!(source, facts, root => {
            root.descendants()
                .filter_map(ast::FieldAccess::cast)
                .map(|access| facts.is_array_length(&access))
                .collect()
        })
    }

    /// `a.length` is the operator only when the receiver really is an array — and the name is
    /// compared *decoded*.
    ///
    /// `FieldAccess::field` hands back the token's raw text, so both backends' `== Some("length")`
    /// missed `a.length` written with a unicode escape. That is legal Java (JLS §3.3): the escape is
    /// resolved before the lexer runs, so `length` *is* the identifier `length`. Taking the
    /// member path instead reports `length` as unresolved on a program javac compiles.
    #[test]
    fn an_array_length_is_the_operator_and_the_name_is_decoded() {
        assert_eq!(
            array_lengths(
                "class C { int[] a; C c; int m() { return a.length + c.length + a.\\u006cength; } }"
            ),
            [true, false, true]
        );
    }
}
