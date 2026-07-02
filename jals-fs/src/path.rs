//! Free helpers for the `/`-separated UTF-8 virtual paths used by [`FileTree`](crate::FileTree).
//!
//! `std::path::Path`/`PathBuf` have no `core`/`alloc` equivalent, so these operate on plain `&str`.
//! [`parent`] mirrors [`std::path::Path::parent`] for `/`-separated absolute paths so an upward
//! config-discovery walk terminates identically on an [`OsFileTree`](crate::OsFileTree) and an
//! [`InMemoryFileTree`](crate::InMemoryFileTree) (a parity test asserts the match).

use alloc::string::String;

/// The parent directory of `path`, mirroring [`std::path::Path::parent`] for `/`-separated paths.
///
/// `"/a/b" -> Some("/a")`, `"/a" -> Some("/")`, `"/" -> None`, `"a/b" -> Some("a")`,
/// `"a" -> Some("")`, `"" -> None`. A trailing `/` is ignored (`"/a/b/" -> Some("/a")`).
pub fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        // "" and the root ("/", "///", …) have no parent.
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some(&trimmed[..1]),     // "/a" -> "/"
        Some(idx) => Some(&trimmed[..idx]), // "/a/b" -> "/a", "a/b" -> "a"
        None => Some(""),                   // "a" -> ""
    }
}

/// The final component of `path` (after the last `/`).
///
/// `"/a/b.txt" -> Some("b.txt")`, `"/a/" -> Some("a")`, `"/" -> None`, `"" -> None`.
pub fn file_name(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(idx) => Some(&trimmed[idx + 1..]),
        None => Some(trimmed),
    }
}

/// The extension of `path`'s file name — the part after its last `.`, not counting a leading dot.
///
/// `"a/b.java" -> Some("java")`, `"a/b" -> None`, `".hidden" -> None`, `"a/b.tar.gz" -> Some("gz")`.
pub fn extension(path: &str) -> Option<&str> {
    let name = file_name(path)?;
    match name.rfind('.') {
        Some(0) | None => None, // ".hidden" / no dot -> no extension
        Some(idx) => Some(&name[idx + 1..]),
    }
}

/// Join `child` onto `base` with a single `/` separator.
///
/// `join("/a", "b") -> "/a/b"`, `join("/a/", "b") -> "/a/b"`, `join("/", "b") -> "/b"`,
/// `join("", "b") -> "b"`.
pub fn join(base: &str, child: &str) -> String {
    let child = child.trim_start_matches('/');
    if base.is_empty() {
        return String::from(child);
    }
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        // `base` was the root ("/", "///", …).
        let mut s = String::from("/");
        s.push_str(child);
        return s;
    }
    let mut s = String::from(base);
    s.push('/');
    s.push_str(child);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parent_matches_std_path() {
        for input in [
            "/a/b/c",
            "/a/b",
            "/a",
            "/",
            "a/b",
            "a",
            "",
            "/a/b/",
            "/home/u/proj",
        ] {
            let ours = parent(input);
            let std = Path::new(input).parent().and_then(Path::to_str);
            assert_eq!(ours, std, "parent({input:?}) diverged from std::path::Path");
        }
    }

    #[test]
    fn file_name_and_extension() {
        assert_eq!(file_name("/a/b.txt"), Some("b.txt"));
        assert_eq!(file_name("/a/"), Some("a"));
        assert_eq!(file_name("/"), None);
        assert_eq!(file_name(""), None);

        assert_eq!(extension("a/b.java"), Some("java"));
        assert_eq!(extension("a/b"), None);
        assert_eq!(extension(".hidden"), None);
        assert_eq!(extension("a/b.tar.gz"), Some("gz"));
    }

    #[test]
    fn join_variants() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/a/", "b"), "/a/b");
        assert_eq!(join("/", "b"), "/b");
        assert_eq!(join("", "b"), "b");
        assert_eq!(join("/a", "/b"), "/a/b");
    }
}
