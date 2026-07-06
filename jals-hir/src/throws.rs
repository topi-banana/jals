//! `unreported-exception` analysis: a **checked** exception a method / constructor body can raise
//! that is neither declared in its `throws` clause nor handled by an enclosing `try` / `catch`.
//!
//! This is javac's "unreported exception X; must be caught or declared to be thrown". It is the
//! index-aware counterpart of [`type_mismatches`](crate::type_mismatches) and is built on the same
//! machinery: [`infer`] for expression types, [`call_target`] + [`ProjectIndex::resolve_members_all`]
//! for call resolution, and [`ProjectIndex::is_subtype`] for the exception-hierarchy walk.
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
//! `RuntimeException` / `Error` cut resolve); with no index, or with the stubs absent, it reports
//! nothing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_syntax::SyntaxKind::{
    CALL_EXPR, CONSTRUCTOR_DECL, INITIALIZER, LAMBDA_EXPR, METHOD_DECL, NEW_EXPR, THROW_STMT,
    TRY_STMT,
};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{self, AstNode};

use crate::def::{DefKind, Namespace};
use crate::infer::{TypeInference, call_target, infer, member_type_to_ty, node_span};
use crate::project::{FileId, ItemId, ProjectIndex, TypeResolution, throws_clause_types};
use crate::resolve::Resolved;

/// A checked exception a method / constructor can raise that is neither declared nor caught.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreportedException {
    /// The byte range of the raising site (the `throw`, method call, or `new`).
    pub range: Range<usize>,
    /// The simple name of the unreported checked exception.
    pub name: String,
}

impl UnreportedException {
    /// The human-readable diagnostic message.
    pub fn message(&self) -> String {
        format!(
            "unreported exception {}; must be caught or declared to be thrown",
            self.name
        )
    }
}

/// Every checked exception raised in `root` that its enclosing method / constructor neither declares
/// in `throws` nor catches. Requires a project `index` (with stdlib stubs) — returns empty otherwise.
pub fn unreported_exceptions(
    root: &SyntaxNode,
    resolved: &Resolved,
    project: Option<(&ProjectIndex, FileId)>,
) -> Vec<UnreportedException> {
    let Some((index, file)) = project else {
        return Vec::new();
    };
    // Without the modelled top of the `Throwable` hierarchy nothing can be classified checked.
    let Some(classifier) = Classifier::new(index, file) else {
        return Vec::new();
    };
    let ti = infer(root, resolved, index, file);
    let cx = Cx {
        index,
        file,
        ti: &ti,
        classifier,
    };
    let mut out = Vec::new();
    for node in root.descendants() {
        if matches!(node.kind(), METHOD_DECL | CONSTRUCTOR_DECL) {
            cx.check_decl(&node, &mut out);
        }
    }
    out
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
    fn new(index: &ProjectIndex, file: FileId) -> Option<Classifier> {
        let resolve = |simple, fqn| match index.resolve_type_name(file, simple, Some(fqn)) {
            TypeResolution::Project(id) => Some(id),
            _ => None,
        };
        Some(Classifier {
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
    fn check_decl(&self, decl: &SyntaxNode, out: &mut Vec<UnreportedException>) {
        let body = ast::MethodDecl::cast(decl.clone())
            .and_then(|m| m.body())
            .or_else(|| ast::ConstructorDecl::cast(decl.clone()).and_then(|c| c.body()));
        let Some(body) = body else {
            return; // an abstract / interface method (`;`) has no body.
        };
        let declared = self.declared_throws(decl);
        for node in body.syntax().descendants() {
            // Only a throw / call / `new` can raise; skip the boundary walk for every other node.
            if !matches!(node.kind(), THROW_STMT | CALL_EXPR | NEW_EXPR) {
                continue;
            }
            // Only sources whose nearest throws boundary is *this* declaration belong to it.
            if nearest_throws_boundary(&node).as_ref() != Some(decl) {
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
                    range: node_span(&node),
                    name: self.index.item(exc).fqn.simple_name().into(),
                });
            }
        }
    }

    /// The indexed types named in `decl`'s `throws` clause (unresolvable names dropped).
    fn declared_throws(&self, decl: &SyntaxNode) -> Vec<ItemId> {
        throws_clause_types(decl)
            .filter_map(|ty| self.resolve_type(&ty))
            .collect()
    }

    /// The exceptions a single source node can raise: the thrown type of a `throw`, or the intersection
    /// of the `throws` of a call / `new`'s bindable overloads. Empty when the node is not a source or
    /// nothing is provably raised.
    fn raised_at(&self, node: &SyntaxNode) -> Vec<ItemId> {
        match node.kind() {
            THROW_STMT => ast::ThrowStmt::cast(node.clone())
                .and_then(|t| t.expr())
                .and_then(|e| self.expr_item(e.syntax()))
                .into_iter()
                .collect(),
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
        let Some((owner, name)) = call_target(call, self.ti, self.index, self.file) else {
            return Vec::new();
        };
        let argc = arg_count(call.args());
        let candidates: Vec<&crate::Member> = self
            .index
            .resolve_members_all(owner, &name, Namespace::Method)
            .into_iter()
            .map(|id| self.index.member(id))
            .filter(|m| m.kind == DefKind::Method && applies_to_arity(m, argc))
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
        let argc = arg_count(new.args());
        // Constructors are never inherited, so only `owner`'s own members can apply — no supertype walk.
        let candidates: Vec<&crate::Member> = self
            .index
            .own_members(owner)
            .iter()
            .map(|&id| self.index.member(id))
            .filter(|m| m.kind == DefKind::Constructor && applies_to_arity(m, argc))
            .collect();
        self.intersect_member_throws(&candidates)
    }

    /// The intersection of the resolvable `throws` items across `members`. Empty when `members` is
    /// empty or the intersection is empty (nothing is thrown by *every* candidate).
    fn intersect_member_throws(&self, members: &[&crate::Member]) -> Vec<ItemId> {
        members
            .iter()
            .map(|m| self.member_throws(m))
            .reduce(|mut acc, next| {
                acc.retain(|id| next.contains(id));
                acc
            })
            .unwrap_or_default()
    }

    /// The resolvable exception items a single member declares in its `throws`, in its declaring
    /// file's context.
    fn member_throws(&self, member: &crate::Member) -> Vec<ItemId> {
        member
            .throws
            .iter()
            .filter_map(|mt| {
                member_type_to_ty(self.index, member.file, member.owner, mt).project_id()
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
            if !guards(&try_stmt, source) {
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
        self.ti.type_of_expr(node_span(expr))?.project_id()
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
}

/// The nearest ancestor of `node` that establishes a `throws` boundary — a method / constructor
/// declaration, an initializer, or a lambda. Used to attribute a raising site to exactly one
/// declaration: [`unreported_exceptions`] only analyzes sites whose nearest boundary is a
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

/// Whether `try_stmt` protects `source`: `source` lies within the guarded block or the resource list,
/// not within a `catch` or `finally` clause.
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

/// Whether a member's arity can bind a call of `argc` arguments: an exact match, or a varargs method
/// whose fixed parameters are no more than `argc`. Including varargs candidates keeps the
/// intersection in [`Cx::call_throws`] sound (the actually-resolved overload is never excluded).
fn applies_to_arity(member: &crate::Member, argc: usize) -> bool {
    if member.varargs {
        member.params.len().saturating_sub(1) <= argc
    } else {
        member.params.len() == argc
    }
}
