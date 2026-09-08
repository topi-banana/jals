//! The four `FileId` id-spaces a [`Workspace`](super::Workspace) addresses.
//!
//! A [`jals_hir::FileId`] is opaque to `jals-hir` (the host assigns it; the index only compares and
//! stores it) *except* at the top of the range, which that crate reserves for text no host can open
//! — a package's Java, a classpath pseudo-file. Below that, this workspace partitions the space
//! into three regions of its own: the project's own `.java`, a `-sources.jar` overlay, and a
//! `git`/`path` source dependency. The partition is an invariant nothing in the raw `u32` enforces;
//! [`WorkspaceFileId`] makes it a type: [`from_raw`](WorkspaceFileId::from_raw) /
//! [`to_raw`](WorkspaceFileId::to_raw) are the *only* place the bit-ranges live, so allocation is
//! a constructor and routing ([`ws_file`](super::Workspace::ws_file)) is one exhaustive match.
//!
//! The reserved region is **asked for, not restated**. `jals-hir` answers
//! [`FileId::library_index`] and [`FileId::is_openable`], so there is one definition of where that
//! region starts. A copy here would be a partition that agrees only until one side moves — which is
//! what a fourth base of this module's own was, sitting a fixed distance below a block the other
//! crate anchored to `u32::MAX` and grew downward.

use jals_hir::FileId;

/// Base [`FileId`] for extracted `-sources.jar` overlay files, far above any project file's id (a
/// project has nowhere near 2³¹ files) and below [`SOURCE_DEP_FILE_BASE`], so the id spaces never
/// collide.
const SOURCES_JAR_FILE_BASE: u32 = 1 << 31;

/// Base [`FileId`] for `git`/`path` library-source files, a third id space above
/// [`SOURCES_JAR_FILE_BASE`], so project / `-sources.jar` / `git`-`path` ids never collide.
const SOURCE_DEP_FILE_BASE: u32 = (1 << 31) + (1 << 30);

/// Which id-space a [`FileId`] belongs to, plus its index within that space. The partition of the
/// raw `u32` lives entirely in [`from_raw`](Self::from_raw) / [`to_raw`](Self::to_raw); every other
/// site allocates and routes through this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceFileId {
    /// A project's own `.java`, indexed and linted. Id `index` (base 0).
    Project(u32),
    /// A `-sources.jar` overlay: navigation-only library source. Id
    /// <code>[SOURCES_JAR_FILE_BASE] + index</code>.
    SourcesJar(u32),
    /// A `git`/`path` source dependency: an index input *and* a navigation target. Id
    /// <code>[SOURCE_DEP_FILE_BASE] + index</code>.
    SourceDep(u32),
    /// One compilation unit of the Java a package publishes, or a classpath pseudo-file: an index
    /// input with **no file behind it**. Its text is a compile-time constant in the binary that
    /// shipped the package, or a class file that was never source, so
    /// [`ws_file`](super::Workspace::ws_file) answers `None` for one — the same answer
    /// [`ItemOrigin::Library`](jals_hir::ItemOrigin::Library) gives a go-to-definition.
    ///
    /// Allocated by [`FileId::library`], never here: this space is `jals-hir`'s, and the index
    /// within it is whatever that crate's allocator assigned.
    Reserved,
}

impl WorkspaceFileId {
    /// Decode a raw [`FileId`] into its id-space. Total: every `u32` falls in exactly one space (the
    /// regions tile `[0, u32::MAX]`).
    #[inline]
    pub(crate) const fn from_raw(id: FileId) -> Self {
        if !id.is_openable() {
            Self::Reserved
        } else if id.0 >= SOURCE_DEP_FILE_BASE {
            Self::SourceDep(id.0 - SOURCE_DEP_FILE_BASE)
        } else if id.0 >= SOURCES_JAR_FILE_BASE {
            Self::SourcesJar(id.0 - SOURCES_JAR_FILE_BASE)
        } else {
            Self::Project(id.0)
        }
    }

    /// Encode an id-space + within-space index back into a raw [`FileId`] (`base + index`).
    ///
    /// [`Reserved`](Self::Reserved) has no encoding, and that is the point: the one way to make an
    /// id in that space is [`FileId::library`], so a second allocator cannot exist here.
    #[inline]
    const fn to_raw(self) -> Option<FileId> {
        Some(match self {
            Self::Project(i) => FileId(i),
            Self::SourcesJar(i) => FileId(SOURCES_JAR_FILE_BASE + i),
            Self::SourceDep(i) => FileId(SOURCE_DEP_FILE_BASE + i),
            Self::Reserved => return None,
        })
    }

    /// The raw id of the `index`-th file of `space`. A within-space index is bounded by the set
    /// of files on disk — nowhere near 2³⁰ — so the narrowing saturates only defensively.
    ///
    /// Callable only with a space that *has* an encoding; `Reserved` is a fieldless variant, so it
    /// does not typecheck as the `fn(u32) -> Self` this takes.
    #[inline]
    pub(crate) fn of_index(space: fn(u32) -> Self, index: usize) -> FileId {
        space(u32::try_from(index).unwrap_or(u32::MAX))
            .to_raw()
            .unwrap_or(FileId(u32::MAX))
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
            WorkspaceFileId::from_raw(FileId(SOURCES_JAR_FILE_BASE)),
            WorkspaceFileId::SourcesJar(0)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(SOURCES_JAR_FILE_BASE + 3)),
            WorkspaceFileId::SourcesJar(3)
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
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(SOURCES_JAR_FILE_BASE - 1)),
            WorkspaceFileId::Project(SOURCES_JAR_FILE_BASE - 1)
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(SOURCE_DEP_FILE_BASE - 1)),
            WorkspaceFileId::SourcesJar(SOURCE_DEP_FILE_BASE - 1 - SOURCES_JAR_FILE_BASE)
        );
    }

    #[test]
    fn round_trips_in_every_encodable_space() {
        for id in [
            WorkspaceFileId::Project(0),
            WorkspaceFileId::Project(41),
            WorkspaceFileId::SourcesJar(0),
            WorkspaceFileId::SourcesJar(9),
            WorkspaceFileId::SourceDep(0),
            WorkspaceFileId::SourceDep(2),
        ] {
            let raw = id.to_raw().expect("an encodable space");
            assert_eq!(WorkspaceFileId::from_raw(raw), id);
        }
    }

    /// Both spaces `jals-hir` reserves decode to the one variant that has no file, and neither can
    /// be mistaken for an id this workspace handed out.
    ///
    /// The regression: the reserved block used to be anchored to `u32::MAX` and grown *downward*,
    /// while this module's fourth base was a fixed constant — so how far down the block reached
    /// depended on how many library units were indexed, and nothing said where it had to stop.
    #[test]
    fn every_reserved_id_decodes_to_the_fileless_space() {
        assert_eq!(
            WorkspaceFileId::from_raw(FileId::library(0)),
            WorkspaceFileId::Reserved
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId::library(4_000)),
            WorkspaceFileId::Reserved
        );
        assert_eq!(
            WorkspaceFileId::from_raw(FileId(u32::MAX)),
            WorkspaceFileId::Reserved
        );
        assert_eq!(WorkspaceFileId::Reserved.to_raw(), None);
        assert_eq!(FileId::library(0).library_index(), Some(0));
        assert_eq!(FileId::library(12).library_index(), Some(12));
        assert_eq!(FileId(SOURCE_DEP_FILE_BASE).library_index(), None);
        assert!(FileId(SOURCE_DEP_FILE_BASE).is_openable());
        assert!(!FileId::library(0).is_openable());
    }
}
