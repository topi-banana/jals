//! Type inference: assign a [`Ty`] to each declaration and expression of one file.
//!
//! This sits on top of [`Resolved`] (name resolution) without changing it: inference is a separate
//! pass returning a separate [`TypeInference`], exactly as [`ProjectIndex`] layers cross-file type
//! resolution over the same `Resolved`. Two sub-passes:
//!
//! 1. **Declared types** ([`Inferer::collect_declared_types`]) records the written type of every
//!    explicitly-typed binding (field, parameter, typed local, …), resolving reference type names
//!    against the project so a `Foo` field becomes [`ClassTy::Project`]. A `var` binding is left for
//!    pass 2.
//! 2. **Expression inference** ([`Inferer::infer_in`]) walks the tree post-order, so every
//!    expression's children are typed before it. A `var` local's type is filled from its
//!    initializer here.
//!
//! Scope is the structural / local subset (literals, names, arithmetic with numeric promotion,
//! casts, `new`, arrays) plus member access — `obj.field` and `recv.method()` resolve against the
//! project member model when the receiver is a project type, walking its project-internal
//! supertypes and substituting the receiver's generic type arguments into the member's type
//! ([`member_ty_substituted`]), so `Box<String>.get()` is `String`. A member of an external
//! (unindexed) type, and the target-typed forms (method references, lambdas, switch expressions),
//! stay [`Ty::Unknown`]. The pass never panics: every accessor is `Option`/iterator and an
//! unresolvable form is `Unknown`.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use hashbrown::{HashMap, HashSet};
use jals_exec::Yielder;

use jals_syntax::SyntaxKind::{
    AMP, AMP_AMP, ASSIGNMENT_EXPR, BANG, BANG_EQ, BOOLEAN_KW, BYTE_KW, CALL_EXPR, CARET,
    CATCH_CLAUSE, CHAR_KW, CHAR_LITERAL, COMMA, CONSTRUCTOR_DECL, DOT, DOUBLE_KW, ELLIPSIS, EQ,
    EQ_EQ, FALSE_KW, FIELD_ACCESS, FIELD_DECL, FLOAT_KW, FLOAT_LITERAL, FOR_EACH_STMT, GT, IDENT,
    INSTANCEOF_KW, INT_KW, INT_LITERAL, LAMBDA_EXPR, LBRACK, LOCAL_VAR_DECL, LONG_KW, LSHIFT, LT,
    LT_EQ, METHOD_DECL, MINUS, NEW_EXPR, NULL_KW, PARAM, PERCENT, PIPE, PIPE_PIPE, PLUS,
    RECORD_COMPONENT, RESOURCE, RETURN_STMT, SHORT_KW, SLASH, STAR, STRING_LITERAL, SUPER_KW,
    TEXT_BLOCK, THIS_KW, TILDE, TRUE_KW, VAR_KW, VOID_KW,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::def::{Def, DefId, DefKind, Namespace};
use crate::project::{FileId, ItemId, MemberId, MemberType, ProjectIndex, TypeResolution};
use crate::reference::Resolution;
use crate::resolve::Resolved;
use crate::resolve::collect::Collect;
use crate::ty::{ClassTy, Primitive, Ty};

/// The inferred types of one file's declarations and expressions.
///
/// Produced by [`infer`] / [`infer_node`]. Declaration types are indexed by [`DefId`] (parallel to
/// [`Resolved::defs`](crate::Resolved)); expression types are keyed by the expression's byte span,
/// and [`type_at`](TypeInference::type_at) answers the hover query "what type is under the cursor".
pub struct TypeInference {
    /// One entry per [`Def`](crate::Def), in [`DefId`] order; [`Ty::Unknown`] where not inferred.
    def_types: Vec<Ty>,
    /// Every expression's type, keyed by its byte span `(start, end)`. Read by exact span
    /// ([`type_of_expr`](TypeInference::type_of_expr), and internally while a parent reads its
    /// children) and scanned for the innermost cover ([`type_at`](TypeInference::type_at)).
    expr_by_span: HashMap<(usize, usize), Ty>,
    /// The member each call binds to, keyed by the `CALL_EXPR`'s byte span. Empty without a project
    /// index, and missing an entry wherever selection found no answer.
    call_targets: HashMap<(usize, usize), MemberId>,
    /// The field or enum constant each member access binds to, keyed by the `FIELD_ACCESS`'s span.
    field_targets: HashMap<(usize, usize), MemberId>,
}

impl TypeInference {
    /// The type inferred for the definition `id`.
    pub fn type_of_def(&self, id: DefId) -> &Ty {
        &self.def_types[id.0 as usize]
    }

    /// The member the call spanning exactly `span` binds to.
    ///
    /// The decision inference already made, kept rather than discarded. A consumer that only needs
    /// the call's *type* reads [`type_of_expr`](Self::type_of_expr); one that needs to name the
    /// method — a code generator emitting an `invokevirtual`, which needs the selected overload's
    /// exact descriptor — needs the member itself, and re-deriving it downstream would be a second
    /// selection free to disagree with this one.
    ///
    /// `None` when inference ran without a project index, when the receiver is not an indexed type,
    /// or when no same-arity overload accepts the arguments.
    pub fn call_target_of(&self, span: Range<usize>) -> Option<MemberId> {
        self.call_targets.get(&(span.start, span.end)).copied()
    }

    /// The field or enum constant the member access spanning exactly `span` binds to.
    ///
    /// The counterpart of [`call_target_of`](Self::call_target_of) for `receiver.name`. Reading a
    /// field needs the same three facts a call does — the declaring type, the descriptor, and
    /// whether it is `static` — so the resolution is recorded rather than left to be redone.
    pub fn field_target_of(&self, span: Range<usize>) -> Option<MemberId> {
        self.field_targets.get(&(span.start, span.end)).copied()
    }

    /// The type of the expression spanning exactly `span`, if one was inferred there.
    pub fn type_of_expr(&self, span: Range<usize>) -> Option<&Ty> {
        self.expr_by_span.get(&(span.start, span.end))
    }

    /// The type of the innermost (narrowest) expression covering byte `offset` — the hover query.
    pub fn type_at(&self, offset: usize) -> Option<&Ty> {
        self.expr_by_span
            .iter()
            .filter(|(span, _)| span.0 <= offset && offset < span.1)
            .min_by_key(|(span, _)| span.1 - span.0)
            .map(|(_, t)| t)
    }
}

impl TypeInference {
    /// Infers types for `root` (a `SOURCE_FILE`), resolving reference type names against `index` from
    /// the perspective of `file`. `resolved` is the file's name resolution.
    pub async fn infer(
        root: &SyntaxNode,
        resolved: &Resolved,
        index: &ProjectIndex,
        file: FileId,
    ) -> Self {
        Inferer::new(root, resolved, Some((index, file)))
            .run()
            .await
    }

    /// Infers types for `root` without a project index.
    ///
    /// Reference type names resolve only to [`ClassTy::External`] (by spelling), but all structural
    /// inference — primitives, arrays, literals, numeric promotion, `var` from initializer — still
    /// works. For file-local tooling holding no index.
    pub async fn infer_node(root: &SyntaxNode, resolved: &Resolved) -> Self {
        Inferer::new(root, resolved, None).run().await
    }
}

/// The outcome of [`TypeInference::resolve_call`].
struct CallResolution<'a> {
    /// The type the method was looked up on.
    owner: ItemId,
    /// The method's simple name.
    name: String,
    /// The byte span of each argument expression, in order.
    arg_spans: Vec<Range<usize>>,
    /// Each argument's inferred type, or `None` where inference had no answer.
    arg_tys: Vec<Option<&'a Ty>>,
    /// Same-arity, non-varargs candidates, nearest declaration first.
    candidates: Vec<MemberId>,
    /// The candidate every argument is assignable to, if any.
    selected: Option<MemberId>,
}

/// A type error: a value not assignable to the slot it is written into, or a call matching no
/// overload of the named method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeMismatch {
    /// The byte range the diagnostic is anchored at (the offending value / argument, or the call).
    pub range: Range<usize>,
    kind: MismatchKind,
}

/// What kind of type error a [`TypeMismatch`] is — its `message` differs by kind, but consumers read
/// only [`TypeMismatch::range`] and [`TypeMismatch::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum MismatchKind {
    /// A value of type `found` assigned where `expected` is required.
    Assignment { expected: Ty, found: Ty },
    /// A call to `name` whose argument types `args` match none of its overloads.
    NoOverload { name: String, args: Vec<Ty> },
}

impl TypeMismatch {
    /// An assignment-context mismatch (initializer, assignment, return, or a single-overload call
    /// argument): `found` is not assignable to `expected`.
    const fn assignment(range: Range<usize>, expected: Ty, found: Ty) -> Self {
        Self {
            range,
            kind: MismatchKind::Assignment { expected, found },
        }
    }

    /// A call to `name` with argument types `args` that no same-arity overload accepts.
    const fn no_overload(range: Range<usize>, name: String, args: Vec<Ty>) -> Self {
        Self {
            range,
            kind: MismatchKind::NoOverload { name, args },
        }
    }

    /// A human-readable description of the type error.
    pub fn message(&self) -> String {
        match &self.kind {
            MismatchKind::Assignment { expected, found } => {
                format!("incompatible types: `{found}` cannot be assigned to `{expected}`")
            }
            MismatchKind::NoOverload { name, args } => {
                let list = args
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("no overload of `{name}` accepts the argument types ({list})")
            }
        }
    }
}

impl TypeInference {
    /// Reports the assignment-context type mismatches in `root` (a `SOURCE_FILE`): a variable
    /// initializer or a simple `=` assignment whose value type is not assignable to its slot type.
    ///
    /// `project = Some((index, file))` infers reference types against the project, so a project-internal
    /// subtyping mismatch (a `Sub`/`Base` confusion) is caught; `None` infers file-locally, where
    /// reference types stay external and lenient, so only primitive, `null`, and array mismatches
    /// surface. Conservative throughout (it builds on [`Ty::is_assignable_to`]): an `Unknown` type, an
    /// external/boxing pair, and a numeric constant that narrowing could rescue are never reported, so a
    /// consumer turning these into diagnostics never shows a false positive. Pure; never panics.
    pub async fn type_mismatches(
        root: &SyntaxNode,
        resolved: &Resolved,
        project: Option<(&ProjectIndex, FileId)>,
    ) -> Vec<TypeMismatch> {
        let ti = match project {
            Some((index, file)) => Self::infer(root, resolved, index, file).await,
            None => Self::infer_node(root, resolved).await,
        };
        let index = project.map(|(index, _)| index);
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                LOCAL_VAR_DECL | FIELD_DECL => {
                    ti.check_initializer(&node, resolved, index, &mut out);
                }
                ASSIGNMENT_EXPR => ti.check_assignment(&node, index, &mut out),
                RETURN_STMT => ti.check_return(&node, resolved, index, &mut out),
                // Argument checking needs the project member model (formal parameter types), so it runs
                // only with an index — like project subtyping.
                CALL_EXPR => {
                    if let Some((index, file)) = project {
                        ti.check_call(&node, index, file, &mut out);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Checks each declarator's initializer in a variable / field declaration against its declared
    /// type.
    ///
    /// A declaration may bind several variables at once (`int a = 1, b = 2.0;`); the CST is flat, so
    /// the declarators are paired by walking the direct children in order — each `IDENT` declarator
    /// name takes the next direct expression child as its initializer (a declarator without one is
    /// skipped when the next name arrives). A `var` declaration (always single-name) has no written
    /// type to disagree with and is skipped whole.
    fn check_initializer(
        &self,
        node: &SyntaxNode,
        resolved: &Resolved,
        index: Option<&ProjectIndex>,
        out: &mut Vec<TypeMismatch>,
    ) {
        if node
            .children()
            .find_map(ast::Type::cast)
            .as_ref()
            .is_some_and(Cst::is_var_type)
        {
            return;
        }
        // The declarator name is a *definition*, recovered with `symbol_at` (not `definition_at`,
        // which looks up a reference).
        for (name, value) in Cst::declarator_initializers(node) {
            if let Some(def_id) = resolved.symbol_at(Collect::token_start(&name)) {
                self.record_if_mismatch(value.syntax(), self.type_of_def(def_id), index, out);
            }
        }
    }

    /// Checks a simple `=` assignment's value against its target's type. Compound assignments
    /// (`+=`, `>>=`, …) carry an implicit narrowing cast, so only a lone `=` is checked.
    fn check_assignment(
        &self,
        node: &SyntaxNode,
        index: Option<&ProjectIndex>,
        out: &mut Vec<TypeMismatch>,
    ) {
        if Cst::op_kinds(node).as_slice() != [EQ] {
            return;
        }
        let Some(assign) = ast::AssignmentExpr::cast(node.clone()) else {
            return;
        };
        let (Some(target), Some(value)) = (assign.target(), assign.value()) else {
            return;
        };
        let Some(expected) = self.type_of_expr(Collect::node_span(target.syntax())) else {
            return;
        };
        self.record_if_mismatch(value.syntax(), expected, index, out);
    }

    /// Checks a `return <expr>;` against the enclosing method's return type. Only methods are checked:
    /// a `return` whose nearest function-like ancestor is a lambda (its return type is target-typed
    /// and unknown here) or a constructor (no return type) is skipped, as is a bare `return;`.
    fn check_return(
        &self,
        node: &SyntaxNode,
        resolved: &Resolved,
        index: Option<&ProjectIndex>,
        out: &mut Vec<TypeMismatch>,
    ) {
        let Some(value) = ast::ReturnStmt::cast(node.clone()).and_then(|r| r.expr()) else {
            return;
        };
        // The nearest enclosing function-like node decides whose return this is.
        let enclosing = node
            .ancestors()
            .find(|a| matches!(a.kind(), METHOD_DECL | LAMBDA_EXPR | CONSTRUCTOR_DECL));
        let Some(method) = enclosing.filter(|a| a.kind() == METHOD_DECL) else {
            return;
        };
        // The method's definition is typed with its return type (see `declares_typed_bindings`).
        let Some(def_id) = Collect::first_ident_token(&method)
            .and_then(|n| resolved.symbol_at(Collect::token_start(&n)))
        else {
            return;
        };
        self.record_if_mismatch(value.syntax(), self.type_of_def(def_id), index, out);
    }

    /// Checks a method call's arguments against the called method's formal parameter types.
    ///
    /// Resolves the call against the named method's overloads by argument type, then reports a mismatch
    /// only when *no* overload accepts the arguments. Argument conversion is method-invocation conversion
    /// (JLS §5.3), which — unlike assignment — does not permit constant narrowing, so a plain
    /// [`Ty::is_assignable_to`] is used: `f(1)` for a `byte` parameter is a real error.
    ///
    /// Conservative: a varargs method is skipped (variable arity), an `Unknown`/external argument keeps a
    /// candidate applicable (no false positive), and a "no overload" conclusion is reported only when the
    /// method set is fully known ([`ProjectIndex::method_set_complete`]) — a type extending an external
    /// class, or an `Object` method, may have overloads the index cannot see.
    fn check_call(
        &self,
        node: &SyntaxNode,
        index: &ProjectIndex,
        file: FileId,
        out: &mut Vec<TypeMismatch>,
    ) {
        let Some(call) = ast::CallExpr::cast(node.clone()) else {
            return;
        };
        let Some(resolution) = self.resolve_call(&call, index, file) else {
            return;
        };
        // The call binds to some overload — no argument error to report.
        if resolution.selected.is_some() {
            return;
        }
        let CallResolution {
            owner,
            name,
            arg_spans,
            arg_tys,
            candidates,
            ..
        } = resolution;
        // No overload accepts the arguments; report only when the overload set is fully known.
        if !index.method_set_complete(owner, &name) {
            return;
        }
        let matching: Vec<&crate::Member> = candidates.iter().map(|&id| index.member(id)).collect();
        if let [only] = matching.as_slice() {
            // A single overload: precise per-argument diagnostics against it.
            for ((arg_ty, span), param) in arg_tys.iter().zip(&arg_spans).zip(&only.params) {
                let param_ty = index.member_type_to_ty(only.file, only.owner, &param.ty);
                if let Some(ty) = arg_ty
                    && !ty.is_assignable_to(&param_ty, Some(index))
                {
                    out.push(TypeMismatch::assignment(
                        span.clone(),
                        param_ty,
                        (*ty).clone(),
                    ));
                }
            }
        } else {
            // Several same-arity overloads, none applicable: the call matches no overload.
            let arg_tys = arg_tys
                .iter()
                .map(|ty| ty.cloned().unwrap_or(Ty::Unknown))
                .collect();
            out.push(TypeMismatch::no_overload(
                Collect::node_span(call.syntax()),
                name,
                arg_tys,
            ));
        }
    }

    /// Bind every call and `new` under `root` to the member it selects, recording the result for
    /// [`call_target_of`](Self::call_target_of).
    ///
    /// Selection therefore runs twice per call: once in [`check_call`](Self::check_call) during
    /// pass 2, and once here. That is not redundancy to remove — pass 2 weighs argument types that
    /// are still being inferred, so its answer is provisional, which is the whole reason this pass
    /// exists. Merging them would mean either checking against incomplete types or reporting
    /// diagnostics a pass later than the rest.
    async fn record_member_targets(
        &mut self,
        root: &SyntaxNode,
        index: &ProjectIndex,
        file: FileId,
    ) {
        let mut yielder = Yielder::new();
        let (mut calls, mut fields) = (Vec::new(), Vec::new());
        for node in root.descendants() {
            yielder.tick().await;
            let span = Collect::node_span(&node);
            match node.kind() {
                CALL_EXPR => {
                    if let Some(call) = ast::CallExpr::cast(node.clone())
                        && let Some(selected) = self
                            .resolve_call(&call, index, file)
                            .and_then(|resolution| resolution.selected)
                            // A bare `this(…)` / `super(…)` is a constructor, which no name lookup
                            // finds — so it resolves the way a `new` does.
                            .or_else(|| self.resolve_explicit_constructor(&call, index, file))
                    {
                        calls.push(((span.start, span.end), selected));
                    }
                }
                // A `new` selects a constructor exactly as a call selects a method, so it is
                // recorded in the same map: a consumer emitting the allocation asks
                // `call_target_of` for the `NEW_EXPR`'s own span.
                NEW_EXPR => {
                    if let Some(new) = ast::NewExpr::cast(node.clone())
                        && let Some(selected) = self.resolve_new(&new, index)
                    {
                        calls.push(((span.start, span.end), selected));
                    }
                }
                FIELD_ACCESS => {
                    if let Some(access) = ast::FieldAccess::cast(node.clone())
                        && let Some(member) = self.resolve_field(&access, index, file)
                    {
                        fields.push(((span.start, span.end), member));
                    }
                }
                _ => {}
            }
        }
        self.call_targets.extend(calls);
        self.field_targets.extend(fields);
    }

    /// Everything selecting a call's overload produced: the owner and name it looked up, the
    /// arguments it weighed, the same-arity candidates, and which one it picked.
    ///
    /// One routine answers both questions asked of a call. Type checking wants to know whether
    /// *any* overload accepts the arguments, and code generation wants to know *which* — and a
    /// second implementation of "which" would be free to disagree with the first, which is exactly
    /// the drift that turns into a `NoSuchMethodError` at run time rather than a diagnostic.
    ///
    /// Selection is same arity, then applicability (every argument assignable to its parameter),
    /// then most-specific among what survives — the part of JLS §15.12.2 that changes *which*
    /// method is called rather than merely whether one is. Without it, `println(Object)` declared
    /// before `println(String)` would swallow every string, and the mistake would surface as a
    /// run-time `NoSuchMethodError` from a descriptor nothing ever checked.
    ///
    /// What is *not* modelled: the first two phases (strict / loose) are collapsed into one and
    /// boxing is whatever [`Ty::is_assignable_to`] admits. Variable arity *is* a separate phase,
    /// because it has to be: it may only be consulted once fixed arity has found nothing.
    fn resolve_call(
        &self,
        call: &ast::CallExpr,
        index: &ProjectIndex,
        file: FileId,
    ) -> Option<CallResolution<'_>> {
        let (owner, name) = self.call_target(call, index, file)?;
        let args: Vec<ast::Expr> = call
            .args()
            .map(|list| list.args().collect())
            .unwrap_or_default();
        let reachable = index.resolve_members_all(owner, &name, Namespace::Method);
        // Phase one: fixed arity, which is every non-varargs method taking exactly this many
        // arguments.
        let mut candidates: Vec<MemberId> = reachable
            .iter()
            .copied()
            .filter(|&id| {
                let member = index.member(id);
                !member.varargs && member.params.len() == args.len()
            })
            .collect();
        // Phase two: variable arity (JLS §15.12.2.4), and *only* when phase one found nothing —
        // which is the order the specification gives and the reason `f(Object[])` must not win over
        // `f(String, String)`. A varargs method takes its fixed parameters plus any number more, so
        // one argument short of its arity is legal (`f()` against `f(int...)`) and any number over is.
        if candidates.is_empty() {
            candidates = reachable
                .iter()
                .copied()
                .filter(|&id| {
                    let member = index.member(id);
                    member.varargs && args.len() + 1 >= member.params.len()
                })
                .collect();
        }
        if candidates.is_empty() {
            return None;
        }
        // Argument spans and inferred types, computed once and reused across every overload (an
        // un-inferred argument is `None`, and treated as applicable — never blocking).
        let arg_spans: Vec<Range<usize>> = args
            .iter()
            .map(|a| Collect::node_span(a.syntax()))
            .collect();
        let arg_tys: Vec<Option<&Ty>> = arg_spans
            .iter()
            .map(|s| self.type_of_expr(s.clone()))
            .collect();
        let selected = Self::select_overload(&candidates, &arg_tys, index);
        Some(CallResolution {
            owner,
            name,
            arg_spans,
            arg_tys,
            candidates,
            selected,
        })
    }

    /// Which of `candidates` the arguments select: applicable first, then most specific.
    ///
    /// Every call shape shares this. A method call and a `new` differ in how the candidate set is
    /// *found* — inherited members against a type's own constructors — and not at all in how one
    /// of them is chosen, so writing the choice twice is exactly the drift
    /// [`call_target_of`](Self::call_target_of) exists to prevent.
    fn select_overload(
        candidates: &[MemberId],
        arg_tys: &[Option<&Ty>],
        index: &ProjectIndex,
    ) -> Option<MemberId> {
        // A candidate is applicable when every argument is assignable to its parameter.
        let applicable: Vec<MemberId> = candidates
            .iter()
            .copied()
            .filter(|&id| {
                let member = index.member(id);
                let fits = |arg_ty: Option<&Ty>, target: &Ty| {
                    arg_ty.is_none_or(|ty| ty.is_assignable_to(target, Some(index)))
                };
                let declared = |param: &crate::Param| {
                    index.member_type_to_ty(member.file, member.owner, &param.ty)
                };
                if !member.varargs {
                    return arg_tys
                        .iter()
                        .zip(&member.params)
                        .all(|(arg_ty, param)| fits(*arg_ty, &declared(param)));
                }
                // A varargs method's *last* parameter stands for however many arguments are left, so
                // each of those is checked against its **element** type rather than against the array.
                // `zip` alone would compare a `String` to a `String[]` and reject the call.
                let Some((last, fixed)) = member.params.split_last() else {
                    return false;
                };
                if !arg_tys
                    .iter()
                    .zip(fixed)
                    .all(|(arg_ty, param)| fits(*arg_ty, &declared(param)))
                {
                    return false;
                }
                let element = match declared(last) {
                    Ty::Array(element) => *element,
                    // A varargs parameter is an array by construction; anything else is an index that
                    // could not work the type out, and staying lenient is this module's habit.
                    other => other,
                };
                let rest = &arg_tys[fixed.len().min(arg_tys.len())..];
                // Exactly one trailing argument may be the array itself rather than an element
                // (JLS §15.12.4.2): `total(new int[] {1, 2})` passes straight through to `int...`.
                if let [only] = rest
                    && fits(*only, &declared(last))
                {
                    return true;
                }
                rest.iter().all(|arg_ty| fits(*arg_ty, &element))
            })
            .collect();
        Self::most_specific(&applicable, index)
    }

    /// The constructor a bare `this(args)` or `super(args)` binds to.
    ///
    /// Neither goes through [`call_target`](Self::call_target): a constructor is not reachable by name
    /// lookup (it is indexed under its class's own name and in no name space a call searches), and
    /// `this` / `super` carry no identifier token to look up either. So the candidate set is a type's
    /// *own* constructors, exactly as a `new` uses — the enclosing type for `this(…)` and its class
    /// supertype for `super(…)`.
    ///
    /// Without this a constructor's own delegation resolved to nothing, and `this(1)` was
    /// indistinguishable from a call to a method named `this`.
    fn resolve_explicit_constructor(
        &self,
        call: &ast::CallExpr,
        index: &ProjectIndex,
        file: FileId,
    ) -> Option<MemberId> {
        let Some(ast::Expr::NameRef(name)) = call.callee() else {
            return None;
        };
        let keyword = name
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| matches!(token.kind(), THIS_KW | SUPER_KW))?;
        let enclosing = index.enclosing_item(file, call.syntax())?;
        let owner = if keyword.kind() == THIS_KW {
            enclosing
        } else {
            // The class supertype, which is the only one a `super(…)` can name. An interface has no
            // constructor to reach.
            index
                .item(enclosing)
                .supertypes
                .iter()
                .map(|supertype| supertype.id)
                .find(|&id| index.item(id).kind != DefKind::Interface)?
        };
        let args: Vec<ast::Expr> = call
            .args()
            .map(|list| list.args().collect())
            .unwrap_or_default();
        let candidates: Vec<MemberId> = index
            .own_members(owner)
            .iter()
            .copied()
            .filter(|&id| {
                let member = index.member(id);
                member.kind == DefKind::Constructor
                    && !member.varargs
                    && member.params.len() == args.len()
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let arg_tys: Vec<Option<&Ty>> = args
            .iter()
            .map(|argument| self.type_of_expr(Collect::node_span(argument.syntax())))
            .collect();
        Self::select_overload(&candidates, &arg_tys, index)
    }

    /// The constructor `new C(args)` binds to.
    ///
    /// Constructors are never inherited, so the candidate set is the instantiated type's *own*
    /// members rather than a supertype walk; the choice among them is
    /// [`select_overload`](Self::select_overload), the same one a method call makes. `None` when
    /// the type declares no constructor at all — the implicit default one is not an indexed member
    /// — or when none of them accepts the arguments.
    fn resolve_new(&self, new: &ast::NewExpr, index: &ProjectIndex) -> Option<MemberId> {
        let owner = self
            .type_of_expr(Collect::node_span(new.syntax()))?
            .project_id()?;
        let args: Vec<ast::Expr> = new
            .syntax()
            .children()
            .find_map(ast::ArgList::cast)
            .map(|list| list.args().collect())
            .unwrap_or_default();
        let candidates: Vec<MemberId> = index
            .own_members(owner)
            .iter()
            .copied()
            .filter(|&id| {
                let member = index.member(id);
                member.kind == DefKind::Constructor
                    && !member.varargs
                    && member.params.len() == args.len()
            })
            .collect();
        let arg_tys: Vec<Option<&Ty>> = args
            .iter()
            .map(|argument| self.type_of_expr(Collect::node_span(argument.syntax())))
            .collect();
        Self::select_overload(&candidates, &arg_tys, index)
    }

    /// The single most specific of `applicable` — the one whose every parameter type is assignable
    /// to the corresponding parameter of all the others (JLS §15.12.2.5).
    ///
    /// Falls back to the first candidate when no single one dominates, which covers both a genuine
    /// ambiguity (a real compiler would reject it, and this crate does not check) and the far more
    /// common case of an incomparable pair the shallow subtyping model cannot order. The
    /// nearest-first order the supertype walk produces makes that fallback the inherited-member
    /// shadowing answer, which is the right one when the set is one method and its overrides.
    fn most_specific(applicable: &[MemberId], index: &ProjectIndex) -> Option<MemberId> {
        let first = applicable.first().copied()?;
        let at_least_as_specific = |left: MemberId, right: MemberId| {
            let (left, right) = (index.member(left), index.member(right));
            left.params.iter().zip(&right.params).all(|(from, to)| {
                index
                    .member_type_to_ty(left.file, left.owner, &from.ty)
                    .is_assignable_to(
                        &index.member_type_to_ty(right.file, right.owner, &to.ty),
                        Some(index),
                    )
            })
        };
        applicable
            .iter()
            .copied()
            .find(|&candidate| {
                applicable
                    .iter()
                    .all(|&other| candidate == other || at_least_as_specific(candidate, other))
            })
            .or(Some(first))
    }

    /// The `(owner type, method name)` a call resolves against: a qualified call `recv.m(..)` on the
    /// receiver's project type, or a bare call `m(..)` on the enclosing type (an implicit `this`).
    /// `None` when the receiver is not an indexed project type or the callee is neither a name nor a
    /// field access.
    pub(crate) fn call_target(
        &self,
        call: &ast::CallExpr,
        index: &ProjectIndex,
        file: FileId,
    ) -> Option<(ItemId, String)> {
        match call.callee()? {
            ast::Expr::FieldAccess(fa) => {
                let name = fa.field()?;
                let owner = self.access_owner(&fa.receiver()?, index, file)?;
                Some((owner, name))
            }
            ast::Expr::NameRef(n) => {
                let name = Collect::first_ident_token(n.syntax())?.text().to_owned();
                Some((index.enclosing_item(file, call.syntax())?, name))
            }
            _ => None,
        }
    }

    /// The indexed type a member access through `receiver` is looked up on.
    ///
    /// A receiver is either a value — its inferred type names the owner — or a *type*, which is
    /// how every `static` member is reached (`System.out`, `Math.abs(x)`). A class name in
    /// expression position is not a value and has no inferred type at all, so the second case has
    /// to be recognised rather than fall out of the first.
    fn access_owner(
        &self,
        receiver: &ast::Expr,
        index: &ProjectIndex,
        file: FileId,
    ) -> Option<ItemId> {
        self.type_of_expr(Collect::node_span(receiver.syntax()))
            .and_then(Ty::project_id)
            .or_else(|| Cst::type_qualifier(receiver, index, file))
    }

    /// The field or enum constant the access `receiver.name` binds to.
    fn resolve_field(
        &self,
        access: &ast::FieldAccess,
        index: &ProjectIndex,
        file: FileId,
    ) -> Option<MemberId> {
        let name = access.field()?;
        let owner = self.access_owner(&access.receiver()?, index, file)?;
        index.resolve_member(owner, &name, Namespace::Value)
    }

    /// Pushes a [`TypeMismatch`] for `value` against `expected` when the value's inferred type is not
    /// assignable there — unless the value is untyped (no entry) or the pair is a constant narrowing
    /// the type system cannot see is legal.
    fn record_if_mismatch(
        &self,
        value: &SyntaxNode,
        expected: &Ty,
        index: Option<&ProjectIndex>,
        out: &mut Vec<TypeMismatch>,
    ) {
        let span = Collect::node_span(value);
        let Some(found) = self.type_of_expr(span.clone()) else {
            return;
        };
        if found.is_assignable_to(expected, index)
            || Self::rescued_by_constant_narrowing(expected, found)
        {
            return;
        }
        out.push(TypeMismatch::assignment(
            span,
            expected.clone(),
            found.clone(),
        ));
    }

    /// Whether a primitive mismatch could be a legal narrowing of a constant expression (JLS §5.2): a
    /// numeric value assigned to a `byte` / `short` / `char` slot. Without a constant evaluator we
    /// cannot tell whether the value is a constant in range, so we never report these —
    /// under-reporting (missing `byte b = someInt;`) rather than risk a false positive on the legal,
    /// common `byte b = 1;`.
    fn rescued_by_constant_narrowing(expected: &Ty, found: &Ty) -> bool {
        matches!(
            expected,
            Ty::Primitive(Primitive::Byte | Primitive::Short | Primitive::Char)
        ) && found.as_numeric().is_some()
    }
}

/// The working state of one inference run.
struct Inferer<'a> {
    root: SyntaxNode,
    resolved: &'a Resolved,
    project: Option<(&'a ProjectIndex, FileId)>,
    /// `def name-token start -> DefId`, for binding a declaration node to its [`Def`].
    def_by_name_start: HashMap<usize, DefId>,
    /// `reference range start -> index into resolved.references`, for resolving a type name and for
    /// looking a name reference's definition up cheaply.
    ref_by_start: HashMap<usize, usize>,
    def_types: Vec<Ty>,
    expr_by_span: HashMap<(usize, usize), Ty>,
}

impl<'a> Inferer<'a> {
    fn new(
        root: &SyntaxNode,
        resolved: &'a Resolved,
        project: Option<(&'a ProjectIndex, FileId)>,
    ) -> Self {
        let def_by_name_start = resolved
            .defs
            .iter()
            .map(|d| (d.name_range.start, d.id))
            .collect();
        let ref_by_start = resolved
            .references
            .iter()
            .enumerate()
            .map(|(i, r)| (r.range.start, i))
            .collect();
        Inferer {
            root: root.clone(),
            resolved,
            project,
            def_by_name_start,
            ref_by_start,
            def_types: vec![Ty::Unknown; resolved.defs.len()],
            expr_by_span: HashMap::new(),
        }
    }

    async fn run(mut self) -> TypeInference {
        let root = self.root.clone();
        self.collect_declared_types(&root).await;
        // After the written types, because a lambda's target is one of them.
        self.collect_lambda_params(&root).await;
        self.infer_in(&root).await;
        let project = self.project;
        let mut inference = TypeInference {
            def_types: self.def_types,
            expr_by_span: self.expr_by_span,
            call_targets: HashMap::new(),
            field_targets: HashMap::new(),
        };
        // Pass 3, once every expression has a type: selecting a call's overload weighs its
        // arguments, so it cannot run while those arguments are still being inferred.
        if let Some((index, file)) = project {
            inference.record_member_targets(&root, index, file).await;
        }
        inference
    }

    // --- Pass 1: declared types ---------------------------------------------------------------

    /// Records the written type of every explicitly-typed binding under `root`. A `var` binding is
    /// skipped here (it has no written type) and filled from its initializer in pass 2. Each node's
    /// handling is independent, so the recursive walk is a plain pre-order loop here.
    async fn collect_declared_types(&mut self, root: &SyntaxNode) {
        let mut yielder = Yielder::new();
        for node in root.descendants() {
            yielder.tick().await;
            if Self::declares_typed_bindings(node.kind()) {
                let ty = node.children().find_map(ast::Type::cast);
                if !ty.as_ref().is_some_and(Cst::is_var_type) {
                    let mut t = self.ty_of_opt_type(ty.as_ref());
                    // A `...` parameter is an array binding inside the body: `int... xs` writes the
                    // element type, and only the ellipsis says the local is an `int[]`. Without this
                    // the body sees `xs` as an `int` and every use of it is typed one dimension short.
                    if Self::is_variable_arity(&node) {
                        t = Ty::Array(Box::new(t));
                    }
                    for tok in Collect::direct_ident_tokens(&node) {
                        self.set_def_type(Collect::token_start(&tok), t.clone());
                    }
                }
            }
        }
    }

    /// Type every lambda parameter from the interface the lambda is being converted to.
    ///
    /// `n -> n * 2` writes no type for `n`, and nothing else can supply one: the parameter's type *is* the
    /// functional interface's, position by position (JLS §15.27.1). Without this the body cannot be typed at
    /// all, which is what stopped a backend emitting one even with the target type known.
    ///
    /// Runs in pass 1, so it reads only what pass 1 has: a declaration's written type and a method's return
    /// type. A lambda in an assignment or an argument keeps untyped parameters, for the same reason its own
    /// type stays unknown there.
    async fn collect_lambda_params(&mut self, root: &SyntaxNode) {
        let Some((index, _)) = self.project else {
            return;
        };
        let mut yielder = Yielder::new();
        for lambda in root.descendants().filter(|n| n.kind() == LAMBDA_EXPR) {
            yielder.tick().await;
            let Some(item) = self.target_ty(&lambda).project_id() else {
                continue;
            };
            // One method, or this is no functional interface and there is nothing to take a shape from.
            let mut methods = index
                .own_members(item)
                .iter()
                .copied()
                .filter(|&id| index.member(id).kind == DefKind::Method);
            let Some(method) = methods.next() else {
                continue;
            };
            if methods.next().is_some() {
                continue;
            }
            let params = index.member(method).params.clone();
            let owner = index.member(method).owner;
            let file = index.member(method).file;
            let declared: Vec<SyntaxToken> = lambda
                .descendants()
                .filter(|node| node.kind() == PARAM)
                .filter_map(|param| {
                    param
                        .children_with_tokens()
                        .filter_map(SyntaxElement::into_token)
                        .find(|token| token.kind() == IDENT)
                })
                .collect();
            for (position, name) in declared.iter().enumerate() {
                let Some(param) = params.get(position) else {
                    continue;
                };
                let ty = index.member_type_to_ty(file, owner, &param.ty);
                self.set_def_type(usize::from(name.text_range().start()), ty);
            }
        }
    }

    fn set_def_type(&mut self, name_start: usize, ty: Ty) {
        if let Some(&id) = self.def_by_name_start.get(&name_start) {
            self.def_types[id.0 as usize] = ty;
        }
    }

    // --- Pass 2: expression inference ---------------------------------------------------------

    /// Walks `root` post-order, typing every expression (children first), and fills a `var` local's
    /// type from its already-typed initializer.
    ///
    /// The walk is an explicit-stack post-order rather than per-node recursion: each node is pushed
    /// unexpanded, re-pushed expanded above its (reversed) children, and processed when popped
    /// expanded — visiting nodes in exactly the order the recursive walk did, so every child's type
    /// is memoised before its parent reads it and a same-span parent still overwrites last.
    async fn infer_in(&mut self, root: &SyntaxNode) {
        let mut yielder = Yielder::new();
        let mut stack: Vec<(SyntaxNode, bool)> = vec![(root.clone(), false)];
        while let Some((node, expanded)) = stack.pop() {
            if !expanded {
                stack.push((node.clone(), true));
                // `SyntaxNodeChildren` is not double-ended, so the reversal needs the buffer
                // (clippy's `needless_collect` suggestion of `.children().rev()` does not compile).
                let children: Vec<SyntaxNode> = node.children().collect();
                for child in children.into_iter().rev() {
                    stack.push((child, false));
                }
                continue;
            }
            yielder.tick().await;
            if let Some(expr) = ast::Expr::cast(node.clone()) {
                let r = node.text_range();
                let span = (usize::from(r.start()), usize::from(r.end()));
                let ty = self.compute_expr_ty(&expr);
                self.expr_by_span.insert(span, ty);
            } else if matches!(node.kind(), LOCAL_VAR_DECL | RESOURCE) {
                self.fill_var_binding(&node);
            }
        }
    }

    /// For a `var` local / resource, sets its definition type to the type of its initializer (which
    /// pass 2 has already inferred, since children are visited first).
    fn fill_var_binding(&mut self, node: &SyntaxNode) {
        let ty = node.children().find_map(ast::Type::cast);
        if !ty.as_ref().is_some_and(Cst::is_var_type) {
            return;
        }
        let init_ty = node
            .children()
            .find_map(ast::Expr::cast)
            .map_or(Ty::Unknown, |e| self.expr_ty(e.syntax()));
        for tok in Collect::direct_ident_tokens(node) {
            self.set_def_type(Collect::token_start(&tok), init_ty.clone());
        }
    }

    /// Computes an expression's type from its (already-typed) children.
    fn compute_expr_ty(&self, expr: &ast::Expr) -> Ty {
        match expr {
            ast::Expr::Literal(l) => Self::literal_ty(l),
            ast::Expr::NameRef(n) => self.nameref_ty(n.syntax()),
            ast::Expr::Paren(p) => self.child_ty(p.expr()),
            ast::Expr::Unary(u) => self.unary_ty(u),
            ast::Expr::Postfix(p) => self.child_ty(p.operand()),
            ast::Expr::Binary(b) => self.binary_ty(b),
            ast::Expr::Cast(c) => self.ty_of_opt_type(c.ty().as_ref()),
            ast::Expr::New(n) => self.new_ty(n),
            ast::Expr::Assignment(a) => self.child_ty(a.target()),
            ast::Expr::ArrayInit(a) => {
                let elem = self.child_ty(a.elements().next());
                if elem == Ty::Unknown {
                    Ty::Unknown
                } else {
                    Ty::Array(Box::new(elem))
                }
            }
            ast::Expr::Index(i) => self.index_ty(i),
            ast::Expr::Ternary(t) => self.ternary_ty(t),
            ast::Expr::FieldAccess(f) => self.field_access_ty(f),
            ast::Expr::Call(c) => self.call_ty(c),
            ast::Expr::Switch(s) => self.switch_ty(s),
            // A method reference and a lambda have no type of their own: they take one from the context
            // they appear in, which is the whole meaning of "target-typed".
            ast::Expr::MethodRef(e) => self.target_ty(e.syntax()),
            ast::Expr::Lambda(e) => self.target_ty(e.syntax()),
            ast::Expr::ClassLiteral(_) => self.java_lang_ty("Class"),
        }
    }

    /// The memoised type of the (already-visited) expression node, or [`Ty::Unknown`].
    /// The type a target-typed expression takes from its context (JLS §15.27.3): a lambda and a method
    /// reference are the two forms that have none of their own.
    ///
    /// Three contexts are read, which are the ones that name a type outright: a declaration's written type,
    /// an assignment's target, and the enclosing method's return type. An *argument* position needs the
    /// selected overload's parameter, and overload selection runs after this — so one stays `Unknown`
    /// rather than being guessed at from a candidate that may not be the one chosen.
    fn target_ty(&self, node: &SyntaxNode) -> Ty {
        let Some(parent) = node.parent() else {
            return Ty::Unknown;
        };
        match parent.kind() {
            // `F f = …` / `F f; f = …`: the declared type is written right there.
            LOCAL_VAR_DECL | FIELD_DECL => {
                let ty = parent.children().find_map(ast::Type::cast);
                if ty.as_ref().is_some_and(Cst::is_var_type) {
                    // `var f = () -> …` is not a Java program: there is no type to infer *from*.
                    return Ty::Unknown;
                }
                self.ty_of_opt_type(ty.as_ref())
            }
            ASSIGNMENT_EXPR => ast::AssignmentExpr::cast(parent)
                .and_then(|assignment| assignment.target())
                .map_or(Ty::Unknown, |target| self.expr_ty(target.syntax())),
            // `return () -> …`: the method's own return type, which is where a `return` value is checked
            // against anyway.
            RETURN_STMT => node
                .ancestors()
                .find(|ancestor| ancestor.kind() == METHOD_DECL)
                .and_then(|method| {
                    method
                        .children()
                        .find_map(ast::Type::cast)
                        .map(|ty| self.ty_of_opt_type(Some(&ty)))
                })
                .unwrap_or(Ty::Unknown),
            _ => Ty::Unknown,
        }
    }

    fn expr_ty(&self, node: &SyntaxNode) -> Ty {
        let r = node.text_range();
        self.expr_by_span
            .get(&(usize::from(r.start()), usize::from(r.end())))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }

    fn child_ty(&self, expr: Option<ast::Expr>) -> Ty {
        expr.map_or(Ty::Unknown, |e| self.expr_ty(e.syntax()))
    }

    fn nameref_ty(&self, node: &SyntaxNode) -> Ty {
        // A reference is keyed by its identifier *token* start, which excludes the leading trivia
        // that the `NAME_REF` node carries; look that token up, not the node.
        let Some(tok) = Collect::first_ident_token(node) else {
            // `this` has no identifier token and is never recorded as a reference, so its type has to
            // come from where it appears: the enclosing type. That is what makes `this.field` and
            // `this.method()` resolve at all — a constructor writing `this.x = x` is the ordinary
            // shape, and without it the access binds to nothing.
            //
            // `super` is deliberately left unknown. Its member lookup has to start at the
            // *superclass*, and answering it with the enclosing type would bind an overridden member
            // to the override rather than to the one `super` names.
            let is_this = node
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .any(|token| token.kind() == THIS_KW);
            return if is_this {
                self.self_ty(node)
            } else {
                Ty::Unknown
            };
        };
        if let Some(&ri) = self.ref_by_start.get(&Collect::token_start(&tok))
            && let Resolution::Def(id) = self.resolved.references[ri].resolution
        {
            return self.def_types[id.0 as usize].clone();
        }
        // Nothing in the file declared it, which an **inherited** field never is: name resolution is
        // file-local and a superclass's field may not even be in this file. So the name is looked up on
        // the enclosing type, which is the implicit `this` a bare call already reads its callee from —
        // and the member lookup walks the supertypes. Without this `own + 1` had an operand of no type
        // at all, in a class where the read itself is perfectly ordinary.
        self.member_ty(&self.self_ty(node), tok.text(), Namespace::Value)
    }

    /// The indexed type `name` resolves to from this file, or an external one by that name.
    ///
    /// A `.class` literal's type is `java.lang.Class`, and reaching *its* members — `getName()` — needs
    /// the indexed stub. An external type by that name has no members at all, so the access resolved
    /// to nothing and the call after it with it.
    fn java_lang_ty(&self, name: &str) -> Ty {
        let Some((index, file)) = self.project else {
            return Ty::Class(ClassTy::external(name));
        };
        index
            .resolve_type_name(file, name, None)
            .project_id()
            .map_or_else(
                || Ty::Class(ClassTy::external(name)),
                |id| {
                    Ty::Class(ClassTy::Project {
                        id,
                        name: name.to_owned(),
                        args: Vec::new(),
                    })
                },
            )
    }

    /// The type `this` has where `node` appears: the enclosing type declaration, raw.
    ///
    /// Raw — no type arguments — because inside a generic type's own body its parameters stand for
    /// themselves, and a member read through `this` substitutes them by name.
    fn self_ty(&self, node: &SyntaxNode) -> Ty {
        let (Some(item), Some((index, _))) = (self.enclosing_item(node), self.project) else {
            return Ty::Unknown;
        };
        let fqn = index.item(item).fqn.as_str();
        Ty::Class(ClassTy::Project {
            id: item,
            name: fqn.rsplit('.').next().unwrap_or(fqn).to_owned(),
            args: Vec::new(),
        })
    }

    fn unary_ty(&self, u: &ast::UnaryExpr) -> Ty {
        let operand = self.child_ty(u.operand());
        match Cst::op_kinds(u.syntax()).first() {
            Some(BANG) => Ty::Primitive(Primitive::Boolean),
            Some(TILDE | PLUS | MINUS) => operand.unary_promote(),
            // Prefix `++` / `--` keep the operand type.
            _ => operand,
        }
    }

    fn binary_ty(&self, b: &ast::BinaryExpr) -> Ty {
        let ops = Cst::op_kinds(b.syntax());
        // `instanceof` carries no right operand (its RHS is a type/pattern).
        if ops.contains(&INSTANCEOF_KW) {
            return Ty::Primitive(Primitive::Boolean);
        }
        let lhs = self.child_ty(b.lhs());
        let rhs = self.child_ty(b.rhs());
        let boolean = Ty::Primitive(Primitive::Boolean);
        match ops.as_slice() {
            // Conditional, equality, and relational operators (`>` is `GT`, `>=` is `GT EQ`).
            [AMP_AMP | PIPE_PIPE | EQ_EQ | BANG_EQ | LT | LT_EQ | GT] | [GT, EQ] => boolean,
            // Shifts (`<<` is `LSHIFT`; `>>` / `>>>` are repeated `GT`): the promoted left operand.
            [LSHIFT] | [GT, GT] | [GT, GT, GT] => lhs.unary_promote(),
            // `+` is string concatenation when either side is a `String`, else arithmetic.
            [PLUS] => {
                if lhs.is_string() || rhs.is_string() {
                    Ty::string()
                } else {
                    lhs.binary_numeric(&rhs)
                }
            }
            [MINUS | STAR | SLASH | PERCENT] => lhs.binary_numeric(&rhs),
            // `&` `|` `^` are logical on booleans, bitwise (numeric) otherwise.
            [AMP | PIPE | CARET] => {
                if Self::is_boolean(&lhs) && Self::is_boolean(&rhs) {
                    Ty::Primitive(Primitive::Boolean)
                } else {
                    lhs.binary_numeric(&rhs)
                }
            }
            _ => Ty::Unknown,
        }
    }

    fn new_ty(&self, n: &ast::NewExpr) -> Ty {
        let base = self.ty_of_opt_type(n.ty().as_ref());
        base.array_of(Cst::lbrack_count(n.syntax()))
    }

    fn index_ty(&self, i: &ast::IndexExpr) -> Ty {
        let parts: Vec<ast::Expr> = i.parts().collect();
        let Some(base) = parts.first() else {
            return Ty::Unknown;
        };
        // The first part is the array; each remaining part is an index, peeling one array level.
        let mut t = self.expr_ty(base.syntax());
        for _ in 1..parts.len() {
            t = match t {
                Ty::Array(elem) => *elem,
                _ => Ty::Unknown,
            };
        }
        t
    }

    fn ternary_ty(&self, t: &ast::TernaryExpr) -> Ty {
        // parts: condition, then-branch, else-branch.
        let mut parts = t.parts();
        let _cond = parts.next();
        let then_ty = self.child_ty(parts.next());
        let else_ty = self.child_ty(parts.next());
        Self::join_exact([then_ty, else_ty])
    }

    /// A switch expression's type: the [`join_exact`](Inferer::join_exact) of every arm that
    /// produces a value ([`ast::SwitchExpr::result_exprs`]). A `throw` arm produces no value and
    /// is ignored; a body-less or value-less switch joins empty, to [`Ty::Unknown`].
    fn switch_ty(&self, s: &ast::SwitchExpr) -> Ty {
        Self::join_exact(s.result_exprs().map(|e| self.expr_ty(e.syntax())))
    }

    /// The join shared by the branching expressions (ternary, switch): the arms' common type when
    /// they all agree, and [`Ty::Unknown`] for an empty join or an un-inferable arm.
    ///
    /// When the arms disagree, one case is still answerable without a class-hierarchy walk. JLS
    /// §15.25 gives a *numeric* conditional the binary numeric promotion of its arms, and where that
    /// promotion is `long` / `float` / `double` the answer is unambiguous: the wide type wins
    /// outright, with none of §15.25's constant-narrowing special cases in play. `cond ? 1 : 2L` is a
    /// `long`, and leaving it unknown made every overload taking it unselectable.
    ///
    /// Everything else stays `Ty::Unknown`, which keeps the "never a false type" guarantee. A
    /// promotion among the sub-`long` integrals depends on whether an arm is a constant in range
    /// (`cond ? aByte : aShort` is `short`, not `int`) and there is no constant evaluator here; a
    /// mixed *reference* join needs a least upper bound over a hierarchy; `null` widening would need
    /// the other arm to be known reference-typed.
    fn join_exact(tys: impl IntoIterator<Item = Ty>) -> Ty {
        let tys: Vec<Ty> = tys.into_iter().collect();
        let Some(first) = tys.first() else {
            return Ty::Unknown;
        };
        if tys.iter().all(|ty| ty == first) {
            return first.clone();
        }
        Self::join_numeric(&tys)
    }

    /// The binary numeric promotion of every arm, when they are all numeric and it lands on one of
    /// the three wide types. See [`join_exact`](Inferer::join_exact) for why the narrow ones do not.
    fn join_numeric(tys: &[Ty]) -> Ty {
        let mut widest = 0u8;
        for ty in tys {
            let Ty::Primitive(primitive) = ty else {
                return Ty::Unknown;
            };
            widest = widest.max(match primitive {
                Primitive::Double => 3,
                Primitive::Float => 2,
                Primitive::Long => 1,
                Primitive::Byte | Primitive::Short | Primitive::Char | Primitive::Int => 0,
                // `boolean` takes part in no numeric promotion at all.
                Primitive::Boolean => return Ty::Unknown,
            });
        }
        match widest {
            3 => Ty::Primitive(Primitive::Double),
            2 => Ty::Primitive(Primitive::Float),
            1 => Ty::Primitive(Primitive::Long),
            _ => Ty::Unknown,
        }
    }

    // --- Syntactic type -> Ty -----------------------------------------------------------------

    fn ty_of_opt_type(&self, ty: Option<&ast::Type>) -> Ty {
        ty.map_or(Ty::Unknown, |t| self.ty_of_type(t))
    }

    fn ty_of_type(&self, ty: &ast::Type) -> Ty {
        let base = self.base_ty_of_type(ty);
        base.array_of(Cst::lbrack_count(ty.syntax()))
    }

    fn base_ty_of_type(&self, ty: &ast::Type) -> Ty {
        if ty.is_primitive_or_var() {
            Collect::direct_tokens(ty.syntax())
                .find_map(|t| Self::primitive_ty(t.kind()))
                .unwrap_or(Ty::Unknown)
        } else {
            self.class_ty_of_ref_type(ty)
        }
    }

    /// Resolves a reference type's simple name against the project, falling back to an external
    /// (by-name) type. A type parameter or other file-local non-type-decl name resolves to nothing
    /// in the index and is treated as external by spelling — fine for display.
    fn class_ty_of_ref_type(&self, ty: &ast::Type) -> Ty {
        let Some(tok) = ty.simple_name_token() else {
            return Ty::Unknown;
        };
        let name = tok.text().to_owned();
        let args = self.type_args_of(ty);
        if let Some((index, file)) = self.project
            && let Some(&ri) = self.ref_by_start.get(&Collect::token_start(&tok))
            && let TypeResolution::Project(id) =
                index.resolve_reference(file, &self.resolved.references[ri])
        {
            return Ty::Class(ClassTy::Project { id, name, args });
        }
        Ty::Class(ClassTy::External { name, args })
    }

    /// The type arguments written on a reference type (`List<String>` → `[String]`), each converted
    /// to a [`Ty`]; empty when the type is raw or argument-free. A wildcard argument (`?`,
    /// `? extends T`) has no nameable type and converts to [`Ty::Unknown`].
    fn type_args_of(&self, ty: &ast::Type) -> Vec<Ty> {
        ty.type_arg_types()
            .map(|arg| self.ty_of_type(&arg))
            .collect()
    }

    // --- Member-dependent inference -----------------------------------------------------------

    /// `receiver.field`: the type of the field on the receiver's project type. Member-typed only
    /// when the receiver is an indexed project type; an external receiver (a JDK type) stays
    /// [`Ty::Unknown`], since its members are not indexed.
    fn field_access_ty(&self, fa: &ast::FieldAccess) -> Ty {
        self.field_access_member_ty(fa, Namespace::Value)
    }

    /// `receiver.member` resolved in `namespace`: the member's type on the receiver's project type.
    /// Shared by a plain field access (a value) and a qualified call's `recv.method` callee (a
    /// method), which differ only in the name-space they look the member up in.
    fn field_access_member_ty(&self, fa: &ast::FieldAccess, namespace: Namespace) -> Ty {
        let Some(name) = fa.field() else {
            return Ty::Unknown;
        };
        let Some(expr) = fa.receiver() else {
            return Ty::Unknown;
        };
        let receiver = self.child_ty(Some(expr.clone()));
        // JLS §10.7: every array type has exactly one member, a `public final int length`. It is
        // declared nowhere, so no index lookup can find it — leaving `a.length` untyped, which is
        // the type every `for (int i = 0; i < a.length; i++)` in Java depends on.
        if namespace == Namespace::Value && name == "length" && matches!(receiver, Ty::Array(_)) {
            return Ty::Primitive(Primitive::Int);
        }
        match receiver {
            Ty::Unknown => match self.project {
                // Not a value: `System.out` names the declaring class, not an instance of it.
                Some((index, file)) => Cst::type_qualifier(&expr, index, file)
                    .map_or(Ty::Unknown, |owner| {
                        self.member_ty_in(owner, &[], &name, namespace)
                    }),
                None => Ty::Unknown,
            },
            receiver => self.member_ty(&receiver, &name, namespace),
        }
    }

    /// `callee(args)`: the called method's return type. A qualified call `receiver.method()` looks
    /// the method up on the receiver's type; a bare call `method()` looks it up on the enclosing
    /// type (an implicit `this`). Only project types resolve — everything else is [`Ty::Unknown`].
    fn call_ty(&self, call: &ast::CallExpr) -> Ty {
        match call.callee() {
            Some(ast::Expr::FieldAccess(fa)) => self.field_access_member_ty(&fa, Namespace::Method),
            Some(ast::Expr::NameRef(n)) => {
                let Some(name) =
                    Collect::first_ident_token(n.syntax()).map(|t| t.text().to_owned())
                else {
                    return Ty::Unknown;
                };
                // A bare (`this`) call resolves against the enclosing type, which is used raw so its
                // own type variables stay un-substituted (they survive by name); no enclosing type
                // means the callee is unknown.
                self.enclosing_item(call.syntax())
                    .map_or(Ty::Unknown, |owner| {
                        self.member_ty_in(owner, &[], &name, Namespace::Method)
                    })
            }
            _ => Ty::Unknown,
        }
    }

    /// The type of the member `name` (in `namespace`) reachable from receiver type `receiver` — a
    /// field's type or a method's return type — when `receiver` is an indexed project type.
    fn member_ty(&self, receiver: &Ty, name: &str, namespace: Namespace) -> Ty {
        // A project receiver carries the type arguments to substitute into the member's type; any
        // other receiver (primitive, external, array, type variable) has no indexed members.
        match receiver {
            Ty::Class(ClassTy::Project { id, args, .. }) => {
                self.member_ty_in(*id, args, name, namespace)
            }
            _ => Ty::Unknown,
        }
    }

    /// The type of member `name` (in `namespace`) on project type `owner` with type arguments
    /// `owner_args` bound — [`ProjectIndex::member_ty_substituted`] guarded by the project index being
    /// present.
    fn member_ty_in(
        &self,
        owner: ItemId,
        owner_args: &[Ty],
        name: &str,
        namespace: Namespace,
    ) -> Ty {
        match self.project {
            Some((index, _)) => index.member_ty_substituted(owner, owner_args, name, namespace),
            None => Ty::Unknown,
        }
    }

    /// The enclosing project type of `node`: the nearest ancestor type declaration that is an
    /// indexed item, for resolving a bare (`this`) method call.
    fn enclosing_item(&self, node: &SyntaxNode) -> Option<ItemId> {
        let (index, file) = self.project?;
        index.enclosing_item(file, node)
    }
}

impl ProjectIndex {
    /// Turns a member's captured [`MemberType`] into a concrete [`Ty`], resolving a named type against
    /// the project from the member's *declaring* `file` (its import / package context). `owner` is the
    /// type whose declaration the `MemberType` lives in: a bare name matching one of its type
    /// parameters becomes a [`Ty::TypeVar`] (to be substituted by the caller) rather than an external
    /// by-name type. Exposed so a caller holding only a [`TypeInference`] (e.g. argument checking) can
    /// use it.
    pub(crate) fn member_type_to_ty(&self, file: FileId, owner: ItemId, mt: &MemberType) -> Ty {
        match mt {
            MemberType::Primitive { keyword, dims } => {
                let base = Primitive::from_keyword(keyword).map_or(Ty::Unknown, Ty::Primitive);
                base.array_of(*dims as usize)
            }
            MemberType::Void => Ty::Void,
            MemberType::Named {
                name,
                qualified,
                dims,
                args,
            } => {
                let base = if qualified.is_none() && self.is_type_param(owner, name) {
                    // A bare name matching one of `owner`'s type parameters is a type variable (`E`),
                    // recorded for later substitution (a type variable takes no arguments of its own).
                    Ty::TypeVar {
                        owner,
                        name: name.clone(),
                    }
                } else {
                    // Otherwise a project / external type, with its concrete arguments carried
                    // recursively (`List<String>` → element `String`; `List<E>` → element var `E`).
                    let ty_args = args
                        .iter()
                        .map(|a| self.member_type_to_ty(file, owner, a))
                        .collect();
                    match self.resolve_type_name(file, name, qualified.as_deref()) {
                        TypeResolution::Project(id) => Ty::Class(ClassTy::Project {
                            id,
                            name: name.clone(),
                            args: ty_args,
                        }),
                        TypeResolution::External | TypeResolution::Unresolved => {
                            Ty::Class(ClassTy::External {
                                name: name.clone(),
                                args: ty_args,
                            })
                        }
                    }
                };
                base.array_of(*dims as usize)
            }
            MemberType::Unknown => Ty::Unknown,
        }
    }

    /// The concrete type of member `name` (in `namespace`) accessed on a receiver of project type
    /// `owner` with type arguments `owner_args` — i.e. [`member_type_to_ty`](ProjectIndex::member_type_to_ty)
    /// with the receiver's generic arguments bound. Searches `owner` and its project-internal
    /// supertypes nearest-first (mirroring [`ProjectIndex::resolve_member`]'s shadowing), substituting
    /// each type variable by the argument propagated down the inheritance chain (`Sub extends
    /// Base<String>` binds `Base`'s `T` to `String`). [`Ty::Unknown`] when no such member resolves. A
    /// raw receiver (no arguments) leaves the member's type variables un-substituted, so they survive
    /// by name.
    fn member_ty_substituted(
        &self,
        owner: ItemId,
        owner_args: &[Ty],
        name: &str,
        namespace: Namespace,
    ) -> Ty {
        // Each frame's state is a type's concrete type arguments, as seen from the original receiver;
        // the shared inheritance walk threads them down — binding a supertype's arguments through the
        // current type's substitution so a type variable threaded `Sub<U> extends Base<U>` resolves all
        // the way down.
        self.walk_supertypes_stateful(
            owner,
            owner_args.to_vec(),
            |current, args| {
                let member_id = self.declared_member(current, name, namespace)?;
                let member = self.member(member_id);
                Some(self.subst_member_ty(current, args, member.file, &member.ty))
            },
            |current, args, sup| {
                let file = self.item(current).file;
                sup.args
                    .iter()
                    .map(|mt| self.subst_member_ty(current, args, file, mt))
                    .collect()
            },
        )
        .unwrap_or(Ty::Unknown)
    }

    /// [`member_type_to_ty`](ProjectIndex::member_type_to_ty) for a member-type `mt` declared in
    /// `current` (in `file`), with `current`'s type parameters bound to `args`. A raw frame (`args`
    /// empty — a non-generic or raw receiver, the common case) needs no binding, so the converted type
    /// is returned directly instead of cloning the whole tree through a no-op [`Ty::substitute`].
    fn subst_member_ty(&self, current: ItemId, args: &[Ty], file: FileId, mt: &MemberType) -> Ty {
        let ty = self.member_type_to_ty(file, current, mt);
        if args.is_empty() {
            ty
        } else {
            ty.substitute(&self.subst_fn(current, args))
        }
    }

    /// The substitution for a use of `owner` with type arguments `args`: a function binding each of
    /// `owner`'s type parameters, by position, to the supplied argument (suitable for
    /// [`Ty::substitute`]). A raw use (fewer arguments than parameters, typically none) leaves the
    /// surplus parameters unbound, so they stay type variables.
    fn subst_fn(&self, owner: ItemId, args: &[Ty]) -> impl Fn(ItemId, &str) -> Option<Ty> {
        let bindings: HashMap<String, Ty> = self
            .item(owner)
            .type_params
            .iter()
            .zip(args)
            .map(|(p, arg)| (p.name.clone(), arg.clone()))
            .collect();
        move |o, n| (o == owner).then(|| bindings.get(n).cloned()).flatten()
    }

    /// The nearest ancestor type declaration of `node` that is an indexed project item, in `file`.
    /// Shared by the [`Inferer`] (bare-call resolution) and argument checking.
    fn enclosing_item(&self, file: FileId, node: &SyntaxNode) -> Option<ItemId> {
        let decl = node
            .ancestors()
            .find(|a| Self::type_decl_kind(a.kind()).is_some())?;
        let name = Collect::first_ident_token(&decl)?;
        self.item_by_decl(file, Collect::token_start(&name))
    }
}

impl Inferer<'_> {
    /// Whether a node kind has an explicitly-written type as a direct `TYPE` child and its declared
    /// name(s) as direct `IDENT` tokens. For a `METHOD_DECL` the direct `TYPE` child is the *return*
    /// type and the direct `IDENT` is the method name, so the method's definition is typed with its
    /// return type — used to check `return` statements (its parameters' types are nested and
    /// unaffected).
    const fn declares_typed_bindings(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            FIELD_DECL
                | LOCAL_VAR_DECL
                | PARAM
                | RECORD_COMPONENT
                | CATCH_CLAUSE
                | FOR_EACH_STMT
                | RESOURCE
                | METHOD_DECL
        )
    }

    /// Whether `node` is a `PARAM` declared with `...`.
    fn is_variable_arity(node: &SyntaxNode) -> bool {
        node.kind() == PARAM
            && node
                .children_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .any(|t| t.kind() == ELLIPSIS)
    }

    /// The [`Ty`] of a primitive (or `void`) type keyword, or `None` for any other token.
    const fn primitive_ty(kind: SyntaxKind) -> Option<Ty> {
        let p = match kind {
            BOOLEAN_KW => Primitive::Boolean,
            BYTE_KW => Primitive::Byte,
            SHORT_KW => Primitive::Short,
            INT_KW => Primitive::Int,
            LONG_KW => Primitive::Long,
            CHAR_KW => Primitive::Char,
            FLOAT_KW => Primitive::Float,
            DOUBLE_KW => Primitive::Double,
            VOID_KW => return Some(Ty::Void),
            _ => return None,
        };
        Some(Ty::Primitive(p))
    }

    /// The type of a literal, by its token kind (and suffix, for numbers).
    fn literal_ty(l: &ast::Literal) -> Ty {
        fn ends_with_ignore_case(text: &str, suffix: char) -> bool {
            text.chars()
                .next_back()
                .is_some_and(|c| c.eq_ignore_ascii_case(&suffix))
        }

        let Some(tok) = l.token() else {
            return Ty::Unknown;
        };
        let text = tok.text();
        match tok.kind() {
            INT_LITERAL => {
                if ends_with_ignore_case(text, 'l') {
                    Ty::Primitive(Primitive::Long)
                } else {
                    Ty::Primitive(Primitive::Int)
                }
            }
            FLOAT_LITERAL => {
                if ends_with_ignore_case(text, 'f') {
                    Ty::Primitive(Primitive::Float)
                } else {
                    Ty::Primitive(Primitive::Double)
                }
            }
            CHAR_LITERAL => Ty::Primitive(Primitive::Char),
            STRING_LITERAL | TEXT_BLOCK => Ty::string(),
            TRUE_KW | FALSE_KW => Ty::Primitive(Primitive::Boolean),
            NULL_KW => Ty::Null,
            _ => Ty::Unknown,
        }
    }

    const fn is_boolean(t: &Ty) -> bool {
        matches!(t, Ty::Primitive(Primitive::Boolean))
    }
}

/// Namespace for the low-level CST token / span helpers shared across inference, the constant-`if`
/// analysis, and the checked-exception analysis.
pub(crate) struct Cst;

impl Cst {
    /// The indexed type a *type-qualified* member access is looked up on: the `System` of
    /// `System.out`.
    ///
    /// A receiver is normally a value, and its inferred type names the owner. A class name in
    /// expression position is not a value and has no inferred type at all — nothing declares it, so
    /// the name-reference lookup finds no definition and the whole access stays `Unknown`. That is
    /// the shape every access to a `static` member takes, so resolving the qualifier as a *type
    /// name* is what makes `static` members reachable at all.
    ///
    /// Only a simple name is handled. A fully-qualified qualifier (`java.io.PrintStream.out`) is a
    /// nested field access, not a name reference, and is not modelled.
    fn type_qualifier(receiver: &ast::Expr, index: &ProjectIndex, file: FileId) -> Option<ItemId> {
        let ast::Expr::NameRef(name) = receiver else {
            return None;
        };
        let token = Collect::first_ident_token(name.syntax())?;
        index
            .resolve_type_name(file, token.text(), None)
            .project_id()
    }
}

impl Cst {
    /// Whether the syntactic type is `var` (local variable type inference).
    fn is_var_type(ty: &ast::Type) -> bool {
        Collect::direct_tokens(ty.syntax()).any(|t| t.kind() == VAR_KW)
    }

    /// The non-trivia operator token kinds directly under `node` (operands are child nodes, so for a
    /// binary/unary expression these are exactly the operator tokens).
    pub(crate) fn op_kinds(node: &SyntaxNode) -> Vec<SyntaxKind> {
        Collect::direct_tokens(node)
            .filter(|t| !t.kind().is_trivia())
            .map(|t| t.kind())
            .collect()
    }

    /// The count of `[` tokens directly under `node` — the array dimension count of a type or `new`.
    fn lbrack_count(node: &SyntaxNode) -> usize {
        Collect::direct_tokens(node)
            .filter(|t| t.kind() == LBRACK)
            .count()
    }

    /// The declarator-name → initializer pairs of a (possibly multi-declarator) variable or field
    /// declaration. The CST is flat (`final int a = 1, b = 2;` has no per-declarator node), so each
    /// direct `IDENT` token takes the next direct expression child as its initializer; a declarator
    /// without one yields no pair. The declared `TYPE` / `MODIFIERS` children are not expressions, so
    /// the `Expr::cast` skips them and they are never mistaken for an initializer.
    pub(crate) fn declarator_initializers(
        node: &SyntaxNode,
    ) -> impl Iterator<Item = (SyntaxToken, ast::Expr)> {
        let mut current: Option<SyntaxToken> = None;
        node.children_with_tokens().filter_map(move |elem| {
            if let Some(token) = elem.as_token() {
                if token.kind() == IDENT {
                    current = Some(token.clone());
                }
                return None;
            }
            let value = elem.into_node().and_then(ast::Expr::cast)?;
            Some((current.take()?, value))
        })
    }
}

// ===== Signature help =====

/// Signature help for a call site: the callee's overloads and where the cursor sits.
///
/// Produced by [`signature_help`]. A pure data shape (no LSP types), so a host can map it to its
/// protocol — the language server turns each [`Signature`] into an LSP `SignatureInformation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelp {
    /// The callee's overloads, nearest-type first (the order of
    /// [`ProjectIndex::resolve_members_all`]).
    pub signatures: Vec<Signature>,
    /// The overload to highlight: the first that has a parameter at [`active_parameter`], else 0.
    pub active_signature: usize,
    /// The zero-based index of the argument the cursor is in (the count of commas before it).
    pub active_parameter: usize,
}

/// One overload's rendered signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The full signature text, e.g. `area(int width, int height)`.
    pub label: String,
    /// The byte range within [`label`](Signature::label) of each parameter, for client-side
    /// highlighting of the active one.
    pub parameters: Vec<Range<usize>>,
}

impl ProjectIndex {
    /// Signature help for the call whose argument list contains byte `offset`: the overloads of the
    /// method being called, plus the argument index the cursor is on.
    ///
    /// Resolves the callee like [`TypeInference::check_call`] — a qualified `recv.m(..)` on the
    /// receiver's project type, or a bare `m(..)` on the enclosing type — then renders every overload.
    /// Returns `None` when the cursor is in no call, the receiver is not an indexed project type (e.g.
    /// an external/JDK type), or the method names no project member. Never panics.
    pub async fn signature_help(
        &self,
        root: &SyntaxNode,
        resolved: &Resolved,
        file: FileId,
        offset: usize,
    ) -> Option<SignatureHelp> {
        let (call, active_parameter) = Cst::enclosing_call(root, offset)?;
        let ti = TypeInference::infer(root, resolved, self, file).await;
        let (owner, name) = ti.call_target(&call, self, file)?;
        let signatures: Vec<Signature> = self
            .resolve_members_all(owner, &name, Namespace::Method)
            .into_iter()
            .map(|id| self.render_signature(self.member(id)))
            .collect();
        if signatures.is_empty() {
            return None;
        }
        // Highlight the overload that actually has a parameter at the cursor's index; if none does
        // (the cursor is past every overload's arity), fall back to the first.
        let active_signature = signatures
            .iter()
            .position(|s| s.parameters.len() > active_parameter)
            .unwrap_or(0);
        Some(SignatureHelp {
            signatures,
            active_signature,
            active_parameter,
        })
    }

    /// Renders one member's signature as `name(type1 p1, type2 p2)`, recording each parameter's byte
    /// range within the label. A parameter with no readable name is rendered as its type alone.
    fn render_signature(&self, member: &crate::Member) -> Signature {
        let mut label = String::new();
        label.push_str(&member.name);
        label.push('(');
        let mut parameters = Vec::with_capacity(member.params.len());
        for (i, param) in member.params.iter().enumerate() {
            if i > 0 {
                label.push_str(", ");
            }
            let ty = self
                .member_type_to_ty(member.file, member.owner, &param.ty)
                .to_string();
            let text = match &param.name {
                Some(name) => format!("{ty} {name}"),
                None => ty,
            };
            let start = label.len();
            label.push_str(&text);
            parameters.push(start..label.len());
        }
        label.push(')');
        Signature { label, parameters }
    }
}

impl Cst {
    /// The innermost call whose argument list (between the parens) contains `offset`, with the
    /// cursor's argument index (commas before it). Scans every call so a nested `outer(inner(|))`
    /// picks `inner` (the smallest containing argument list).
    fn enclosing_call(root: &SyntaxNode, offset: usize) -> Option<(ast::CallExpr, usize)> {
        let (call, args, _) = root
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .filter_map(|call| {
                let args = call.args()?;
                let span = Collect::node_span(args.syntax());
                (span.start <= offset && offset <= span.end).then_some((
                    call,
                    args,
                    span.end - span.start,
                ))
            })
            .min_by_key(|(.., width)| *width)?;
        let active = Self::active_parameter(&args, offset);
        Some((call, active))
    }

    /// The argument index the cursor at `offset` is on: the number of top-level commas in `args` that
    /// end at or before it. `f(|)` → 0, `f(a, |)` → 1.
    fn active_parameter(args: &ast::ArgList, offset: usize) -> usize {
        Collect::direct_tokens(args.syntax())
            .filter(|t| t.kind() == COMMA && usize::from(t.text_range().end()) <= offset)
            .count()
    }
}

// ===== Member completion =====

/// One member-access completion candidate: a field or method reachable on the receiver's type.
///
/// Produced by [`member_completions`]. A pure data shape (no LSP types), so a host maps it to its
/// protocol — the language server turns each into an LSP `CompletionItem`, using [`kind`](Completion::kind)
/// for the item icon and [`detail`](Completion::detail) for the type / signature shown beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The member's simple name — what is inserted and what the editor filters the typed prefix on.
    pub label: String,
    /// Whether the member is a [`Field`](DefKind::Field) or a [`Method`](DefKind::Method), for the
    /// completion-item kind.
    pub kind: DefKind,
    /// The type / signature shown beside the label: a field's type (`int`), or a method's parameter
    /// list and return type (`(int w, int h): int`).
    pub detail: String,
}

impl ProjectIndex {
    /// The member-access completions for `receiver.` at byte `offset`: the fields and methods
    /// reachable on the receiver's type, when that receiver is an indexed project type.
    ///
    /// Anchors on the `.` just left of the cursor — both for a bare `receiver.` (which does not parse
    /// as a field access, the dot having no member name yet) and a partially-typed `receiver.fo` —
    /// then infers the receiver and enumerates its members (its own and inherited,
    /// [`ProjectIndex::members_of`]). A `this.` / `super.` receiver completes the enclosing type's
    /// members. Returns an empty list when the cursor is on no member access, or the receiver is not
    /// an indexed project type (an external / JDK type, whose members are not indexed). One entry per
    /// distinct name (a field shadows, overloads collapse to one); the editor filters by the typed
    /// prefix. Never panics.
    pub async fn member_completions(
        &self,
        root: &SyntaxNode,
        resolved: &Resolved,
        file: FileId,
        offset: usize,
    ) -> Vec<Completion> {
        let Some(owner) = self.receiver_owner(root, resolved, file, offset).await else {
            return Vec::new();
        };
        let mut yielder = Yielder::new();
        let mut seen: HashSet<(String, Namespace)> = HashSet::new();
        let mut out = Vec::new();
        for id in self.members_of(owner) {
            yielder.tick().await;
            let member = self.member(id);
            // Only instance-accessible members complete after `.`: fields and methods, not
            // constructors or enum constants.
            if !matches!(member.kind, DefKind::Field | DefKind::Method) {
                continue;
            }
            // Nearest-first order means an own / overriding member is seen before the one it hides;
            // keep the first per (name, name-space) and drop the rest (a shadowed field, a further
            // overload).
            if seen.insert((member.name.clone(), member.kind.namespace())) {
                out.push(self.completion_of(member));
            }
        }
        out
    }

    /// Builds a [`Completion`] for `member`: a field's detail is its type; a method's is its parameter
    /// list and return type (`(int w, int h): int`), reusing
    /// [`render_signature`](ProjectIndex::render_signature) for the parameters.
    fn completion_of(&self, member: &crate::Member) -> Completion {
        let detail = match member.kind {
            DefKind::Method => {
                let signature = self.render_signature(member);
                let params = &signature.label[member.name.len()..];
                let ret = self.member_type_to_ty(member.file, member.owner, &member.ty);
                format!("{params}: {ret}")
            }
            _ => self
                .member_type_to_ty(member.file, member.owner, &member.ty)
                .to_string(),
        };
        Completion {
            label: member.name.clone(),
            kind: member.kind,
            detail,
        }
    }

    /// The indexed project type whose member is being completed at `offset`: the inferred type of the
    /// expression before the `.` just left of the cursor, or — for a `this.` / `super.` receiver — the
    /// enclosing type. `None` when the cursor is on no member access or the receiver is not a project
    /// type.
    ///
    /// Anchors structurally first and only runs the (whole-file) type inference once a real receiver
    /// expression is found — so a cursor on no member access, or a `this.` / `super.` receiver, costs
    /// no inference at all (member completion is triggered on every `.`).
    async fn receiver_owner(
        &self,
        root: &SyntaxNode,
        resolved: &Resolved,
        file: FileId,
        offset: usize,
    ) -> Option<ItemId> {
        let dot = Cst::member_access_dot(root, offset)?;
        let before = Cst::prev_significant(&dot)?;
        // A `this` / `super` receiver has no inferred type; its members are the enclosing type's (for
        // `super` the strictly-inherited ones — approximated here by the whole enclosing member set).
        if matches!(before.kind(), THIS_KW | SUPER_KW) {
            return self.enclosing_item(file, &before.parent()?);
        }
        let dot_start = usize::from(dot.text_range().start());
        let receiver = Cst::receiver_node(&before, dot_start)?;
        let ti = TypeInference::infer(root, resolved, self, file).await;
        ti.type_of_expr(Collect::node_span(&receiver))?.project_id()
    }

    /// Whether the cursor at byte `offset` is in a member-access position (just after a `.`, or in a
    /// member name following one).
    ///
    /// The host dispatches on this: a member access completes members
    /// ([`member_completions`](ProjectIndex::member_completions)); any other position completes the
    /// scope ([`scope_completions`](ProjectIndex::scope_completions)). A purely syntactic pre-check —
    /// it reads only `root` and `offset`, not the index.
    pub fn at_member_access(root: &SyntaxNode, offset: usize) -> bool {
        Cst::member_access_dot(root, offset).is_some()
    }

    /// The scope completions at byte `offset`: every binding visible there plus every project type by
    /// simple name.
    ///
    /// These are the candidates for a bare identifier position (not after a `.`; the host gates on
    /// [`at_member_access`](ProjectIndex::at_member_access)).
    ///
    /// Bindings come from the cursor's scope chain, innermost outward: a block / `for` / resources
    /// scope contributes only the locals declared before the cursor (sequential visibility), every
    /// other scope all of its bindings (parameters, type parameters, and hoisted type members — a
    /// field or method is reachable without `this.`). An inner binding shadows an outer one of the
    /// same name and name-space. Project types from other files are then added by simple name. One
    /// entry per (name, name-space); the editor filters by the typed prefix. Never panics.
    pub async fn scope_completions(
        &self,
        root: &SyntaxNode,
        resolved: &Resolved,
        file: FileId,
        offset: usize,
    ) -> Vec<Completion> {
        let ti = TypeInference::infer(root, resolved, self, file).await;
        let mut yielder = Yielder::new();
        let mut seen: HashSet<(String, Namespace)> = HashSet::new();
        let mut out = Vec::new();
        // Visible bindings, innermost scope outward (the first seen per name / name-space wins, so an
        // inner binding shadows an outer one).
        for def in resolved.visible_defs(offset) {
            yielder.tick().await;
            // A constructor is not a name completed in an expression position.
            if def.kind == DefKind::Constructor {
                continue;
            }
            if seen.insert((def.name.clone(), def.kind.namespace())) {
                out.push(ti.binding_completion(def));
            }
        }
        // Project type names from other files (a sibling type already in scope is deduped away). The
        // simple name completes; the fully-qualified name is the detail.
        for (_, item) in self.items() {
            yielder.tick().await;
            let name = item.fqn.simple_name().to_owned();
            if seen.insert((name.clone(), Namespace::Type)) {
                out.push(Completion {
                    label: name,
                    kind: item.kind,
                    detail: item.fqn.to_string(),
                });
            }
        }
        out
    }
}

impl Cst {
    /// The receiver expression that ends at the `.`: the outermost expression node containing `before`
    /// (the token just before the dot) that still ends at or before `dot_start` — so for `a.b.c.|` it
    /// is `a.b.c`, and for a partial `recv.fo|` it is `recv` (the enclosing `recv.fo` field access
    /// ends *after* the dot and is excluded).
    fn receiver_node(before: &SyntaxToken, dot_start: usize) -> Option<SyntaxNode> {
        // The nearest expression ancestor of `before`.
        let mut node = before.parent()?;
        while ast::Expr::cast(node.clone()).is_none() {
            node = node.parent()?;
        }
        // Climb to the outermost expression that still ends before the dot.
        while let Some(parent) = node.parent() {
            if ast::Expr::cast(parent.clone()).is_some()
                && usize::from(parent.text_range().end()) <= dot_start
            {
                node = parent;
            } else {
                break;
            }
        }
        Some(node)
    }

    /// The token just left of byte `offset`: the one ending at or covering it (left-biased at a
    /// boundary, so a cursor right after `.` lands on the `.`). `None` before the first token.
    fn token_left_of(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
        root.descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| {
                let range = token.text_range();
                usize::from(range.start()) < offset && offset <= usize::from(range.end())
            })
            .max_by_key(|token| usize::from(token.text_range().start()))
    }

    /// The nearest non-trivia token before `token`, or `None` at the start of the file.
    fn prev_significant(token: &SyntaxToken) -> Option<SyntaxToken> {
        let mut current = token.prev_token();
        while let Some(tok) = current {
            if !tok.kind().is_trivia() {
                return Some(tok);
            }
            current = tok.prev_token();
        }
        None
    }

    /// The `.` of the member access at byte `offset`, if the cursor is in one: the `.` token itself
    /// for `receiver.|`, or the `.` before a partially-typed member name for `receiver.fo|`. The
    /// anchor both member completion and
    /// [`ProjectIndex::at_member_access`] are built on.
    fn member_access_dot(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
        let token = Self::token_left_of(root, offset)?;
        match token.kind() {
            DOT => Some(token),
            IDENT => Self::prev_significant(&token).filter(|t| t.kind() == DOT),
            _ => None,
        }
    }
}

// ===== Scope completion =====

impl TypeInference {
    /// Builds a [`Completion`] for a visible binding: a value / method binding shows its inferred type
    /// as the detail (when known); a type binding (a sibling class, a type parameter) has none.
    fn binding_completion(&self, def: &Def) -> Completion {
        let ty = self.type_of_def(def.id);
        let detail = if def.kind.namespace() == Namespace::Type || *ty == Ty::Unknown {
            String::new()
        } else {
            ty.to_string()
        };
        Completion {
            label: def.name.clone(),
            kind: def.kind,
            detail,
        }
    }
}
