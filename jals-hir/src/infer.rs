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

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use jals_syntax::SyntaxKind::*;
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::def::{Def, DefId, DefKind, Namespace};
use crate::project::{FileId, ItemId, MemberType, ProjectIndex, TypeResolution};
use crate::reference::Resolution;
use crate::resolve::Resolved;
use crate::resolve::collect::first_ident_token;
use crate::ty::{ClassTy, Primitive, Ty, binary_numeric, string_ty, unary_promote};

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
}

impl TypeInference {
    /// The type inferred for the definition `id`.
    pub fn type_of_def(&self, id: DefId) -> &Ty {
        &self.def_types[id.0 as usize]
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

/// Infers types for `root` (a `SOURCE_FILE`), resolving reference type names against `index` from
/// the perspective of `file`. `resolved` is the file's name resolution.
pub fn infer(
    root: &SyntaxNode,
    resolved: &Resolved,
    index: &ProjectIndex,
    file: FileId,
) -> TypeInference {
    Inferer::new(root, resolved, Some((index, file))).run()
}

/// Infers types for `root` without a project index: reference type names resolve only to
/// [`ClassTy::External`] (by spelling), but all structural inference — primitives, arrays, literals,
/// numeric promotion, `var` from initializer — still works. For file-local tooling holding no index.
pub fn infer_node(root: &SyntaxNode, resolved: &Resolved) -> TypeInference {
    Inferer::new(root, resolved, None).run()
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
    fn assignment(range: Range<usize>, expected: Ty, found: Ty) -> TypeMismatch {
        TypeMismatch {
            range,
            kind: MismatchKind::Assignment { expected, found },
        }
    }

    /// A call to `name` with argument types `args` that no same-arity overload accepts.
    fn no_overload(range: Range<usize>, name: String, args: Vec<Ty>) -> TypeMismatch {
        TypeMismatch {
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
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("no overload of `{name}` accepts the argument types ({list})")
            }
        }
    }
}

/// Reports the assignment-context type mismatches in `root` (a `SOURCE_FILE`): a variable
/// initializer or a simple `=` assignment whose value type is not assignable to its slot type.
///
/// `project = Some((index, file))` infers reference types against the project, so a project-internal
/// subtyping mismatch (a `Sub`/`Base` confusion) is caught; `None` infers file-locally, where
/// reference types stay external and lenient, so only primitive, `null`, and array mismatches
/// surface. Conservative throughout (it builds on [`Ty::is_assignable_to`]): an `Unknown` type, an
/// external/boxing pair, and a numeric constant that narrowing could rescue are never reported, so a
/// consumer turning these into diagnostics never shows a false positive. Pure; never panics.
pub fn type_mismatches(
    root: &SyntaxNode,
    resolved: &Resolved,
    project: Option<(&ProjectIndex, FileId)>,
) -> Vec<TypeMismatch> {
    let ti = match project {
        Some((index, file)) => infer(root, resolved, index, file),
        None => infer_node(root, resolved),
    };
    let index = project.map(|(index, _)| index);
    let mut out = Vec::new();
    for node in root.descendants() {
        match node.kind() {
            LOCAL_VAR_DECL | FIELD_DECL => check_initializer(&node, resolved, &ti, index, &mut out),
            ASSIGNMENT_EXPR => check_assignment(&node, &ti, index, &mut out),
            RETURN_STMT => check_return(&node, resolved, &ti, index, &mut out),
            // Argument checking needs the project member model (formal parameter types), so it runs
            // only with an index — like project subtyping.
            CALL_EXPR => {
                if let Some((index, file)) = project {
                    check_call(&node, &ti, index, file, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

/// Checks each declarator's initializer in a variable / field declaration against its declared type.
///
/// A declaration may bind several variables at once (`int a = 1, b = 2.0;`); the CST is flat, so the
/// declarators are paired by walking the direct children in order — each `IDENT` declarator name
/// takes the next direct expression child as its initializer (a declarator without one is skipped
/// when the next name arrives). A `var` declaration (always single-name) has no written type to
/// disagree with and is skipped whole.
fn check_initializer(
    node: &SyntaxNode,
    resolved: &Resolved,
    ti: &TypeInference,
    index: Option<&ProjectIndex>,
    out: &mut Vec<TypeMismatch>,
) {
    if node
        .children()
        .find_map(ast::Type::cast)
        .as_ref()
        .is_some_and(is_var_type)
    {
        return;
    }
    // The declarator name is a *definition*, recovered with `symbol_at` (not `definition_at`, which
    // looks up a reference). The declared `TYPE` / `MODIFIERS` children are not expressions, so they
    // are skipped by the `Expr::cast` below and never mistaken for an initializer.
    let mut current: Option<SyntaxToken> = None;
    for elem in node.children_with_tokens() {
        if let Some(token) = elem.as_token() {
            if token.kind() == IDENT {
                current = Some(token.clone());
            }
            continue;
        }
        if let Some(value) = elem.into_node().and_then(ast::Expr::cast)
            && let Some(def_id) = current
                .take()
                .and_then(|n| resolved.symbol_at(token_start(&n)))
        {
            record_if_mismatch(value.syntax(), ti.type_of_def(def_id), ti, index, out);
        }
    }
}

/// Checks a simple `=` assignment's value against its target's type. Compound assignments
/// (`+=`, `>>=`, …) carry an implicit narrowing cast, so only a lone `=` is checked.
fn check_assignment(
    node: &SyntaxNode,
    ti: &TypeInference,
    index: Option<&ProjectIndex>,
    out: &mut Vec<TypeMismatch>,
) {
    if op_kinds(node).as_slice() != [EQ] {
        return;
    }
    let Some(assign) = ast::AssignmentExpr::cast(node.clone()) else {
        return;
    };
    let (Some(target), Some(value)) = (assign.target(), assign.value()) else {
        return;
    };
    let Some(expected) = ti.type_of_expr(node_span(target.syntax())) else {
        return;
    };
    record_if_mismatch(value.syntax(), expected, ti, index, out);
}

/// Checks a `return <expr>;` against the enclosing method's return type. Only methods are checked:
/// a `return` whose nearest function-like ancestor is a lambda (its return type is target-typed and
/// unknown here) or a constructor (no return type) is skipped, as is a bare `return;`.
fn check_return(
    node: &SyntaxNode,
    resolved: &Resolved,
    ti: &TypeInference,
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
    let Some(def_id) = first_ident_token(&method).and_then(|n| resolved.symbol_at(token_start(&n)))
    else {
        return;
    };
    record_if_mismatch(value.syntax(), ti.type_of_def(def_id), ti, index, out);
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
    node: &SyntaxNode,
    ti: &TypeInference,
    index: &ProjectIndex,
    file: FileId,
    out: &mut Vec<TypeMismatch>,
) {
    let Some(call) = ast::CallExpr::cast(node.clone()) else {
        return;
    };
    let Some((owner, name)) = call_target(&call, ti, index, file) else {
        return;
    };
    let args: Vec<ast::Expr> = call
        .args()
        .map(|list| list.args().collect())
        .unwrap_or_default();
    // Candidates of the right arity (a varargs method is skipped — its arity is variable).
    let matching: Vec<&crate::Member> = index
        .resolve_members_all(owner, &name, Namespace::Method)
        .into_iter()
        .map(|id| index.member(id))
        .filter(|m| !m.varargs && m.params.len() == args.len())
        .collect();
    if matching.is_empty() {
        return;
    }
    // Argument spans and inferred types, computed once and reused across every overload (an
    // un-inferred argument is `None`, and treated as applicable — never blocking).
    let arg_spans: Vec<Range<usize>> = args.iter().map(|a| node_span(a.syntax())).collect();
    let arg_tys: Vec<Option<&Ty>> = arg_spans
        .iter()
        .map(|s| ti.type_of_expr(s.clone()))
        .collect();
    // A candidate is applicable when every argument is assignable to its parameter.
    let applicable = |m: &crate::Member| {
        arg_tys.iter().zip(&m.params).all(|(arg_ty, param)| {
            arg_ty.is_none_or(|ty| {
                ty.is_assignable_to(
                    &member_type_to_ty(index, m.file, m.owner, &param.ty),
                    Some(index),
                )
            })
        })
    };
    // The call binds to some overload — no argument error to report.
    if matching.iter().any(|&m| applicable(m)) {
        return;
    }
    // No overload accepts the arguments; report only when the overload set is fully known.
    if !index.method_set_complete(owner, &name) {
        return;
    }
    if let [only] = matching.as_slice() {
        // A single overload: precise per-argument diagnostics against it.
        for ((arg_ty, span), param) in arg_tys.iter().zip(&arg_spans).zip(&only.params) {
            let param_ty = member_type_to_ty(index, only.file, only.owner, &param.ty);
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
            node_span(call.syntax()),
            name,
            arg_tys,
        ));
    }
}

/// The `(owner type, method name)` a call resolves against: a qualified call `recv.m(..)` on the
/// receiver's project type, or a bare call `m(..)` on the enclosing type (an implicit `this`).
/// `None` when the receiver is not an indexed project type or the callee is neither a name nor a
/// field access.
fn call_target(
    call: &ast::CallExpr,
    ti: &TypeInference,
    index: &ProjectIndex,
    file: FileId,
) -> Option<(ItemId, String)> {
    match call.callee()? {
        ast::Expr::FieldAccess(fa) => {
            let name = fa.field()?;
            let receiver = ti.type_of_expr(node_span(fa.receiver()?.syntax()))?;
            Some((receiver.project_id()?, name))
        }
        ast::Expr::NameRef(n) => {
            let name = first_ident_token(n.syntax())?.text().to_string();
            Some((enclosing_item(index, file, call.syntax())?, name))
        }
        _ => None,
    }
}

/// Pushes a [`TypeMismatch`] for `value` against `expected` when the value's inferred type is not
/// assignable there — unless the value is untyped (no entry) or the pair is a constant narrowing the
/// type system cannot see is legal.
fn record_if_mismatch(
    value: &SyntaxNode,
    expected: &Ty,
    ti: &TypeInference,
    index: Option<&ProjectIndex>,
    out: &mut Vec<TypeMismatch>,
) {
    let span = node_span(value);
    let Some(found) = ti.type_of_expr(span.clone()) else {
        return;
    };
    if found.is_assignable_to(expected, index) || rescued_by_constant_narrowing(expected, found) {
        return;
    }
    out.push(TypeMismatch::assignment(
        span,
        expected.clone(),
        found.clone(),
    ));
}

/// Whether a primitive mismatch could be a legal narrowing of a constant expression (JLS §5.2): a
/// numeric value assigned to a `byte` / `short` / `char` slot. Without a constant evaluator we cannot
/// tell whether the value is a constant in range, so we never report these — under-reporting (missing
/// `byte b = someInt;`) rather than risk a false positive on the legal, common `byte b = 1;`.
fn rescued_by_constant_narrowing(expected: &Ty, found: &Ty) -> bool {
    matches!(
        expected,
        Ty::Primitive(Primitive::Byte | Primitive::Short | Primitive::Char)
    ) && found.as_numeric().is_some()
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
    ) -> Inferer<'a> {
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

    fn run(mut self) -> TypeInference {
        let root = self.root.clone();
        self.collect_declared_types(&root);
        self.infer_in(&root);
        TypeInference {
            def_types: self.def_types,
            expr_by_span: self.expr_by_span,
        }
    }

    // --- Pass 1: declared types ---------------------------------------------------------------

    /// Records the written type of every explicitly-typed binding under `node`. A `var` binding is
    /// skipped here (it has no written type) and filled from its initializer in pass 2.
    fn collect_declared_types(&mut self, node: &SyntaxNode) {
        if declares_typed_bindings(node.kind()) {
            let ty = node.children().find_map(ast::Type::cast);
            if !ty.as_ref().is_some_and(is_var_type) {
                let t = self.ty_of_opt_type(ty.as_ref());
                for tok in direct_ident_tokens(node) {
                    self.set_def_type(token_start(&tok), t.clone());
                }
            }
        }
        for child in node.children() {
            self.collect_declared_types(&child);
        }
    }

    fn set_def_type(&mut self, name_start: usize, ty: Ty) {
        if let Some(&id) = self.def_by_name_start.get(&name_start) {
            self.def_types[id.0 as usize] = ty;
        }
    }

    // --- Pass 2: expression inference ---------------------------------------------------------

    /// Walks `node` post-order, typing every expression (children first), and fills a `var` local's
    /// type from its already-typed initializer.
    fn infer_in(&mut self, node: &SyntaxNode) {
        for child in node.children() {
            self.infer_in(&child);
        }
        if let Some(expr) = ast::Expr::cast(node.clone()) {
            let r = node.text_range();
            let span = (usize::from(r.start()), usize::from(r.end()));
            let ty = self.compute_expr_ty(&expr);
            self.expr_by_span.insert(span, ty);
        } else if matches!(node.kind(), LOCAL_VAR_DECL | RESOURCE) {
            self.fill_var_binding(node);
        }
    }

    /// For a `var` local / resource, sets its definition type to the type of its initializer (which
    /// pass 2 has already inferred, since children are visited first).
    fn fill_var_binding(&mut self, node: &SyntaxNode) {
        let ty = node.children().find_map(ast::Type::cast);
        if !ty.as_ref().is_some_and(is_var_type) {
            return;
        }
        let init_ty = node
            .children()
            .find_map(ast::Expr::cast)
            .map(|e| self.expr_ty(e.syntax()))
            .unwrap_or(Ty::Unknown);
        for tok in direct_ident_tokens(node) {
            self.set_def_type(token_start(&tok), init_ty.clone());
        }
    }

    /// Computes an expression's type from its (already-typed) children.
    fn compute_expr_ty(&self, expr: &ast::Expr) -> Ty {
        match expr {
            ast::Expr::Literal(l) => literal_ty(l),
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
            // Target-typed forms still need a later phase (a method reference / lambda takes its
            // type from context; a switch expression unifies its arms).
            ast::Expr::MethodRef(_) | ast::Expr::Lambda(_) | ast::Expr::Switch(_) => Ty::Unknown,
            ast::Expr::ClassLiteral(_) => Ty::Class(ClassTy::external("Class")),
        }
    }

    /// The memoised type of the (already-visited) expression node, or [`Ty::Unknown`].
    fn expr_ty(&self, node: &SyntaxNode) -> Ty {
        let r = node.text_range();
        self.expr_by_span
            .get(&(usize::from(r.start()), usize::from(r.end())))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }

    fn child_ty(&self, expr: Option<ast::Expr>) -> Ty {
        expr.map(|e| self.expr_ty(e.syntax()))
            .unwrap_or(Ty::Unknown)
    }

    fn nameref_ty(&self, node: &SyntaxNode) -> Ty {
        // A reference is keyed by its identifier *token* start, which excludes the leading trivia
        // that the `NAME_REF` node carries; look that token up, not the node. `this` / `super` have
        // no identifier token (and are never recorded as references), so they yield `Unknown`.
        let Some(tok) = first_ident_token(node) else {
            return Ty::Unknown;
        };
        if let Some(&ri) = self.ref_by_start.get(&token_start(&tok))
            && let Resolution::Def(id) = self.resolved.references[ri].resolution
        {
            return self.def_types[id.0 as usize].clone();
        }
        Ty::Unknown
    }

    fn unary_ty(&self, u: &ast::UnaryExpr) -> Ty {
        let operand = self.child_ty(u.operand());
        match op_kinds(u.syntax()).first() {
            Some(BANG) => Ty::Primitive(Primitive::Boolean),
            Some(TILDE | PLUS | MINUS) => unary_promote(&operand),
            // Prefix `++` / `--` keep the operand type.
            _ => operand,
        }
    }

    fn binary_ty(&self, b: &ast::BinaryExpr) -> Ty {
        let ops = op_kinds(b.syntax());
        // `instanceof` carries no right operand (its RHS is a type/pattern).
        if ops.contains(&INSTANCEOF_KW) {
            return Ty::Primitive(Primitive::Boolean);
        }
        let lhs = self.child_ty(b.lhs());
        let rhs = self.child_ty(b.rhs());
        let boolean = Ty::Primitive(Primitive::Boolean);
        match ops.as_slice() {
            // Conditional, equality, and relational operators (`>` is `GT`, `>=` is `GT EQ`).
            [AMP_AMP] | [PIPE_PIPE] | [EQ_EQ] | [BANG_EQ] | [LT] | [LT_EQ] | [GT] | [GT, EQ] => {
                boolean
            }
            // Shifts (`<<` is `LSHIFT`; `>>` / `>>>` are repeated `GT`): the promoted left operand.
            [LSHIFT] | [GT, GT] | [GT, GT, GT] => unary_promote(&lhs),
            // `+` is string concatenation when either side is a `String`, else arithmetic.
            [PLUS] => {
                if lhs.is_string() || rhs.is_string() {
                    string_ty()
                } else {
                    binary_numeric(&lhs, &rhs)
                }
            }
            [MINUS] | [STAR] | [SLASH] | [PERCENT] => binary_numeric(&lhs, &rhs),
            // `&` `|` `^` are logical on booleans, bitwise (numeric) otherwise.
            [AMP] | [PIPE] | [CARET] => {
                if is_boolean(&lhs) && is_boolean(&rhs) {
                    Ty::Primitive(Primitive::Boolean)
                } else {
                    binary_numeric(&lhs, &rhs)
                }
            }
            _ => Ty::Unknown,
        }
    }

    fn new_ty(&self, n: &ast::NewExpr) -> Ty {
        let base = self.ty_of_opt_type(n.ty().as_ref());
        array_of(base, lbrack_count(n.syntax()))
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
        // parts: condition, then-branch, else-branch. Take the type only when both branches agree.
        let mut parts = t.parts();
        let _cond = parts.next();
        let then_ty = self.child_ty(parts.next());
        let else_ty = self.child_ty(parts.next());
        if then_ty == else_ty {
            then_ty
        } else {
            Ty::Unknown
        }
    }

    // --- Syntactic type -> Ty -----------------------------------------------------------------

    fn ty_of_opt_type(&self, ty: Option<&ast::Type>) -> Ty {
        ty.map(|t| self.ty_of_type(t)).unwrap_or(Ty::Unknown)
    }

    fn ty_of_type(&self, ty: &ast::Type) -> Ty {
        let base = self.base_ty_of_type(ty);
        array_of(base, lbrack_count(ty.syntax()))
    }

    fn base_ty_of_type(&self, ty: &ast::Type) -> Ty {
        if ty.is_primitive_or_var() {
            direct_tokens(ty.syntax())
                .find_map(|t| primitive_ty(t.kind()))
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
        let name = tok.text().to_string();
        let args = self.type_args_of(ty);
        if let Some((index, file)) = self.project
            && let Some(&ri) = self.ref_by_start.get(&token_start(&tok))
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
        let receiver = self.child_ty(fa.receiver());
        self.member_ty(&receiver, &name, namespace)
    }

    /// `callee(args)`: the called method's return type. A qualified call `receiver.method()` looks
    /// the method up on the receiver's type; a bare call `method()` looks it up on the enclosing
    /// type (an implicit `this`). Only project types resolve — everything else is [`Ty::Unknown`].
    fn call_ty(&self, call: &ast::CallExpr) -> Ty {
        match call.callee() {
            Some(ast::Expr::FieldAccess(fa)) => self.field_access_member_ty(&fa, Namespace::Method),
            Some(ast::Expr::NameRef(n)) => {
                let Some(name) = first_ident_token(n.syntax()).map(|t| t.text().to_string()) else {
                    return Ty::Unknown;
                };
                match self.enclosing_item(call.syntax()) {
                    // A bare (`this`) call: the enclosing type is used raw, so its own type variables
                    // stay un-substituted (they survive by name).
                    Some(owner) => self.member_ty_in(owner, &[], &name, Namespace::Method),
                    None => Ty::Unknown,
                }
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
    /// `owner_args` bound — [`member_ty_substituted`] guarded by the project index being present.
    fn member_ty_in(
        &self,
        owner: ItemId,
        owner_args: &[Ty],
        name: &str,
        namespace: Namespace,
    ) -> Ty {
        match self.project {
            Some((index, _)) => member_ty_substituted(index, owner, owner_args, name, namespace),
            None => Ty::Unknown,
        }
    }

    /// The enclosing project type of `node`: the nearest ancestor type declaration that is an
    /// indexed item, for resolving a bare (`this`) method call.
    fn enclosing_item(&self, node: &SyntaxNode) -> Option<ItemId> {
        let (index, file) = self.project?;
        enclosing_item(index, file, node)
    }
}

/// Turns a member's captured [`MemberType`] into a concrete [`Ty`], resolving a named type against
/// the project from the member's *declaring* `file` (its import / package context). `owner` is the
/// type whose declaration the `MemberType` lives in: a bare name matching one of its type parameters
/// becomes a [`Ty::TypeVar`] (to be substituted by the caller) rather than an external by-name type.
/// A free function so a caller holding only a [`TypeInference`] (e.g. argument checking) can use it.
fn member_type_to_ty(index: &ProjectIndex, file: FileId, owner: ItemId, mt: &MemberType) -> Ty {
    match mt {
        MemberType::Primitive { keyword, dims } => {
            let base = Primitive::from_keyword(keyword).map_or(Ty::Unknown, Ty::Primitive);
            array_of(base, *dims as usize)
        }
        MemberType::Void => Ty::Void,
        MemberType::Named {
            name,
            qualified,
            dims,
            args,
        } => {
            let base = if qualified.is_none() && index.is_type_param(owner, name) {
                // A bare name matching one of `owner`'s type parameters is a type variable (`E`),
                // recorded for later substitution (a type variable takes no arguments of its own).
                Ty::TypeVar {
                    owner,
                    name: name.clone(),
                }
            } else {
                // Otherwise a project / external type, with its concrete arguments carried
                // recursively (`List<String>` → element `String`; `List<E>` → element type var `E`).
                let ty_args = args
                    .iter()
                    .map(|a| member_type_to_ty(index, file, owner, a))
                    .collect();
                match index.resolve_type_name(file, name, qualified.as_deref()) {
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
            array_of(base, *dims as usize)
        }
        MemberType::Unknown => Ty::Unknown,
    }
}

/// The concrete type of member `name` (in `namespace`) accessed on a receiver of project type
/// `owner` with type arguments `owner_args` — i.e. [`member_type_to_ty`] with the receiver's generic
/// arguments bound. Searches `owner` and its project-internal supertypes nearest-first (mirroring
/// [`ProjectIndex::resolve_member`]'s shadowing), substituting each type variable by the argument
/// propagated down the inheritance chain (`Sub extends Base<String>` binds `Base`'s `T` to
/// `String`). [`Ty::Unknown`] when no such member resolves. A raw receiver (no arguments) leaves the
/// member's type variables un-substituted, so they survive by name.
fn member_ty_substituted(
    index: &ProjectIndex,
    owner: ItemId,
    owner_args: &[Ty],
    name: &str,
    namespace: Namespace,
) -> Ty {
    // Each frame's state is a type's concrete type arguments, as seen from the original receiver;
    // the shared inheritance walk threads them down — binding a supertype's arguments through the
    // current type's substitution so a type variable threaded `Sub<U> extends Base<U>` resolves all
    // the way down.
    index
        .walk_supertypes_stateful(
            owner,
            owner_args.to_vec(),
            |current, args| {
                let member_id = index.declared_member(current, name, namespace)?;
                let member = index.member(member_id);
                Some(subst_member_ty(
                    index,
                    current,
                    args,
                    member.file,
                    &member.ty,
                ))
            },
            |current, args, sup| {
                let file = index.item(current).file;
                sup.args
                    .iter()
                    .map(|mt| subst_member_ty(index, current, args, file, mt))
                    .collect()
            },
        )
        .unwrap_or(Ty::Unknown)
}

/// [`member_type_to_ty`] for a member-type `mt` declared in `current` (in `file`), with `current`'s
/// type parameters bound to `args`. A raw frame (`args` empty — a non-generic or raw receiver, the
/// common case) needs no binding, so the converted type is returned directly instead of cloning the
/// whole tree through a no-op [`Ty::substitute`].
fn subst_member_ty(
    index: &ProjectIndex,
    current: ItemId,
    args: &[Ty],
    file: FileId,
    mt: &MemberType,
) -> Ty {
    let ty = member_type_to_ty(index, file, current, mt);
    if args.is_empty() {
        ty
    } else {
        ty.substitute(&subst_fn(index, current, args))
    }
}

/// The substitution for a use of `owner` with type arguments `args`: a function binding each of
/// `owner`'s type parameters, by position, to the supplied argument (suitable for [`Ty::substitute`]).
/// A raw use (fewer arguments than parameters, typically none) leaves the surplus parameters unbound,
/// so they stay type variables.
fn subst_fn(
    index: &ProjectIndex,
    owner: ItemId,
    args: &[Ty],
) -> impl Fn(ItemId, &str) -> Option<Ty> {
    let bindings: HashMap<String, Ty> = index
        .item(owner)
        .type_params
        .iter()
        .zip(args)
        .map(|(p, arg)| (p.name.clone(), arg.clone()))
        .collect();
    move |o, n| (o == owner).then(|| bindings.get(n).cloned()).flatten()
}

/// The nearest ancestor type declaration of `node` that is an indexed project item, in `file`. A
/// free function shared by the [`Inferer`] (bare-call resolution) and argument checking.
fn enclosing_item(index: &ProjectIndex, file: FileId, node: &SyntaxNode) -> Option<ItemId> {
    let decl = node
        .ancestors()
        .find(|a| crate::project::type_decl_kind(a.kind()).is_some())?;
    let name = first_ident_token(&decl)?;
    index.item_by_decl(file, token_start(&name))
}

/// Wraps `base` in `dims` array levels (`dims = 2` → `base[][]`).
fn array_of(base: Ty, dims: usize) -> Ty {
    (0..dims).fold(base, |acc, _| Ty::Array(Box::new(acc)))
}

/// Whether a node kind has an explicitly-written type as a direct `TYPE` child and its declared
/// name(s) as direct `IDENT` tokens. For a `METHOD_DECL` the direct `TYPE` child is the *return*
/// type and the direct `IDENT` is the method name, so the method's definition is typed with its
/// return type — used to check `return` statements (its parameters' types are nested and unaffected).
fn declares_typed_bindings(kind: SyntaxKind) -> bool {
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

/// Whether the syntactic type is `var` (local variable type inference).
fn is_var_type(ty: &ast::Type) -> bool {
    direct_tokens(ty.syntax()).any(|t| t.kind() == VAR_KW)
}

/// The [`Ty`] of a primitive (or `void`) type keyword, or `None` for any other token.
fn primitive_ty(kind: SyntaxKind) -> Option<Ty> {
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
        STRING_LITERAL | TEXT_BLOCK => string_ty(),
        TRUE_KW | FALSE_KW => Ty::Primitive(Primitive::Boolean),
        NULL_KW => Ty::Null,
        _ => Ty::Unknown,
    }
}

fn ends_with_ignore_case(text: &str, suffix: char) -> bool {
    text.chars()
        .next_back()
        .is_some_and(|c| c.eq_ignore_ascii_case(&suffix))
}

fn is_boolean(t: &Ty) -> bool {
    matches!(t, Ty::Primitive(Primitive::Boolean))
}

/// The non-trivia operator token kinds directly under `node` (operands are child nodes, so for a
/// binary/unary expression these are exactly the operator tokens).
fn op_kinds(node: &SyntaxNode) -> Vec<SyntaxKind> {
    direct_tokens(node)
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.kind())
        .collect()
}

/// The count of `[` tokens directly under `node` — the array dimension count of a type or `new`.
fn lbrack_count(node: &SyntaxNode) -> usize {
    direct_tokens(node).filter(|t| t.kind() == LBRACK).count()
}

/// The direct `IDENT` token children of `node` (a declaration's names; its type is a nested node).
fn direct_ident_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    direct_tokens(node).filter(|t| t.kind() == IDENT)
}

fn direct_tokens(node: &SyntaxNode) -> impl Iterator<Item = SyntaxToken> {
    node.children_with_tokens().filter_map(|it| it.into_token())
}

fn token_start(tok: &SyntaxToken) -> usize {
    usize::from(tok.text_range().start())
}

/// The byte span of `node` in the source — the key shape used to look an expression's type up in a
/// [`TypeInference`] and to anchor a [`TypeMismatch`].
fn node_span(node: &SyntaxNode) -> Range<usize> {
    let r = node.text_range();
    usize::from(r.start())..usize::from(r.end())
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

/// Signature help for the call whose argument list contains byte `offset`: the overloads of the
/// method being called, plus the argument index the cursor is on.
///
/// Resolves the callee like [`check_call`] — a qualified `recv.m(..)` on the receiver's project
/// type, or a bare `m(..)` on the enclosing type — then renders every overload. Returns `None` when
/// the cursor is in no call, the receiver is not an indexed project type (e.g. an external/JDK
/// type), or the method names no project member. Never panics.
pub fn signature_help(
    root: &SyntaxNode,
    resolved: &Resolved,
    index: &ProjectIndex,
    file: FileId,
    offset: usize,
) -> Option<SignatureHelp> {
    let (call, active_parameter) = enclosing_call(root, offset)?;
    let ti = infer(root, resolved, index, file);
    let (owner, name) = call_target(&call, &ti, index, file)?;
    let signatures: Vec<Signature> = index
        .resolve_members_all(owner, &name, Namespace::Method)
        .into_iter()
        .map(|id| render_signature(index, index.member(id)))
        .collect();
    if signatures.is_empty() {
        return None;
    }
    // Highlight the overload that actually has a parameter at the cursor's index; if none does (the
    // cursor is past every overload's arity), fall back to the first.
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

/// The innermost call whose argument list (between the parens) contains `offset`, with the cursor's
/// argument index (commas before it). Scans every call so a nested `outer(inner(|))` picks `inner`
/// (the smallest containing argument list).
fn enclosing_call(root: &SyntaxNode, offset: usize) -> Option<(ast::CallExpr, usize)> {
    let (call, args, _) = root
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter_map(|call| {
            let args = call.args()?;
            let span = node_span(args.syntax());
            (span.start <= offset && offset <= span.end).then_some((
                call,
                args,
                span.end - span.start,
            ))
        })
        .min_by_key(|(.., width)| *width)?;
    let active = active_parameter(&args, offset);
    Some((call, active))
}

/// The argument index the cursor at `offset` is on: the number of top-level commas in `args` that
/// end at or before it. `f(|)` → 0, `f(a, |)` → 1.
fn active_parameter(args: &ast::ArgList, offset: usize) -> usize {
    direct_tokens(args.syntax())
        .filter(|t| t.kind() == COMMA && usize::from(t.text_range().end()) <= offset)
        .count()
}

/// Renders one member's signature as `name(type1 p1, type2 p2)`, recording each parameter's byte
/// range within the label. A parameter with no readable name is rendered as its type alone.
fn render_signature(index: &ProjectIndex, member: &crate::Member) -> Signature {
    let mut label = String::new();
    label.push_str(&member.name);
    label.push('(');
    let mut parameters = Vec::with_capacity(member.params.len());
    for (i, param) in member.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let ty = member_type_to_ty(index, member.file, member.owner, &param.ty).to_string();
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

/// The member-access completions for `receiver.` at byte `offset`: the fields and methods reachable
/// on the receiver's type, when that receiver is an indexed project type.
///
/// Anchors on the `.` just left of the cursor — both for a bare `receiver.` (which does not parse as
/// a field access, the dot having no member name yet) and a partially-typed `receiver.fo` — then
/// infers the receiver and enumerates its members (its own and inherited, [`ProjectIndex::members_of`]).
/// A `this.` / `super.` receiver completes the enclosing type's members. Returns an empty list when
/// the cursor is on no member access, or the receiver is not an indexed project type (an external /
/// JDK type, whose members are not indexed). One entry per distinct name (a field shadows, overloads
/// collapse to one); the editor filters by the typed prefix. Never panics.
pub fn member_completions(
    root: &SyntaxNode,
    resolved: &Resolved,
    index: &ProjectIndex,
    file: FileId,
    offset: usize,
) -> Vec<Completion> {
    let Some(owner) = receiver_owner(root, resolved, index, file, offset) else {
        return Vec::new();
    };
    let mut seen: HashSet<(String, Namespace)> = HashSet::new();
    let mut out = Vec::new();
    for id in index.members_of(owner) {
        let member = index.member(id);
        // Only instance-accessible members complete after `.`: fields and methods, not constructors
        // or enum constants.
        if !matches!(member.kind, DefKind::Field | DefKind::Method) {
            continue;
        }
        // Nearest-first order means an own / overriding member is seen before the one it hides; keep
        // the first per (name, name-space) and drop the rest (a shadowed field, a further overload).
        if seen.insert((member.name.clone(), member.kind.namespace())) {
            out.push(completion_of(index, member));
        }
    }
    out
}

/// Builds a [`Completion`] for `member`: a field's detail is its type; a method's is its parameter
/// list and return type (`(int w, int h): int`), reusing [`render_signature`] for the parameters.
fn completion_of(index: &ProjectIndex, member: &crate::Member) -> Completion {
    let detail = match member.kind {
        DefKind::Method => {
            let signature = render_signature(index, member);
            let params = &signature.label[member.name.len()..];
            let ret = member_type_to_ty(index, member.file, member.owner, &member.ty);
            format!("{params}: {ret}")
        }
        _ => member_type_to_ty(index, member.file, member.owner, &member.ty).to_string(),
    };
    Completion {
        label: member.name.clone(),
        kind: member.kind,
        detail,
    }
}

/// The indexed project type whose member is being completed at `offset`: the inferred type of the
/// expression before the `.` just left of the cursor, or — for a `this.` / `super.` receiver — the
/// enclosing type. `None` when the cursor is on no member access or the receiver is not a project type.
///
/// Anchors structurally first and only runs the (whole-file) type inference once a real receiver
/// expression is found — so a cursor on no member access, or a `this.` / `super.` receiver, costs no
/// inference at all (member completion is triggered on every `.`).
fn receiver_owner(
    root: &SyntaxNode,
    resolved: &Resolved,
    index: &ProjectIndex,
    file: FileId,
    offset: usize,
) -> Option<ItemId> {
    let dot = member_access_dot(root, offset)?;
    let before = prev_significant(&dot)?;
    // A `this` / `super` receiver has no inferred type; its members are the enclosing type's (for
    // `super` the strictly-inherited ones — approximated here by the whole enclosing member set).
    if matches!(before.kind(), THIS_KW | SUPER_KW) {
        return enclosing_item(index, file, &before.parent()?);
    }
    let dot_start = usize::from(dot.text_range().start());
    let receiver = receiver_node(&before, dot_start)?;
    let ti = infer(root, resolved, index, file);
    ti.type_of_expr(node_span(&receiver))?.project_id()
}

/// The receiver expression that ends at the `.`: the outermost expression node containing `before`
/// (the token just before the dot) that still ends at or before `dot_start` — so for `a.b.c.|` it is
/// `a.b.c`, and for a partial `recv.fo|` it is `recv` (the enclosing `recv.fo` field access ends
/// *after* the dot and is excluded).
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
        .filter_map(|element| element.into_token())
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

/// The `.` of the member access at byte `offset`, if the cursor is in one: the `.` token itself for
/// `receiver.|`, or the `.` before a partially-typed member name for `receiver.fo|`. The anchor both
/// member completion and [`at_member_access`] are built on.
fn member_access_dot(root: &SyntaxNode, offset: usize) -> Option<SyntaxToken> {
    let token = token_left_of(root, offset)?;
    match token.kind() {
        DOT => Some(token),
        IDENT => prev_significant(&token).filter(|t| t.kind() == DOT),
        _ => None,
    }
}

/// Whether the cursor at byte `offset` is in a member-access position (just after a `.`, or in a
/// member name following one). The host dispatches on this: a member access completes members
/// ([`member_completions`]); any other position completes the scope ([`scope_completions`]).
pub fn at_member_access(root: &SyntaxNode, offset: usize) -> bool {
    member_access_dot(root, offset).is_some()
}

// ===== Scope completion =====

/// The scope completions at byte `offset`: every binding visible there plus every project type by
/// simple name — the candidates for a bare identifier position (not after a `.`; the host gates on
/// [`at_member_access`]).
///
/// Bindings come from the cursor's scope chain, innermost outward: a block / `for` / resources scope
/// contributes only the locals declared before the cursor (sequential visibility), every other scope
/// all of its bindings (parameters, type parameters, and hoisted type members — a field or method is
/// reachable without `this.`). An inner binding shadows an outer one of the same name and name-space.
/// Project types from other files are then added by simple name. One entry per (name, name-space);
/// the editor filters by the typed prefix. Never panics.
pub fn scope_completions(
    root: &SyntaxNode,
    resolved: &Resolved,
    index: &ProjectIndex,
    file: FileId,
    offset: usize,
) -> Vec<Completion> {
    let ti = infer(root, resolved, index, file);
    let mut seen: HashSet<(String, Namespace)> = HashSet::new();
    let mut out = Vec::new();
    // Visible bindings, innermost scope outward (the first seen per name / name-space wins, so an
    // inner binding shadows an outer one).
    for def in resolved.visible_defs(offset) {
        // A constructor is not a name completed in an expression position.
        if def.kind == DefKind::Constructor {
            continue;
        }
        if seen.insert((def.name.clone(), def.kind.namespace())) {
            out.push(binding_completion(def, &ti));
        }
    }
    // Project type names from other files (a sibling type already in scope is deduped away). The
    // simple name completes; the fully-qualified name is the detail.
    for item in index.items() {
        let name = item.fqn.simple_name().to_string();
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

/// Builds a [`Completion`] for a visible binding: a value / method binding shows its inferred type as
/// the detail (when known); a type binding (a sibling class, a type parameter) has none.
fn binding_completion(def: &Def, ti: &TypeInference) -> Completion {
    let ty = ti.type_of_def(def.id);
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
