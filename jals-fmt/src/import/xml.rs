//! XML readers for the two XML-backed native formats, behind the `std` feature.
//!
//! quick-xml is std-only, so this module — and only this module — is gated. Each reader lowers its
//! document into the same flat `key → value` map the portable readers produce, so the typed models
//! in [`super::eclipse`] / [`super::intellij`] are reused unchanged:
//! - the Eclipse exported profile shares the `org.eclipse.jdt.core.formatter.*` id namespace with
//!   `.prefs`, so it lowers to the identical map;
//! - the IntelliJ scheme uses `UPPER_SNAKE` option names and *integer* enum values, so it is
//!   normalized to the `.editorconfig` `ij_java_*` key + token shape here (via the portable
//!   [`super::intellij`] token tables) before deserialization.

// Native product / option names (IntelliJ, `UPPER_SNAKE`, …) recur in the docs as prose.
#![allow(clippy::doc_markdown)]

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::ImportError;
use super::intellij::{IjBraceStyle, IjEndOfLine, IjWrap};
use super::text::ECLIPSE_FORMATTER_PREFIX;

/// Shared XML helpers.
struct Xml;

impl Xml {
    /// Read one string attribute (`want`) off an element, unescaping entities.
    fn attr(element: &BytesStart<'_>, want: &[u8]) -> Result<Option<String>, ImportError> {
        for attribute in element.attributes() {
            let attribute = attribute.map_err(|err| ImportError::Xml(err.to_string()))?;
            if attribute.key.as_ref() == want {
                let value = attribute
                    .unescape_value()
                    .map_err(|err| ImportError::Xml(err.to_string()))?;
                return Ok(Some(value.into_owned()));
            }
        }
        Ok(None)
    }
}

/// Reader for an exported Eclipse XML formatter profile (`<setting id=… value=…/>`).
pub(crate) struct EclipseProfileReader;

impl EclipseProfileReader {
    /// Lower the profile to the formatter-id map that [`super::eclipse::EclipseConfig`] expects.
    pub(crate) fn parse(src: &str) -> Result<BTreeMap<String, String>, ImportError> {
        let mut reader = Reader::from_str(src);
        let mut out = BTreeMap::new();
        loop {
            let event = reader
                .read_event()
                .map_err(|err| ImportError::Xml(err.to_string()))?;
            match event {
                Event::Eof => break,
                Event::Empty(element) | Event::Start(element)
                    if element.name().as_ref() == b"setting" =>
                {
                    if let (Some(id), Some(value)) =
                        (Xml::attr(&element, b"id")?, Xml::attr(&element, b"value")?)
                        && id.starts_with(ECLIPSE_FORMATTER_PREFIX)
                    {
                        out.insert(id, value);
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

/// One entry of an IntelliJ `IMPORT_LAYOUT_TABLE`, in document order.
enum ImportEntry {
    /// A `<package name=… static=…/>` row.
    Package { name: String, is_static: bool },
    /// An `<emptyLine/>` (blank-line separator).
    Blank,
}

/// Reader for an IntelliJ code-style scheme (`<option name=… value=…/>` plus the import-layout
/// table).
pub(crate) struct IntellijSchemeReader;

impl IntellijSchemeReader {
    /// Lower the scheme to the `.editorconfig` `ij_java_*` shape, translating raw integer enums to
    /// tokens so [`super::intellij::IntellijConfig`] deserializes it unchanged.
    pub(crate) fn parse(src: &str) -> Result<BTreeMap<String, String>, ImportError> {
        let mut reader = Reader::from_str(src);
        // Raw UPPER_SNAKE option name → raw value (integer / bool / separator).
        let mut raw: BTreeMap<String, String> = BTreeMap::new();
        let mut imports: Vec<ImportEntry> = Vec::new();
        let mut in_import_layout = false;
        // Whether the reader is inside Java-relevant settings. Options sit either at the top level
        // / inside `<JavaCodeStyleSettings>` (global — read), or inside a per-language
        // `<codeStyleSettings language="…">` block; a non-Java block must not overwrite Java's
        // indent / wrap / brace options with its own (DESIGN §A.4.4).
        let mut java_scope = true;

        loop {
            let event = reader
                .read_event()
                .map_err(|err| ImportError::Xml(err.to_string()))?;
            match event {
                Event::Eof => break,
                // A per-language block scopes every option it wraps to that one language.
                Event::Start(element) if element.name().as_ref() == b"codeStyleSettings" => {
                    java_scope = Xml::attr(&element, b"language")?
                        .is_none_or(|language| language.eq_ignore_ascii_case("JAVA"));
                }
                Event::End(element) if element.name().as_ref() == b"codeStyleSettings" => {
                    java_scope = true;
                }
                // Inside a non-Java block, drop everything until it closes.
                _ if !java_scope => {}
                Event::End(element) if in_import_layout && element.name().as_ref() == b"option" => {
                    in_import_layout = false;
                }
                Event::Empty(element) | Event::Start(element) => match element.name().as_ref() {
                    b"option" => match (
                        Xml::attr(&element, b"name")?,
                        Xml::attr(&element, b"value")?,
                    ) {
                        (Some(name), Some(value)) => {
                            raw.insert(name, value);
                        }
                        (Some(name), None) if name == "IMPORT_LAYOUT_TABLE" => {
                            in_import_layout = true;
                        }
                        _ => {}
                    },
                    b"package" if in_import_layout => {
                        let name = Xml::attr(&element, b"name")?.unwrap_or_default();
                        let is_static = Xml::attr(&element, b"static")?.as_deref() == Some("true");
                        imports.push(ImportEntry::Package { name, is_static });
                    }
                    b"emptyLine" if in_import_layout => imports.push(ImportEntry::Blank),
                    _ => {}
                },
                _ => {}
            }
        }

        let mut pairs = BTreeMap::new();
        for (name, value) in raw {
            if let Some((key, translated)) = Self::translate_option(&name, &value) {
                pairs.insert(key.to_owned(), translated);
            }
        }
        if !imports.is_empty() {
            pairs.insert(
                "ij_java_imports_layout".to_owned(),
                Self::imports_layout(&imports),
            );
        }
        Ok(pairs)
    }

    /// Translate one raw IntelliJ scheme option into its `.editorconfig` (key, token) form, or
    /// `None` for options outside the modeled common-rule subset. Enum-valued options use the
    /// per-property token tables (never one table reused — DESIGN §A.4.2).
    fn translate_option(name: &str, value: &str) -> Option<(&'static str, String)> {
        let pass = |key: &'static str| Some((key, value.to_owned()));
        match name {
            "RIGHT_MARGIN" => pass("max_line_length"),
            "INDENT_SIZE" => pass("indent_size"),
            "CONTINUATION_INDENT_SIZE" => pass("ij_continuation_indent_size"),
            "KEEP_BLANK_LINES_IN_CODE" => pass("ij_java_keep_blank_lines_in_code"),
            "SPACE_BEFORE_COLON" => pass("ij_java_space_before_colon"),
            "SPACE_AFTER_COLON" => pass("ij_java_space_after_colon"),
            "BINARY_OPERATION_SIGN_ON_NEXT_LINE" => {
                pass("ij_java_binary_operation_sign_on_next_line")
            }
            "USE_TAB_CHARACTER" => Some((
                "indent_style",
                if value == "true" { "tab" } else { "space" }.to_owned(),
            )),
            "LINE_SEPARATOR" => {
                IjEndOfLine::token_from_str(value).map(|token| ("end_of_line", token.to_owned()))
            }
            "CLASS_BRACE_STYLE" => Self::enum_token(value, IjBraceStyle::token_from_int)
                .map(|t| ("ij_java_class_brace_style", t)),
            "METHOD_BRACE_STYLE" => Self::enum_token(value, IjBraceStyle::token_from_int)
                .map(|t| ("ij_java_method_brace_style", t)),
            "METHOD_PARAMETERS_WRAP" => Self::enum_token(value, IjWrap::token_from_int)
                .map(|t| ("ij_java_method_parameters_wrap", t)),
            "CLASS_ANNOTATION_WRAP" => Self::enum_token(value, IjWrap::token_from_int)
                .map(|t| ("ij_java_class_annotation_wrap", t)),
            "METHOD_ANNOTATION_WRAP" => Self::enum_token(value, IjWrap::token_from_int)
                .map(|t| ("ij_java_method_annotation_wrap", t)),
            _ => None,
        }
    }

    /// Parse a raw integer value and map it to a token via one of the per-property tables.
    fn enum_token(value: &str, table: fn(i64) -> Option<&'static str>) -> Option<String> {
        value
            .parse::<i64>()
            .ok()
            .and_then(table)
            .map(ToOwned::to_owned)
    }

    /// Rebuild an `ij_java_imports_layout` mini-list from the parsed `IMPORT_LAYOUT_TABLE` entries
    /// so the shared editorconfig model parses it identically. A `static` row is prefixed with `$`,
    /// a blank row becomes `|`, and the two catch-alls (empty package name) become `$*` / `*`. Any
    /// *named* package gets a `.**` suffix unconditionally: jals import groups are subpackage-prefix
    /// matches, so the IntelliJ `withSubpackages` flag (not modeled) has no jals counterpart to
    /// distinguish.
    fn imports_layout(entries: &[ImportEntry]) -> String {
        entries
            .iter()
            .map(|entry| match entry {
                ImportEntry::Blank => "|".to_owned(),
                ImportEntry::Package { name, is_static } => {
                    let marker = if *is_static { "$" } else { "" };
                    if name.is_empty() {
                        format!("{marker}*")
                    } else {
                        format!("{marker}{name}.**")
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}
