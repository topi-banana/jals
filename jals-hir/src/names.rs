//! The value and method names that bind to nothing: javac's "cannot find symbol" for a *name*,
//! where [`unresolved_types`](crate::FileSemantics::unresolved_types) answers it for a *type*.
//!
//! The two are one question asked in three name-spaces, and they are separate functions because
//! the type half needs only the index (a type name is nameable or it is not) while this half has
//! to reconstruct the whole of what could have bound the name. A [`Value`](Namespace::Value) or
//! [`Method`](Namespace::Method) reference the file-local pass left
//! [`Unresolved`](crate::Resolution::Unresolved) has five other ways to be perfectly good, and
//! each one is a stand-down here:
//!
//! - **An inherited member.** `baseField` inside a subclass binds to the superclass's field, which
//!   the file-local pass cannot see. Resolved here, up the whole supertype chain.
//! - **A member the index cannot see.** A type that `extends` a JDK or third-party class may
//!   inherit anything, and a standard-library *stub* is deliberately partial — so where
//!   [`member_set_complete`](crate::ProjectIndex::member_set_complete) is false there is no
//!   negative answer to give.
//! - **A static import.** `import static java.lang.Math.*;` makes `max` a bare name with no
//!   declaration anywhere in the file.
//! - **An ambiguous name** (JLS §6.5.2). The `System` of `System.out` is recorded as a value
//!   reference because only reclassification decides it is a type; the same shape covers
//!   `Outer.staticField`, `Holder::get`, `X.class`, and the leading `java` of a package-qualified
//!   name. [`RefPosition::Qualifier`] is what the resolver marks these with.
//! - **A `case` label's constant** (JLS §14.11), which binds against the switch selector's type
//!   rather than against any scope. [`RefPosition::CaseLabel`].
//!
//! What is left over is the genuine article, and it is the same set javac rejects: an undeclared
//! name, a misspelt one, and a local used before its declaration.
//!
//! The answer is **per feature set**, and the last row is why: a name whose only declaration sits
//! in a `cfg`-disabled host is reported, because the compile frontend blanks that host and the
//! build really does not have the declaration. Under the other selection the same file is clean.
//! Nothing is reported from *inside* a disabled host — the resolver records no reference there at
//! all — so the two directions do not cancel: this pass speaks about the feature set it was given
//! and says nothing about any other.
//!
//! The [`enclosing type`](Enclosing) is what every member lookup runs against, and finding it is a
//! walk of its own rather than the inference layer's `enclosing_item`: that one stops at the
//! nearest **named** declaration, so an anonymous class body or an `enum` constant body would hand
//! its supertype's members to the class around it — reporting every one of them. Both are indexed
//! types in their own right, keyed on the `new` / constant position, and this walk stops at them.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use hashbrown::HashMap;
use jals_exec::Yielder;
use jals_syntax::SyntaxKind::{CLASS_BODY, ENUM_CONSTANT, NAME_REF, NEW_EXPR};
use jals_syntax::SyntaxNode;

use crate::def::Namespace;
use crate::project::{FileId, ItemId, ProjectIndex};
use crate::reference::{RefPosition, Resolution};
use crate::resolve::collect::Collect;

/// A value or method name that resolves to nothing: what
/// [`unresolved_names`](crate::FileSemantics::unresolved_names) answers with.
///
/// Carries its [`namespace`](Self::namespace) because the two read differently to a person — javac
/// says `variable x` against `method x()` — and deciding that from the source text again would be
/// re-doing the syntactic classification the resolver already made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedName {
    /// The byte range of the referencing identifier token.
    pub range: Range<usize>,
    /// The simple name that resolved to nothing.
    pub name: String,
    /// Which name-space the lookup ran in: [`Value`](Namespace::Value) for a variable or field
    /// read, [`Method`](Namespace::Method) for a bare call's callee.
    pub namespace: Namespace,
}

/// The type a reference is written inside, as the member lookups here need it: every reachable
/// state named, because two of the three are stand-downs and telling them apart is the whole point
/// of walking rather than calling [`ProjectIndex::enclosing_item`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enclosing {
    /// An indexed type surrounds the reference — including an anonymous class or an `enum`
    /// constant body, which are items in their own right.
    Item(ItemId),
    /// No type declaration surrounds it at all: a compact source file's implicit class (JEP 512).
    /// It declares no supertype, so `java.lang.Object` is the whole of what it inherits and the
    /// file-local answer is final for everything else.
    ImplicitClass,
    /// A type declaration surrounds it that the index does not carry, so nothing can be concluded
    /// about its members.
    Unindexed,
}

impl crate::analysis::FileSemantics<'_> {
    /// The file's value and method references that resolve to nothing — not file-locally, not
    /// through the supertype chain, and not through a static import.
    ///
    /// The sibling of [`unresolved_types`](Self::unresolved_types) in the other two name-spaces,
    /// and conservative in the same direction: every position where a negative answer would need
    /// something this index does not have stands down instead of reporting. Needs no inference —
    /// a name binds by scope and by member set, and neither is a type judgement.
    pub async fn unresolved_names(&self) -> Vec<UnresolvedName> {
        let (index, file) = (self.index(), self.file());
        let mut yielder = Yielder::new();
        let sites = Sites::of(self.root(), &mut yielder).await;
        let mut out = Vec::new();
        for r in self.analysis().references() {
            yielder.tick().await;
            if !matches!(r.namespace, Namespace::Value | Namespace::Method)
                || r.resolution != Resolution::Unresolved
                || r.position != RefPosition::Plain
            {
                continue;
            }
            if Sites::static_import_may_bind(index, file, &r.name, r.namespace) {
                continue;
            }
            // No `NAME_REF` at the recorded offset would mean the reference and the tree disagree,
            // which is exactly the case not to conclude anything from.
            let Some(site) = sites.at(r.range.start) else {
                continue;
            };
            if !Sites::is_undefined(index, file, site, &r.name, r.namespace) {
                continue;
            }
            out.push(UnresolvedName {
                range: r.range.clone(),
                name: r.name.clone(),
                namespace: r.namespace,
            });
        }
        out
    }
}

/// The file's `NAME_REF` nodes, keyed on the start of the identifier token each one carries —
/// which is the offset [`Reference::range`](crate::Reference::range) begins at.
///
/// Built once per call rather than searched per reference: the walk is the file, and doing it per
/// reference would make the pass quadratic on exactly the files that have the most to say.
struct Sites(HashMap<usize, SyntaxNode>);

impl Sites {
    /// Indexes every `NAME_REF` under `root`.
    async fn of(root: &SyntaxNode, yielder: &mut Yielder) -> Self {
        let mut map = HashMap::new();
        for node in root.descendants() {
            yielder.tick().await;
            if node.kind() == NAME_REF
                && let Some(tok) = Collect::first_ident_token(&node)
            {
                map.insert(Collect::token_start(&tok), node);
            }
        }
        Self(map)
    }

    /// The `NAME_REF` whose identifier starts at `offset`.
    fn at(&self, offset: usize) -> Option<&SyntaxNode> {
        self.0.get(&offset)
    }

    /// Whether `name` is undefined at `site` — the verdict, after the enclosing type has been
    /// found and its member set consulted.
    ///
    /// `false` covers both "it binds to an inherited member" and "the index cannot say", because
    /// the caller acts on them identically: only a definite *no* is worth a diagnostic.
    fn is_undefined(
        index: &ProjectIndex,
        file: FileId,
        site: &SyntaxNode,
        name: &str,
        namespace: Namespace,
    ) -> bool {
        match Self::enclosing(index, file, site) {
            Enclosing::Unindexed => false,
            // The implicit class of a compact source file inherits `Object` and nothing else, so
            // the only member set to except is `Object`'s own.
            Enclosing::ImplicitClass => !Self::is_inherited_from_object(name, namespace),
            Enclosing::Item(owner) => {
                index.resolve_member(owner, name, namespace).is_none()
                    && index.member_set_complete(owner)
                    && !Self::is_inherited_from_object(name, namespace)
            }
        }
    }

    /// Whether `name` could be the `java.lang.Object` member every type inherits.
    ///
    /// Only a method can be: `Object` declares no field, so a value-namespace name is never one of
    /// these. The check is by name rather than by lookup because it has to hold for an index built
    /// with no stubs and no classpath at all, where `Object` is not an item to ask.
    fn is_inherited_from_object(name: &str, namespace: Namespace) -> bool {
        namespace == Namespace::Method && ProjectIndex::is_object_method(name)
    }

    /// The type `site` is written inside.
    ///
    /// Walked here rather than shared with the inference layer's `enclosing_item` for the one case
    /// that separates them: an anonymous class body and an `enum` constant body are indexed types
    /// whose *own* supertypes supply members, and skipping to the named class around them would
    /// report every inherited member as undefined. Both are keyed on their opening node's offset,
    /// which is what the index recorded as their declaration.
    ///
    /// The `CLASS_BODY` test is what keeps a `new Foo(arg)` **argument** out: it sits under the
    /// same `NEW_EXPR` but outside the body, so the enclosing type there is the one around the
    /// `new`.
    fn enclosing(index: &ProjectIndex, file: FileId, site: &SyntaxNode) -> Enclosing {
        let mut prev: Option<SyntaxNode> = None;
        for ancestor in site.ancestors() {
            let in_body = prev.as_ref().is_some_and(|p| p.kind() == CLASS_BODY);
            let start = match ancestor.kind() {
                NEW_EXPR | ENUM_CONSTANT if in_body => usize::from(ancestor.text_range().start()),
                kind if ProjectIndex::type_decl_kind(kind).is_some() => {
                    let Some(tok) = Collect::first_ident_token(&ancestor) else {
                        return Enclosing::Unindexed;
                    };
                    Collect::token_start(&tok)
                }
                _ => {
                    prev = Some(ancestor);
                    continue;
                }
            };
            return index
                .item_by_decl(file, start)
                .map_or(Enclosing::Unindexed, Enclosing::Item);
        }
        Enclosing::ImplicitClass
    }

    /// Whether a static import could be what binds `name`.
    ///
    /// A single `import static a.b.C.name;` binds it by writing it, so there is nothing further to
    /// check — an import of a member that does not exist is the import's own error, not this
    /// name's. An on-demand `import static a.b.C.*;` binds it when `C` declares it, and where `C`
    /// is not indexed or its member set is partial, no answer is available.
    fn static_import_may_bind(
        index: &ProjectIndex,
        file: FileId,
        name: &str,
        namespace: Namespace,
    ) -> bool {
        let Some((single, on_demand)) = index.static_imports(file) else {
            return false;
        };
        if single.iter().any(|(member, _)| member == name) {
            return true;
        }
        on_demand.iter().any(|owner| {
            index.item_by_fqn(owner).is_none_or(|owner| {
                index.resolve_member(owner, name, namespace).is_some()
                    || !index.member_set_complete(owner)
            })
        })
    }
}
