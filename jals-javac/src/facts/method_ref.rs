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
    pub(crate) fn ty_of_type(self, node: &ast::Type) -> Result<Ty> {
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

    /// The dotted name a chain of plain names spells, or `None` for anything that is not one.
    ///
    /// `Outer.Inner` and `java.lang.String` parse as a `FIELD_ACCESS` whose receiver is a *node*,
    /// so a walk over the access's own direct tokens sees `.Inner` and has already lost the head.
    /// Every such spelling therefore resolved as nothing, which made `java.lang.String.class` — and
    /// `Outer.Inner::go`, which fell through to being read as a *value* — a compile error in
    /// ordinary Java, on both backends, because each held its own copy of the same walk.
    ///
    /// The chain is followed structurally instead, and a link that is not a plain name ends it:
    /// `foo().bar` is a value access and must never be offered to a type lookup. What the chain
    /// *denotes* is still the index's answer rather than this one's — `System.err` is a name chain
    /// whose head is a type and whose whole is not, and it stays a value because no item is
    /// registered under it.
    fn dotted_name(node: &SyntaxNode) -> Option<alloc::string::String> {
        if let Some(access) = ast::FieldAccess::cast(node.clone()) {
            let receiver = Self::dotted_name(access.receiver()?.syntax())?;
            let name = node
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .filter(|token| token.kind() == SyntaxKind::IDENT)
                .last()?;
            return Some(alloc::format!(
                "{receiver}.{}",
                jals_syntax::decoded_ident(&name)
            ));
        }
        let mut text = alloc::string::String::new();
        for token in node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| matches!(token.kind(), SyntaxKind::IDENT | SyntaxKind::DOT))
        {
            text.push_str(&jals_syntax::decoded_ident(&token));
        }
        (!text.is_empty()).then_some(text)
    }

    /// The type a *name* names, when the grammar parsed it as an expression.
    ///
    /// `String.class`'s base is a name reference, not a type node, because nothing tells the parser
    /// which of the two it is until the `.class` arrives. So the dotted text is resolved against
    /// the index directly.
    pub(crate) fn ty_of_name(self, node: &SyntaxNode) -> Result<Ty> {
        let text = Self::dotted_name(node).ok_or(FactError::Unsupported(
            "a qualifier that is not a plain name",
        ))?;
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

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned as _;
    use alloc::format;
    use alloc::string::{String, ToString as _};
    use alloc::vec::Vec;

    use jals_exec::block_on_inline;
    use jals_hir::{FileAnalysis, FileId, ProjectIndex};
    use jals_syntax::SyntaxKind;

    use crate::facts::Facts;

    /// Every method reference in `source`, rendered as `written => receiver owner.member/arity`.
    ///
    /// The chain is spelled out rather than hidden behind a helper returning a [`Facts`], for the
    /// reason `constant.rs`'s suite gives: a `TypedFile` borrows the binding, which borrows the
    /// analysis *and* the index, so nothing shorter than the whole chain can be handed back. The
    /// stdlib stubs are folded in because a `String::new` needs `java.lang.String` to resolve, and
    /// they are parsed in memory rather than read from a host — which is what lets this run in
    /// CI's wasm cell, where the end-to-end tests stand down.
    fn refs(source: &str) -> Vec<String> {
        let root = block_on_inline(jals_syntax::Parse::parse(source)).syntax();
        let analysis = block_on_inline(FileAnalysis::of(&root));
        let index = block_on_inline(
            ProjectIndex::builder(&[(FileId(0), root.clone())])
                .with_stdlib()
                .build(),
        );
        let semantics = analysis.in_project(&index, FileId(0));
        let facts = Facts::of(block_on_inline(semantics.typed()));
        root.descendants()
            .filter(|node| node.kind() == SyntaxKind::METHOD_REF_EXPR)
            .map(|node| {
                let written = node.text().to_string().trim().to_owned();
                match facts.method_ref(&node) {
                    Ok(found) => {
                        let target = found.target.map_or_else(
                            || "<no member>".to_owned(),
                            |id| {
                                let info = index.member(id);
                                format!("{}/{}", info.name, info.params.len())
                            },
                        );
                        format!(
                            "{written} => {:?} {}.{target}",
                            found.receiver,
                            index.item(found.owner).fqn,
                        )
                    }
                    Err(err) => format!("{written} => Err({err:?})"),
                }
            })
            .collect()
    }

    /// The type each class literal's base names, in source order.
    fn literals(source: &str) -> Vec<String> {
        let root = block_on_inline(jals_syntax::Parse::parse(source)).syntax();
        let analysis = block_on_inline(FileAnalysis::of(&root));
        let index = block_on_inline(
            ProjectIndex::builder(&[(FileId(0), root.clone())])
                .with_stdlib()
                .build(),
        );
        let semantics = analysis.in_project(&index, FileId(0));
        let facts = Facts::of(block_on_inline(semantics.typed()));
        root.descendants()
            .filter(|node| node.kind() == SyntaxKind::CLASS_LITERAL)
            .map(|node| {
                let written = node.text().to_string().trim().to_owned();
                let Some(base) = node.children().next() else {
                    return format!("{written} => <no base>");
                };
                match facts.ty_of_name(&base) {
                    Ok(ty) => format!(
                        "{written} => {}",
                        ty.project_id().map_or_else(
                            || "<not a project type>".to_owned(),
                            |id| index.item(id).fqn.to_string()
                        )
                    ),
                    Err(err) => format!("{written} => Err({err:?})"),
                }
            })
            .collect()
    }

    /// A class literal whose base is written with dots resolves to the type those dots name.
    ///
    /// `java.lang.String.class` is as ordinary as Java gets, and it did not compile: the base is a
    /// `FIELD_ACCESS`, the walk read only the access's own direct tokens, and `.String` resolved as
    /// nothing. The failure was invisible because the corpus harnesses count a file that does not
    /// lower without saying which construct stopped it.
    #[test]
    fn a_class_literal_written_with_dots_names_the_type_the_dots_spell() {
        assert_eq!(
            literals(
                "class Outer {
                     static class Inner {}
                     void use() {
                         Class<?> a = java.lang.String.class;
                         Class<?> b = String.class;
                         Class<?> c = Outer.Inner.class;
                         Class<?> d = Inner.class;
                     }
                 }",
            ),
            [
                "java.lang.String.class => java.lang.String",
                "String.class => java.lang.String",
                "Outer.Inner.class => Outer.Inner",
                "Inner.class => Outer.Inner",
            ]
        );
    }

    /// A nested type names the member it qualifies, whichever way the nesting is spelled.
    ///
    /// `Outer.Inner::go` used to lose its head to the direct-token walk, find no type, and fall
    /// through to the branch that reads the qualifier as a *value* — reporting a reference to a
    /// `static` method as one on a value of an unindexed type.
    #[test]
    fn a_nested_type_qualifies_a_reference_however_the_nesting_is_written() {
        assert_eq!(
            refs(
                "interface Run { void run(); }
                 class Outer {
                     static class Inner { static void go() {} }
                     void use() {
                         Run a = Inner::go;
                         Run b = Outer.Inner::go;
                     }
                 }",
            ),
            [
                "Inner::go => Static Outer.Inner.go/0",
                "Outer.Inner::go => Static Outer.Inner.go/0",
            ]
        );
    }

    /// A dotted chain whose head is a type and whose whole is not stays a **value**.
    ///
    /// `System.err::println` is the shape that following the chain could plausibly have broken:
    /// `System` resolves, so a lookup that stopped at the head would call `println` on the class.
    /// It is the index that decides — no item is registered under `java.lang.System.err` — which is
    /// what keeps JLS §6.5.2's ambiguity resolved by what exists rather than by what parses.
    #[test]
    fn a_dotted_chain_whose_head_is_a_type_is_still_a_value_when_the_whole_is_not() {
        assert_eq!(
            refs(
                "interface Give { int give(); }
                 class P {
                     P held;
                     int size() { return 0; }
                     void use(P p) { Give g = p.held::size; }
                 }",
            ),
            ["p.held::size => Bound P.size/0"]
        );
    }

    /// The same written reference names two different methods, and which one is the *interface's*
    /// arity rather than the order they were declared in.
    ///
    /// This is the module's founding incident. The wasm lowering selected by **name alone** and so
    /// took whichever overload `own_members` yielded first — declaration order — while the JVM one
    /// already read the arity. `go(int, int)` is declared **before** `go(int)` here precisely so
    /// that an order-driven selection would answer `go/2` for both, and be visible.
    #[test]
    fn the_interfaces_arity_selects_the_overload_and_declaration_order_does_not() {
        assert_eq!(
            refs(
                "interface Take { void take(int v); }
                 interface Both { void both(int a, int b); }
                 class P {
                     static void go(int a, int b) {}
                     static void go(int a) {}
                     void use() {
                         Take one = P::go;
                         Both two = P::go;
                     }
                 }",
            ),
            ["P::go => Static P.go/1", "P::go => Static P.go/2"]
        );
    }

    /// A `static` and an instance method of one name and one arity are told apart by whether the
    /// qualifier is a type or a value, and the `static` one wins the *unbound* reading.
    ///
    /// `P::val` could be read two ways against a one-argument interface: the `static val(P)`, or the
    /// instance `val()` with the interface's argument supplied as the receiver. JLS §15.13.1 makes
    /// the first the answer, which is why the primary search runs before the unbound fallback —
    /// reversing them compiles a call to the wrong body with no diagnostic.
    #[test]
    fn a_static_and_an_instance_method_of_one_name_are_told_apart_by_the_qualifier() {
        assert_eq!(
            refs(
                "interface Grab { int grab(P p); }
                 interface Give { int give(); }
                 class P {
                     static int val(P p) { return 0; }
                     int val() { return 0; }
                     void use(P p) {
                         Grab byType = P::val;
                         Give byValue = p::val;
                     }
                 }",
            ),
            ["P::val => Static P.val/1", "p::val => Bound P.val/0"]
        );
    }

    /// With no `static` method to take the arity, `Type::instanceMethod` is the *unbound* form: the
    /// interface passes the receiver as its first argument, so the referenced method takes one
    /// fewer parameter than the interface declares.
    #[test]
    fn the_unbound_form_takes_one_fewer_parameter_than_the_interface_declares() {
        assert_eq!(
            refs(
                "interface Grab { int grab(P p); }
                 class P {
                     int size() { return 0; }
                     void use() { Grab g = P::size; }
                 }",
            ),
            ["P::size => Unbound P.size/0"]
        );
    }

    /// `this::m` is a bound reference, and `this` is not an expression the inference records.
    ///
    /// The type of the qualifier is asked three ways because the three shapes are recorded in three
    /// places, and this is the arm no memo answers: a lookup that only consulted `type_of_expr`
    /// reports `this::m` as a reference on a value of an unindexed type — a construct the source is
    /// entitled to write, rejected.
    #[test]
    fn a_bound_reference_on_this_is_read_through_the_enclosing_type() {
        assert_eq!(
            refs(
                "interface Give { int give(); }
                 class P {
                     int size() { return 0; }
                     void use() { Give g = this::size; }
                 }",
            ),
            ["this::size => Bound P.size/0"]
        );
    }

    /// A constructor reference matches the interface's arity, and a class that declares none still
    /// answers — with the descriptor but no member.
    ///
    /// `String::new` is the documented `target: None`: the stubs carry no constructor for
    /// `java.lang.String`, and `()V` exists where the member does not. A search that treated the
    /// missing member as a failure would reject the commonest constructor reference there is.
    #[test]
    fn a_constructor_reference_answers_even_where_the_class_declares_no_constructor() {
        assert_eq!(
            refs(
                "interface Make { P make(int v); }
                 interface MakeText { String make(); }
                 class P {
                     P(int v) {}
                     void use() {
                         Make m = P::new;
                         MakeText t = String::new;
                     }
                 }",
            ),
            [
                "P::new => Constructs P.P/1",
                "String::new => Constructs java.lang.String.<no member>"
            ]
        );
    }

    /// `super::m` is reported rather than compiled.
    ///
    /// It is a *non-virtual* call on an inherited method, and no `LambdaMetafactory` handle spells
    /// one: javac synthesises a bridge that makes the `invokespecial` and points the handle at
    /// that. Compiling it as `this::m` is the same bytes dispatching virtually — a program that
    /// runs and calls the override the source wrote `super` to avoid, which is why the refusal is
    /// asserted rather than left to a lowering to notice.
    #[test]
    fn a_super_method_reference_is_reported_rather_than_compiled_as_this() {
        assert_eq!(
            refs(
                "interface Give { String give(); }
                 class P {
                     public String toString() { return \"\"; }
                     void use() { Give g = super::toString; }
                 }",
            ),
            ["super::toString => Err(Unsupported(\"a `super` method reference\"))"]
        );
    }
}
