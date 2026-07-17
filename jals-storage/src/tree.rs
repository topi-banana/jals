use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Bound;

use crate::error::TreeError;
use crate::{DirKey, FileKey};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFile {
    key: FileKey,
    bytes: Arc<[u8]>,
}

impl CodeFile {
    pub const fn key(&self) -> &FileKey {
        &self.key
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn text(&self) -> core::result::Result<&str, core::str::Utf8Error> {
        core::str::from_utf8(&self.bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Directory(DirKey),
    File(FileKey, Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRef<'a> {
    Directory(&'a DirKey),
    File(&'a CodeFile),
}

/// An immutable, ordered project tree. Root and every directory are explicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeTree {
    directories: BTreeSet<DirKey>,
    files: BTreeMap<FileKey, CodeFile>,
}

impl Default for CodeTree {
    fn default() -> Self {
        let mut directories = BTreeSet::new();
        directories.insert(DirKey::ROOT);
        Self {
            directories,
            files: BTreeMap::new(),
        }
    }
}

impl CodeTree {
    pub fn new(entries: impl IntoIterator<Item = Entry>) -> core::result::Result<Self, TreeError> {
        let mut tree = Self::default();
        let mut supplied_dirs = BTreeSet::new();
        for entry in entries {
            match entry {
                Entry::Directory(key) => {
                    if !supplied_dirs.insert(key.clone()) {
                        return Err(TreeError::DuplicateDirectory(key));
                    }
                    tree.insert_directory_with_parents(&key)?;
                }
                Entry::File(key, bytes) => {
                    if tree.files.contains_key(&key) {
                        return Err(TreeError::DuplicateFile(key));
                    }
                    tree.insert_file_with_parents(key, bytes)?;
                }
            }
        }
        Ok(tree)
    }

    pub fn file(&self, key: &FileKey) -> Option<&CodeFile> {
        self.files.get(key)
    }
    pub fn directory(&self, key: &DirKey) -> Option<&DirKey> {
        self.directories.get(key)
    }

    pub fn lookup_file(&self, key: &FileKey) -> Option<EntryRef<'_>> {
        if let Some(file) = self.files.get(key) {
            return Some(EntryRef::File(file));
        }
        let dir = DirKey::new(key.path().clone());
        self.directories.get(&dir).map(EntryRef::Directory)
    }

    pub fn lookup_dir(&self, key: &DirKey) -> Option<EntryRef<'_>> {
        if let Some(dir) = self.directories.get(key) {
            return Some(EntryRef::Directory(dir));
        }
        FileKey::new(key.path().clone())
            .ok()
            .and_then(|file| self.files.get(&file))
            .map(EntryRef::File)
    }

    pub fn children(&self, key: &DirKey) -> impl Iterator<Item = EntryRef<'_>> {
        let depth = key.path().segments().len() + 1;
        let dirs = self
            .directories
            .range((Bound::Excluded(key), Bound::Unbounded))
            .take_while(|dir| dir.path().starts_with(key.path()))
            .filter(move |dir| dir.path().segments().len() == depth)
            .map(EntryRef::Directory);
        let files = self
            .descendant_files(key)
            .filter(move |file| file.key.path().segments().len() == depth)
            .map(EntryRef::File);
        let mut children: Vec<_> = dirs.chain(files).collect();
        children.sort_by(|a, b| {
            Self::child_name(a)
                .as_bytes()
                .cmp(Self::child_name(b).as_bytes())
        });
        children.into_iter()
    }

    pub fn files_under(&self, key: &DirKey) -> impl Iterator<Item = &CodeFile> {
        self.descendant_files(key)
    }

    /// Every file under `key`. Keys sort lexicographically by segment, so a directory's
    /// descendants are a contiguous range.
    fn descendant_files(&self, key: &DirKey) -> impl Iterator<Item = &CodeFile> {
        let start = FileKey::new(key.path().clone()).map_or(Bound::Unbounded, Bound::Included);
        self.files
            .range((start, Bound::Unbounded))
            .take_while(move |(file, _)| file.path().starts_with(key.path()))
            .map(|(_, file)| file)
    }

    pub fn files(&self) -> impl Iterator<Item = &CodeFile> {
        self.files.values()
    }

    pub(crate) fn insert_directory_with_parents(
        &mut self,
        key: &DirKey,
    ) -> core::result::Result<(), TreeError> {
        if let Ok(file) = FileKey::new(key.path().clone())
            && self.files.contains_key(&file)
        {
            return Err(TreeError::FileDirectoryCollision(file));
        }
        let mut lineage: Vec<_> = key.ancestors().collect();
        lineage.reverse();
        for dir in lineage {
            if let Ok(file) = FileKey::new(dir.path().clone())
                && self.files.contains_key(&file)
            {
                return Err(TreeError::FileAncestor(file));
            }
            self.directories.insert(dir);
        }
        Ok(())
    }

    pub(crate) fn insert_file_with_parents(
        &mut self,
        key: FileKey,
        bytes: impl Into<Arc<[u8]>>,
    ) -> core::result::Result<(), TreeError> {
        let collision = DirKey::new(key.path().clone());
        if self.directories.contains(&collision) {
            return Err(TreeError::FileDirectoryCollision(key));
        }
        if let Some(ancestor) = key.parent().ancestors().find_map(|dir| {
            FileKey::new(dir.path().clone())
                .ok()
                .filter(|file| self.files.contains_key(file))
        }) {
            return Err(TreeError::FileAncestor(ancestor));
        }
        self.insert_directory_with_parents(&key.parent())?;
        // Keys sort lexicographically by segment, so the new file's would-be descendants are
        // contiguous right after it: checking the single next key covers them all.
        if let Some((descendant, _)) = self
            .files
            .range((
                core::ops::Bound::Excluded(&key),
                core::ops::Bound::Unbounded,
            ))
            .next()
            && descendant.path().starts_with(key.path())
        {
            return Err(TreeError::FileAncestor(key));
        }
        self.files.insert(
            key.clone(),
            CodeFile {
                key,
                bytes: bytes.into(),
            },
        );
        Ok(())
    }

    pub(crate) fn remove_file(&mut self, key: &FileKey) {
        self.files.remove(key);
    }

    pub(crate) fn remove_directory(&mut self, key: &DirKey) {
        if key == &DirKey::ROOT {
            return;
        }
        let files: Vec<_> = self
            .descendant_files(key)
            .map(|file| file.key.clone())
            .collect();
        for file in files {
            self.files.remove(&file);
        }
        let dirs: Vec<_> = self
            .directories
            .range((Bound::Included(key), Bound::Unbounded))
            .take_while(|dir| dir.path().starts_with(key.path()))
            .cloned()
            .collect();
        for dir in dirs {
            self.directories.remove(&dir);
        }
    }

    fn child_name<'a>(child: &EntryRef<'a>) -> &'a str {
        match child {
            EntryRef::Directory(dir) => dir.name().expect("root is not a child").as_str(),
            EntryRef::File(file) => file.key.name().as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: &[u8]) -> Entry {
        Entry::File(FileKey::parse(path).unwrap(), bytes.to_vec())
    }

    #[test]
    fn order_is_stable_and_empty_directories_survive() {
        let tree = CodeTree::new([
            file("z.java", b"z"),
            Entry::Directory(DirKey::parse("empty").unwrap()),
            file("a/B.java", b"b"),
            file("a/A.java", b"a"),
        ])
        .unwrap();
        let keys: Vec<_> = tree.files().map(|f| f.key().to_string()).collect();
        assert_eq!(keys, ["a/A.java", "a/B.java", "z.java"]);
        assert!(tree.directory(&DirKey::parse("empty").unwrap()).is_some());
    }

    #[test]
    fn rejects_collisions_duplicates_and_file_ancestors() {
        assert!(matches!(
            CodeTree::new([file("a", b"1"), file("a", b"2")]),
            Err(TreeError::DuplicateFile(_))
        ));
        assert!(matches!(
            CodeTree::new([
                Entry::Directory(DirKey::parse("a").unwrap()),
                file("a", b"x")
            ]),
            Err(TreeError::FileDirectoryCollision(_))
        ));
        assert!(matches!(
            CodeTree::new([file("a", b"x"), file("a/b", b"y")]),
            Err(TreeError::FileAncestor(_))
        ));
    }
}
