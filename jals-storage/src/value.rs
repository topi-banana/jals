use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use crate::error::{NameError, PathError};

/// A portable UTF-8 project-path segment.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

impl Name {
    pub fn new(value: impl Into<String>) -> core::result::Result<Self, NameError> {
        let value = value.into();
        Self::validate(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(value: &str) -> core::result::Result<(), NameError> {
        if value.is_empty() {
            return Err(NameError::Empty);
        }
        if value == "." {
            return Err(NameError::CurrentDirectory);
        }
        if value == ".." {
            return Err(NameError::ParentDirectory);
        }
        if value.chars().any(|ch| ch == '/' || ch == '\\') {
            return Err(NameError::Separator);
        }
        if value.chars().any(|ch| ch == '\0' || ch.is_control()) {
            return Err(NameError::ControlCharacter);
        }
        if value
            .chars()
            .any(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err(NameError::WindowsReservedCharacter);
        }
        if value.ends_with('.') || value.ends_with(' ') {
            return Err(NameError::WindowsReservedSuffix);
        }
        let stem = value.split('.').next().unwrap_or(value);
        let upper = stem.to_ascii_uppercase();
        if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || Self::is_numbered_device(&upper, "COM")
            || Self::is_numbered_device(&upper, "LPT")
        {
            return Err(NameError::WindowsReservedName);
        }
        Ok(())
    }

    fn is_numbered_device(value: &str, prefix: &str) -> bool {
        value
            .strip_prefix(prefix)
            .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Name {
    type Err = NameError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// A root-relative path consisting solely of validated [`Name`] values.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath(Vec<Name>);

impl RelativePath {
    pub const ROOT: Self = Self(Vec::new());

    pub fn new(segments: impl IntoIterator<Item = Name>) -> Self {
        Self(segments.into_iter().collect())
    }

    pub fn parse(value: &str) -> core::result::Result<Self, PathError> {
        if value.is_empty() {
            return Ok(Self::ROOT);
        }
        if value.starts_with("//") || value.starts_with("\\\\") {
            return Err(PathError::Unc);
        }
        if value.starts_with('/') || value.starts_with('\\') {
            return Err(PathError::Absolute);
        }
        if value.as_bytes().get(1) == Some(&b':') && value.as_bytes()[0].is_ascii_alphabetic() {
            return Err(PathError::Drive);
        }
        value
            .split('/')
            .map(|part| Name::new(part).map_err(PathError::InvalidName))
            .collect::<core::result::Result<Vec<_>, _>>()
            .map(Self)
    }

    pub const fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn segments(&self) -> impl ExactSizeIterator<Item = &Name> {
        self.0.iter()
    }

    pub fn parent(&self) -> Option<Self> {
        (!self.0.is_empty()).then(|| Self(self.0[..self.0.len() - 1].to_vec()))
    }

    /// Whether `prefix` is a segment-wise prefix of (or equal to) this path.
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.0.len() >= prefix.0.len() && self.0[..prefix.0.len()] == prefix.0
    }

    /// This path with `prefix` removed, or `None` when `prefix` is not one.
    ///
    /// The inverse of [`concat`](Self::concat), and the one place the rebasing rule lives: several
    /// subsystems address the same bytes relative to different roots (a source directory, an archive
    /// prefix, a publication destination), and a hand-rolled `skip(prefix.len())` beside a
    /// `starts_with` is how two of them quietly disagree about where a file is.
    pub fn strip_prefix(&self, prefix: &Self) -> Option<Self> {
        self.starts_with(prefix)
            .then(|| Self(self.0[prefix.0.len()..].to_vec()))
    }

    pub fn name(&self) -> Option<&Name> {
        self.0.last()
    }

    #[must_use]
    pub fn join(&self, name: Name) -> Self {
        let mut segments = self.0.clone();
        segments.push(name);
        Self(segments)
    }

    /// This path followed by every segment of `other`. Both sides are already validated, so
    /// concatenation cannot fail.
    #[must_use]
    pub fn concat(&self, other: &Self) -> Self {
        Self(self.0.iter().chain(other.0.iter()).cloned().collect())
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, name) in self.0.iter().enumerate() {
            if index != 0 {
                f.write_str("/")?;
            }
            name.fmt(f)?;
        }
        Ok(())
    }
}

impl FromStr for RelativePath {
    type Err = PathError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A typed file identity. Root cannot be constructed as a file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileKey(RelativePath);

impl FileKey {
    pub fn new(path: RelativePath) -> core::result::Result<Self, PathError> {
        if path.is_root() {
            Err(PathError::FileIsRoot)
        } else {
            Ok(Self(path))
        }
    }

    pub fn parse(value: &str) -> core::result::Result<Self, PathError> {
        Self::new(RelativePath::parse(value)?)
    }

    pub fn in_dir(dir: &DirKey, name: Name) -> Self {
        Self(dir.0.join(name))
    }

    pub const fn path(&self) -> &RelativePath {
        &self.0
    }

    pub fn parent(&self) -> DirKey {
        DirKey(self.0.parent().unwrap_or_default())
    }

    /// This file's path reinterpreted as a directory identity, for collision checks and the
    /// diagnostics that report them.
    pub(crate) fn as_dir_key(&self) -> DirKey {
        DirKey(self.0.clone())
    }

    pub fn name(&self) -> &Name {
        self.0.name().expect("FileKey is never root")
    }

    pub fn extension(&self) -> Option<&str> {
        let name = self.name().as_str();
        match name.rfind('.') {
            Some(0) | None => None,
            Some(index) => Some(&name[index + 1..]),
        }
    }

    /// Whether the file's extension equals `extension`, ASCII case-insensitively. This is the
    /// single extension policy shared by every consumer; do not re-derive it from
    /// [`extension`](Self::extension) at call sites.
    pub fn has_extension(&self, extension: &str) -> bool {
        self.extension()
            .is_some_and(|found| found.eq_ignore_ascii_case(extension))
    }
}

impl fmt::Display for FileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for FileKey {
    type Err = PathError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A typed directory identity. The project root is represented only by [`DirKey::ROOT`].
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirKey(RelativePath);

impl DirKey {
    pub const ROOT: Self = Self(RelativePath::ROOT);

    pub const fn new(path: RelativePath) -> Self {
        Self(path)
    }

    pub fn parse(value: &str) -> core::result::Result<Self, PathError> {
        RelativePath::parse(value).map(Self)
    }

    pub const fn path(&self) -> &RelativePath {
        &self.0
    }

    pub fn parent(&self) -> Option<Self> {
        self.0.parent().map(Self)
    }

    /// This directory's path reinterpreted as a file identity (`None` for the root), for
    /// collision checks and the diagnostics that report them.
    pub(crate) fn as_file_key(&self) -> Option<FileKey> {
        FileKey::new(self.0.clone()).ok()
    }

    pub fn name(&self) -> Option<&Name> {
        self.0.name()
    }

    #[must_use]
    pub fn directory(&self, name: Name) -> Self {
        Self(self.0.join(name))
    }

    pub fn file(&self, name: Name) -> FileKey {
        FileKey::in_dir(self, name)
    }

    /// The directory at `relative` under this directory.
    #[must_use]
    pub fn join_path(&self, relative: &RelativePath) -> Self {
        Self(self.0.concat(relative))
    }

    /// The file at `relative` under this directory. [`PathError::FileIsRoot`] when `relative`
    /// is the root path.
    pub fn file_at(&self, relative: &RelativePath) -> core::result::Result<FileKey, PathError> {
        FileKey::new(self.0.concat(relative))
    }

    pub fn ancestors(&self) -> Ancestors {
        Ancestors(Some(self.clone()))
    }
}

impl fmt::Display for DirKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for DirKey {
    type Err = PathError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        Self::parse(s)
    }
}

pub struct Ancestors(Option<DirKey>);

impl Iterator for Ancestors {
    type Item = DirKey;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.0.take()?;
        self.0 = current.parent();
        Some(current)
    }
}

/// Monotonic generation of a project snapshot, overlay, or committed transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
    pub const INITIAL: Self = Self(0);

    pub const fn get(self) -> u64 {
        self.0
    }

    pub(crate) const fn next(self) -> Self {
        Self(self.0.checked_add(1).expect("revision overflow"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_round_trip_and_ancestors() {
        let path = RelativePath::parse("src/main/A.java").unwrap();
        assert_eq!(path.to_string(), "src/main/A.java");
        let file = FileKey::new(path).unwrap();
        let dirs: Vec<_> = file.parent().ancestors().map(|d| d.to_string()).collect();
        assert_eq!(dirs, ["src/main", "src", ""]);
    }

    #[test]
    fn rejects_non_portable_names_and_paths() {
        for name in [
            "", ".", "..", "a/b", "a\\b", "a\0b", "a:b", "CON", "lpt9.txt", "x.",
        ] {
            assert!(Name::new(name).is_err(), "accepted {name:?}");
        }
        for path in ["/x", "C:/x", "../x", "a//b", "a\\b"] {
            assert!(RelativePath::parse(path).is_err(), "accepted {path:?}");
        }
        assert_eq!(RelativePath::parse("").unwrap(), RelativePath::ROOT);
        assert!(FileKey::parse("").is_err());
    }
}
