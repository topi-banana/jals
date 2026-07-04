//! [`OsFileTree`]: the [`FileTree`] backed by synchronous `std::fs` I/O. Host-only (`std`-gated).

use std::path::Path;

use crate::FileTree;
use crate::error::{FsError, Result};

/// A stateless handle to the host filesystem.
///
/// A virtual path *is* the real OS path string (`/`-separated on Unix), converted with
/// `Path::new`. A non-UTF-8 filesystem entry surfaced by a directory walk is skipped (matching the
/// host tools' existing "skip unreadable" tolerance), since [`FileTree`] speaks UTF-8 `&str`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsFileTree;

impl OsFileTree {
    /// Create a new handle.
    pub fn new() -> Self {
        OsFileTree
    }
}

impl FileTree for OsFileTree {
    fn read_to_string(&self, path: &str) -> Result<String> {
        match std::fs::read(Path::new(path)) {
            Ok(bytes) => {
                String::from_utf8(bytes).map_err(|_| FsError::InvalidUtf8(path.to_string()))
            }
            Err(err) => Err(map_io(path, &err)),
        }
    }

    fn read(&self, path: &str) -> Result<Vec<u8>> {
        std::fs::read(Path::new(path)).map_err(|err| map_io(path, &err))
    }

    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }

    fn is_dir(&self, path: &str) -> bool {
        Path::new(path).is_dir()
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>> {
        let dir = Path::new(path);
        // A path that exists but isn't a directory is `NotADirectory`; anything else (missing path,
        // permission error, …) is what `map_io` already renders — including its `NotFound` case.
        let entries = std::fs::read_dir(dir).map_err(|err| {
            if dir.exists() {
                FsError::NotADirectory(path.to_string())
            } else {
                map_io(path, &err)
            }
        })?;
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if let Some(child) = entry.path().to_str() {
                out.push(child.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    fn walk_ext(&self, root: &str, ext: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        collect_ext(Path::new(root), ext, &mut out);
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn write(&mut self, path: &str, contents: &[u8]) -> Result<()> {
        let p = Path::new(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|err| map_io(path, &err))?;
        }
        // Atomic create-or-replace: write to a `.part` sibling in the same directory, then rename it
        // into place. `rename` is atomic on a single filesystem, so a reader never observes a
        // partially-written file, and an interrupted write leaves only the `.part` (whose extension
        // is `part`, so `walk_ext` never surfaces it) — "the file exists" therefore implies it is
        // complete, which callers rely on for skip-if-exists caching. `InMemoryFileTree::write` is
        // already atomic (a single map insert); this brings the OS impl to the same contract.
        let mut tmp = String::with_capacity(path.len() + 5);
        tmp.push_str(path);
        tmp.push_str(".part");
        std::fs::write(Path::new(&tmp), contents).map_err(|err| map_io(path, &err))?;
        std::fs::rename(Path::new(&tmp), p).map_err(|err| map_io(path, &err))
    }
}

/// Render a `std::io::Error` into an [`FsError`], preserving the not-found case.
fn map_io(path: &str, err: &std::io::Error) -> FsError {
    match err.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound(path.to_string()),
        _ => FsError::Io(format!("{path}: {err}")),
    }
}

/// Recursively collect files with extension `ext` under `dir`. Hand-rolled (no `walkdir`) recursion
/// mirroring `jals-cli`'s `collect_dir`; symlinked directories are not followed (via `file_type`),
/// avoiding cycles. Unreadable directories are silently skipped.
fn collect_ext(dir: &Path, ext: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_ext(&path, ext, out);
        } else if file_type.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some(ext)
            && let Some(child) = path.to_str()
        {
            out.push(child.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileTree;

    /// The logical tree used by both the OS and in-memory sides of the parity test.
    const TREE: &[(&str, &str)] = &[
        ("jalsfmt.toml", "indent-width = 2"),
        ("src/A.java", "class A {}"),
        ("src/sub/B.java", "class B {}"),
        ("src/notes.txt", "hi"),
    ];

    fn materialize_os(root: &std::path::Path) {
        let mut fs = OsFileTree;
        for (rel, contents) in TREE {
            let abs = root.join(rel);
            fs.write(abs.to_str().unwrap(), contents.as_bytes())
                .unwrap();
        }
    }

    fn materialize_mem(root: &str) -> InMemoryFileTree {
        InMemoryFileTree::from_files(
            TREE.iter()
                .map(|(rel, contents)| (crate::path::join(root, rel), *contents)),
        )
    }

    #[test]
    fn os_basics() {
        let dir = tempfile::tempdir().unwrap();
        materialize_os(dir.path());
        let fs = OsFileTree;
        let a = dir.path().join("src/A.java");
        assert_eq!(
            fs.read_to_string(a.to_str().unwrap()).unwrap(),
            "class A {}"
        );
        assert!(fs.is_file(a.to_str().unwrap()));
        assert!(fs.is_dir(dir.path().join("src").to_str().unwrap()));
        assert_eq!(
            fs.read_to_string("/definitely/not/here").unwrap_err(),
            FsError::NotFound("/definitely/not/here".into())
        );
    }

    #[test]
    fn os_and_memory_agree() {
        let dir = tempfile::tempdir().unwrap();
        materialize_os(dir.path());
        let root = dir.path().to_str().unwrap();

        let os = OsFileTree;
        let mem = materialize_mem(root);

        // read_to_string parity for every file.
        for (rel, _) in TREE {
            let abs = crate::path::join(root, rel);
            assert_eq!(
                os.read_to_string(&abs),
                mem.read_to_string(&abs),
                "read {abs}"
            );
        }

        // predicate parity.
        let src = crate::path::join(root, "src");
        assert_eq!(os.is_dir(&src), mem.is_dir(&src));
        assert_eq!(os.is_file(&src), mem.is_file(&src));
        let a = crate::path::join(root, "src/A.java");
        assert_eq!(os.is_file(&a), mem.is_file(&a));

        // read_dir + walk_ext parity (both sorted, full virtual paths).
        assert_eq!(os.read_dir(&src).unwrap(), mem.read_dir(&src).unwrap());
        assert_eq!(
            os.walk_ext(root, "java").unwrap(),
            mem.walk_ext(root, "java").unwrap()
        );
    }
}
