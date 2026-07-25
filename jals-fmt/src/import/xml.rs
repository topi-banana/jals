//! XML readers for the two XML-backed native formats, behind the `std` feature.
//!
//! quick-xml is std-only, so this module — and only this module — is gated. Each reader lowers its
//! document into the same flat `key → value` map the portable readers produce, so the typed models
//! in [`super::eclipse`] / [`super::intellij`] are reused unchanged:
//! - the Eclipse exported profile shares the `org.eclipse.jdt.core.formatter.*` id namespace with
//!   `.prefs`, so it lowers to the identical map;
//! - the IntelliJ scheme uses `UPPER_SNAKE` option names, which is exactly how
//!   [`super::intellij::IntellijConfig`] is keyed, so only its element-valued
//!   `PackageEntryTable` options need reshaping.

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

/// One entry of an IntelliJ `PackageEntryTable`, in document order.
enum ImportEntry {
    /// A `<package name=… withSubpackages=… static=…/>` row.
    Package {
        /// The package name, empty for the catch-all row.
        name: String,
        /// `withSubpackages="true"`.
        with_subpackages: bool,
        /// `static="true"`.
        is_static: bool,
    },
    /// An `<emptyLine/>` (blank-line separator).
    Blank,
}

/// The `PackageEntryTable`-valued options, which are element lists rather than `value=` attributes.
const PACKAGE_TABLES: [&str; 2] = ["IMPORT_LAYOUT_TABLE", "PACKAGES_TO_USE_IMPORT_ON_DEMAND"];

/// What one scan of a scheme document accumulates.
#[derive(Default)]
struct SchemeScan {
    /// `UPPER_SNAKE` option name → raw value (integer / bool / separator), verbatim.
    raw: BTreeMap<String, String>,
    /// The rows of the `PackageEntryTable` option currently open, in document order.
    entries: Vec<ImportEntry>,
    /// The name of that option, when one is open.
    open_table: Option<String>,
}

impl SchemeScan {
    /// Record one opening / empty element.
    fn visit(&mut self, element: &BytesStart<'_>) -> Result<(), ImportError> {
        match element.name().as_ref() {
            b"option" => match (Xml::attr(element, b"name")?, Xml::attr(element, b"value")?) {
                (Some(name), Some(value)) => {
                    self.raw.insert(name, value);
                }
                // A table-valued option carries its rows as children instead of a `value=`.
                (Some(name), None) if PACKAGE_TABLES.contains(&name.as_str()) => {
                    self.open_table = Some(name);
                    self.entries.clear();
                }
                _ => {}
            },
            b"package" if self.open_table.is_some() => {
                self.entries.push(ImportEntry::Package {
                    name: Xml::attr(element, b"name")?.unwrap_or_default(),
                    with_subpackages: Xml::attr(element, b"withSubpackages")?.as_deref()
                        == Some("true"),
                    is_static: Xml::attr(element, b"static")?.as_deref() == Some("true"),
                });
            }
            b"emptyLine" if self.open_table.is_some() => self.entries.push(ImportEntry::Blank),
            _ => {}
        }
        Ok(())
    }

    /// Close the open table-valued option, lowering its rows to the mini-list form.
    fn close_table(&mut self) {
        if let Some(name) = self.open_table.take()
            && !self.entries.is_empty()
        {
            let list = self
                .entries
                .iter()
                .map(|entry| match entry {
                    ImportEntry::Blank => "|".to_owned(),
                    ImportEntry::Package {
                        name,
                        with_subpackages,
                        is_static,
                    } => {
                        let marker = if *is_static { "$" } else { "" };
                        let wildcard = if *with_subpackages { "**" } else { "*" };
                        if name.is_empty() {
                            format!("{marker}{wildcard}")
                        } else {
                            format!("{marker}{name}.{wildcard}")
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.raw.insert(name, list);
        }
        self.entries.clear();
    }
}

/// Reader for an IntelliJ code-style scheme.
pub(crate) struct IntellijSchemeReader;

impl IntellijSchemeReader {
    /// Lower the scheme to the `UPPER_SNAKE` option map [`super::intellij::IntellijConfig`] reads.
    ///
    /// Values are passed through **verbatim** — the model's value types accept a raw integer just
    /// as they accept an `.editorconfig` token — so no per-property int→token table is applied
    /// here. The one shape that is not a `value=` attribute is a `PackageEntryTable`, whose
    /// `<package>` / `<emptyLine/>` rows are lowered to the same comma-separated mini-list the
    /// `.editorconfig` form uses.
    pub(crate) fn parse(src: &str) -> Result<BTreeMap<String, String>, ImportError> {
        let mut reader = Reader::from_str(src);
        let mut scan = SchemeScan::default();
        // Open-element depth inside a language-scoped block belonging to *another* language; `0`
        // means the scan is reading. Java options are not confined to a Java block (old exported
        // schemes put import options at the top level — DESIGN §A.4.5), so foreign blocks are
        // skipped by denylist instead of Java being allowlisted.
        let mut skip_depth = 0usize;

        loop {
            let event = reader
                .read_event()
                .map_err(|err| ImportError::Xml(err.to_string()))?;

            // Drop a foreign block's entire subtree: every language reuses the same UPPER_SNAKE
            // option vocabulary, so reading one would overwrite Java's values (DESIGN §A.4.4).
            if skip_depth > 0 {
                match event {
                    Event::Eof => break,
                    Event::Start(_) => skip_depth += 1,
                    Event::End(_) => skip_depth -= 1,
                    _ => {}
                }
                continue;
            }

            match event {
                Event::Eof => break,
                Event::Start(element) => {
                    if Self::is_foreign_language_block(&element)? {
                        skip_depth = 1;
                    } else {
                        scan.visit(&element)?;
                    }
                }
                // An empty element opens no subtree, so a foreign one needs no skip state.
                Event::Empty(element) => {
                    if !Self::is_foreign_language_block(&element)? {
                        scan.visit(&element)?;
                    }
                }
                Event::End(element)
                    if scan.open_table.is_some() && element.name().as_ref() == b"option" =>
                {
                    scan.close_table();
                }
                _ => {}
            }
        }
        // An unterminated document still yields whatever rows were read.
        scan.close_table();

        Ok(scan.raw)
    }

    /// Whether an element opens a settings block scoped to a language other than Java.
    ///
    /// Two element shapes carry a language scope (DESIGN §A.4.4): `<codeStyleSettings language=…>`
    /// holds the whitespace / wrap / indent options, and the per-language
    /// `<…CodeStyleSettings>` siblings (`<JavaCodeStyleSettings>`, `<KotlinCodeStyleSettings>`, …)
    /// hold the import policy. Anything else — notably the `<code_scheme>` top level — is global
    /// and read as Java's.
    fn is_foreign_language_block(element: &BytesStart<'_>) -> Result<bool, ImportError> {
        let name = element.name();
        let name = name.as_ref();
        if name == b"codeStyleSettings" {
            // Without a `language` attribute the block is not language-scoped, so read it.
            return Ok(Xml::attr(element, b"language")?
                .is_some_and(|language| !language.eq_ignore_ascii_case("JAVA")));
        }
        // `codeStyleSettings` itself does not match this suffix (its leading `c` is lowercase).
        Ok(name.ends_with(b"CodeStyleSettings") && name != b"JavaCodeStyleSettings")
    }
}
