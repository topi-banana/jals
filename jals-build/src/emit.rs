//! Writing jals output into a directory jals does not exclusively own.
//!
//! [`StagedTree`](crate::StagedTree) writes into managed build output and is allowed to delete
//! anything the current tree does not name, because nothing else may live there. Two steps write
//! somewhere that rule does not hold: `jals expand --out-dir <dir>`, whose destination the user
//! chose, and the resource copy into `[build] classes-dir`, which is shared with the compiler's
//! own output. In both, an unknown file may be something a person wrote or something another step
//! produced, and deleting it would destroy work.
//!
//! What replaces the rule is an **ownership journal**: each step records the paths it wrote, and a
//! later run removes only what its own previous run recorded and no longer emits. A file jals
//! never wrote is therefore never a candidate for removal — with no journal at all, nothing is
//! deleted, which is exactly the behaviour a first run into a populated directory needs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jals_exec::tokio_rt::on_blocking_pool;
use jals_storage::RelativePath;

use crate::backend::{BackendError, BackendSource};

/// The journal `jals expand` keeps inside its `--out-dir`.
///
/// Beside the output rather than under the project's managed build root because the destination is
/// the only thing the two runs are known to share: a second checkout, a different `--manifest-path`
/// or a `target/` someone cleaned must still be able to tell what the last expansion put here.
pub const EXPAND_JOURNAL: &str = ".jals-expand";

/// The journal the resource copy keeps for `[build] classes-dir`.
///
/// Under the managed build root instead, because here the destination is *already* jals-owned
/// output — what the journal separates is the resources from the class files beside them, and a
/// `jals clean` that removes both together loses nothing.
pub const RESOURCE_JOURNAL: &str = "target/jals/build/resources";

/// Write one file, creating the directories above it.
///
/// Shared with [`StagedTree`](crate::StagedTree) so both writers skip an identical file the same
/// way: leaving the bytes alone leaves the mtime alone, which is what keeps `javac`'s own staleness
/// checks working across a warm rebuild.
pub(crate) async fn write_file(target: PathBuf, bytes: Vec<u8>) -> Result<(), BackendError> {
    on_blocking_pool(move || -> std::io::Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read(&target).is_ok_and(|existing| existing == bytes) {
            return Ok(());
        }
        std::fs::write(&target, &bytes)
    })
    .await
    .map_err(|error| BackendError::Io(error.to_string()))
}

/// The record of what one emit step wrote into a directory it does not own.
pub struct EmitJournal {
    root: PathBuf,
    journal: PathBuf,
}

impl EmitJournal {
    /// A journal for output written under `root`, stored at `journal`.
    pub const fn new(root: PathBuf, journal: PathBuf) -> Self {
        Self { root, journal }
    }

    /// Remove what the previous run recorded and `current` does not name, then record `current`.
    /// Returns the paths removed, in journal order.
    ///
    /// Removal is by path and never by walk: only a file this journal names can be deleted, so a
    /// directory the user filled, a symlink someone left, and the compiler's class files beside a
    /// copied resource are all outside what this can touch. A journal entry that no longer parses
    /// as a project-relative path is skipped rather than guessed at — the cost is a leftover file,
    /// which is the safe direction.
    ///
    /// # Errors
    /// Returns [`BackendError::Io`] when a recorded file exists and cannot be removed, or when the
    /// journal cannot be written.
    pub async fn reconcile(
        &self,
        current: &BTreeSet<RelativePath>,
    ) -> Result<Vec<PathBuf>, BackendError> {
        let stale: Vec<RelativePath> = self
            .read()
            .await
            .into_iter()
            .filter(|path| !current.contains(path))
            .collect();
        let removed = self.remove(stale).await?;
        self.record(current).await?;
        Ok(removed)
    }

    /// The paths the previous run recorded. A missing or unreadable journal is an empty one:
    /// nothing is known to be ours, so nothing is deleted.
    async fn read(&self) -> Vec<RelativePath> {
        let journal = self.journal.clone();
        on_blocking_pool(move || std::fs::read_to_string(&journal))
            .await
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .filter_map(|line| RelativePath::parse(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Delete the recorded files that are gone from the current emit, and any directory that
    /// emptied as a result.
    async fn remove(&self, stale: Vec<RelativePath>) -> Result<Vec<PathBuf>, BackendError> {
        if stale.is_empty() {
            return Ok(Vec::new());
        }
        let root = self.root.clone();
        on_blocking_pool(move || -> std::io::Result<Vec<PathBuf>> {
            let mut removed = Vec::new();
            for path in stale {
                let target = path.to_host_path(&root);
                // `symlink_metadata` rather than `is_file`: a journal names files, so an entry that
                // has since become a directory is not ours to delete, and a symlink is unlinked
                // rather than followed.
                match std::fs::symlink_metadata(&target) {
                    Ok(metadata) if metadata.is_dir() => continue,
                    Ok(_) => std::fs::remove_file(&target)?,
                    // Already gone: the record is stale in both directions, which is not an error.
                    Err(_) => continue,
                }
                // Directories the removal emptied, up to but never including the root. A
                // still-populated one fails, which is the intended no-op.
                let mut parent = target.parent().map(Path::to_path_buf);
                while let Some(dir) = parent {
                    if dir == root || !dir.starts_with(&root) || std::fs::remove_dir(&dir).is_err()
                    {
                        break;
                    }
                    parent = dir.parent().map(Path::to_path_buf);
                }
                removed.push(target);
            }
            Ok(removed)
        })
        .await
        .map_err(|error| BackendError::Io(error.to_string()))
    }

    /// Write the journal for this run. Sorted, one path per line, so two runs that emitted the same
    /// set produce the same file.
    async fn record(&self, current: &BTreeSet<RelativePath>) -> Result<(), BackendError> {
        let mut text = String::from(
            "# jals ownership journal — generated, do not edit.\n\
             # Every path below was written by jals and is removed when a later run stops\n\
             # emitting it. A path that is not listed here is never deleted.\n",
        );
        for path in current {
            text.push_str(&path.to_string());
            text.push('\n');
        }
        let journal = self.journal.clone();
        on_blocking_pool(move || -> std::io::Result<()> {
            if let Some(parent) = journal.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&journal, text.as_bytes())
        })
        .await
        .map_err(|error| BackendError::Io(error.to_string()))
    }
}

/// A lowered tree written into a directory jals does not own.
///
/// The counterpart of [`StagedTree`](crate::StagedTree): same bytes, same layout, but the stale
/// rule is the journal above instead of "delete what the tree does not name". `jals expand` is the
/// one producer — a host that compiles the lowered tree itself chooses where it goes, and that
/// choice can be a directory holding a Gradle build, a checkout, or someone's notes.
pub struct EmittedTree {
    removed: Vec<PathBuf>,
}

impl EmittedTree {
    /// Write `tree` under `root` and reconcile the journal beside it.
    ///
    /// What reached disk is `tree` itself and so is not reported back: the caller already holds it,
    /// and the only thing this step knows that the caller does not is what it *removed*.
    ///
    /// # Errors
    /// Returns [`BackendError::Io`] when a file cannot be written, a recorded file cannot be
    /// removed, or the journal cannot be updated.
    pub async fn write(tree: &[BackendSource], root: PathBuf) -> Result<Self, BackendError> {
        let mut emitted = BTreeSet::new();
        for source in tree {
            write_file(source.path.to_host_path(&root), source.bytes.clone()).await?;
            emitted.insert(source.path.clone());
        }
        let journal = EmitJournal::new(root.clone(), root.join(EXPAND_JOURNAL));
        let removed = journal.reconcile(&emitted).await?;
        Ok(Self { removed })
    }

    /// What a previous expansion into this directory left behind and this one removed.
    pub fn removed(&self) -> &[PathBuf] {
        &self.removed
    }
}
