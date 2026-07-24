//! Portable (`no_std`, `&str`-in) readers for the two non-XML native config formats:
//! Eclipse's `.settings/org.eclipse.jdt.core.prefs` (a Java *properties* file) and IntelliJ's
//! `.editorconfig` (an INI-like file). Both lower to a flat `key → value` [`BTreeMap`] that
//! [`super::serde_kv::Kv::from_pairs`] then turns into a typed model. The XML forms need a real
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
pub(crate) struct Properties;

impl Properties {
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
            let Some((key, value)) = Self::split(line) else {
                continue;
            };
            if key.starts_with(ECLIPSE_FORMATTER_PREFIX) {
                out.insert(key.to_owned(), Self::unescape(value));
            }
        }
        out
    }

    /// Split a properties line at its first unescaped `=` or `:`.
    fn split(line: &str) -> Option<(&str, &str)> {
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
    fn unescape(value: &str) -> String {
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
pub(crate) struct EditorConfig;

impl EditorConfig {
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
            if let Some(header) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                in_java_section = Self::section_matches_java(header);
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

    /// Whether an `.editorconfig` section header applies to `*.java` files.
    ///
    /// Approximated (sufficient for the importer) as a universal header (`[*]` / `[**]`), or any
    /// header carrying `java` as a whole extension segment — so `[*.java]` and `[{*.java,*.kt}]`
    /// match while `[*.javascript]` and `[*.jsp]` do not.
    fn section_matches_java(header: &str) -> bool {
        matches!(header, "*" | "**")
            || header
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|segment| segment == "java")
    }
}
