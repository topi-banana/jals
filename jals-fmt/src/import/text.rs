//! Portable (`no_std`, `&str`-in) readers for the two non-XML native config formats:
//! Eclipse's `.settings/org.eclipse.jdt.core.prefs` (a Java *properties* file) and IntelliJ's
//! `.editorconfig` (an INI-like file). Both lower to a flat `key → value` [`BTreeMap`] that
//! [`super::serde_kv::from_pairs`] then turns into a typed model. The XML forms need a real
//! XML reader and live behind the `std` feature in [`super::xml`].

// Native file / product names (EditorConfig, `.editorconfig`, …) appear in the docs as prose.
#![allow(clippy::doc_markdown)]

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;

/// Prefix shared by every Eclipse formatter setting id (`org.eclipse.jdt.core.formatter.*`). A
/// `.prefs` file also carries `compiler.*` / `codeComplete.*` namespaces we drop.
pub(crate) const ECLIPSE_FORMATTER_PREFIX: &str = "org.eclipse.jdt.core.formatter.";

/// Reader for a Java *properties* file (Eclipse `.prefs`).
pub(crate) mod properties {
    use super::{BTreeMap, ECLIPSE_FORMATTER_PREFIX, String, ToOwned};

    /// Parse a properties file, keeping only formatter settings.
    ///
    /// Java properties rules honored for the formatter subset: `#` / `!` line comments, `=` or `:`
    /// key/value separator, and `\uXXXX` / `\:` / `\=` / `\\` escapes in the value. Line
    /// continuations (`\` at end of line) are not used by exported Eclipse prefs and are not
    /// handled. Only keys under [`ECLIPSE_FORMATTER_PREFIX`] are retained, stored under their full
    /// id (matching the `#[serde(rename = "org.eclipse.jdt.core.formatter.…")]` on the model).
    pub(crate) fn parse(src: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for line in src.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let Some((key, value)) = split(line) else {
                continue;
            };
            if key.starts_with(ECLIPSE_FORMATTER_PREFIX) {
                out.insert(key.to_owned(), unescape(value));
            }
        }
        out
    }

    /// Split a properties line at its first unescaped `=` or `:`.
    pub(crate) fn split(line: &str) -> Option<(&str, &str)> {
        let mut escaped = false;
        for (i, &b) in line.as_bytes().iter().enumerate() {
            if escaped {
                escaped = false;
                continue;
            }
            match b {
                b'\\' => escaped = true,
                b'=' | b':' => return Some((line[..i].trim_end(), line[i + 1..].trim_start())),
                _ => {}
            }
        }
        None
    }

    /// Decode the `\uXXXX` / `\:` / `\=` / `\\` / `\t` / `\n` escapes a properties value may carry.
    pub(crate) fn unescape(value: &str) -> String {
        if !value.contains('\\') {
            return value.to_owned();
        }
        let mut out = String::with_capacity(value.len());
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(decoded) =
                        u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                    {
                        out.push(decoded);
                    }
                }
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('f') => out.push('\u{c}'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        out
    }
}

/// Reader for an `.editorconfig` file (IntelliJ's primary form).
pub(crate) mod editor_config {
    use super::{BTreeMap, String, ToOwned};

    /// Parse an `.editorconfig`, collecting every property that applies to `*.java` files.
    ///
    /// Sections are glob headers; a property applies to Java when its section matches a `*.java`
    /// path — approximated (sufficient for the IntelliJ importer) as `[*]` or any header whose
    /// glob mentions `java`. Later matching sections override earlier ones. Keys are lowercased
    /// (per spec); values keep their case. `root = true` and non-matching sections are ignored.
    /// Both the universal keys (`indent_style`, …) and IntelliJ's `ij_*` keys are returned
    /// verbatim, matching the `#[serde(rename = …)]` on the model.
    pub(crate) fn parse(src: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let mut in_java_section = false;
        for line in src.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                // A trailing comment is not part of the glob. Without this the line fails the
                // `]` test, falls through, and silently leaves the *previous* section open — so
                // the next properties land in whichever section came before.
                let rest = strip_comment(rest);
                in_java_section = rest.strip_suffix(']').is_some_and(section_matches_java);
                continue;
            }
            if !in_java_section {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                out.insert(key.trim().to_lowercase(), value.trim().to_owned());
            }
        }
        out
    }

    /// Drop a trailing `#` / `;` comment from a section-header line.
    pub(crate) fn strip_comment(line: &str) -> &str {
        line.find(['#', ';'])
            .map_or(line, |at| line[..at].trim_end())
    }

    /// Whether an `.editorconfig` section header applies to `*.java` files.
    ///
    /// Still an approximation — the importer is handed one file's text with no target path, so a
    /// directory-scoped section cannot actually be resolved — but the decision is taken from the
    /// **extension** the glob selects, never from a directory component:
    ///
    /// - `[*.java]`, `[{*.java,*.kt}]`, `[*.{java,kt}]` match, because `java` occurs as a whole
    ///   glob segment that is not a path component;
    /// - `[*.javascript]`, `[*.jsp]`, `[*.kt]` do not;
    /// - `[src/main/java/**/*.xml]` does **not**, even though it spells `java`: the segment is a
    ///   directory, and the glob names `xml` as its extension;
    /// - `[*]`, `[**]`, `[src/main/java/**]` match, because they name no extension at all —
    ///   erring toward applying a section jals cannot resolve, which is what `[*]` already does.
    pub(crate) fn section_matches_java(header: &str) -> bool {
        names_java(header) || !names_an_extension(header)
    }

    /// Whether `java` occurs in `header` as a whole glob segment that is not a directory
    /// component (`*.java`, `*.{java,kt}` — but not `java/**`).
    pub(crate) fn names_java(header: &str) -> bool {
        let bytes = header.as_bytes();
        let mut from = 0;
        while let Some(offset) = header[from..].find("java") {
            let start = from + offset;
            let end = start + "java".len();
            let preceded_by_segment = start > 0 && bytes[start - 1].is_ascii_alphanumeric();
            let next = bytes.get(end).copied();
            let followed_by_segment = next.is_some_and(|byte| byte.is_ascii_alphanumeric());
            if !preceded_by_segment && !followed_by_segment && next != Some(b'/') {
                return true;
            }
            from = end;
        }
        false
    }

    /// Whether the header's file-name component names an extension at all — `*.xml` does,
    /// `**` and `src/main/java/**` do not.
    pub(crate) fn names_an_extension(header: &str) -> bool {
        header
            .rsplit('/')
            .next()
            .is_some_and(|file| file.contains('.'))
    }
}
