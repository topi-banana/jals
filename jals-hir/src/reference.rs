//! References (uses): the identifier occurrences that name resolution tries to bind.

use std::ops::Range;

use crate::def::{DefId, Namespace};

/// The outcome of resolving a [`Reference`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The reference binds to a file-local definition.
    Def(DefId),
    /// The reference was examined but not bound. This covers names that legitimately have no
    /// file-local definition — an imported or external type, an inherited member, `this` / `super`
    /// — as well as a genuinely undeclared name. Phase 1 does not distinguish these.
    Unresolved,
}

/// A reference: an identifier occurrence in value or method-invocation position.
///
/// Type-name occurrences (inside a `TYPE` node) are not references in Phase 1 — they are not
/// recorded here — because type resolution is out of scope. The right-hand name of a member access
/// (`obj.field`) is likewise absent: it needs a type to resolve, and structurally it is a bare
/// token rather than a name-reference node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The byte range of the referencing identifier token.
    pub range: Range<usize>,
    /// The referenced name.
    pub name: String,
    /// The name-space the reference looks up in (value vs. method, decided by syntactic position).
    pub namespace: Namespace,
    /// What the reference resolved to.
    pub resolution: Resolution,
}
