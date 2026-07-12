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
    pub qualified: Option<String>,
}
