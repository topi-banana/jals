//! The value vocabularies an IntelliJ code-style setting can take, in **both** of its
//! representations.
//!
//! The same setting is a raw integer in a scheme XML and a named token in `.editorconfig`, and
//! the integer→token table is **per property** — `*_WRAP`, `*_BRACE_STYLE`, and `*_BRACE_FORCE`
//! each have their own, and reusing one for another is the classic import bug (`DESIGN.md`
//! §A.4.2, P-gen-5). Each type below therefore carries its own table and accepts either
//! spelling, so the models can be keyed by the XML name while still reading an `.editorconfig`.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Deserializer};

/// The shared setting-text reader, grouped so it is not a free function.
pub(crate) mod api {
    use super::{Deserialize, Deserializer, String, ToOwned};

    /// Read one setting's text, trimmed. Every value type below parses from this, which is what
    /// lets a model keyed by the XML name still read an `.editorconfig`.
    pub(crate) fn read<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
        Ok(String::deserialize(deserializer)?.trim().to_owned())
    }
}

/// A `*_WRAP` value.
///
/// The token names are counter-intuitive and are the reason this is an enum rather than an
/// integer: [`SplitIntoLines`](Self::SplitIntoLines) is IntelliJ's *Wrap Always*, while
/// [`OnEveryItem`](Self::OnEveryItem) is *Chop Down If Long*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IjWrap {
    /// `off` / `0` — Do Not Wrap.
    Off,
    /// `normal` / `1` — Wrap If Long.
    Normal,
    /// `split_into_lines` / `2` — Wrap Always.
    SplitIntoLines,
    /// `on_every_item` / `4` (and the lossy duplicate `5`) — Chop Down If Long.
    OnEveryItem,
}

impl IjWrap {
    /// Parse either representation.
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "off" | "0" => Self::Off,
            "normal" | "1" => Self::Normal,
            "split_into_lines" | "2" => Self::SplitIntoLines,
            // `5` round-trips back out as `4`; IntelliJ itself loses the distinction.
            "on_every_item" | "4" | "5" => Self::OnEveryItem,
            _ => return None,
        })
    }

    /// `deserialize_with` coercer.
    pub(crate) fn opt_deserialize<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::parse(&api::read(deserializer)?))
    }
}

/// A `*_BRACE_STYLE` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IjBraceStyle {
    /// `end_of_line` / `1` — K&R.
    EndOfLine,
    /// `next_line` / `2` — Allman.
    NextLine,
    /// `whitesmiths` / `3` — brace and body both shifted.
    Whitesmiths,
    /// `gnu` / `4` — braces shifted, body not.
    Gnu,
    /// `next_line_if_wrapped` / `5`.
    NextLineIfWrapped,
}

impl IjBraceStyle {
    /// Parse either representation.
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "end_of_line" | "1" => Self::EndOfLine,
            "next_line" | "2" => Self::NextLine,
            "whitesmiths" | "3" => Self::Whitesmiths,
            "gnu" | "4" => Self::Gnu,
            "next_line_if_wrapped" | "5" => Self::NextLineIfWrapped,
            _ => return None,
        })
    }

    /// `deserialize_with` coercer.
    pub(crate) fn opt_deserialize<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::parse(&api::read(deserializer)?))
    }
}

/// A `*_BRACE_FORCE` value — a third, unrelated table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IjForceBraces {
    /// `never` / `0` — `DO_NOT_FORCE`.
    Never,
    /// `if_multiline` / `1` — `FORCE_BRACES_IF_MULTILINE`.
    IfMultiline,
    /// `always` / `3` — `FORCE_BRACES_ALWAYS`.
    Always,
}

impl IjForceBraces {
    /// Parse either representation.
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "never" | "0" => Self::Never,
            "if_multiline" | "1" => Self::IfMultiline,
            "always" | "3" => Self::Always,
            _ => return None,
        })
    }

    /// `deserialize_with` coercer.
    pub(crate) fn opt_deserialize<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::parse(&api::read(deserializer)?))
    }
}

/// One entry of a `PackageEntryTable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageEntry {
    /// A package prefix. `name` is empty for IntelliJ's "all other imports" catch-all entry.
    Package {
        /// The package name, or empty for the catch-all.
        name: String,
        /// `withSubpackages="true"` — spelled `name.**` in `.editorconfig`, `name.*` without.
        with_subpackages: bool,
        /// `static="true"` — spelled with a leading `$`.
        is_static: bool,
        /// `module="true"` — the "all module imports" row (`import module M;`). IntelliJ's
        /// `.editorconfig` serializer has no token for it, so the XML reader marks it with a
        /// leading `%`, which no package name can start with.
        is_module: bool,
    },
    /// `<emptyLine/>` / `|` — a blank line between groups.
    BlankLine,
}

/// An ordered `PackageEntryTable` (`IMPORT_LAYOUT_TABLE`, `PACKAGES_TO_USE_IMPORT_ON_DEMAND`).
///
/// XML spells it as `<package .../>` and `<emptyLine/>` children; `.editorconfig` spells it as a
/// comma-separated mini-list (`$*, |, java.**, |, *`). The XML reader lowers its form to the
/// mini-list, so one parser serves both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageEntryTable(pub Vec<PackageEntry>);

impl PackageEntryTable {
    /// Parse the `.editorconfig` mini-list form.
    fn parse(value: &str) -> Self {
        let mut entries = Vec::new();
        for raw in value.split(',') {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            if entry == "|" {
                entries.push(PackageEntry::BlankLine);
                continue;
            }
            let (is_module, entry) = entry
                .strip_prefix('%')
                .map_or((false, entry), |rest| (true, rest));
            let (is_static, entry) = entry
                .strip_prefix('$')
                .map_or((false, entry), |rest| (true, rest));
            let (with_subpackages, name) = entry.strip_suffix("**").map_or_else(
                || {
                    entry
                        .strip_suffix('*')
                        .map_or((false, entry), |name| (false, name))
                },
                |name| (true, name),
            );
            entries.push(PackageEntry::Package {
                name: name.trim_end_matches('.').to_owned(),
                with_subpackages,
                is_static,
                is_module,
            });
        }
        Self(entries)
    }

    /// `deserialize_with` coercer.
    pub(crate) fn opt_deserialize<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = api::read(deserializer)?;
        Ok(if value.is_empty() {
            None
        } else {
            Some(Self::parse(&value))
        })
    }
}
