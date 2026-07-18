use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::cache::{ArtifactCache, CacheBackend, MemoryCache};
use crate::error::{Diagnostic, Error, Result};
use crate::tree::{CodeTree, EntryRef};
use crate::{DirKey, FileKey, Revision};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    CreateFile(FileKey, Vec<u8>),
    ReplaceFile(FileKey, Vec<u8>),
    RemoveFile(FileKey),
    CreateDirectory(DirKey),
    RemoveDirectory(DirKey),
}

#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct SourceSnapshot {
    pub tree: CodeTree,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) mod private {
    pub trait Sealed {}
}

/// Closed persistence seam. Only the memory and native Adapters supplied by this crate implement it.
pub trait SourceBackend: private::Sealed {
    #[doc(hidden)]
    fn snapshot(&self) -> Result<SourceSnapshot>;
    /// Persist `changes`. `base` is the tree the changes were planned against, so a backend whose
    /// storage can change underneath it (the native filesystem) can refuse to overwrite content
    /// that no longer matches the snapshot instead of clobbering a concurrent external edit.
    #[doc(hidden)]
    fn apply(&mut self, changes: &[Change], base: &CodeTree) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct MemorySource {
    tree: CodeTree,
}

impl MemorySource {
    pub const fn new(tree: CodeTree) -> Self {
        Self { tree }
    }
}

impl private::Sealed for MemorySource {}

impl SourceBackend for MemorySource {
    fn snapshot(&self) -> Result<SourceSnapshot> {
        Ok(SourceSnapshot {
            tree: self.tree.clone(),
            diagnostics: Vec::new(),
        })
    }

    fn apply(&mut self, changes: &[Change], _base: &CodeTree) -> Result<()> {
        // In-memory storage cannot change underneath the aggregate, so the base snapshot carries
        // no extra precondition beyond the tree's own structural checks.
        let mut next = self.tree.clone();
        next.apply_changes(changes)?;
        self.tree = next;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ProjectView {
    revision: Revision,
    tree: Arc<CodeTree>,
}

impl ProjectView {
    pub const fn revision(&self) -> Revision {
        self.revision
    }
    pub fn tree(&self) -> &CodeTree {
        &self.tree
    }

    pub fn file(&self, key: &FileKey) -> Result<&crate::CodeFile> {
        match self.tree.lookup_file(key) {
            Some(EntryRef::File(file)) => Ok(file),
            Some(EntryRef::Directory(_)) => {
                Err(Error::ExpectedFile(DirKey::new(key.path().clone())))
            }
            None => Err(Error::NotFoundFile(key.clone())),
        }
    }

    /// The file's text, or [`Error::InvalidUtf8`] when its bytes are not UTF-8.
    pub fn file_text(&self, key: &FileKey) -> Result<&str> {
        self.file(key)?
            .text()
            .map_err(|_| Error::InvalidUtf8(key.clone()))
    }

    pub fn directory(&self, key: &DirKey) -> Result<&DirKey> {
        match self.tree.lookup_dir(key) {
            Some(EntryRef::Directory(dir)) => Ok(dir),
            Some(EntryRef::File(_)) => Err(Error::ExpectedDirectory(
                FileKey::new(key.path().clone()).expect("non-root directory collision"),
            )),
            None => Err(Error::NotFoundDirectory(key.clone())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshOutcome {
    pub revision: Revision,
    pub changed: bool,
    pub diagnostics: Vec<Diagnostic>,
}

/// Aggregate root for project source, editor overlay, artifact cache, and revision.
#[derive(Debug, Clone)]
pub struct ProjectStorage<S: SourceBackend, C: CacheBackend> {
    source: S,
    base: CodeTree,
    overlay: BTreeMap<FileKey, Arc<[u8]>>,
    cache: ArtifactCache<C>,
    current: Arc<CodeTree>,
    revision: Revision,
    diagnostics: Vec<Diagnostic>,
}

impl<S: SourceBackend, C: CacheBackend> ProjectStorage<S, C> {
    pub fn open(source: S, cache: C) -> Result<Self> {
        let snapshot = source.snapshot()?;
        let current = Arc::new(snapshot.tree.clone());
        Ok(Self {
            source,
            base: snapshot.tree,
            overlay: BTreeMap::new(),
            cache: ArtifactCache::new(cache),
            current,
            revision: Revision::INITIAL,
            diagnostics: snapshot.diagnostics,
        })
    }

    pub fn view(&self) -> ProjectView {
        ProjectView {
            revision: self.revision,
            tree: Arc::clone(&self.current),
        }
    }

    pub const fn revision(&self) -> Revision {
        self.revision
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub const fn artifacts(&self) -> &ArtifactCache<C> {
        &self.cache
    }
    pub const fn artifacts_mut(&mut self) -> &mut ArtifactCache<C> {
        &mut self.cache
    }

    /// Consume a detached storage snapshot and retain its verified artifact cache.
    pub fn into_artifacts(self) -> ArtifactCache<C> {
        self.cache
    }

    /// Install a cache produced by work against a detached snapshot. Source and overlay state are
    /// deliberately left untouched.
    pub fn replace_artifacts(&mut self, cache: ArtifactCache<C>) {
        self.cache = cache;
    }

    pub fn refresh(&mut self) -> Result<RefreshOutcome> {
        let snapshot = self.source.snapshot()?;
        let mut diagnostics = snapshot.diagnostics;
        for key in self.overlay.keys() {
            let before = self.base.file(key).map(crate::CodeFile::bytes);
            let after = snapshot.tree.file(key).map(crate::CodeFile::bytes);
            if before != after {
                diagnostics.push(Diagnostic::ExternalChangeShadowed(key.clone()));
            }
        }
        let changed = snapshot.tree != self.base;
        if changed {
            let current = Self::build_current(&snapshot.tree, &self.overlay)?;
            self.base = snapshot.tree;
            self.current = Arc::new(current);
            self.diagnostics.clone_from(&diagnostics);
            self.revision = self.revision.next();
        } else {
            self.diagnostics.clone_from(&diagnostics);
        }
        Ok(RefreshOutcome {
            revision: self.revision,
            changed,
            diagnostics,
        })
    }

    /// Set unsaved editor content. Overlay content wins over later external refreshes.
    pub fn set_overlay(
        &mut self,
        expected: Revision,
        key: FileKey,
        bytes: Vec<u8>,
    ) -> Result<Revision> {
        self.set_overlays(expected, core::iter::once((key, bytes)))
    }

    /// Set unsaved editor content for a batch of files under a single revision bump and rebuild.
    ///
    /// The batch is staged onto a *clone* of the overlay and rebuilt into a candidate tree before
    /// anything is published: a rejected entry (a file/ancestor collision surfacing only once the
    /// whole batch is applied) leaves the overlay, current tree, and revision untouched, so a
    /// failed call can never poison later ones or advance the revision past a stale tree.
    pub fn set_overlays(
        &mut self,
        expected: Revision,
        entries: impl IntoIterator<Item = (FileKey, Vec<u8>)>,
    ) -> Result<Revision> {
        self.check_revision(expected)?;
        let mut overlay = self.overlay.clone();
        let mut staged = false;
        for (key, bytes) in entries {
            if matches!(self.current.lookup_file(&key), Some(EntryRef::Directory(_))) {
                return Err(Error::ExpectedFile(DirKey::new(key.path().clone())));
            }
            overlay.insert(key, Arc::from(bytes));
            staged = true;
        }
        if !staged {
            return Ok(self.revision);
        }
        let tree = Self::build_current(&self.base, &overlay)?;
        self.overlay = overlay;
        self.current = Arc::new(tree);
        self.revision = self.revision.next();
        Ok(self.revision)
    }

    pub fn transaction(&mut self, expected: Revision) -> Result<Transaction<'_, S, C>> {
        self.check_revision(expected)?;
        let preview = (*self.current).clone();
        Ok(Transaction {
            storage: self,
            staged: Vec::new(),
            preview,
        })
    }

    fn check_revision(&self, expected: Revision) -> Result<()> {
        if expected == self.revision {
            Ok(())
        } else {
            Err(Error::StaleRevision {
                expected,
                actual: self.revision,
            })
        }
    }

    fn rebuild_current(&mut self) -> Result<()> {
        self.current = Arc::new(Self::build_current(&self.base, &self.overlay)?);
        Ok(())
    }

    /// Build the current tree from `base` with `overlay` applied on top (overlay content wins over
    /// external content). Total and side-effect-free: the caller publishes the result only on
    /// success, so a collision never mutates published state.
    fn build_current(base: &CodeTree, overlay: &BTreeMap<FileKey, Arc<[u8]>>) -> Result<CodeTree> {
        let mut tree = base.clone();
        for (key, bytes) in overlay {
            let collision = DirKey::new(key.path().clone());
            tree.remove_directory(&collision);
            tree.remove_file(key);
            tree.insert_file_with_parents(key.clone(), Arc::clone(bytes))?;
        }
        Ok(tree)
    }
}

impl ProjectStorage<MemorySource, MemoryCache> {
    pub fn memory(tree: CodeTree) -> Self {
        Self::open(MemorySource::new(tree), MemoryCache::default())
            .expect("memory source is infallible")
    }
}

pub struct Transaction<'a, S: SourceBackend, C: CacheBackend> {
    storage: &'a mut ProjectStorage<S, C>,
    staged: Vec<Change>,
    /// The current tree with every staged change applied, so each new change validates against
    /// the state it would commit into without replaying the whole batch.
    preview: CodeTree,
}

impl<S: SourceBackend, C: CacheBackend> Transaction<'_, S, C> {
    /// Validate `change` against the preview tree and stage it. Validation applies the change to
    /// a scratch copy so a rejected change leaves the preview untouched.
    fn stage(&mut self, change: Change) -> Result<&mut Self> {
        let mut next = self.preview.clone();
        next.apply_changes(core::slice::from_ref(&change))?;
        self.preview = next;
        self.staged.push(change);
        Ok(self)
    }

    pub fn create_file(&mut self, key: FileKey, bytes: Vec<u8>) -> Result<&mut Self> {
        self.stage(Change::CreateFile(key, bytes))
    }

    pub fn replace_file(&mut self, key: FileKey, bytes: Vec<u8>) -> Result<&mut Self> {
        self.stage(Change::ReplaceFile(key, bytes))
    }

    pub fn create_directory(&mut self, key: DirKey) -> Result<&mut Self> {
        self.stage(Change::CreateDirectory(key))
    }

    pub fn remove_file(&mut self, key: FileKey) -> Result<&mut Self> {
        self.stage(Change::RemoveFile(key))
    }

    pub fn remove_directory(&mut self, key: DirKey) -> Result<&mut Self> {
        self.stage(Change::RemoveDirectory(key))
    }

    pub fn commit(self) -> Result<Revision> {
        if self.staged.is_empty() {
            return Ok(self.storage.revision);
        }
        let mut next_base = self.storage.base.clone();
        next_base.apply_changes(&self.staged)?;
        // Persist against the base the changes were planned on, so a native backend can detect a
        // file that was externally rewritten after the snapshot and refuse rather than clobber it.
        let storage = &mut *self.storage;
        storage.source.apply(&self.staged, &storage.base)?;
        for change in &self.staged {
            match change {
                Change::CreateFile(key, _)
                | Change::ReplaceFile(key, _)
                | Change::RemoveFile(key) => {
                    self.storage.overlay.remove(key);
                }
                Change::RemoveDirectory(dir) => {
                    self.storage
                        .overlay
                        .retain(|key, _| !key.path().starts_with(dir.path()));
                }
                Change::CreateDirectory(_) => {}
            }
        }
        self.storage.base = next_base;
        self.storage.revision = self.storage.revision.next();
        self.storage.rebuild_current()?;
        Ok(self.storage.revision)
    }
}

impl CodeTree {
    pub(crate) fn apply_changes(&mut self, changes: &[Change]) -> Result<()> {
        for change in changes {
            match change {
                Change::CreateFile(key, bytes) => {
                    match self.lookup_file(key) {
                        Some(EntryRef::File(_)) => {
                            return Err(Error::AlreadyExistsFile(key.clone()));
                        }
                        Some(EntryRef::Directory(_)) => {
                            return Err(Error::AlreadyExistsDirectory(DirKey::new(
                                key.path().clone(),
                            )));
                        }
                        None => {}
                    }
                    self.insert_file_with_parents(key.clone(), bytes.clone())?;
                }
                Change::ReplaceFile(key, bytes) => {
                    match self.lookup_file(key) {
                        Some(EntryRef::File(_)) => {}
                        Some(EntryRef::Directory(_)) => {
                            return Err(Error::ExpectedFile(DirKey::new(key.path().clone())));
                        }
                        None => return Err(Error::NotFoundFile(key.clone())),
                    }
                    self.remove_file(key);
                    self.insert_file_with_parents(key.clone(), bytes.clone())?;
                }
                Change::RemoveFile(key) => match self.lookup_file(key) {
                    Some(EntryRef::File(_)) => {
                        self.remove_file(key);
                    }
                    Some(EntryRef::Directory(_)) => {
                        return Err(Error::ExpectedFile(DirKey::new(key.path().clone())));
                    }
                    None => return Err(Error::NotFoundFile(key.clone())),
                },
                Change::CreateDirectory(key) => {
                    match self.lookup_dir(key) {
                        Some(EntryRef::Directory(_)) => {
                            return Err(Error::AlreadyExistsDirectory(key.clone()));
                        }
                        Some(EntryRef::File(_)) => {
                            return Err(Error::AlreadyExistsFile(
                                FileKey::new(key.path().clone()).expect("non-root"),
                            ));
                        }
                        None => {}
                    }
                    self.insert_directory_with_parents(key)?;
                }
                Change::RemoveDirectory(key) => {
                    if key == &DirKey::ROOT {
                        return Err(Error::AlreadyExistsDirectory(key.clone()));
                    }
                    match self.lookup_dir(key) {
                        Some(EntryRef::Directory(_)) => {
                            self.remove_directory(key);
                        }
                        Some(EntryRef::File(_)) => {
                            return Err(Error::ExpectedDirectory(
                                FileKey::new(key.path().clone()).expect("non-root"),
                            ));
                        }
                        None => return Err(Error::NotFoundDirectory(key.clone())),
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Entry;

    fn storage() -> ProjectStorage<MemorySource, MemoryCache> {
        ProjectStorage::memory(
            CodeTree::new([Entry::File(
                FileKey::parse("A.java").unwrap(),
                b"old".to_vec(),
            )])
            .unwrap(),
        )
    }

    #[test]
    fn views_are_immutable_and_stale_transactions_fail() {
        let mut storage = storage();
        let old = storage.view();
        let rev = storage
            .set_overlay(
                old.revision(),
                FileKey::parse("A.java").unwrap(),
                b"edit".to_vec(),
            )
            .unwrap();
        assert_eq!(
            old.file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"old"
        );
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"edit"
        );
        assert!(matches!(
            storage.transaction(Revision::INITIAL),
            Err(Error::StaleRevision { .. })
        ));
        assert_eq!(rev.get(), 1);
    }

    #[test]
    fn transaction_advances_only_on_commit() {
        let mut storage = storage();
        let before = storage.revision();
        let mut tx = storage.transaction(before).unwrap();
        tx.create_file(FileKey::parse("B.java").unwrap(), b"b".to_vec())
            .unwrap();
        assert_eq!(tx.commit().unwrap().get(), before.get() + 1);
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("B.java").unwrap())
                .unwrap()
                .bytes(),
            b"b"
        );
    }

    #[derive(Debug)]
    struct FailingSource {
        tree: CodeTree,
    }
    impl private::Sealed for FailingSource {}
    impl SourceBackend for FailingSource {
        fn snapshot(&self) -> Result<SourceSnapshot> {
            Ok(SourceSnapshot {
                tree: self.tree.clone(),
                diagnostics: Vec::new(),
            })
        }
        fn apply(&mut self, _changes: &[Change], _base: &CodeTree) -> Result<()> {
            Err(Error::Io("injected persistence failure".into()))
        }
    }

    #[test]
    fn persistence_failure_does_not_publish_a_revision() {
        let tree = CodeTree::new([Entry::File(
            FileKey::parse("A.java").unwrap(),
            b"old".to_vec(),
        )])
        .unwrap();
        let mut storage =
            ProjectStorage::open(FailingSource { tree }, MemoryCache::default()).unwrap();
        let revision = storage.revision();
        let mut transaction = storage.transaction(revision).unwrap();
        transaction
            .replace_file(FileKey::parse("A.java").unwrap(), b"new".to_vec())
            .unwrap();
        assert!(transaction.commit().is_err());
        assert_eq!(storage.revision(), revision);
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"old"
        );
    }

    #[test]
    fn failed_overlay_batch_leaves_storage_consistent() {
        let mut storage = storage();
        let revision = storage.revision();
        // Staging `A.java` and then `A.java/B.java` passes the per-entry directory check against
        // the unchanged current tree, but rebuilding collides (a file cannot have a file ancestor).
        let batch = [
            (FileKey::parse("A.java").unwrap(), b"edit".to_vec()),
            (FileKey::parse("A.java/B.java").unwrap(), b"nested".to_vec()),
        ];
        assert!(matches!(
            storage.set_overlays(revision, batch),
            Err(Error::InvalidTree(crate::TreeError::FileAncestor(_)))
        ));
        // Revision, overlay, and current tree are all untouched: the rejected batch neither
        // advanced the revision nor poisoned later operations.
        assert_eq!(storage.revision(), revision);
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"old"
        );
        // A subsequent valid overlay still succeeds against the unchanged revision.
        let next = storage
            .set_overlay(
                revision,
                FileKey::parse("A.java").unwrap(),
                b"edit".to_vec(),
            )
            .unwrap();
        assert_eq!(next.get(), revision.get() + 1);
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"edit"
        );
    }

    #[test]
    fn single_overlay_onto_a_file_ancestor_is_rejected_without_poisoning() {
        let mut storage = storage();
        let revision = storage.revision();
        // `A.java` is a file, so overlaying `A.java/Nested.java` cannot rebuild.
        assert!(
            storage
                .set_overlay(
                    revision,
                    FileKey::parse("A.java/Nested.java").unwrap(),
                    b"x".to_vec(),
                )
                .is_err()
        );
        assert_eq!(storage.revision(), revision);
        // The overlay was not retained: a later rebuild (via a fresh valid overlay) succeeds.
        storage
            .set_overlay(
                revision,
                FileKey::parse("A.java").unwrap(),
                b"edit".to_vec(),
            )
            .unwrap();
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"edit"
        );
    }

    #[test]
    fn external_change_is_shadowed_by_overlay() {
        let mut storage = storage();
        let key = FileKey::parse("A.java").unwrap();
        storage
            .set_overlay(storage.revision(), key.clone(), b"local".to_vec())
            .unwrap();
        storage.source.tree.remove_file(&key);
        storage
            .source
            .tree
            .insert_file_with_parents(key.clone(), b"external".to_vec())
            .unwrap();
        let outcome = storage.refresh().unwrap();
        assert!(
            outcome
                .diagnostics
                .contains(&Diagnostic::ExternalChangeShadowed(key.clone()))
        );
        assert_eq!(storage.view().file(&key).unwrap().bytes(), b"local");
    }

    #[test]
    fn failed_refresh_does_not_publish_base_or_revision() {
        let mut storage = ProjectStorage::memory(
            CodeTree::new([Entry::File(
                FileKey::parse("A/B.java").unwrap(),
                b"old".to_vec(),
            )])
            .unwrap(),
        );
        let revision = storage.revision();
        storage
            .set_overlay(
                revision,
                FileKey::parse("A/B.java").unwrap(),
                b"edit".to_vec(),
            )
            .unwrap();
        let overlay_revision = storage.revision();
        let published = storage.view();
        storage.source.tree = CodeTree::new([Entry::File(
            FileKey::parse("A").unwrap(),
            b"external".to_vec(),
        )])
        .unwrap();

        assert!(matches!(
            storage.refresh(),
            Err(Error::InvalidTree(crate::TreeError::FileAncestor(_)))
        ));
        assert_eq!(storage.revision(), overlay_revision);
        assert_eq!(storage.view().tree(), published.tree());
        assert!(storage.base.file(&FileKey::parse("A").unwrap()).is_none());
    }
}
