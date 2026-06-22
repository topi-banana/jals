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
//! project member model ([`ProjectIndex::resolve_member`]) when the receiver is a project type,
//! walking its project-internal supertypes. A member of an external (unindexed) type, and the
//! target-typed forms (method references, lambdas, switch expressions), stay [`Ty::Unknown`]. The
//! pass never panics: every accessor is `Option`/iterator and an unresolvable form is `Unknown`.

use std::collections::HashMap;
use std::ops::Range;

use jals_syntax::SyntaxKind::*;
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::def::{DefId, Namespace};
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
            ast::Expr::ClassLiteral(_) => Ty::Class(ClassTy::External("Class".to_string())),
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
        if let Some((index, file)) = self.project
            && let Some(&ri) = self.ref_by_start.get(&token_start(&tok))
            && let TypeResolution::Project(id) =
                index.resolve_reference(file, &self.resolved.references[ri])
        {
            return Ty::Class(ClassTy::Project { id, name });
        }
        Ty::Class(ClassTy::External(name))
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
                    Some(owner) => self.member_ty_of_item(owner, &name, Namespace::Method),
                    None => Ty::Unknown,
                }
            }
            _ => Ty::Unknown,
        }
    }

    /// The type of the member `name` (in `namespace`) reachable from receiver type `receiver` — a
    /// field's type or a method's return type — when `receiver` is an indexed project type.
    fn member_ty(&self, receiver: &Ty, name: &str, namespace: Namespace) -> Ty {
        match receiver.project_id() {
            Some(id) => self.member_ty_of_item(id, name, namespace),
            None => Ty::Unknown,
        }
    }

    /// The type of the member `name` (in `namespace`) reachable from project type `owner`, searching
    /// the type and its project-internal supertypes.
    fn member_ty_of_item(&self, owner: ItemId, name: &str, namespace: Namespace) -> Ty {
        let Some((index, _)) = self.project else {
            return Ty::Unknown;
        };
        let Some(member_id) = index.resolve_member(owner, name, namespace) else {
            return Ty::Unknown;
        };
        let member = index.member(member_id);
        self.ty_of_member_type(index, member.file, &member.ty)
    }

    /// Turns a member's captured [`MemberType`] into a concrete [`Ty`], resolving a named type
    /// against the project from the member's *declaring* file (its import / package context).
    fn ty_of_member_type(&self, index: &ProjectIndex, file: FileId, mt: &MemberType) -> Ty {
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
            } => {
                let base = match index.resolve_type_name(file, name, qualified.as_deref()) {
                    TypeResolution::Project(id) => Ty::Class(ClassTy::Project {
                        id,
                        name: name.clone(),
                    }),
                    TypeResolution::External | TypeResolution::Unresolved => {
                        Ty::Class(ClassTy::External(name.clone()))
                    }
                };
                array_of(base, *dims as usize)
            }
            MemberType::Unknown => Ty::Unknown,
        }
    }

    /// The enclosing project type of `node`: the nearest ancestor type declaration that is an
    /// indexed item, for resolving a bare (`this`) method call.
    fn enclosing_item(&self, node: &SyntaxNode) -> Option<ItemId> {
        let (index, file) = self.project?;
        let decl = node
            .ancestors()
            .find(|a| crate::project::type_decl_kind(a.kind()).is_some())?;
        let name = first_ident_token(&decl)?;
        index.item_by_decl(file, token_start(&name))
    }
}

/// Wraps `base` in `dims` array levels (`dims = 2` → `base[][]`).
fn array_of(base: Ty, dims: usize) -> Ty {
    (0..dims).fold(base, |acc, _| Ty::Array(Box::new(acc)))
}

/// Whether a node kind introduces explicitly-typed value bindings whose written type is a direct
/// `TYPE` child and whose names are direct `IDENT` tokens.
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
