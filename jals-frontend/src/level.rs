//! The IR levels a frontend may ask to observe.

/// The lowest intermediate representation a frontend can work from.
///
/// The levels form a total order, and the driver forces every stage up to the one a frontend
/// declares — never further. The declaration is not a performance hint: it is the *scope of the
/// frontend's cache key*. A frontend that only reads one file's bytes is keyed on that file, so
/// editing a sibling cannot invalidate it; a frontend that reads the project index is keyed on
/// the whole project, because it genuinely observed the whole project. Under-declaring is
/// therefore a correctness bug, not a slow path.
///
/// Only [`Bytes`](Self::Bytes) exists today. Later variants are purely additive: every match on
/// this enum is exhaustive and in-tree, so adding one is a compile error at each site that must
/// learn about it rather than a silent fallthrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum IrLevel {
    /// Raw file bytes. The bottom of the lattice, always available, and the only level that
    /// needs neither a parser nor cross-file context.
    Bytes = 0,
}

impl IrLevel {
    /// The discriminant folded into a frontend's cache provenance.
    ///
    /// Written as an explicit match rather than an `as` cast so that reordering the variants
    /// cannot silently renumber a shipped frontend's cache keys.
    pub const fn tag(self) -> u8 {
        match self {
            Self::Bytes => 0,
        }
    }
}

/// The version of the observable shape of every stage in this crate.
///
/// Folded into all frontend provenance, so a change to what a level *hands out* invalidates
/// cached output instead of being silently trusted. Mirrors `BUILD_SCRIPT_API_VERSION`.
pub const PIPELINE_API_VERSION: u32 = 1;
