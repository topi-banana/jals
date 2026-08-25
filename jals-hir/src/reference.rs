//! References (uses): the identifier occurrences that name resolution tries to bind.

use alloc::string::String;
use core::ops::Range;

use crate::def::{DefId, Namespace};

/// The outcome of resolving a [`Reference`] within one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The reference binds to a file-local definition.
    Def(DefId),
    /// The reference was examined but not bound to a *file-local* definition. This covers names
    /// that legitimately have no file-local definition — an imported or external type, an
    /// inherited member, `this` / `super` — as well as a genuinely undeclared name. The file-local
    /// pass does not distinguish these; a [`Type`](Namespace::Type) reference left `Unresolved`
    /// here is what the project layer ([`crate::ProjectIndex`]) then tries to bind cross-file.
    Unresolved,
}

impl Resolution {
    /// The definition this reference bound to, or `None` if it stayed
    /// [`Unresolved`](Self::Unresolved).
    pub const fn def_id(self) -> Option<DefId> {
        match self {
            Self::Def(id) => Some(id),
            Self::Unresolved => None,
        }
    }
}

/// Where a [`Reference`] sits syntactically, for the one question its position decides: whether a
/// name that stays [`Unresolved`](Resolution::Unresolved) after the project layer means *nothing
/// defines it*, or only that this is not the pass that binds it.
///
/// Every other consumer reads a reference by name and name-space alone. This exists because a
/// **negative** answer is sound only where the scope chain and the project index are the whole of
/// what could have bound the name, and in two positions they are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefPosition {
    /// An ordinary use: between them, the scope chain and the project index decide it.
    Plain,
    /// The left-hand side of a qualified name (`Holder.field`, `Holder::get`, `Holder.class`) —
    /// what JLS §6.5.2 calls an *ambiguous name*. It denotes a variable, a type, or a package, and
    /// only reclassification decides which; the value-namespace lookup that records it therefore
    /// misses on the latter two without the name being undefined.
    Qualifier,
    /// A `case` label's constant. JLS §14.11 lets an `enum` constant be written there unqualified,
    /// and it binds against the *selector's* type — which no lookup on the name alone reaches.
    CaseLabel,
}

/// A reference: an identifier occurrence the resolver examines.
///
/// References cover value and method-invocation positions and — since Phase 2 — type-name
/// positions (the name inside a `TYPE` node, in [`Namespace::Type`]). The right-hand name of a
/// member access (`obj.field`) is still absent: it needs a type to resolve, and structurally it is
/// a bare token rather than a name-reference node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The byte range of the referencing identifier token (the simple name; for a dotted type
    /// `a.b.C` this is the last segment `C`).
    pub range: Range<usize>,
    /// The referenced simple name.
    pub name: String,
    /// The name-space the reference looks up in (value / method / type, by syntactic position).
    pub namespace: Namespace,
    /// What the reference resolved to within the file.
    pub resolution: Resolution,
    /// For a qualified type reference (`a.b.C`), its full dotted text (`"a.b.C"`); `None` for a
    /// simple name. The project layer resolves a qualified type against a fully-qualified name
    /// rather than the scope chain, so this is recorded but left [`Resolution::Unresolved`] here.
    pub(crate) qualified: Option<String>,
    /// Where the reference is written, which is what says whether an [`Unresolved`] answer is a
    /// verdict about the *name* or only about this pass. Read by the "cannot resolve" fact and by
    /// nothing else.
    ///
    /// [`Unresolved`]: Resolution::Unresolved
    pub(crate) position: RefPosition,
}
