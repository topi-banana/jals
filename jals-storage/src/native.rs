use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use jals_exec::Exec;

use crate::cache::{self, ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest};
use crate::error::{CacheError, Diagnostic, Error, Result};
use crate::io::{self, IoError, SeekFrom};
use crate::storage::{self, Change, SourceBackend, SourceSnapshot};
use crate::{CodeTree, DirKey, Entry, FileKey, Name, ProjectStorage, RelativePath};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

use jals_exec::tokio_rt::on_blocking_pool;

#[derive(Debug, Clone)]
pub struct NativeSource {
    root: PathBuf,
    excluded: Vec<RelativePath>,
    scopes: Vec<NativeScope>,
    restricted: bool,
}

/// A subtree selected for a native snapshot.
///
/// An optional extension limits file contents while directories are still traversed, so a Java
/// source root can observe additions/removals without ingesting binaries beside the sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeScope {
    root: RelativePath,
    extension: Option<String>,
}

impl NativeScope {
    /// Include every file at or below `root`.
    pub const fn all(root: RelativePath) -> Self {
        Self {
            root,
            extension: None,
        }
    }

    /// Include only files with `extension` at or below `root` (ASCII case-insensitive).
    pub fn extension(root: RelativePath, extension: impl Into<String>) -> Self {
        Self {
            root,
            extension: Some(extension.into()),
        }
    }

    fn visits_directory(&self, path: &RelativePath) -> bool {
        path.starts_with(&self.root) || self.root.starts_with(path)
    }

    fn includes_file(&self, key: &FileKey) -> bool {
        key.path().starts_with(&self.root)
            && self.extension.as_deref().is_none_or(|extension| {
                key.extension()
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(extension))
            })
    }
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
            scopes: Vec::new(),
            restricted: false,
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

    /// Restrict snapshots to the supplied scopes. An empty scope list produces an empty project
    /// tree; callers that explicitly need a complete filesystem mirror omit this method.
    #[must_use]
    pub fn scoped(mut self, scopes: impl IntoIterator<Item = NativeScope>) -> Self {
        self.scopes.extend(scopes);
        self.restricted = true;
        self
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

/// The structural half of a snapshot: directory entries in scan order, the files whose contents
/// are still to be read, and everything diagnosed along the way.
struct ScanOutcome {
    entries: Vec<Entry>,
    files: Vec<(FileKey, PathBuf)>,
    diagnostics: Vec<Diagnostic>,
}

impl ScanOutcome {
    /// Fold per-file read outcomes into the scanned structure: files land in scan order and
    /// unreadable entries become diagnostics, so the snapshot is deterministic however the
    /// reads were produced (fan-out or inline).
    fn assemble(
        self,
        contents: impl IntoIterator<Item = (FileKey, core::result::Result<Vec<u8>, String>)>,
    ) -> Result<SourceSnapshot> {
        let Self {
            mut entries,
            mut diagnostics,
            ..
        } = self;
        for (key, outcome) in contents {
            match outcome {
                Ok(bytes) => entries.push(Entry::File(key, bytes)),
                Err(message) => diagnostics.push(Diagnostic::UnreadableEntry(message)),
            }
        }
        let tree = CodeTree::new(entries)?;
        Ok(SourceSnapshot { tree, diagnostics })
    }
}

impl SourceBackend for NativeSource {
    /// Two phases: one blocking pass walks the directory structure (never reading file
    /// contents), then the file reads fan out across workers. Entries, diagnostics, and file
    /// contents are all assembled in scan order, so the snapshot is deterministic at any
    /// parallelism.
    async fn snapshot(&self, exec: &Exec) -> Result<SourceSnapshot> {
        let source = self.clone();
        let mut scan = on_blocking_pool(move || source.scan_structure()).await?;
        let files = std::mem::take(&mut scan.files);
        let contents = exec
            .fan_out(files, |(key, path)| async move {
                let outcome =
                    fs::read(&path).map_err(|error| format!("{}: {error}", path.display()));
                (key, outcome)
            })
            .await;
        scan.assemble(contents)
    }

    /// The whole batch — precondition checks, mutations, and the undo journal — runs as one
    /// blocking task. The journal must stay strictly sequential, and a single closure can never
    /// be cancelled between a mutation and its undo record the way a per-change await could.
    async fn apply(&mut self, changes: Arc<[Change]>, base: &CodeTree, _exec: &Exec) -> Result<()> {
        let source = self.clone();
        let base = base.clone();
        on_blocking_pool(move || source.apply_sync(&changes, &base)).await
    }
}

impl NativeSource {
    /// Walk the project structure, recording directories and the host locations of in-scope
    /// files without reading their contents.
    fn scan_structure(&self) -> Result<ScanOutcome> {
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|error| NativeFs::io_error(&self.root, &error))?;
        let mut outcome = ScanOutcome {
            entries: vec![Entry::Directory(DirKey::ROOT)],
            files: Vec::new(),
            diagnostics: Vec::new(),
        };
        let mut stack = vec![canonical_root.clone()];
        let mut scan = NativeScan {
            canonical_root: &canonical_root,
            stack: &mut stack,
            entries: &mut outcome.entries,
            files: &mut outcome.files,
            diagnostics: &mut outcome.diagnostics,
            excluded: &self.excluded,
            scopes: &self.scopes,
            restricted: self.restricted,
        };
        NativeFs::scan_directory(&self.root, &DirKey::ROOT, &mut scan, true)?;
        Ok(outcome)
    }

    /// Complete synchronous snapshot, for use inside blocking sections (directory-removal
    /// preconditions compare a live subtree against the base while the batch holds the thread).
    fn snapshot_sync(&self) -> Result<SourceSnapshot> {
        let mut scan = self.scan_structure()?;
        let files = std::mem::take(&mut scan.files);
        let contents = files.into_iter().map(|(key, path)| {
            let outcome = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()));
            (key, outcome)
        });
        scan.assemble(contents)
    }

    fn apply_sync(&self, changes: &[Change], base: &CodeTree) -> Result<()> {
        let canonical_root =
            fs::canonicalize(&self.root).map_err(|error| NativeFs::io_error(&self.root, &error))?;
        self.require_preconditions(changes, base, &canonical_root)?;
        let mut journal = ApplyJournal::default();
        let mut expected = base.clone();
        for change in changes {
            if let Err(error) = self.apply_change(change, &expected, &canonical_root, &mut journal)
            {
                return Err(journal.roll_back(error));
            }
            expected.apply_changes(core::slice::from_ref(change))?;
        }
        journal.discard();
        Ok(())
    }
    /// Validate the complete write set before the first mutation. The per-change checks in
    /// `apply_change` run again immediately before publication to narrow the residual host-FS
    /// check/write window; this pass guarantees a known-stale later entry never causes an earlier
    /// entry to be written and rolled back.
    fn require_preconditions(
        &self,
        changes: &[Change],
        base: &CodeTree,
        canonical_root: &Path,
    ) -> Result<()> {
        let mut checked = BTreeSet::new();
        for change in changes {
            let path = match change {
                Change::CreateFile(key, _)
                | Change::ReplaceFile(key, _)
                | Change::RemoveFile(key) => key.path(),
                Change::CreateDirectory(key) | Change::RemoveDirectory(key) => key.path(),
            };
            if checked.insert(path.clone()) {
                match change {
                    Change::ReplaceFile(key, _) | Change::RemoveFile(key) => {
                        let host = self.confined_path(canonical_root, key.path())?;
                        Self::require_unchanged(base, key, &host)?;
                    }
                    Change::RemoveDirectory(key) => {
                        let host = self.confined_path(canonical_root, key.path())?;
                        Self::require_directory_unchanged(base, key, &host, &[])?;
                    }
                    Change::CreateFile(_, _) | Change::CreateDirectory(_) => {}
                }
            }
        }
        Ok(())
    }

    fn apply_change(
        &self,
        change: &Change,
        base: &CodeTree,
        canonical_root: &Path,
        journal: &mut ApplyJournal,
    ) -> Result<()> {
        match change {
            Change::CreateFile(key, bytes) => {
                let path = self.confined_path(canonical_root, key.path())?;
                let parent = path.parent().ok_or_else(|| NativeFs::no_parent(&path))?;
                journal.create_directories(parent)?;
                NativeFs::atomic_write(&path, bytes, WriteMode::Create)?;
                journal.record(Undo::RemoveCreatedFile(path));
            }
            Change::ReplaceFile(key, bytes) => {
                let path = self.confined_path(canonical_root, key.path())?;
                if !path.is_file() {
                    return Err(Error::NotFoundFile(key.clone()));
                }
                // The transaction observed this file as `base` at snapshot time; if the bytes on
                // disk no longer match, an external editor saved after the read. Refuse rather than
                // silently overwrite the newer content. (A residual TOCTOU window remains between
                // this check and the write, which only OS-level locking could close.)
                Self::require_unchanged(base, key, &path)?;
                journal.back_up_file(&path)?;
                NativeFs::atomic_write(&path, bytes, WriteMode::Replace)?;
            }
            Change::RemoveFile(key) => {
                let path = self.confined_path(canonical_root, key.path())?;
                Self::require_unchanged(base, key, &path)?;
                journal.remove_file(&path)?;
            }
            Change::CreateDirectory(key) => {
                let path = self.confined_path(canonical_root, key.path())?;
                journal.create_directories(&path)?;
            }
            Change::RemoveDirectory(key) => {
                let path = self.confined_path(canonical_root, key.path())?;
                let ignored: Vec<_> = journal.backup_paths().cloned().collect();
                Self::require_directory_unchanged(base, key, &path, &ignored)?;
                journal.remove_directory(&path)?;
            }
        }
        Ok(())
    }

    /// Refuse a replacement whose target's on-disk bytes no longer match the base snapshot the
    /// transaction was planned against. A key absent from `base` (e.g. a file this same batch
    /// created) carries no precondition and is left to the batch's own structural checks.
    fn require_unchanged(base: &CodeTree, key: &FileKey, path: &Path) -> Result<()> {
        let Some(expected) = base.file(key) else {
            return Ok(());
        };
        let actual = fs::read(path).map_err(|error| NativeFs::io_error(path, &error))?;
        if actual == expected.bytes() {
            Ok(())
        } else {
            Err(Error::ExternalConflict(key.clone()))
        }
    }

    fn require_directory_unchanged(
        base: &CodeTree,
        key: &DirKey,
        path: &Path,
        ignored: &[PathBuf],
    ) -> Result<()> {
        if base.directory(key).is_none() {
            return Ok(());
        }
        let mut source = Self::new(path.to_path_buf())?;
        for ignored in ignored {
            if let Some(relative) = RelativePath::from_host_path(path, ignored) {
                source = source.excluding(relative);
            }
        }
        let live = source.snapshot_sync()?.tree;
        let live_files: Vec<_> = live.files().collect();
        if base.files_under(key).count() != live_files.len()
            || live_files.iter().any(|file| {
                let original = FileKey::new(key.path().concat(file.key().path()))
                    .expect("a directory plus a file path is a file path");
                base.file(&original)
                    .is_none_or(|expected| expected.bytes() != file.bytes())
            })
        {
            return Err(Error::ExternalDirectoryConflict(key.clone()));
        }
        let expected_directories = base.directories_under(key).count();
        let live_directories = live.directories_under(&DirKey::ROOT).count();
        if expected_directories != live_directories {
            return Err(Error::ExternalDirectoryConflict(key.clone()));
        }
        Ok(())
    }

    /// The host location of `path` for a mutation. The staged logical parents may not all exist
    /// on disk yet, so only the deepest existing ancestor is canonicalized — and a prefix that
    /// resolves outside the project root (a parent that is an escaping symlink) is refused
    /// rather than written through.
    fn confined_path(&self, canonical_root: &Path, path: &RelativePath) -> Result<PathBuf> {
        let host = path.to_host_path(&self.root);
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

    fn backup_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.undo.iter().filter_map(|undo| match undo {
            Undo::RestoreFile { backup, .. } | Undo::RestoreDir { backup, .. } => Some(backup),
            Undo::RemoveCreatedFile(_) | Undo::RemoveCreatedDir(_) => None,
        })
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
        let parent = path.parent().ok_or_else(|| NativeFs::no_parent(path))?;
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
    /// In-scope files in scan order; contents are read after the walk (possibly fanned out).
    files: &'a mut Vec<(FileKey, PathBuf)>,
    diagnostics: &'a mut Vec<Diagnostic>,
    excluded: &'a [RelativePath],
    scopes: &'a [NativeScope],
    restricted: bool,
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
                Self::scan_symlink(&path, &logical_dir, &logical_file, scan)?;
            } else if file_type.is_dir() {
                if !Self::visits_directory(logical_dir.path(), scan.scopes, scan.restricted) {
                    continue;
                }
                scan.entries.push(Entry::Directory(logical_dir.clone()));
                let Some(canonical) = Self::canonicalize_for_scan(&path, scan) else {
                    continue;
                };
                scan.stack.push(canonical);
                Self::scan_directory(&path, &logical_dir, scan, false)?;
                scan.stack.pop();
            } else if file_type.is_file()
                && Self::includes_file(&logical_file, scan.scopes, scan.restricted)
            {
                scan.files.push((logical_file, path));
            }
        }
        Ok(())
    }

    /// Scan one symlinked entry: the link is followed only when its target stays inside the
    /// project root and off the current directory stack (an escaping or cyclic link is
    /// diagnosed, never followed), then treated like a plain directory or file.
    fn scan_symlink(
        path: &Path,
        logical_dir: &DirKey,
        logical_file: &FileKey,
        scan: &mut NativeScan<'_>,
    ) -> Result<()> {
        let Some(canonical) = Self::canonicalize_for_scan(path, scan) else {
            return Ok(());
        };
        if !canonical.starts_with(scan.canonical_root) {
            scan.diagnostics
                .push(Diagnostic::SymlinkEscapesRoot(logical_file.to_string()));
            return Ok(());
        }
        if scan.stack.contains(&canonical) {
            scan.diagnostics
                .push(Diagnostic::SymlinkCycle(logical_file.to_string()));
            return Ok(());
        }
        match fs::metadata(&canonical) {
            Ok(metadata) if metadata.is_dir() => {
                if !Self::visits_directory(logical_dir.path(), scan.scopes, scan.restricted) {
                    return Ok(());
                }
                scan.entries.push(Entry::Directory(logical_dir.clone()));
                scan.stack.push(canonical.clone());
                Self::scan_directory(&canonical, logical_dir, scan, false)?;
                scan.stack.pop();
            }
            Ok(metadata) if metadata.is_file() => {
                if Self::includes_file(logical_file, scan.scopes, scan.restricted) {
                    scan.files.push((logical_file.clone(), canonical));
                }
            }
            Ok(_) => {}
            Err(error) => scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                "{}: {error}",
                path.display()
            ))),
        }
        Ok(())
    }

    /// Canonicalize `path` for the scan, recording an unreadable-entry diagnostic and yielding
    /// `None` when the path cannot be resolved.
    fn canonicalize_for_scan(path: &Path, scan: &mut NativeScan<'_>) -> Option<PathBuf> {
        match fs::canonicalize(path) {
            Ok(canonical) => Some(canonical),
            Err(error) => {
                scan.diagnostics.push(Diagnostic::UnreadableEntry(format!(
                    "{}: {error}",
                    path.display()
                )));
                None
            }
        }
    }

    fn is_excluded(path: &RelativePath, excluded: &[RelativePath]) -> bool {
        excluded.iter().any(|prefix| path.starts_with(prefix))
    }

    fn visits_directory(path: &RelativePath, scopes: &[NativeScope], restricted: bool) -> bool {
        !restricted || scopes.iter().any(|scope| scope.visits_directory(path))
    }

    fn includes_file(key: &FileKey, scopes: &[NativeScope], restricted: bool) -> bool {
        !restricted || scopes.iter().any(|scope| scope.includes_file(key))
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
        let parent = path.parent().ok_or_else(|| Self::no_parent(path))?;
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

    fn cache_io_error(path: &Path, error: &std::io::Error) -> CacheError {
        CacheError::Io(format!("{}: {error}", path.display()))
    }

    fn no_parent(path: &Path) -> Error {
        Error::Io(format!("path has no parent: {}", path.display()))
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
    pub async fn materialize_source(
        &self,
        key: &CacheKey,
        logical: &RelativePath,
    ) -> core::result::Result<PathBuf, CacheError> {
        let bytes = self.lookup(key).await?.ok_or(CacheError::Corrupt)?;
        let base = self
            .backend()
            .root
            .join("source-view")
            .join(key.provenance().to_hex())
            .join(key.content().to_hex());
        let path = logical.to_host_path(&base);
        on_blocking_pool(move || {
            match fs::read(&path) {
                Ok(existing) if existing == bytes => return Ok(path),
                Ok(_) => return Err(CacheError::Corrupt),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(NativeFs::cache_io_error(&path, &error)),
            }
            NativeFs::create_once_accepting_identical(&path, &bytes)?;
            Ok(path)
        })
        .await
    }
}

/// Buffered, cloneable, positioned reader over one opened cache artifact.
///
/// Clones share the open file — content stays pinned to the opened inode, so a
/// post-verification path swap cannot redirect reads — but keep independent positions and
/// read-ahead buffers, which the parallel archive walkers require. `File::try_clone` is
/// unsuitable here: duplicated descriptors share one offset.
#[derive(Debug)]
pub struct NativeArtifactReader {
    file: Arc<ArtifactHandle>,
    /// Captured at open. Artifacts are write-once, so the length is stable and serves
    /// `SeekFrom::End` without a metadata call per seek.
    len: u64,
    pos: u64,
    buf: Box<[u8]>,
    buf_start: u64,
    buf_len: usize,
}

#[cfg(any(unix, windows))]
type ArtifactHandle = File;
/// Targets without positioned reads fall back to seek+read under a lock.
#[cfg(not(any(unix, windows)))]
type ArtifactHandle = std::sync::Mutex<File>;

impl NativeArtifactReader {
    fn new(file: File, len: u64) -> Self {
        #[cfg(any(unix, windows))]
        let handle = file;
        #[cfg(not(any(unix, windows)))]
        let handle = std::sync::Mutex::new(file);
        Self {
            file: Arc::new(handle),
            len,
            pos: 0,
            buf: vec![0; crate::io::BUFFER_CAPACITY].into_boxed_slice(),
            buf_start: 0,
            buf_len: 0,
        }
    }
}

impl Clone for NativeArtifactReader {
    fn clone(&self) -> Self {
        Self {
            file: Arc::clone(&self.file),
            len: self.len,
            pos: self.pos,
            // A fresh empty buffer: clones must never alias buffered state.
            buf: vec![0; self.buf.len()].into_boxed_slice(),
            buf_start: 0,
            buf_len: 0,
        }
    }
}

impl io::Read for NativeArtifactReader {
    async fn read(&mut self, buf: &mut [u8]) -> core::result::Result<usize, IoError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let buffered_end = self.buf_start + self.buf_len as u64;
        if self.pos >= self.buf_start && self.pos < buffered_end {
            let offset = usize::try_from(self.pos - self.buf_start)
                .expect("buffered window is bounded by the buffer length");
            let mut pending = &self.buf[offset..self.buf_len];
            let n = io::read_from_slice(&mut pending, buf);
            self.pos += n as u64;
            return Ok(n);
        }
        if tokio::runtime::Handle::try_current().is_err() {
            // No runtime (a fan-out worker thread, or the inline executor): positioned reads
            // block right here by design. Requests at least as large as the buffer go straight
            // into the caller, so the verification digest pass never double-copies.
            if buf.len() >= self.buf.len() {
                let n = Self::read_at(&self.file, buf, self.pos)?;
                self.pos += n as u64;
                return Ok(n);
            }
            self.buf_start = self.pos;
            self.buf_len = 0;
            self.buf_len = Self::read_at(&self.file, &mut self.buf, self.pos)?;
        } else {
            // On the runtime the syscall moves to the blocking pool, taking the read-ahead
            // buffer with it (the caller's borrowed buffer cannot cross; a short read into the
            // owned buffer is within contract). A cancelled await leaves the buffer empty; the
            // next read reallocates it on the pool — never corruption, never a lost position.
            let file = Arc::clone(&self.file);
            let pos = self.pos;
            self.buf_len = 0;
            let mut scratch = core::mem::take(&mut self.buf);
            let (scratch, outcome) = on_blocking_pool(move || {
                if scratch.is_empty() {
                    scratch = alloc::vec![0; crate::io::BUFFER_CAPACITY].into_boxed_slice();
                }
                let outcome = Self::read_at(&file, &mut scratch, pos);
                (scratch, outcome)
            })
            .await;
            self.buf = scratch;
            self.buf_start = self.pos;
            self.buf_len = 0;
            self.buf_len = outcome?;
        }
        if self.buf_len == 0 {
            return Ok(0);
        }
        let n = self.buf_len.min(buf.len());
        buf[..n].copy_from_slice(&self.buf[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl io::Seek for NativeArtifactReader {
    async fn seek(&mut self, pos: SeekFrom) -> core::result::Result<u64, IoError> {
        self.pos = pos.resolve(self.len, self.pos)?;
        Ok(self.pos)
    }
}

impl NativeArtifactReader {
    #[cfg(unix)]
    fn read_at(
        file: &ArtifactHandle,
        buf: &mut [u8],
        offset: u64,
    ) -> core::result::Result<usize, IoError> {
        use std::os::unix::fs::FileExt;
        loop {
            match file.read_at(buf, offset) {
                Ok(n) => return Ok(n),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(IoError::Failed(error.to_string())),
            }
        }
    }

    #[cfg(windows)]
    fn read_at(
        file: &ArtifactHandle,
        buf: &mut [u8],
        offset: u64,
    ) -> core::result::Result<usize, IoError> {
        // `seek_read` moves the descriptor's file pointer, but every clone reads exclusively
        // through explicit offsets, so the shared pointer is never observed.
        use std::os::windows::fs::FileExt;
        file.seek_read(buf, offset)
            .map_err(|error| IoError::Failed(error.to_string()))
    }

    #[cfg(not(any(unix, windows)))]
    fn read_at(
        file: &ArtifactHandle,
        buf: &mut [u8],
        offset: u64,
    ) -> core::result::Result<usize, IoError> {
        use std::io::{Read, Seek};
        let mut file = file
            .lock()
            .map_err(|_| IoError::Failed("poisoned artifact reader lock".to_string()))?;
        file.seek(std::io::SeekFrom::Start(offset))
            .and_then(|_| file.read(buf))
            .map_err(|error| IoError::Failed(error.to_string()))
    }
}

impl cache::private::Sealed for NativeCache {}

impl CacheBackend for NativeCache {
    type Reader = NativeArtifactReader;

    async fn open(&self, key: &CacheKey) -> core::result::Result<Option<Self::Reader>, CacheError> {
        let path = self.artifact_path(key);
        on_blocking_pool(move || {
            let file = match File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(NativeFs::cache_io_error(&path, &error)),
            };
            let len = file
                .metadata()
                .map_err(|error| NativeFs::cache_io_error(&path, &error))?
                .len();
            Ok(Some(NativeArtifactReader::new(file, len)))
        })
        .await
    }

    async fn load(&self, key: &CacheKey) -> core::result::Result<Option<Vec<u8>>, CacheError> {
        let path = self.artifact_path(key);
        on_blocking_pool(move || match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeFs::cache_io_error(&path, &error)),
        })
        .await
    }

    async fn publish_once(
        &mut self,
        key: &CacheKey,
        bytes: &[u8],
    ) -> core::result::Result<(), CacheError> {
        let path = self.artifact_path(key);
        let bytes = bytes.to_vec();
        on_blocking_pool(move || NativeFs::create_once_accepting_identical(&path, &bytes)).await
    }

    async fn load_index(
        &self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
    ) -> core::result::Result<Option<ContentDigest>, CacheError> {
        let path = self.index_path(namespace, provenance);
        on_blocking_pool(move || match fs::read_to_string(&path) {
            Ok(text) => ContentDigest::from_hex(text.trim())
                .map(Some)
                .ok_or(CacheError::Corrupt),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(NativeFs::cache_io_error(&path, &error)),
        })
        .await
    }

    async fn store_index(
        &mut self,
        namespace: CacheNamespace,
        provenance: &ContentDigest,
        content: &ContentDigest,
    ) -> core::result::Result<(), CacheError> {
        let path = self.index_path(namespace, provenance);
        let rendered = content.to_hex();
        // `Replace` semantics: the pointer is last-writer-wins by design (see
        // `ArtifactCache::record_index`), unlike write-once artifact publication.
        on_blocking_pool(move || {
            NativeFs::atomic_write(&path, rendered.as_bytes(), WriteMode::Replace)
                .map_err(|error| CacheError::Io(error.to_string()))
        })
        .await
    }
}

impl ProjectStorage<NativeSource, NativeCache> {
    /// The conventional per-project cache location, shared by every host.
    pub const PROJECT_CACHE_DIR: &'static str = "target/jals/cache";

    /// Open a native project snapshot restricted to declared inputs. Directory scopes continue to
    /// observe matching files added after opening while never reading unrelated file contents.
    pub async fn for_project_scoped(
        root: impl AsRef<Path>,
        scopes: impl IntoIterator<Item = NativeScope>,
        exec: Exec,
    ) -> Result<Self> {
        let root = root.as_ref();
        let cache_root = root.join(Self::PROJECT_CACHE_DIR);
        let source = Self::source_excluding_cache(root, &cache_root)?
            .excluding(RelativePath::parse(".git").expect(".git is a portable path"))
            .scoped(scopes);
        Self::open(source, NativeCache::new(cache_root), exec).await
    }

    pub async fn native(
        root: impl AsRef<Path>,
        cache_root: impl AsRef<Path>,
        exec: Exec,
    ) -> Result<Self> {
        let root = root.as_ref();
        let cache_root = cache_root.as_ref().to_path_buf();
        let source = Self::source_excluding_cache(root, &cache_root)?;
        Self::open(source, NativeCache::new(cache_root), exec).await
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

    /// Drives a test body on the tokio bootstrap so the `spawn_blocking` paths are exercised
    /// exactly as hosts run them.
    fn run<T, Fut: core::future::Future<Output = T>>(f: impl FnOnce(Exec) -> Fut) -> T {
        jals_exec::tokio_rt::run(f).expect("test runtime bootstraps")
    }

    #[test]
    fn native_snapshot_changes_only_after_refresh() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("A.java"), b"one").unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
            let old = storage.view();
            fs::write(dir.path().join("A.java"), b"two").unwrap();
            assert_eq!(
                old.file(&FileKey::parse("A.java").unwrap())
                    .unwrap()
                    .bytes(),
                b"one"
            );
            storage.refresh().await.unwrap();
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
        });
    }

    #[test]
    fn native_snapshot_scopes_file_contents_by_root_and_extension() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("src/nested")).unwrap();
            fs::create_dir_all(dir.path().join("target")).unwrap();
            fs::write(dir.path().join("src/A.java"), b"class A {}").unwrap();
            fs::write(dir.path().join("src/blob.bin"), vec![0; 1024]).unwrap();
            fs::write(dir.path().join("target/unrelated.bin"), vec![0; 1024]).unwrap();
            let source = NativeSource::new(dir.path().to_path_buf())
                .unwrap()
                .scoped([NativeScope::extension(
                    RelativePath::parse("src").unwrap(),
                    "java",
                )]);
            let mut storage = ProjectStorage::open(source, crate::MemoryCache::default(), exec)
                .await
                .unwrap();

            assert!(
                storage
                    .view()
                    .tree()
                    .file(&FileKey::parse("src/A.java").unwrap())
                    .is_some()
            );
            assert!(
                storage
                    .view()
                    .tree()
                    .file(&FileKey::parse("src/blob.bin").unwrap())
                    .is_none()
            );
            assert!(
                storage
                    .view()
                    .tree()
                    .file(&FileKey::parse("target/unrelated.bin").unwrap())
                    .is_none()
            );

            fs::write(dir.path().join("src/nested/B.java"), b"class B {}").unwrap();
            fs::write(dir.path().join("src/nested/ignored.txt"), b"ignored").unwrap();
            storage.refresh().await.unwrap();
            assert!(
                storage
                    .view()
                    .tree()
                    .file(&FileKey::parse("src/nested/B.java").unwrap())
                    .is_some()
            );
            assert!(
                storage
                    .view()
                    .tree()
                    .file(&FileKey::parse("src/nested/ignored.txt").unwrap())
                    .is_none()
            );
        });
    }

    #[test]
    fn explicitly_empty_native_scope_reads_no_project_files() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("unrelated.bin"), b"bytes").unwrap();
            let storage = ProjectStorage::for_project_scoped(
                dir.path(),
                core::iter::empty::<NativeScope>(),
                exec,
            )
            .await
            .unwrap();
            assert!(storage.view().tree().files().next().is_none());
        });
    }

    #[test]
    fn native_cache_detects_tampering() {
        run(|_exec| async move {
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
            cache.publish(&key, bytes).await.unwrap();
            fs::write(stored, b"tampered").unwrap();
            assert_eq!(cache.lookup(&key).await, Err(CacheError::Corrupt));
        });
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
                    // Runtime-less threads drive the publish inline: exactly the fan-out
                    // worker execution mode.
                    jals_exec::block_on_inline(async move {
                        let mut cache = ArtifactCache::new(NativeCache::new(root));
                        cache.publish(&key, b"jar").await
                    })
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), Ok(()));
        }
        let cache = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
        assert_eq!(
            jals_exec::block_on_inline(cache.lookup(&key)).unwrap(),
            Some(b"jar".to_vec())
        );
    }

    #[test]
    fn native_cache_root_is_not_project_source() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("A.java"), b"class A {}").unwrap();
            let cache_root = dir.path().join(".cache");
            let mut storage = ProjectStorage::native(dir.path(), cache_root.clone(), exec)
                .await
                .unwrap();
            let key = CacheKey::new(
                CacheNamespace::DependencyJar,
                ContentDigest::of(b"source"),
                ContentDigest::of(b"jar"),
            );
            storage.artifacts_mut().publish(&key, b"jar").await.unwrap();
            storage.refresh().await.unwrap();
            assert!(
                storage
                    .view()
                    .tree()
                    .directory(&DirKey::parse(".cache").unwrap())
                    .is_none()
            );
            assert!(cache_root.is_dir());
        });
    }

    #[test]
    fn native_source_accepts_an_empty_root_as_current_directory() {
        let source = NativeSource::new(PathBuf::new()).unwrap();
        assert_eq!(source.root(), Path::new("."));
    }

    #[test]
    fn native_transaction_failure_rolls_back_earlier_changes() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("A.java"), b"old").unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
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
            assert!(transaction.commit().await.is_err());

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
        });
    }

    #[test]
    fn native_transaction_rolls_back_removals() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("pkg")).unwrap();
            fs::write(dir.path().join("pkg/Kept.java"), b"kept").unwrap();
            fs::write(dir.path().join("Gone.java"), b"gone").unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
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
            assert!(transaction.commit().await.is_err());

            assert_eq!(fs::read(dir.path().join("Gone.java")).unwrap(), b"gone");
            assert_eq!(fs::read(dir.path().join("pkg/Kept.java")).unwrap(), b"kept");
            assert_eq!(fs::read(dir.path().join("New.java")).unwrap(), b"external");
            assert_eq!(storage.revision(), revision);
        });
    }

    #[test]
    fn native_replace_refuses_to_clobber_concurrent_external_edit() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("A.java"), b"old").unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
            let revision = storage.revision();
            let mut transaction = storage.transaction(revision).unwrap();
            transaction
                .replace_file(FileKey::parse("A.java").unwrap(), b"formatted".to_vec())
                .unwrap();
            // Another editor saves A.java after it was snapshotted/read but before the commit.
            fs::write(dir.path().join("A.java"), b"external-edit").unwrap();
            assert!(matches!(
                transaction.commit().await,
                Err(Error::ExternalConflict(_))
            ));
            // The concurrent edit survives untouched and the revision never advanced.
            assert_eq!(
                fs::read(dir.path().join("A.java")).unwrap(),
                b"external-edit"
            );
            assert_eq!(storage.revision(), revision);
        });
    }

    #[test]
    fn native_batch_checks_every_target_before_writing_any_file() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("A.java"), b"a-old").unwrap();
            fs::write(dir.path().join("B.java"), b"b-old").unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
            let mut transaction = storage.transaction(storage.revision()).unwrap();
            transaction
                .replace_file(FileKey::parse("A.java").unwrap(), b"a-new".to_vec())
                .unwrap()
                .replace_file(FileKey::parse("B.java").unwrap(), b"b-new".to_vec())
                .unwrap();
            fs::write(dir.path().join("B.java"), b"external").unwrap();

            assert!(matches!(
                transaction.commit().await,
                Err(Error::ExternalConflict(_))
            ));
            assert_eq!(fs::read(dir.path().join("A.java")).unwrap(), b"a-old");
            assert_eq!(fs::read(dir.path().join("B.java")).unwrap(), b"external");
        });
    }

    #[test]
    fn native_removals_refuse_external_file_and_directory_changes() {
        run(|exec| async move {
            let file_dir = tempfile::tempdir().unwrap();
            fs::write(file_dir.path().join("A.java"), b"old").unwrap();
            let mut file_storage = ProjectStorage::native(
                file_dir.path(),
                file_dir.path().join(".cache"),
                exec.clone(),
            )
            .await
            .unwrap();
            let mut file_transaction = file_storage.transaction(file_storage.revision()).unwrap();
            file_transaction
                .remove_file(FileKey::parse("A.java").unwrap())
                .unwrap();
            fs::write(file_dir.path().join("A.java"), b"external").unwrap();
            assert!(matches!(
                file_transaction.commit().await,
                Err(Error::ExternalConflict(_))
            ));
            assert_eq!(
                fs::read(file_dir.path().join("A.java")).unwrap(),
                b"external"
            );

            let tree_dir = tempfile::tempdir().unwrap();
            fs::create_dir(tree_dir.path().join("pkg")).unwrap();
            fs::write(tree_dir.path().join("pkg/A.java"), b"old").unwrap();
            let mut tree_storage =
                ProjectStorage::native(tree_dir.path(), tree_dir.path().join(".cache"), exec)
                    .await
                    .unwrap();
            let mut tree_transaction = tree_storage.transaction(tree_storage.revision()).unwrap();
            tree_transaction
                .remove_directory(DirKey::parse("pkg").unwrap())
                .unwrap();
            fs::write(tree_dir.path().join("pkg/B.java"), b"external").unwrap();
            assert!(matches!(
                tree_transaction.commit().await,
                Err(Error::ExternalDirectoryConflict(_))
            ));
            assert!(tree_dir.path().join("pkg/A.java").is_file());
            assert!(tree_dir.path().join("pkg/B.java").is_file());
        });
    }

    #[test]
    fn native_preflight_compares_a_later_directory_removal_to_the_original_base() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("pkg")).unwrap();
            fs::write(dir.path().join("pkg/A.java"), b"old").unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
            let mut transaction = storage.transaction(storage.revision()).unwrap();
            transaction
                .replace_file(FileKey::parse("pkg/A.java").unwrap(), b"new".to_vec())
                .unwrap()
                .remove_directory(DirKey::parse("pkg").unwrap())
                .unwrap();

            transaction.commit().await.unwrap();
            assert!(!dir.path().join("pkg").exists());
        });
    }

    #[test]
    fn native_transaction_creates_directory_parents() {
        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
            let revision = storage.revision();
            let mut transaction = storage.transaction(revision).unwrap();
            transaction
                .create_directory(DirKey::parse("a/b").unwrap())
                .unwrap();
            transaction.commit().await.unwrap();
            assert!(dir.path().join("a/b").is_dir());
            assert!(
                storage
                    .view()
                    .tree()
                    .directory(&DirKey::parse("a/b").unwrap())
                    .is_some()
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn native_transaction_refuses_writes_through_escaping_symlinks() {
        use std::os::unix::fs::symlink;

        run(|exec| async move {
            let dir = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            symlink(outside.path(), dir.path().join("out")).unwrap();
            let mut storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
            let revision = storage.revision();
            let mut transaction = storage.transaction(revision).unwrap();
            // The escaping symlink was diagnosed out of the snapshot, so the logical parent does
            // not exist and staging reconstructs it — the physical write must still be refused.
            transaction
                .create_file(FileKey::parse("out/X.java").unwrap(), b"x".to_vec())
                .unwrap();
            assert!(transaction.commit().await.is_err());
            assert!(!outside.path().join("X.java").exists());
            assert_eq!(storage.revision(), revision);
        });
    }

    #[test]
    fn native_cache_index_round_trips_and_survives_reopen() {
        run(|_exec| async move {
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
                    .await
                    .unwrap(),
                None
            );
            cache.publish(&key, b"jar").await.unwrap();
            cache.record_index(&key).await.unwrap();

            let reopened = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
            let recovered = reopened
                .indexed_key(CacheNamespace::DependencyJar, ContentDigest::of(b"locator"))
                .await
                .unwrap()
                .expect("index survives reopen");
            assert_eq!(recovered, key);
            assert_eq!(
                reopened.lookup(&recovered).await.unwrap(),
                Some(b"jar".to_vec())
            );

            // The pointer is last-writer-wins: recording a newer content replaces it.
            let newer = CacheKey::new(
                CacheNamespace::DependencyJar,
                ContentDigest::of(b"locator"),
                ContentDigest::of(b"jar-v2"),
            );
            let mut reopened = reopened;
            reopened.publish(&newer, b"jar-v2").await.unwrap();
            reopened.record_index(&newer).await.unwrap();
            assert_eq!(
                reopened
                    .indexed_key(CacheNamespace::DependencyJar, ContentDigest::of(b"locator"))
                    .await
                    .unwrap(),
                Some(newer)
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn native_scan_diagnoses_symlink_escape_cycle_and_non_utf8() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        run(|exec| async move {
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

            let storage = ProjectStorage::native(dir.path(), dir.path().join(".cache"), exec)
                .await
                .unwrap();
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
        });
    }

    fn artifact_key(bytes: &[u8]) -> CacheKey {
        CacheKey::new(
            CacheNamespace::DependencyJar,
            ContentDigest::of(b"origin"),
            ContentDigest::of(bytes),
        )
    }

    #[test]
    fn native_open_verified_streams_a_multi_buffer_artifact() {
        run(|_exec| async move {
            let dir = tempfile::tempdir().unwrap();
            let mut cache = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
            let bytes: Vec<u8> = (0..200 * 1024)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect();
            let key = artifact_key(&bytes);
            cache.publish(&key, &bytes).await.unwrap();

            let mut reader = cache.open_verified(&key).await.unwrap().unwrap();
            let mut out = Vec::new();
            let mut chunk = [0u8; 1000];
            loop {
                match io::Read::read(&mut reader, &mut chunk).await.unwrap() {
                    0 => break,
                    n => out.extend_from_slice(&chunk[..n]),
                }
            }
            assert_eq!(out, bytes);
            assert!(
                cache
                    .open_verified(&artifact_key(b"missing"))
                    .await
                    .unwrap()
                    .is_none()
            );
        });
    }

    #[test]
    fn native_open_verified_rejects_on_disk_tampering() {
        run(|_exec| async move {
            let dir = tempfile::tempdir().unwrap();
            let mut cache = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
            let key = artifact_key(b"artifact");
            cache.publish(&key, b"artifact").await.unwrap();
            fs::write(cache.backend().artifact_path(&key), b"tampered").unwrap();
            assert!(matches!(
                cache.open_verified(&key).await,
                Err(CacheError::Corrupt)
            ));
        });
    }

    #[test]
    fn native_reader_clones_keep_independent_positions() {
        run(|_exec| async move {
            let dir = tempfile::tempdir().unwrap();
            let mut cache = ArtifactCache::new(NativeCache::new(dir.path().to_path_buf()));
            let bytes: Vec<u8> = (0..100_000)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect();
            let key = artifact_key(&bytes);
            cache.publish(&key, &bytes).await.unwrap();

            let mut first = cache.open_verified(&key).await.unwrap().unwrap();
            let mut second = first.clone();
            io::Seek::seek(&mut second, SeekFrom::Start(50_000))
                .await
                .unwrap();
            let mut a = [0u8; 16];
            let mut b = [0u8; 16];
            io::Read::read_exact(&mut first, &mut a).await.unwrap();
            io::Read::read_exact(&mut second, &mut b).await.unwrap();
            assert_eq!(a[..], bytes[..16]);
            assert_eq!(b[..], bytes[50_000..50_016]);
            io::Read::read_exact(&mut first, &mut a).await.unwrap();
            assert_eq!(a[..], bytes[16..32]);
            assert_eq!(
                io::Seek::seek(&mut first, SeekFrom::End(-4)).await.unwrap(),
                bytes.len() as u64 - 4
            );
            io::Read::read_exact(&mut first, &mut a[..4]).await.unwrap();
            assert_eq!(a[..4], bytes[bytes.len() - 4..]);
        });
    }
}
