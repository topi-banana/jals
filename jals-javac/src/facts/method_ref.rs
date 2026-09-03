//! What a `Type::name` method reference names.
//!
//! [`jals_hir`] records no target for one: `call_target_of` is filled for `CALL_EXPR`, `NEW_EXPR`,
//! and `FIELD_ACCESS` only, and a `METHOD_REF_EXPR` gets a synthetic `lambda$N` item rather than a
//! selected member. So the compiler resolves it, and it used to resolve it twice — differently.
//!
//! The JVM lowering selected by name **and** static-ness **and** the functional interface's arity,
//! with a fallback for the *unbound* form where the interface supplies the receiver as its first
//! argument. The wasm lowering selected by **name alone**, and recovered the owner by looking up
//! the qualifier's raw source text as a fully-qualified name — which resolved only when the source
//! happened to spell the whole package, so an imported type never matched and an overload set was
//! decided by declaration order.
//!
//! One rule now, the stricter one, with the owner resolved through the index rather than through
//! text.

use jals_hir::{ItemId, MemberId, Ty};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxKind, SyntaxNode};

use super::{FactError, Facts, Result};

/// Where a method reference's receiver comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefReceiver {
    /// `Type::staticMethod` — there is none.
    Static,
    /// `value::method` — the reference captures whatever its qualifier denotes, and
    /// [`MethodRef::qualifier`] is the expression that produces it.
    ///
    /// It used to carry a [`DefId`], which said *a local* rather than *a value*: `this::run`,
    /// `System.err::println`, `supplier.get()::getB` and `new I()::m` are all bound references
    /// whose qualifier is no local, and every one of them was reported instead of compiled. JLS
    /// §15.13.3 evaluates that expression exactly once, when the method reference itself is
    /// evaluated — which is the call site, so lowering it there is both simpler and the rule.
    Bound,
    /// `Type::instanceMethod` — the interface passes it as the first argument.
    Unbound,
    /// `Type::new` — an allocation rather than a call.
    Constructs,
}

/// The member a method reference names, and how it is reached.
pub(crate) struct MethodRef {
    /// The functional interface the context asked for.
    pub(crate) interface: ItemId,
    /// Its single abstract method — the one the reference implements.
    pub(crate) interface_method: MemberId,
    /// The type declaring the referenced member.
    pub(crate) owner: ItemId,
    /// The referenced member. `None` only for a `Type::new` whose class declares no constructor:
    /// the descriptor `()V` exists where the member does not.
    pub(crate) target: Option<MemberId>,
    pub(crate) receiver: RefReceiver,
    /// The expression a [`RefReceiver::Bound`] reference is qualified by, and `None` for every
    /// other shape.
    pub(crate) qualifier: Option<ast::Expr>,
}

impl Facts<'_> {
    /// The type a `TYPE` node names.
    ///
    /// Inference keys its record by *expression* span and a `TYPE` node is not an expression, so an
    /// `instanceof`'s target has nowhere to be read from and is resolved here instead. A name the
    /// index does not hold is reported rather than guessed at, because an invented package produces
    /// a class that loads and then throws `NoClassDefFoundError`.
    fn ty_of_type(self, node: &ast::Type) -> Result<Ty> {
        let dimensions = node
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::LBRACK)
            .count();
        let mut ty = if node.is_primitive_or_var() {
            Ty::Primitive(
                Self::primitive_of(node)
                    .ok_or(FactError::Unsupported("a type with no primitive keyword"))?,
            )
        } else {
            let name = node
                .simple_name()
                .ok_or(FactError::Unsupported("a type with no name"))?;
            let qualified = node.is_qualified().then(|| node.qualified_text()).flatten();
            let id = self
                .index()
                .resolve_type_name(self.file(), &name, qualified.as_deref())
                .project_id()
                .ok_or_else(|| FactError::Unresolved(name.clone()))?;
            Ty::Class(jals_hir::ClassTy::Project {
                id,
                name,
                args: alloc::vec::Vec::new(),
            })
        };
        for _ in 0..dimensions {
            ty = Ty::Array(alloc::boxed::Box::new(ty));
        }
        Ok(ty)
    }

    /// The type a *name* names, when the grammar parsed it as an expression.
    ///
    /// `String.class`'s base is a name reference, not a type node, because nothing tells the parser
    /// which of the two it is until the `.class` arrives. So the dotted text is resolved against
    /// the index directly.
    pub(crate) fn ty_of_name(self, node: &SyntaxNode) -> Result<Ty> {
        let mut text = alloc::string::String::new();
        for token in node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| matches!(token.kind(), SyntaxKind::IDENT | SyntaxKind::DOT))
        {
            text.push_str(&jals_syntax::decoded_ident(&token));
        }
        let simple = alloc::borrow::ToOwned::to_owned(text.rsplit('.').next().unwrap_or(&text));
        let qualified = text.contains('.').then(|| text.clone());
        let id = self
            .index()
            .resolve_type_name(self.file(), &simple, qualified.as_deref())
            .project_id()
            .ok_or_else(|| FactError::Unresolved(simple.clone()))?;
        Ok(Ty::Class(jals_hir::ClassTy::Project {
            id,
            name: simple,
            args: alloc::vec::Vec::new(),
        }))
    }

    /// The member a `Type::name` / `value::name` / `Type::new` reference names.
    pub(crate) fn method_ref(self, node: &SyntaxNode) -> Result<MethodRef> {
        let index = self.index();
        // The interface the context asked for, and the one method it declares.
        let interface = self
            .typed()
            .type_of_expr(Self::span(node))
            .and_then(Ty::project_id)
            .ok_or(FactError::Unsupported(
                "a method reference with no target type",
            ))?;
        let interface_method = index
            .functional_member(interface)
            .ok_or(FactError::Unsupported("a target with no single method"))?;
        let arity = index.member(interface_method).params.len();

        // A constructor reference names `new` rather than a method.
        let constructs = Self::constructs(node);

        // `Uses::twice` parses its qualifier as an *expression* — a name reference is what a type
        // name looks like before anything resolves it — so both spellings are read.
        let qualifier = node.children().find_map(ast::Expr::cast);
        let named_type = node.children().find_map(ast::Type::cast).map_or_else(
            || {
                qualifier
                    .as_ref()
                    .and_then(|q| self.ty_of_name(q.syntax()).ok())
                    .and_then(|ty| ty.project_id())
            },
            |written| {
                self.ty_of_type(&written)
                    .ok()
                    .and_then(|ty| ty.project_id())
            },
        );
        // Not a type: the qualifier is a *value*, so the reference is bound to it and the call site
        // evaluates it. Which type that value has is asked three ways, because the three shapes are
        // recorded in three places: inference holds a type for an ordinary expression, `this` is
        // not an expression inference records at all, and a plain local name may be bound by the
        // resolver without the inference memo carrying its span.
        let mut bound_to = None;
        let (owner, mut receiver) = if let Some(item) = named_type {
            (item, RefReceiver::Static)
        } else {
            let expr = qualifier.as_ref().ok_or(FactError::Unsupported(
                "a method reference with no qualifier",
            ))?;
            // `super::m` is a *non-virtual* call on an inherited method, which no
            // `LambdaMetafactory` handle spells: javac synthesises a bridge that makes the
            // `invokespecial` and points the handle at that. Reported rather than compiled as
            // `this::m`, which is the same bytes dispatching virtually — a program that runs and
            // calls the override the source wrote `super` to avoid.
            if Self::is_super(expr.syntax()) {
                return Err(FactError::Unsupported("a `super` method reference"));
            }
            let item = self
                .typed()
                .type_of_expr(Self::span(expr.syntax()))
                .and_then(Ty::project_id)
                .or_else(|| {
                    Self::is_this(expr.syntax())
                        .then(|| Self::enclosing_type_of(node, self.file(), self.index()).ok())
                        .flatten()
                })
                .or_else(|| {
                    self.def_at(expr.syntax())
                        .and_then(|id| self.typed().type_of_def(id).project_id())
                })
                .ok_or(FactError::Unsupported(
                    "a method reference on a value of an unindexed type",
                ))?;
            bound_to = Some(expr.clone());
            (item, RefReceiver::Bound)
        };

        if constructs {
            let target = index.own_members(owner).iter().copied().find(|&id| {
                let info = index.member(id);
                info.kind == jals_hir::DefKind::Constructor && info.params.len() == arity
            });
            if target.is_none() && arity > 0 {
                return Err(FactError::Unsupported(
                    "a method reference to a constructor this cannot find",
                ));
            }
            return Ok(MethodRef {
                interface,
                interface_method,
                owner,
                target,
                receiver: RefReceiver::Constructs,
                qualifier: None,
            });
        }

        // The method's own name is a direct token of the reference: everything before the `::` is a
        // node.
        let referenced = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::IDENT)
            .last()
            .ok_or(FactError::Unsupported("a method reference with no name"))?;
        let name = jals_syntax::decoded_ident(&referenced);
        let is_bound = receiver == RefReceiver::Bound;

        let target = index
            .own_members(owner)
            .iter()
            .copied()
            .find(|&id| {
                let info = index.member(id);
                info.kind == jals_hir::DefKind::Method
                    && info.name == name
                    && info.modifiers.is_static != is_bound
                    // A bound reference passes the receiver separately, so the interface method's
                    // own arity is what the referenced method takes.
                    && info.params.len() == arity
            })
            // Not found as that shape: a reference qualified by a *type* may still name an instance
            // method, and then the interface's first argument is the receiver — `Type::method` with
            // one fewer parameter than the interface declares. That is the *unbound* form.
            .or_else(|| {
                if is_bound || arity == 0 {
                    return None;
                }
                let found = index.own_members(owner).iter().copied().find(|&id| {
                    let info = index.member(id);
                    info.kind == jals_hir::DefKind::Method
                        && info.name == name
                        && !info.modifiers.is_static
                        && info.params.len() == arity - 1
                });
                if found.is_some() {
                    receiver = RefReceiver::Unbound;
                }
                found
            })
            .ok_or(FactError::Unsupported(
                "a method reference to a method this cannot find",
            ))?;

        Ok(MethodRef {
            interface,
            interface_method,
            owner,
            target: Some(target),
            receiver,
            qualifier: bound_to,
        })
    }
}
