//! `unreported-exception` analysis: a **checked** exception a method / constructor body can raise
//! that is neither declared in its `throws` clause nor handled by an enclosing `try` / `catch`.
//!
//! This is javac's "unreported exception X; must be caught or declared to be thrown". It is the
//! project-aware counterpart of the type-mismatch analysis and is built on the same machinery: the
//! file's type inference for expression types, call-target resolution for what a call may raise,
//! and [`ProjectIndex::is_subtype`] for the exception-hierarchy walk.
//!
//! **Conservative — never a false positive.** A source is reported only when every fact it depends on
//! is *provable* from the index:
//! - The raised type resolves to an indexed type whose supertype chain shows it is a checked
//!   exception (a `Throwable` that is not a `RuntimeException` / `Error`). A type whose chain reaches
//!   an un-indexed (external) supertype cannot be classified and is skipped.
//! - A call's propagated exceptions are the **intersection** of the declared `throws` across every
//!   overload the call's arity could bind to — so an exception is attributed to the call only if
//!   *whichever* overload is actually selected declares it.
//! - An enclosing `try` whose `catch` clause *might* (but cannot be proven to) catch the exception —
//!   e.g. a `catch` type that does not resolve to an indexed type — suppresses the report.
//!
//! It requires a [`ProjectIndex`] with the standard-library stubs folded in (so `Throwable` and the
//! `RuntimeException` / `Error` cut resolve); with the stubs absent it reports nothing. There is no
//! index-free shape at all — it hangs off [`FileSemantics`](crate::FileSemantics), so "no project"
//! is a receiver a caller does not have rather than an argument it passes.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxKind::{
    CALL_EXPR, CLASS_BODY, CONSTRUCTOR_DECL, INITIALIZER, LAMBDA_EXPR, METHOD_DECL, NEW_EXPR,
    THROW_STMT, TRY_STMT,
};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

use crate::def::{DefKind, Namespace};
use crate::infer::TypeInference;
use crate::project::{FileId, ItemId, ProjectIndex, TypeResolution};
use crate::resolve::collect::Collect;

/// A checked exception a method / constructor can raise that is neither declared nor caught.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreportedException {
    /// The byte range of the raising site (the `throw`, method call, or `new`).
    pub range: Range<usize>,
    /// The simple name of the unreported checked exception.
    pub name: String,
}

impl crate::analysis::FileSemantics<'_> {
    /// Every checked exception raised in the file that its enclosing method / constructor neither
    /// declares in `throws` nor catches.
    ///
    /// Needs the project: checked / unchecked classification walks the `Throwable` hierarchy and
    /// a `throws` clause may name a type from another file. Returns empty when the index does not
    /// model the top of that hierarchy (no stdlib stubs folded in), because then nothing can be
    /// classified checked at all.
    pub async fn unreported_exceptions(&self) -> Vec<UnreportedException> {
        let (index, file) = (self.index(), self.file());
        // Without the modelled top of the `Throwable` hierarchy nothing can be classified checked.
        let Some(classifier) = Classifier::new(index, file) else {
            return Vec::new();
        };
        let typed = self.typed().await;
        let cx = Cx {
            index,
            file,
            ti: typed.inference(),
            classifier,
        };
        let mut yielder = jals_exec::Yielder::new();
        let mut out = Vec::new();
        for node in self.root().descendants() {
            yielder.tick().await;
            if matches!(node.kind(), METHOD_DECL | CONSTRUCTOR_DECL) {
                cx.check_decl(&node, &mut out).await;
            }
        }
        out
    }
}

/// The well-known exception items that partition the `Throwable` hierarchy into checked / unchecked.
struct Classifier {
    throwable: ItemId,
    runtime_exception: ItemId,
    error: ItemId,
}

impl Classifier {
    /// Resolve `java.lang.Throwable` / `RuntimeException` / `Error` (via the stdlib stubs). `None` when
    /// they are not indexed — the analysis then cannot classify anything and is a no-op.
    fn new(index: &ProjectIndex, file: FileId) -> Option<Self> {
        let resolve = |simple, fqn| match index.resolve_type_name(file, simple, Some(fqn)) {
            TypeResolution::Project(id) => Some(id),
            _ => None,
        };
        Some(Self {
            throwable: resolve("Throwable", "java.lang.Throwable")?,
            runtime_exception: resolve("RuntimeException", "java.lang.RuntimeException")?,
            error: resolve("Error", "java.lang.Error")?,
        })
    }

    /// Whether `exc` is a *checked* exception: a `Throwable` that is neither a `RuntimeException` nor
    /// an `Error`. Uses the raw supertype walk ([`ProjectIndex::is_subtype`]) — **not**
    /// [`Ty::is_assignable_to`](crate::Ty::is_assignable_to), which demotes stub types to lenient
    /// externals and would answer `true` unconditionally. A type whose chain does not provably reach
    /// `Throwable` (it hits an un-indexed supertype first) is not checked, so it is left alone.
    fn is_checked(&self, index: &ProjectIndex, exc: ItemId) -> bool {
        index.is_subtype(exc, self.throwable)
            && !index.is_subtype(exc, self.runtime_exception)
            && !index.is_subtype(exc, self.error)
    }
}

/// The per-file resolution context shared across every declaration checked.
struct Cx<'a> {
    index: &'a ProjectIndex,
    file: FileId,
    ti: &'a TypeInference,
    classifier: Classifier,
}

impl Cx<'_> {
    /// Report each checked exception raised directly in `decl`'s body that it neither declares nor
    /// catches. Sources inside a nested throws boundary (a lambda, a local/anonymous-class method) are
    /// left to that boundary's own check.
    async fn check_decl(&self, decl: &SyntaxNode, out: &mut Vec<UnreportedException>) {
        let body = ast::MethodDecl::cast(decl.clone())
            .and_then(|m| m.body())
            .or_else(|| ast::ConstructorDecl::cast(decl.clone()).and_then(|c| c.body()));
        let Some(body) = body else {
            return; // an abstract / interface method (`;`) has no body.
        };
        let declared = self.declared_throws(decl);
        let mut yielder = jals_exec::Yielder::new();
        for node in body.syntax().descendants() {
            yielder.tick().await;
            // Only a throw / call / `new` can raise; skip the boundary walk for every other node.
            if !matches!(node.kind(), THROW_STMT | CALL_EXPR | NEW_EXPR) {
                continue;
            }
            // Only sources whose nearest throws boundary is *this* declaration belong to it.
            if Self::nearest_throws_boundary(&node).as_ref() != Some(decl) {
                continue;
            }
            for exc in self.raised_at(&node) {
                if !self.classifier.is_checked(self.index, exc) {
                    continue;
                }
                if declared.iter().any(|&d| self.index.is_subtype(exc, d)) {
                    continue; // covered by the `throws` clause.
                }
                if self.handled_by_catch(&node, decl, exc) {
                    continue; // caught by an enclosing `try`.
                }
                out.push(UnreportedException {
                    range: Collect::node_span(&node),
                    name: self.index.item(exc).fqn.simple_name().into(),
                });
            }
        }
    }

    /// The indexed types named in `decl`'s `throws` clause (unresolvable names dropped).
    fn declared_throws(&self, decl: &SyntaxNode) -> Vec<ItemId> {
        ProjectIndex::throws_clause_types(decl)
            .filter_map(|ty| self.resolve_type(&ty))
            .collect()
    }

    /// The exceptions a single source node can raise: the thrown type of a `throw`, or the intersection
    /// of the `throws` of a call / `new`'s bindable overloads. Empty when the node is not a source or
    /// nothing is provably raised.
    fn raised_at(&self, node: &SyntaxNode) -> Vec<ItemId> {
        match node.kind() {
            THROW_STMT => {
                let Some(thrown) = ast::ThrowStmt::cast(node.clone()).and_then(|t| t.expr()) else {
                    return Vec::new();
                };
                // Precise rethrow first (JLS §11.2.2), because the parameter's own type is the
                // wrong answer for it.
                self.rethrown_arms(&thrown)
                    .unwrap_or_else(|| self.expr_item(thrown.syntax()).into_iter().collect())
            }
            CALL_EXPR => ast::CallExpr::cast(node.clone())
                .map(|call| self.call_throws(&call))
                .unwrap_or_default(),
            NEW_EXPR => ast::NewExpr::cast(node.clone())
                .filter(|n| n.args().is_some()) // a constructor call, not array creation.
                .map(|n| self.new_throws(&n))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The checked exceptions a method call is guaranteed to propagate: the intersection of the
    /// declared `throws` across every overload whose arity the call could bind to. Intersecting keeps
    /// it sound — an exception is attributed only if *whichever* overload resolves declares it.
    fn call_throws(&self, call: &ast::CallExpr) -> Vec<ItemId> {
        let Some((owner, name)) = self.ti.call_target(call, self.index, self.file) else {
            return Vec::new();
        };
        let argc = Self::arg_count(call.args());
        let candidates: Vec<crate::MemberId> = self
            .index
            .resolve_members_all(owner, &name, Namespace::Method)
            .into_iter()
            .filter(|&id| {
                let m = self.index.member(id);
                m.kind == DefKind::Method && Self::applies_to_arity(m, argc)
            })
            .collect();
        self.intersect_member_throws(&candidates)
    }

    /// The checked exceptions a constructor invocation (`new T(..)`) is guaranteed to propagate, by the
    /// same intersection-over-bindable-overloads rule as [`call_throws`](Self::call_throws).
    fn new_throws(&self, new: &ast::NewExpr) -> Vec<ItemId> {
        // The constructed type: the inferred type of the `new` expression, else its written type name.
        let Some(owner) = self
            .expr_item(new.syntax())
            .or_else(|| new.ty().and_then(|ty| self.resolve_type(&ty)))
        else {
            return Vec::new();
        };
        let argc = Self::arg_count(new.args());
        // Constructors are never inherited, so only `owner`'s own members can apply — no supertype walk.
        let candidates: Vec<crate::MemberId> = self
            .index
            .own_members(owner)
            .iter()
            .copied()
            .filter(|&id| {
                let m = self.index.member(id);
                m.kind == DefKind::Constructor && Self::applies_to_arity(m, argc)
            })
            .collect();
        self.intersect_member_throws(&candidates)
    }

    /// The intersection of the resolvable `throws` items across `members`. Empty when `members` is
    /// empty or the intersection is empty (nothing is thrown by *every* candidate).
    fn intersect_member_throws(&self, members: &[crate::MemberId]) -> Vec<ItemId> {
        members
            .iter()
            .map(|&id| self.member_throws(id))
            .reduce(|mut acc, next| {
                acc.retain(|id| next.contains(id));
                acc
            })
            .unwrap_or_default()
    }

    /// The resolvable exception items a single member declares in its `throws`, in its declaring
    /// file's context.
    fn member_throws(&self, id: crate::MemberId) -> Vec<ItemId> {
        let member = self.index.member(id);
        member
            .throws
            .iter()
            .filter_map(|mt| {
                self.index
                    .member_type_to_ty(member.file, member.owner, Some(id), mt)
                    .project_id()
            })
            .collect()
    }

    /// Whether the exception `exc` raised at `source` is caught by a `try` enclosing `source` within
    /// `decl`. A `try` protects a source only when the source lies in its guarded region (the `try`
    /// block or its resources), not in a `catch` / `finally`. A guarded `try` with a `catch` clause
    /// that *might* catch `exc` — one whose caught type does not resolve — conservatively suppresses.
    fn handled_by_catch(&self, source: &SyntaxNode, decl: &SyntaxNode, exc: ItemId) -> bool {
        for ancestor in source.ancestors() {
            if &ancestor == decl {
                break;
            }
            if ancestor.kind() != TRY_STMT {
                continue;
            }
            let Some(try_stmt) = ast::TryStmt::cast(ancestor.clone()) else {
                continue;
            };
            if !Self::guards(&try_stmt, source) {
                continue; // the source is in this try's catch / finally, not its protected region.
            }
            for catch in try_stmt.catches() {
                for caught in catch.types() {
                    // A resolvable catch type that is a supertype of `exc` catches it; an
                    // unresolvable one *might* catch it, so it suppresses too — conservative.
                    if self
                        .resolve_type(&caught)
                        .is_none_or(|ct| self.index.is_subtype(exc, ct))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// The indexed item an expression's inferred type denotes, if it is a project/stub/classpath type.
    fn expr_item(&self, expr: &SyntaxNode) -> Option<ItemId> {
        self.ti.type_of_expr(Collect::node_span(expr))?.project_id()
    }

    /// The exception types a `throw e;` propagates when `e` is the `catch` parameter of an enclosing
    /// clause — its **arms**, not its own type (JLS §11.2.2's precise rethrow). `None` when the
    /// statement is not that shape, in which case the thrown expression's type is the answer.
    ///
    /// It matters most for a **multi-catch**, whose parameter's type is the *lub* of its arms:
    /// `catch (RuntimeException | Error e) { throw e; }` rethrows two unchecked exceptions and the
    /// lub of them is `Throwable`, which is checked. Reading the parameter's type there asks a
    /// method to declare `throws Throwable` for a rethrow that can raise neither — the diagnostic
    /// this rule was added to Java 7 to prevent, alongside multi-catch itself.
    ///
    /// The parameter must be **effectively final**: an assignment to it takes the rule away, because
    /// what it then holds is anything of its declared type. That is the whole of §11.2.2's
    /// precondition, and it is checked here rather than assumed.
    ///
    /// Matched by name against the enclosing clause rather than through the resolver — JLS §14.20
    /// forbids shadowing a `catch` parameter inside its own block — but **only up to the enclosing
    /// declaration space**. That rule governs locals and parameters of the same method; a *class*
    /// written inside the catch block is a new declaration space and may legally declare a field, a
    /// parameter, or a local of the name. `catch (IOException e) { … new R() { RuntimeException e =
    /// …; public void run() { throw e; } } … }` is a program javac compiles, and reading the outer
    /// clause's arms for it reported an `IOException` nothing can raise. A `CLASS_BODY` is where the
    /// walk stops, because a class body is what every one of those shapes needs.
    fn rethrown_arms(&self, thrown: &ast::Expr) -> Option<Vec<ItemId>> {
        let ast::Expr::NameRef(name) = thrown else {
            return None;
        };
        let name = Collect::first_ident_token(name.syntax())?;
        let name = jals_syntax::decoded_ident(&name);
        let clause = thrown
            .syntax()
            .ancestors()
            .take_while(|ancestor| ancestor.kind() != CLASS_BODY)
            .find_map(|ancestor| {
                let clause = ast::CatchClause::cast(ancestor)?;
                let binding = clause.binding()?;
                (jals_syntax::decoded_ident(&binding) == name).then_some(clause)
            })?;
        if Self::reassigns(&clause, &name) {
            return None;
        }
        let arms: Vec<ItemId> = clause
            .syntax()
            .children()
            .filter_map(ast::Type::cast)
            .filter_map(|ty| self.resolve_type(&ty))
            .collect();
        Some(self.narrowed_to_the_block(&clause, &arms))
    }

    /// §11.2.2's actual answer: not the written arms, but **what the `try` block can raise** that
    /// this clause is the one to catch.
    ///
    /// The arms are an upper bound the source wrote, and the rule exists precisely to improve on it.
    /// `class MyEx extends IOException {}` with `try { throw new MyEx(); } catch (IOException e) {
    /// throw e; }` needs `throws MyEx` and no more — javac compiles it — while the written arm says
    /// `IOException` and reported a method that declared exactly what it raises. An exception a
    /// *preceding* clause already catches is not this one's either.
    ///
    /// **A walk that finds nothing falls back to the arms**, and that is not the conservative
    /// direction it looks like. `raised_at` answers nothing for a call it cannot resolve, and in the
    /// stub configuration — which is what `jals lint` and the LSP run in — most library calls do not
    /// resolve: `try { in.read(); } catch (IOException e) { throw e; }` would report nothing at all,
    /// and a call is the *primary* rethrow shape. So an empty walk is read as "the analysis saw
    /// nothing", which on valid input is the only thing it can mean — JLS §11.2.3 makes a `catch` of
    /// a checked type the block cannot throw a compile error, so a clause that exists has something
    /// to catch.
    ///
    /// Falling back to the arms does not undo what this rule was added for: the multi-catch defect
    /// was the parameter's **lub** (`RuntimeException | Error` is `Throwable`, which is checked),
    /// never the arms, and a fallback to those two arms still reports neither. What it does keep is
    /// the pre-existing over-report on `catch (Exception e) { throw e; }` over a block whose raise
    /// the analysis could not see — the same answer that shape got before this rule existed.
    fn narrowed_to_the_block(&self, clause: &ast::CatchClause, arms: &[ItemId]) -> Vec<ItemId> {
        let Some(try_stmt) = clause.syntax().parent().and_then(ast::TryStmt::cast) else {
            return arms.to_vec();
        };
        // The clauses written before this one, whose catches are not this one's to rethrow.
        let preceding: Vec<ItemId> = try_stmt
            .catches()
            .take_while(|earlier| earlier.syntax() != clause.syntax())
            .flat_map(|earlier| earlier.types().collect::<Vec<_>>())
            .filter_map(|ty| self.resolve_type(&ty))
            .collect();
        let mut out = Vec::new();
        for source in try_stmt.syntax().descendants() {
            if !Self::guards(&try_stmt, &source) {
                continue; // in a `catch` or `finally`, not the protected region.
            }
            for raised in self.raised_at(&source) {
                if !arms.iter().any(|&arm| self.index.is_subtype(raised, arm)) {
                    continue; // this clause does not catch it.
                }
                if preceding
                    .iter()
                    .any(|&earlier| self.index.is_subtype(raised, earlier))
                {
                    continue; // an earlier clause does.
                }
                // An inner `try` inside the block may already handle it, and a raise inside a
                // lambda or a local class's method belongs to that boundary rather than to this one.
                if self.handled_by_catch(&source, try_stmt.syntax(), raised) {
                    continue;
                }
                if Self::nearest_throws_boundary(&source)
                    .is_some_and(|boundary| try_stmt.syntax().descendants().any(|n| n == boundary))
                {
                    continue;
                }
                if !out.contains(&raised) {
                    out.push(raised);
                }
            }
        }
        if out.is_empty() {
            return arms.to_vec();
        }
        out
    }

    /// Whether anything in `clause` assigns to `name`, which is what takes a `catch` parameter out of
    /// being effectively final.
    fn reassigns(clause: &ast::CatchClause, name: &str) -> bool {
        clause
            .syntax()
            .descendants()
            .filter_map(ast::AssignmentExpr::cast)
            .filter_map(|assignment| assignment.syntax().children().find_map(ast::Expr::cast))
            .filter_map(|target| match target {
                ast::Expr::NameRef(target) => Collect::first_ident_token(target.syntax()),
                _ => None,
            })
            .any(|token| jals_syntax::decoded_ident(&token) == name)
    }

    /// Resolve an AST type reference (a `throws` / `catch` type) to an indexed item, honouring whether
    /// it is written qualified. `None` for a primitive, an unresolved name, or an external type.
    fn resolve_type(&self, ty: &ast::Type) -> Option<ItemId> {
        let simple = ty.simple_name()?;
        let qualified = if ty.is_qualified() {
            ty.qualified_text()
        } else {
            None
        };
        match self
            .index
            .resolve_type_name(self.file, &simple, qualified.as_deref())
        {
            TypeResolution::Project(id) => Some(id),
            _ => None,
        }
    }

    /// The nearest ancestor of `node` that establishes a `throws` boundary — a method / constructor
    /// declaration, an initializer, or a lambda. Used to attribute a raising site to exactly one
    /// declaration: [`UnreportedException::collect`] only analyzes sites whose nearest boundary is a
    /// `METHOD_DECL` / `CONSTRUCTOR_DECL`. `INITIALIZER` and `LAMBDA_EXPR` are listed so a raise inside
    /// one is *excluded* from the enclosing method rather than misattributed to it; their own
    /// checked-exception rules (a lambda's are governed by its target type; an initializer's by javac's
    /// static / instance-initializer rules) are not yet modelled, so such a raise is conservatively left
    /// unreported.
    fn nearest_throws_boundary(node: &SyntaxNode) -> Option<SyntaxNode> {
        node.ancestors().find(|a| {
            matches!(
                a.kind(),
                METHOD_DECL | CONSTRUCTOR_DECL | INITIALIZER | LAMBDA_EXPR
            )
        })
    }

    /// Whether `try_stmt` protects `source`: `source` lies within the guarded block or the resource
    /// list, not within a `catch` or `finally` clause.
    fn guards(try_stmt: &ast::TryStmt, source: &SyntaxNode) -> bool {
        let range = source.text_range();
        let in_block = try_stmt
            .block()
            .is_some_and(|b| b.syntax().text_range().contains_range(range));
        let in_resources = try_stmt
            .resources()
            .is_some_and(|r| r.syntax().text_range().contains_range(range));
        in_block || in_resources
    }

    /// The number of arguments in an optional argument list.
    fn arg_count(args: Option<ast::ArgList>) -> usize {
        args.map_or(0, |list| list.args().count())
    }

    /// Whether a member's arity can bind a call of `argc` arguments: an exact match, or a varargs
    /// method whose fixed parameters are no more than `argc`. Including varargs candidates keeps the
    /// intersection in [`Cx::call_throws`] sound (the actually-resolved overload is never excluded).
    const fn applies_to_arity(member: &crate::Member, argc: usize) -> bool {
        if member.varargs {
            member.params.len().saturating_sub(1) <= argc
        } else {
            member.params.len() == argc
        }
    }
}
