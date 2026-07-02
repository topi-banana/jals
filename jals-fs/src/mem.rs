//! [`InMemoryFileTree`]: the entire file tree held in memory, pure `core`/`alloc` (`no_std`).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{FsError, Result};
use crate::{FileTree, path};

/// A file tree built entirely in memory.
///
/// Files are stored in a `BTreeMap` keyed by normalized full path; directories are *implied* by
/// being a `/`-terminated prefix of some file key (they are never stored explicitly). The `BTree`
/// ordering gives deterministic, lexicographically-sorted [`read_dir`](FileTree::read_dir) /
/// [`walk_ext`](FileTree::walk_ext) for free.
///
/// ```
/// use jals_fs::{FileTree, InMemoryFileTree};
/// let fs = InMemoryFileTree::new()
///     .with_file("/proj/jalsfmt.toml", "indent-width = 2")
///     .with_file("/proj/src/A.java", "class A {}");
/// assert!(fs.is_dir("/proj/src"));
/// assert_eq!(fs.walk_ext("/proj", "java"), Ok(vec!["/proj/src/A.java".into()]));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InMemoryFileTree {
    files: BTreeMap<String, Vec<u8>>,
}

impl InMemoryFileTree {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert of a file; returns `self` for chaining.
    #[must_use]
    pub fn with_file(mut self, path: &str, contents: impl AsRef<[u8]>) -> Self {
        self.insert(path, contents);
        self
    }

    /// Insert (or overwrite) the file at `path`.
    pub fn insert(&mut self, path: &str, contents: impl AsRef<[u8]>) {
        self.files
            .insert(normalize(path), contents.as_ref().to_vec());
    }

    /// Build a tree from an iterator of `(path, contents)` pairs.
    pub fn from_files<I, P, C>(files: I) -> Self
    where
        I: IntoIterator<Item = (P, C)>,
        P: AsRef<str>,
        C: AsRef<[u8]>,
    {
        let mut tree = Self::new();
        for (path, contents) in files {
            tree.insert(path.as_ref(), contents);
        }
        tree
    }
}

impl FileTree for InMemoryFileTree {
    fn read_to_string(&self, path: &str) -> Result<String> {
        let key = normalize(path);
        match self.files.get(&key) {
            Some(bytes) => String::from_utf8(bytes.clone()).map_err(|_| FsError::InvalidUtf8(key)),
            None => Err(FsError::NotFound(key)),
        }
    }

    fn read(&self, path: &str) -> Result<Vec<u8>> {
        let key = normalize(path);
        self.files.get(&key).cloned().ok_or(FsError::NotFound(key))
    }

    fn is_file(&self, path: &str) -> bool {
        self.files.contains_key(&normalize(path))
    }

    fn is_dir(&self, path: &str) -> bool {
        let key = normalize(path);
        if is_root(&key) {
            return true;
        }
        let prefix = dir_prefix(&key);
        self.files.keys().any(|k| k.starts_with(&prefix))
    }

    fn read_dir(&self, path: &str) -> Result<Vec<String>> {
        let key = normalize(path);
        let prefix = dir_prefix(&key);
        let mut children = BTreeSet::new();
        for k in self.files.keys() {
            let Some(rest) = k.strip_prefix(&prefix) else {
                continue;
            };
            let Some(segment) = rest.split('/').next().filter(|s| !s.is_empty()) else {
                continue;
            };
            let mut child = prefix.clone();
            child.push_str(segment);
            children.insert(child);
        }
        // A directory is implied by the files under it, so a non-root path with no children under
        // its prefix is not a directory (the same single pass that collects children detects this,
        // with no separate `is_dir` scan). The root always lists, possibly empty.
        if children.is_empty() && !is_root(&key) {
            return Err(FsError::NotADirectory(key));
        }
        Ok(children.into_iter().collect())
    }

    fn walk_ext(&self, root: &str, ext: &str) -> Result<Vec<String>> {
        let prefix = dir_prefix(&normalize(root));
        let mut out = Vec::new();
        for k in self.files.keys() {
            if k.starts_with(&prefix) && path::extension(k) == Some(ext) {
                out.push(k.clone());
            }
        }
        // `BTreeMap` iteration is already lexicographically sorted.
        Ok(out)
    }

    fn write(&mut self, path: &str, contents: &[u8]) -> Result<()> {
        self.files.insert(normalize(path), contents.to_vec());
        Ok(())
    }
}

/// Whether `key` denotes the root (`""` for a relative tree, `"/"` for an absolute one).
fn is_root(key: &str) -> bool {
    key.is_empty() || key == "/"
}

/// The prefix a key must start with to be *inside* the directory `key`.
///
/// Root (`""`/`"/"`) yields itself, so every key of the matching (relative / absolute) form is
/// considered under it; any other directory yields `key + "/"`.
fn dir_prefix(key: &str) -> String {
    if is_root(key) {
        return String::from(key);
    }
    let mut prefix = String::from(key);
    prefix.push('/');
    prefix
}

/// Canonicalize a path key: collapse runs of `/`, drop a trailing `/` (except the root), keep a
/// leading `/`.
fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
            }
            prev_slash = true;
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> InMemoryFileTree {
        InMemoryFileTree::from_files([
            ("/proj/jalsfmt.toml", "indent-width = 2"),
            ("/proj/src/A.java", "class A {}"),
            ("/proj/src/sub/B.java", "class B {}"),
            ("/proj/src/notes.txt", "hi"),
        ])
    }

    #[test]
    fn reads_and_predicates() {
        let fs = sample();
        assert_eq!(fs.read_to_string("/proj/src/A.java").unwrap(), "class A {}");
        assert_eq!(fs.read("/proj/src/A.java").unwrap(), b"class A {}");
        assert!(fs.is_file("/proj/src/A.java"));
        assert!(!fs.is_file("/proj/src"));
        assert!(fs.is_dir("/proj/src"));
        assert!(fs.is_dir("/proj/src/sub"));
        assert!(!fs.is_dir("/proj/src/A.java"));
        // A trailing slash is ignored.
        assert!(fs.is_dir("/proj/src/"));
    }

    #[test]
    fn read_dir_lists_sorted_full_paths_incl_subdirs() {
        let fs = sample();
        assert_eq!(
            fs.read_dir("/proj/src").unwrap(),
            vec![
                "/proj/src/A.java".to_string(),
                "/proj/src/notes.txt".to_string(),
                "/proj/src/sub".to_string(),
            ]
        );
        assert_eq!(
            fs.read_dir("/proj").unwrap(),
            vec!["/proj/jalsfmt.toml".to_string(), "/proj/src".to_string()]
        );
    }

    #[test]
    fn read_dir_on_file_is_not_a_directory() {
        let fs = sample();
        assert_eq!(
            fs.read_dir("/proj/src/A.java"),
            Err(FsError::NotADirectory("/proj/src/A.java".into()))
        );
    }

    #[test]
    fn walk_ext_recurses_sorted() {
        let fs = sample();
        assert_eq!(
            fs.walk_ext("/proj", "java").unwrap(),
            vec![
                "/proj/src/A.java".to_string(),
                "/proj/src/sub/B.java".to_string()
            ]
        );
        assert!(fs.walk_ext("/proj", "class").unwrap().is_empty());
    }

    #[test]
    fn write_inserts_and_overwrites() {
        let mut fs = sample();
        fs.write("/proj/src/C.java", b"class C {}").unwrap();
        assert_eq!(fs.read_to_string("/proj/src/C.java").unwrap(), "class C {}");
        fs.write("/proj/src/C.java", b"class C2 {}").unwrap();
        assert_eq!(
            fs.read_to_string("/proj/src/C.java").unwrap(),
            "class C2 {}"
        );
    }

    #[test]
    fn error_arms() {
        let mut fs = sample();
        assert_eq!(
            fs.read_to_string("/nope"),
            Err(FsError::NotFound("/nope".into()))
        );
        fs.insert("/bin", [0xff, 0xfe, 0x00]);
        assert_eq!(
            fs.read_to_string("/bin"),
            Err(FsError::InvalidUtf8("/bin".into()))
        );
    }
}
