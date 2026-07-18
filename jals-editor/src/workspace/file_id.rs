//! The three `FileId` id-spaces a [`Workspace`](super::Workspace) addresses.
//!
//! A [`jals_hir::FileId`] is opaque to `jals-hir` (the host assigns it; the index only compares and
//! stores it), so the workspace partitions the single `u32` address space into three disjoint
//! regions — the project's own `.java`, a `-sources.jar` overlay, and a `git`/`path` source
//! dependency. That partition is an invariant nothing in the raw `u32` enforces;
//! [`WorkspaceFileId`] makes it a type: [`from_raw`](WorkspaceFileId::from_raw) /
//! [`to_raw`](WorkspaceFileId::to_raw) are the *only* place the bit-ranges live, so allocation is
//! a constructor and routing ([`ws_file`](super::Workspace::ws_file)) is one exhaustive match.

use jals_hir::FileId;

/// Base [`FileId`] for extracted library source files (the `-sources.jar` overlays), far above any
/// project file's id (a project has nowhere near 2³¹ files) and below [`SOURCE_DEP_FILE_BASE`] /
/// `jals-hir`'s reserved stub/classfile block, so the id spaces never collide.
const LIBRARY_FILE_BASE: u32 = 1 << 31;

/// Base [`FileId`] for `git`/`path` library-source files, a third id space above
/// [`LIBRARY_FILE_BASE`] (giving each space ~2³⁰ ids) and still below `jals-hir`'s reserved
/// stub/classfile block near `u32::MAX`, so project / `-sources.jar` / `git`-`path` ids never collide.
const SOURCE_DEP_FILE_BASE: u32 = (1 << 31) + (1 << 30);

/// Which of the workspace's three id-spaces a [`FileId`] belongs to, plus its index within that
/// space. The partition of the raw `u32` lives entirely in [`from_raw`](Self::from_raw) /
/// [`to_raw`](Self::to_raw); every other site allocates and routes through this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceFileId {
    /// A project's own `.java`, indexed and linted. Id `index` (base 0).
    Project(u32),
    /// A `-sources.jar` overlay: navigation-only library source. Id <code>[LIBRARY_FILE_BASE] + index</code>.
    Library(u32),
    /// A `git`/`path` source dependency: an index input *and* a navigation target. Id
    /// <code>[SOURCE_DEP_FILE_BASE] + index</code>.
    SourceDep(u32),
}

impl WorkspaceFileId {
    /// Decode a raw [`FileId`] into its id-space. Total: every `u32` falls in exactly one space (the
    /// regions tile `[0, u32::MAX]`).
    ///
    /// `jals-hir` reserves the top of the `u32` range (`u32::MAX - i`) for its stub/classfile
    /// pseudo-files, which numerically lands in [`SourceDep`](Self::SourceDep)'s upper end. That is
    /// intentional and harmless: such an id decodes to `SourceDep(huge)`, and the caller's bounds
    /// check (`source_dep_files.get(huge)`) then yields `None` — the same "no real file" result the
    /// old range check produced, with no special fourth case to maintain.
    #[inline]
    pub(crate) const fn from_raw(id: FileId) -> Self {
        if id.0 >= SOURCE_DEP_FILE_BASE {
            Self::SourceDep(id.0 - SOURCE_DEP_FILE_BASE)
        } else if id.0 >= LIBRARY_FILE_BASE {
            Self::Library(id.0 - LIBRARY_FILE_BASE)
        } else {
            Self::Project(id.0)
        }
    }

    /// Encode an id-space + within-space index back into a raw [`FileId`] (`base + index`).
    #[inline]
    pub(crate) const fn to_raw(self) -> FileId {
        match self {
            Self::Project(i) => FileId(i),
            Self::Library(i) => FileId(LIBRARY_FILE_BASE + i),
            Self::SourceDep(i) => FileId(SOURCE_DEP_FILE_BASE + i),
        }
    }

    /// The raw id of the `index`-th file of `space`. A within-space index is bounded by the set
    /// of files on disk — nowhere near 2³⁰ — so the narrowing saturates only defensively.
    #[inline]
    pub(crate) fn of_index(space: fn(u32) -> Self, index: usize) -> FileId {
        space(u32::try_from(index).unwrap_or(u32::MAX)).to_raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_routes_each_space() {
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(0)),
            WorkspaceFileId::Project(0)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(7)),
            WorkspaceFileId::Project(7)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(LIBRARY_FILE_BASE)),
            WorkspaceFileId::Library(0)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(LIBRARY_FILE_BASE + 3)),
            WorkspaceFileId::Library(3)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(SOURCE_DEP_FILE_BASE)),
            WorkspaceFileId::SourceDep(0)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(SOURCE_DEP_FILE_BASE + 5)),
            WorkspaceFileId::SourceDep(5)
        );
    }

    #[test]
    fn boundaries_belong_to_the_higher_space() {
        // The first id of each space decodes to index 0 of that space, and the last id of the space
        // below decodes to that lower space — the regions tile without overlap.
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(LIBRARY_FILE_BASE - 1)),
            WorkspaceFileId::Project(LIBRARY_FILE_BASE - 1)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(SOURCE_DEP_FILE_BASE - 1)),
            WorkspaceFileId::Library(SOURCE_DEP_FILE_BASE - 1 - LIBRARY_FILE_BASE)
        );
    }

    #[test]
    fn round_trips_in_every_space() {
        for wfid in [
            WorkspaceFileId::Project(0),
            WorkspaceFileId::Project(42),
            WorkspaceFileId::Library(0),
            WorkspaceFileId::Library(99),
            WorkspaceFileId::SourceDep(0),
            WorkspaceFileId::SourceDep(1234),
        ] {
            assert_eq!(WorkspaceFileId::from_raw(wfid.to_raw()), wfid);
        }
    }

    #[test]
    fn reserved_block_decodes_to_source_dep_out_of_range() {
        // A `jals-hir` reserved stub/classfile id (top of the u32 range) decodes into SourceDep with
        // an index far past any real file — the caller's `.get` then returns `None`.
        match WorkspaceFileId::from_raw(FileId(u32::MAX)) {
            WorkspaceFileId::SourceDep(i) => assert_eq!(i, u32::MAX - SOURCE_DEP_FILE_BASE),
            other => panic!("expected SourceDep, got {other:?}"),
        }
    }
}
