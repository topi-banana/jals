use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::cache::{self, ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest};
use crate::error::{CacheError, Diagnostic, Error, Result};
use crate::storage::{self, Change, SourceBackend, SourceSnapshot};
use crate::{CodeTree, DirKey, Entry, FileKey, Name, ProjectStorage, RelativePath};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct NativeSource {
    root: PathBuf,
    excluded: Vec<RelativePath>,
}

impl NativeSource {
    pub fn new(root: PathBuf) -> Result<Self> {
        // Hosts derive roots with `Path::parent()`, which yields `Some("")` for a bare file
        // name; an empty root addresses the current directory.
        let root = if root.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            root
        };
        let metadata = fs::metadata(&root).map_err(|error| NativeFs::io_error(&root, &error))?;
        if !metadata.is_dir() {
            return Err(Error::Io(format!(
                "project root is not a directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            root,
            excluded: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Exclude a derived native subtree from project snapshots. Native storage uses this for its
    /// cache root; host adapters may use it for metadata directories such as a Git checkout's
    /// `.git` directory.
    #[must_use]
    pub fn excluding(mut self, path: RelativePath) -> Self {
        if !path.is_root() {
            self.excluded.push(path);
        }
        self
    }

    fn os_path(&self, path: &crate::RelativePath) -> PathBuf {
        path.to_host_path(&self.root)
    }
}

impl RelativePath {
    /// Resolve this portable path against a host `root`.
    pub fn to_host_path(&self, root: &Path) -> PathBuf {
        self.segments()
            .fold(root.to_path_buf(), |path, name| path.join(name.as_str()))
    }

    /// Lower a host path under `root` into a portable path. `None` when `path` does not lie
    /// under `root` or a component is not a portable UTF-8 name.
    pub fn from_host_path(root: &Path, path: &Path) -> Option<Self> {
        let relative = path.strip_prefix(root).ok()?;
        let mut segments = Vec::new();
        for component in relative.components() {
            segments.push(Name::new(component.as_os_str().to_str()?).ok()?);
        }
        Some(Self::new(segments))
    }
}

impl storage::private::Sealed for NativeSource {}

impl SourceBackend for NativeSource {
    fn snapshot(&self) -> Result<SourceSnapshot> {
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|error| NativeFs::io_error(&self.root, &error))?;
        let mut entries = vec![Entry::Directory(DirKey::ROOT)];
        let mut diagnostics = Vec::new();
        let mut stack = vec![canonical_root.clone()];
        let mut scan = NativeScan {
            canonical_root: &canonical_root,
            stack: &mut stack,
            entries: &mut entries,
            diagnostics: &mut diagnostics,
            excluded: &self.excluded,
        };
        NativeFs::scan_directory(&self.root, &DirKey::ROOT, &mut scan, true)?;
        let tree = CodeTree::new(entries)?;
        Ok(SourceSnapshot { tree, diagnostics })
    }

    fn apply(&mut self, changes: &[Change]) -> Result<()> {
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|error| NativeFs::io_error(&self.root, &error))?;
        let mut journal = ApplyJournal::default();
        for change in changes {
            if let Err(error) = self.apply_change(change, &canonical_root, &mut journal) {
                return Err(journal.roll_back(error));
            }
        }
        journal.discard();
        Ok(())
    }
}

impl NativeSource {
    fn apply_change(
        &self,
        change: &Change,
        canonical_root: &Path,
        journal: &mut ApplyJournal,
    ) -> Result<()> {
        match change {
            Change::CreateFile(key, bytes) => {
                let path = self.confined_path(canonical_root, key.path())?;
                let parent = path
                    .parent()
                    .ok_or_else(|| Error::Io(format!("path has no parent: {}", path.display())))?;
                journal.create_directories(parent)?;
                NativeFs::atomic_write(&path, bytes, WriteMode::Create)?;
                journal.record(Undo::RemoveCreatedFile(path));
            }
            Change::ReplaceFile(key, bytes) => {
                let path = self.confined_path(canonical_root, key.path())?;
                if !path.is_file() {
                    return Err(Error::NotFoundFile(key.clone()));
                }
                journal.back_up_file(&path)?;
                NativeFs::atomic_write(&path, bytes, WriteMode::Replace)?;
            }
            Change::RemoveFile(key) => {
                let path = self.confined_path(canonical_root, key.path())?;
                journal.remove_file(&path)?;
            }
            Change::CreateDirectory(key) => {
                let path = self.confined_path(canonical_root, key.path())?;
                journal.create_directories(&path)?;
            }
            Change::RemoveDirectory(key) => {
                let path = self.confined_path(canonical_root, key.path())?;
                journal.remove_directory(&path)?;
            }
        }
        Ok(())
    }

    /// The host location of `path` for a mutation. The staged logical parents may not all exist
    /// on disk yet, so only the deepest existing ancestor is canonicalized — and a prefix that
    /// resolves outside the project root (a parent that is an escaping symlink) is refused
    /// rather than written through.
    fn confined_path(&self, canonical_root: &Path, path: &RelativePath) -> Result<PathBuf> {
        let host = self.os_path(path);
        let mut existing = host.as_path();
        loop {
            match fs::symlink_metadata(existing) {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    existing = existing
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .ok_or_else(|| NativeFs::io_error(existing, &error))?;
                }
                Err(error) => return Err(NativeFs::io_error(existing, &error)),
            }
        }
        let canonical =
            fs::canonicalize(existing).map_err(|error| NativeFs::io_error(existing, &error))?;
        if !canonical.starts_with(canonical_root) {
            return Err(Error::Io(format!(
                "refusing to write outside the project root through {}",
                host.display()
            )));
        }
        Ok(host)
    }
}

/// Undo journal for one native `apply` batch. Every mutation records how to reverse itself, so
/// a failure part-way rolls the tree back to the pre-transaction state instead of leaving an
/// arbitrary prefix persisted while the logical revision was never published. Once the whole
/// batch has landed, [`discard`](Self::discard) deletes the backups.
#[derive(Default)]
struct ApplyJournal {
    undo: Vec<Undo>,
}

#[derive(Debug)]
enum Undo {
    /// Remove a file this batch created.
    RemoveCreatedFile(PathBuf),
    /// Remove a directory this batch created. Recorded parent-first, so the reverse walk
    /// removes children before their parents.
    RemoveCreatedDir(PathBuf),
    /// Move a backed-up file over its original location.
    RestoreFile { original: PathBuf, backup: PathBuf },
    /// Move a backed-up directory back to its original location.
    RestoreDir { original: PathBuf, backup: PathBuf },
}

impl ApplyJournal {
    fn record(&mut self, undo: Undo) {
        self.undo.push(undo);
    }

    /// Create every missing ancestor of `path` plus `path` itself, recording each directory
    /// actually created. Existing directories are left alone; a final component that exists as
    /// a non-directory is an error.
    fn create_directories(&mut self, path: &Path) -> Result<()> {
        let mut missing = Vec::new();
        let mut current = path;
        while fs::symlink_metadata(current).is_err() {
            missing.push(current.to_path_buf());
            match current.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => current = parent,
                _ => break,
            }
        }
        for dir in missing.iter().rev() {
            fs::create_dir(dir).map_err(|error| NativeFs::io_error(dir, &error))?;
            self.record(Undo::RemoveCreatedDir(dir.clone()));
        }
        if !path.is_dir() {
            return Err(Error::Io(format!("not a directory: {}", path.display())));
        }
        Ok(())
    }

    /// Copy `path` aside before it is replaced, so a later failure restores the original bytes.
    fn back_up_file(&mut self, path: &Path) -> Result<()> {
        let backup = Self::backup_path(path)?;
        fs::copy(path, &backup).map_err(|error| NativeFs::io_error(path, &error))?;
        self.record(Undo::RestoreFile {
            original: path.to_path_buf(),
            backup,
        });
        Ok(())
    }

    /// Remove `path` by moving it aside, so a later failure restores it.
    fn remove_file(&mut self, path: &Path) -> Result<()> {
        let backup = Self::backup_path(path)?;
        fs::rename(path, &backup).map_err(|error| NativeFs::io_error(path, &error))?;
        self.record(Undo::RestoreFile {
            original: path.to_path_buf(),
            backup,
        });
        Ok(())
    }

    /// Remove the directory at `path` by moving it aside, so a later failure restores it whole.
    fn remove_directory(&mut self, path: &Path) -> Result<()> {
        let backup = Self::backup_path(path)?;
        fs::rename(path, &backup).map_err(|error| NativeFs::io_error(path, &error))?;
        self.record(Undo::RestoreDir {
            original: path.to_path_buf(),
            backup,
        });
        Ok(())
    }

    /// A sibling backup location, so the moves stay on one filesystem.
    fn backup_path(path: &Path) -> Result<PathBuf> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::Io(format!("path has no parent: {}", path.display())))?;
        Ok(NativeFs::temporary_path(
            parent,
            path.file_name().unwrap_or_else(|| OsStr::new("entry")),
        ))
    }

    /// Reverse every applied mutation, newest first. `error` is the failure that triggered the
    /// rollback; any mutation that cannot be reversed is appended so the caller sees the disk
    /// may not be fully restored.
    fn roll_back(self, error: Error) -> Error {
        let mut failures = Vec::new();
        for undo in self.undo.into_iter().rev() {
            let result = match &undo {
                Undo::RemoveCreatedFile(path) => fs::remove_file(path),
                Undo::RemoveCreatedDir(path) => fs::remove_dir(path),
                Undo::RestoreFile { original, backup } | Undo::RestoreDir { original, backup } => {
                    fs::rename(backup, original)
                }
            };
            if let Err(failure) = result {
                failures.push(format!("{undo:?}: {failure}"));
            }
        }
        if failures.is_empty() {
            error
        } else {
            Error::Io(format!(
                "{error}; rollback incomplete: {}",
                failures.join(", ")
            ))
        }
    }

    /// Delete the backups once the whole batch has been persisted.
    fn discard(self) {
        for undo in self.undo {
            match undo {
                Undo::RemoveCreatedFile(_) | Undo::RemoveCreatedDir(_) => {}
                Undo::RestoreFile { backup, .. } => {
                    let _ = fs::remove_file(backup);
                }
                Undo::RestoreDir { backup, .. } => {
                    let _ = fs::remove_dir_all(backup);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum WriteMode {
    Create,
    Replace,
}

struct NativeFs;

struct NativeScan<'a> {
    canonical_root: &'a Path,
    stack: &'a mut Vec<PathBuf>,
    entries: &'a mut Vec<Entry>,
    diagnostics: &'a mut Vec<Diagnostic>,
    excluded: &'a [RelativePath],
}

impl NativeFs {
    #[allow(clippy::unnecessary_debug_formatting)]
    fn scan_directory(
        physical: &Path,
        logical: &DirKey,
        scan: &mut NativeScan<'_>,
        root: bool,
    ) -> Result<()> {
        let read_dir = match fs::read_dir(physical) {
            Ok(read_dir) => read_dir,
            Err(error) if root => return Err(Self::io_error(physical, &error)),
            Err(error) => {
                scan.diagnostics
                    .push(Diagnostic::UnreadableEntry(format!("{logical}: {error}")));
                return Ok(());
            }
        };

        let mut dir_entries: Vec<_> = read_dir.collect();
        // Unreadable entries sort last; readable ones by name bytes. The key is computed once per
        // entry rather than on every comparison.
        dir_entries.sort_by_cached_key(|entry| match entry {
            Ok(entry) => (false, Self::os_bytes(&entry.file_name())),
            Err(error) => (true, error.to_string().into_bytes()),
        });

        for entry in dir_entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    scan.diagnostics
                        .push(Diagnostic::UnreadableEntry(error.to_string()));
                    continue;
                }
            };
            let os_name = entry.file_name();
            let Some(text_name) = os_name.to_str() else {
                scan.diagnostics
                    .push(Diagnostic::NonUtf8Entry(format!("{os_name:?}")));
                continue;
            };
            let name = match Name::new(text_name) {
                Ok(name) => name,
                Err(error) => {
                    scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                        "{logical}/{text_name}: {error:?}"
                    )));
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                        "{}: {error}",
                        path.display()
                    )));
                    continue;
                }
            };
            let logical_dir = logical.directory(name.clone());
            let logical_file = logical.file(name);
            if Self::is_excluded(logical_dir.path(), scan.excluded) {
                continue;
            }

            if file_type.is_symlink() {
                let canonical = match fs::canonicalize(&path) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                            "{}: {error}",
                            path.display()
                        )));
                        continue;
                    }
                };
                if !canonical.starts_with(scan.canonical_root) {
                    scan.diagnostics
                        .push(Diagnostic::SymlinkEscapesRoot(logical_file.to_string()));
                    continue;
                }
                if scan.stack.contains(&canonical) {
                    scan.diagnostics
                        .push(Diagnostic::SymlinkCycle(logical_file.to_string()));
                    continue;
                }
                match fs::metadata(&canonical) {
                    Ok(metadata) if metadata.is_dir() => {
                        scan.entries.push(Entry::Directory(logical_dir.clone()));
                        scan.stack.push(canonical.clone());
                        Self::scan_directory(&canonical, &logical_dir, scan, false)?;
                        scan.stack.pop();
                    }
                    Ok(metadata) if metadata.is_file() => {
                        Self::read_file(&canonical, logical_file, scan.entries, scan.diagnostics);
                    }
                    Ok(_) => {}
                    Err(error) => scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                        "{}: {error}",
                        path.display()
                    ))),
                }
            } else if file_type.is_dir() {
                scan.entries.push(Entry::Directory(logical_dir.clone()));
                let canonical = match fs::canonicalize(&path) {
                    Ok(canonical) => canonical,
                    Err(error) => {
                        scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                            "{}: {error}",
                            path.display()
                        )));
                        continue;
                    }
                };
                scan.stack.push(canonical);
                Self::scan_directory(&path, &logical_dir, scan, false)?;
                scan.stack.pop();
            } else if file_type.is_file() {
                Self::read_file(&path, logical_file, scan.entries, scan.diagnostics);
            }
        }
        Ok(())
    }

    fn read_file(
        path: &Path,
        key: FileKey,
        entries: &mut Vec<Entry>,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        match fs::read(path) {
            Ok(bytes) => entries.push(Entry::File(key, bytes)),
            Err(error) => diagnostics.push(Diagnostic::UnreadableEntry(format!(
                "{}: {error}",
                path.display()
            ))),
        }
    }

    fn is_excluded(path: &RelativePath, excluded: &[RelativePath]) -> bool {
        excluded.iter().any(|prefix| path.starts_with(prefix))
    }

    #[cfg(unix)]
    fn os_bytes(value: &OsStr) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    fn os_bytes(value: &OsStr) -> Vec<u8> {
        value.to_string_lossy().as_bytes().to_vec()
    }

    fn atomic_write(path: &Path, bytes: &[u8], mode: WriteMode) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::Io(format!("path has no parent: {}", path.display())))?;
        fs::create_dir_all(parent).map_err(|error| Self::io_error(parent, &error))?;
        let temporary = Self::temporary_path(
            parent,
            path.file_name().unwrap_or_else(|| OsStr::new("artifact")),
        );
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| Self::io_error(&temporary, &error))?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(Self::io_error(&temporary, &error));
        }
        drop(file);
        let result = match mode {
            WriteMode::Create => fs::hard_link(&temporary, path),
            WriteMode::Replace => fs::rename(&temporary, path),
        };
        let _ = fs::remove_file(&temporary);
        result.map_err(|error| Self::io_error(path, &error))
    }

    /// Write-once creation of `path`. A concurrent writer may win the `Create` race; a winner
    /// that persisted identical bytes counts as success, anything else is a conflict.
    fn create_once_accepting_identical(
        path: &Path,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        if let Err(error) = Self::atomic_write(path, bytes, WriteMode::Create) {
            return match fs::read(path) {
                Ok(winner) if winner == bytes => Ok(()),
                Ok(_) => Err(CacheError::Conflict),
                Err(_) => Err(CacheError::Io(error.to_string())),
            };
        }
        Ok(())
    }

    fn temporary_path(parent: &Path, name: &OsStr) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(".jals-{}-{sequence}.tmp", std::process::id()));
        parent.join(temp_name)
    }

    fn io_error(path: &Path, error: &std::io::Error) -> Error {
        Error::Io(format!("{}: {error}", path.display()))
    }
}

#[derive(Debug, Clone)]
pub struct NativeCache {
    root: PathBuf,
}

impl NativeCache {
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Native location used only when a host adapter must pass an artifact to an OS process.
    pub fn artifact_path(&self, key: &CacheKey) -> PathBuf {
        self.root
            .join(key.namespace().directory())
            .join(key.provenance().to_hex())
            .join(key.content().to_hex())
    }

    /// Native location of the advisory provenance → content pointer for a namespace. Lives under
    /// its own `index` subtree so it can never collide with an artifact path.
    fn index_path(&self, namespace: CacheNamespace, provenance: &ContentDigest) -> PathBuf {
        self.root
            .join("index")
            .join(namespace.directory())
            .join(provenance.to_hex())
    }
}

impl ArtifactCache<NativeCache> {
    /// Materialize a verified byte artifact under a logical file name for an OS process which
    /// requires a meaningful extension (notably `javac` source inputs). The canonical cache entry
    /// remains authoritative; this derived view is created atomically and re-verified on reuse.
    pub fn materialize_source(
        &self,
        key: &CacheKey,
        logical: &RelativePath,
    ) -> core::result::Result<PathBuf, CacheError> {
        let bytes = self.lookup(key)?.ok_or(CacheError::Corrupt)?;
        let base = self
            .backend()
            .root
            .join("source-view")
            .join(key.provenance().to_hex())
            .join(key.content().to_hex());
        let path = logical.to_host_path(&base);
        match fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(path),
            Ok(_) => return Err(CacheError::Corrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CacheError::Io(format!("{}: {error}", path.display()))),
        }
        NativeFs::create_once_accepting_identical(&path, &bytes)?;
        Ok(path)
    }
}

impl cache::private::Sealed for NativeCache {}

impl CacheBackend for NativeCache {
    fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        let path = self.artifact_path(key);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CacheError::Io(format!("{}: {error}", path.display()))),
        }
    }

    fn publish_once(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        NativeFs::create_once_accepting_identical(&self.artifact_path(key), bytes)
    }

    fn load_index(
        &self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
    ) -> core::result::Result<Option<ContentDigest>, CacheError> {
        let path = self.index_path(namespace, provenance);
        match fs::read_to_string(&path) {
            Ok(text) => ContentDigest::from_hex(text.trim())
                .map(Some)
                .ok_or(CacheError::Corrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CacheError::Io(format!("{}: {error}", path.display()))),
        }
    }

    fn store_index(
        &mut self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
        content: &ContentDigest,
    ) -> core::result::Result<(), CacheError> {
        let path = self.index_path(namespace, provenance);
        // `Replace` semantics: the pointer is last-writer-wins by design (see
        // `ArtifactCache::record_index`), unlike write-once artifact publication.
        NativeFs::atomic_write(&path, content.to_hex().as_bytes(), WriteMode::Replace)
            .map_err(|error| CacheError::Io(error.to_string()))
    }
}

impl ProjectStorage<NativeSource, NativeCache> {
    /// The conventional per-project cache location, shared by every host.
    pub const PROJECT_CACHE_DIR: &'static str = "target/jals/cache";

    /// Open a project laid out under `root` with the conventional cache root
    /// ([`PROJECT_CACHE_DIR`](Self::PROJECT_CACHE_DIR)), keeping `.git` metadata out of snapshots.
    pub fn for_project(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let cache_root = root.join(Self::PROJECT_CACHE_DIR);
        let source = Self::source_excluding_cache(root, &cache_root)?
            .excluding(RelativePath::parse(".git").expect(".git is a portable path"));
        Self::open(source, NativeCache::new(cache_root))
    }

    pub fn native(root: impl AsRef<Path>, cache_root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let cache_root = cache_root.as_ref().to_path_buf();
        let source = Self::source_excluding_cache(root, &cache_root)?;
        Self::open(source, NativeCache::new(cache_root))
    }

    fn source_excluding_cache(root: &Path, cache_root: &Path) -> Result<NativeSource> {
        let mut source = NativeSource::new(root.to_path_buf())?;
        if let Some(path) = RelativePath::from_host_path(root, cache_root) {
            source = source.excluding(path);
        }
        Ok(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArtifactCache, CacheNamespace, ContentDigest};

    #[test]
    fn native_snapshot_changes_only_after_refresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("A.java"), b"one").unwrap();
        let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
        let old = storage.view();
        fs::write(dir.path().join("A.java"), b"two").unwrap();
        assert_eq!(
            old.file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"one"
        );
        storage.refresh().unwrap();
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"two"
        );
        assert_eq!(
            old.file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"one"
        );
    }

    #[test]
    fn native_cache_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"jar";
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"url"),
            ContentDigest::of(bytes),
        );
        let backend = NativeCache::new(dir.path().to_path_buf());
        let stored = backend.artifact_path(&key);
        let mut cache = ArtifactCache::new(backend);
        cache.publish(&key, bytes).unwrap();
        fs::write(stored, b"tampered").unwrap();
        assert_eq!(cache.lookup(&key), Err(CacheError::Corrupt));
    }

    #[test]
    fn native_cache_publishers_observe_one_complete_winner() {
        let dir = tempfile::tempdir().unwrap();
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"parallel"),
            ContentDigest::of(b"jar"),
        );
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let root = dir.path().to_path_buf();
                let key = key.clone();
                std::thread::spawn(move || {
                    let mut cache = ArtifactCache::new(NativeCache::new(root));
                    cache.publish(&key, b"jar")
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), Ok(()));
        }
        let cache = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
        assert_eq!(cache.lookup(&key).unwrap(), Some(b"jar".to_vec()));
    }

    #[test]
    fn native_cache_root_is_not_project_source() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("A.java"), b"class A {}").unwrap();
        let cache_root = dir.path().join(".cache");
        let mut storage = ProjectStorage::native(dir.path(), cache_root.clone()).unwrap();
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"source"),
            ContentDigest::of(b"jar"),
        );
        storage.artifacts_mut().publish(&key, b"jar").unwrap();
        storage.refresh().unwrap();
        assert!(
            storage
                .view()
                .tree()
                .directory(&DirKey::parse(".cache").unwrap())
                .is_none()
        );
        assert!(cache_root.is_dir());
    }

    #[test]
    fn native_source_accepts_an_empty_root_as_current_directory() {
        let source = NativeSource::new(PathBuf::new()).unwrap();
        assert_eq!(source.root(), Path::new("."));
    }

    #[test]
    fn native_transaction_failure_rolls_back_earlier_changes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("A.java"), b"old").unwrap();
        let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
        // Created externally after the snapshot: `CreateFile` validates against the captured
        // tree but fails on disk, after the two earlier changes already persisted.
        fs::write(dir.path().join("B.java"), b"external").unwrap();
        let revision = storage.revision();
        let mut transaction = storage.transaction(revision).unwrap();
        transaction
            .replace_file(FileKey::parse("A.java").unwrap(), b"new".to_vec())
            .unwrap();
        transaction
            .create_file(FileKey::parse("sub/C.java").unwrap(), b"c".to_vec())
            .unwrap();
        transaction
            .create_file(FileKey::parse("B.java").unwrap(), b"clobber".to_vec())
            .unwrap();
        assert!(transaction.commit().is_err());

        assert_eq!(fs::read(dir.path().join("A.java")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("B.java")).unwrap(), b"external");
        assert!(!dir.path().join("sub").exists());
        assert_eq!(storage.revision(), revision);
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("A.java").unwrap())
                .unwrap()
                .bytes(),
            b"old"
        );
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray backups: {leftovers:?}");
    }

    #[test]
    fn native_transaction_rolls_back_removals() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("pkg")).unwrap();
        fs::write(dir.path().join("pkg/Kept.java"), b"kept").unwrap();
        fs::write(dir.path().join("Gone.java"), b"gone").unwrap();
        let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
        fs::write(dir.path().join("New.java"), b"external").unwrap();
        let revision = storage.revision();
        let mut transaction = storage.transaction(revision).unwrap();
        transaction
            .remove_file(FileKey::parse("Gone.java").unwrap())
            .unwrap();
        transaction
            .remove_directory(DirKey::parse("pkg").unwrap())
            .unwrap();
        transaction
            .create_file(FileKey::parse("New.java").unwrap(), b"clobber".to_vec())
            .unwrap();
        assert!(transaction.commit().is_err());

        assert_eq!(fs::read(dir.path().join("Gone.java")).unwrap(), b"gone");
        assert_eq!(fs::read(dir.path().join("pkg/Kept.java")).unwrap(), b"kept");
        assert_eq!(fs::read(dir.path().join("New.java")).unwrap(), b"external");
        assert_eq!(storage.revision(), revision);
    }

    #[test]
    fn native_transaction_creates_directory_parents() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
        let revision = storage.revision();
        let mut transaction = storage.transaction(revision).unwrap();
        transaction
            .create_directory(DirKey::parse("a/b").unwrap())
            .unwrap();
        transaction.commit().unwrap();
        assert!(dir.path().join("a/b").is_dir());
        assert!(
            storage
                .view()
                .tree()
                .directory(&DirKey::parse("a/b").unwrap())
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_transaction_refuses_writes_through_escaping_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), dir.path().join("out")).unwrap();
        let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
        let revision = storage.revision();
        let mut transaction = storage.transaction(revision).unwrap();
        // The escaping symlink was diagnosed out of the snapshot, so the logical parent does
        // not exist and staging reconstructs it — the physical write must still be refused.
        transaction
            .create_file(FileKey::parse("out/X.java").unwrap(), b"x".to_vec())
            .unwrap();
        assert!(transaction.commit().is_err());
        assert!(!outside.path().join("X.java").exists());
        assert_eq!(storage.revision(), revision);
    }

    #[test]
    fn native_cache_index_round_trips_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let key = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"locator"),
            ContentDigest::of(b"jar"),
        );
        let mut cache = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
        assert_eq!(
            cache
                .indexed_key(CacheNamespace::DependencyJar, ContentDigest::of(b"locator"))
                .unwrap(),
            None
        );
        cache.publish(&key, b"jar").unwrap();
        cache.record_index(&key).unwrap();

        let reopened = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
        let recovered = reopened
            .indexed_key(CacheNamespace::DependencyJar, ContentDigest::of(b"locator"))
            .unwrap()
            .expect("index survives reopen");
        assert_eq!(recovered, key);
        assert_eq!(reopened.lookup(&recovered).unwrap(), Some(b"jar".to_vec()));

        // The pointer is last-writer-wins: recording a newer content replaces it.
        let newer = CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"locator"),
            ContentDigest::of(b"jar-v2"),
        );
        let mut reopened = reopened;
        reopened.publish(&newer, b"jar-v2").unwrap();
        reopened.record_index(&newer).unwrap();
        assert_eq!(
            reopened
                .indexed_key(CacheNamespace::DependencyJar, ContentDigest::of(b"locator"))
                .unwrap(),
            Some(newer)
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_scan_diagnoses_symlink_escape_cycle_and_non_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("Outside.java"), b"class Outside {}").unwrap();
        fs::create_dir(dir.path().join("loop")).unwrap();
        symlink(outside.path(), dir.path().join("escape")).unwrap();
        symlink(dir.path().join("loop"), dir.path().join("loop/back")).unwrap();
        fs::write(
            dir.path()
                .join(OsString::from_vec(vec![b'b', b'a', b'd', 0xff])),
            b"ignored",
        )
        .unwrap();

        let storage = ProjectStorage::native(dir.path(), dir.path().join(".cache")).unwrap();
        assert!(
            storage
                .diagnostics()
                .iter()
                .any(|diagnostic| matches!(diagnostic, Diagnostic::SymlinkEscapesRoot(_)))
        );
        assert!(
            storage
                .diagnostics()
                .iter()
                .any(|diagnostic| matches!(diagnostic, Diagnostic::SymlinkCycle(_)))
        );
        assert!(
            storage
                .diagnostics()
                .iter()
                .any(|diagnostic| matches!(diagnostic, Diagnostic::NonUtf8Entry(_)))
        );
    }
}
