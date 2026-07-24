//! IntelliJ IDEA importer.
//!
//! IntelliJ has ~270 Java code-style settings across two files with **different encodings** of the
//! same enums (DESIGN §12.3 / §A.4): `.editorconfig` uses lowercase *token* values under `ij_java_*`
//! keys, while `.idea/codeStyles/Project.xml` uses raw *integers* under `UPPER_SNAKE` option names,
//! with a **per-property** int→token table (wrap ≠ brace ≠ force-braces — never one table reused).
//!
//! [`IntellijConfig`] models the editorconfig shape (token-valued), because that is the format
//! serde can read directly. The XML importer ([`IntellijXmlScheme`], `std`-gated) does not need a
//! second model: it normalizes each raw int to its editorconfig token — reusing the portable
//! `IjWrap::token_from_int` / `IjBraceStyle::token_from_int` / `IjEndOfLine::token_from_str` tables
//! here — and its key to the `ij_java_*` spelling, then feeds the same struct.
//!
//! Only the common-rule subset is modeled. IntelliJ's input-whitespace–dependent `keep_*` behavior
//! and its classpath-dependent wildcard import collapse have no jals counterpart and are dropped.

// Native product / token names (IntelliJ, editorconfig, …) recur throughout the docs as prose.
#![allow(clippy::doc_markdown)]

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::{
    AnnotationPlacement, BinopSeparator, BraceStyle, Config, FnParamsLayout, IndentStyle,
    LineEnding,
};
use serde::Deserialize;

use super::serde_kv::Kv;
use super::{ConfigImporter, ImportError};

/// `indent_style` (universal editorconfig key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IjIndentStyle {
    /// Spaces.
    Space,
    /// Tabs.
    Tab,
}

/// `end_of_line` (universal editorconfig key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IjEndOfLine {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
    /// `\r` — jals has no bare-CR terminator, so it falls back to LF.
    Cr,
}

impl IjEndOfLine {
    /// Translate a raw XML `LINE_SEPARATOR` value to its editorconfig token.
    #[cfg(feature = "std")]
    pub(crate) fn token_from_str(value: &str) -> Option<&'static str> {
        Some(match value {
            "\n" => "lf",
            "\r\n" => "crlf",
            "\r" => "cr",
            _ => return None,
        })
    }
}

/// A brace-style token (`ij_java_*_brace_style`). Note IntelliJ's own vocabulary: `whitesmiths`
/// and `gnu` (both next-line variants), *not* Eclipse's `next_line_shifted` (DESIGN §A.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IjBraceStyle {
    /// K&R.
    EndOfLine,
    /// Allman.
    NextLine,
    /// On its own line only when the header wraps — treated as same-line (no conditional form).
    NextLineIfWrapped,
    /// Whitesmiths — a next-line variant.
    Whitesmiths,
    /// GNU — a next-line variant.
    Gnu,
}

impl IjBraceStyle {
    const fn brace_style(self) -> BraceStyle {
        match self {
            Self::EndOfLine | Self::NextLineIfWrapped => BraceStyle::SameLine,
            Self::NextLine | Self::Whitesmiths | Self::Gnu => BraceStyle::NextLine,
        }
    }

    /// Translate a raw XML `*_BRACE_STYLE` integer to its editorconfig token (DESIGN §A.4.2).
    #[cfg(feature = "std")]
    pub(crate) const fn token_from_int(value: i64) -> Option<&'static str> {
        Some(match value {
            1 => "end_of_line",
            2 => "next_line",
            3 => "whitesmiths",
            4 => "gnu",
            5 => "next_line_if_wrapped",
            _ => return None,
        })
    }
}

/// A wrap token (`ij_java_*_wrap` / `ij_java_*_annotation_wrap`).
///
/// The spelling is counter-intuitive (DESIGN §A.4.2): `split_into_lines` means *Wrap Always* and
/// `on_every_item` means *Chop Down If Long* — both one-item-per-line on the jals side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IjWrap {
    /// Do not wrap.
    Off,
    /// Wrap only if the line is too long (all-or-nothing).
    Normal,
    /// Always wrap — one item per line.
    SplitIntoLines,
    /// Chop down if long — one item per line once it overflows.
    OnEveryItem,
}

impl IjWrap {
    /// The two `*_into_lines` / `*_every_item` forms lay items out one per line; the others keep
    /// them together.
    const fn params_layout(self) -> FnParamsLayout {
        match self {
            Self::SplitIntoLines | Self::OnEveryItem => FnParamsLayout::Vertical,
            Self::Off | Self::Normal => FnParamsLayout::Tall,
        }
    }

    const fn annotation_placement(self) -> AnnotationPlacement {
        match self {
            Self::Off => AnnotationPlacement::Compact,
            Self::Normal | Self::SplitIntoLines | Self::OnEveryItem => {
                AnnotationPlacement::Expanded
            }
        }
    }

    /// Translate a raw XML `*_WRAP` integer to its editorconfig token (DESIGN §A.4.2). `5` is a
    /// lossy duplicate of `on_every_item` (`4`).
    #[cfg(feature = "std")]
    pub(crate) const fn token_from_int(value: i64) -> Option<&'static str> {
        Some(match value {
            0 => "off",
            1 => "normal",
            2 => "split_into_lines",
            4 | 5 => "on_every_item",
            _ => return None,
        })
    }
}

/// The modeled subset of an IntelliJ Java code style, in its `.editorconfig` shape.
///
/// Field names bind to the lowercase editorconfig keys via `#[serde(rename)]`; the XML importer
/// translates its `UPPER_SNAKE` names + integer values into this shape before deserializing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct IntellijConfig {
    /// `indent_style`.
    #[serde(rename = "indent_style", deserialize_with = "Kv::opt_enum")]
    pub indent_style: Option<IjIndentStyle>,
    /// `indent_size`.
    #[serde(rename = "indent_size", deserialize_with = "Kv::opt_number")]
    pub indent_size: Option<usize>,
    /// `ij_continuation_indent_size`.
    #[serde(
        rename = "ij_continuation_indent_size",
        deserialize_with = "Kv::opt_number"
    )]
    pub continuation_indent_size: Option<usize>,
    /// `max_line_length`.
    #[serde(rename = "max_line_length", deserialize_with = "Kv::opt_number")]
    pub max_line_length: Option<usize>,
    /// `end_of_line`.
    #[serde(rename = "end_of_line", deserialize_with = "Kv::opt_enum")]
    pub end_of_line: Option<IjEndOfLine>,
    /// `insert_final_newline`.
    #[serde(rename = "insert_final_newline", deserialize_with = "Kv::opt_bool")]
    pub insert_final_newline: Option<bool>,
    /// `ij_java_keep_blank_lines_in_code`.
    #[serde(
        rename = "ij_java_keep_blank_lines_in_code",
        deserialize_with = "Kv::opt_number"
    )]
    pub keep_blank_lines_in_code: Option<usize>,
    /// `ij_java_class_brace_style`.
    #[serde(
        rename = "ij_java_class_brace_style",
        deserialize_with = "Kv::opt_enum"
    )]
    pub class_brace_style: Option<IjBraceStyle>,
    /// `ij_java_method_brace_style` (fallback for `class_brace_style`).
    #[serde(
        rename = "ij_java_method_brace_style",
        deserialize_with = "Kv::opt_enum"
    )]
    pub method_brace_style: Option<IjBraceStyle>,
    /// `ij_java_space_before_colon`.
    #[serde(
        rename = "ij_java_space_before_colon",
        deserialize_with = "Kv::opt_bool"
    )]
    pub space_before_colon: Option<bool>,
    /// `ij_java_space_after_colon`.
    #[serde(
        rename = "ij_java_space_after_colon",
        deserialize_with = "Kv::opt_bool"
    )]
    pub space_after_colon: Option<bool>,
    /// `ij_java_binary_operation_sign_on_next_line` — `true` ⇒ operator leads the wrapped line.
    #[serde(
        rename = "ij_java_binary_operation_sign_on_next_line",
        deserialize_with = "Kv::opt_bool"
    )]
    pub binary_operation_sign_on_next_line: Option<bool>,
    /// `ij_java_method_parameters_wrap`.
    #[serde(
        rename = "ij_java_method_parameters_wrap",
        deserialize_with = "Kv::opt_enum"
    )]
    pub method_parameters_wrap: Option<IjWrap>,
    /// `ij_java_class_annotation_wrap`.
    #[serde(
        rename = "ij_java_class_annotation_wrap",
        deserialize_with = "Kv::opt_enum"
    )]
    pub class_annotation_wrap: Option<IjWrap>,
    /// `ij_java_method_annotation_wrap` (fallback for `class_annotation_wrap`).
    #[serde(
        rename = "ij_java_method_annotation_wrap",
        deserialize_with = "Kv::opt_enum"
    )]
    pub method_annotation_wrap: Option<IjWrap>,
    /// `ij_java_imports_layout` — the ordered import-group mini-list (see
    /// `Self::parse_imports_layout`).
    #[serde(rename = "ij_java_imports_layout")]
    pub imports_layout: Option<String>,
}

impl From<IntellijConfig> for Config {
    fn from(native: IntellijConfig) -> Self {
        let mut config = Self::default();

        if let Some(style) = native.indent_style {
            config.indent_style = match style {
                IjIndentStyle::Space => IndentStyle::Space,
                IjIndentStyle::Tab => IndentStyle::Tab,
            };
        }
        if let Some(size) = native.indent_size {
            config.indent_width = size;
        }
        if let Some(size) = native.continuation_indent_size {
            config.continuation_indent = Some(size);
        }
        if let Some(width) = native.max_line_length {
            config.max_width = width;
        }
        if let Some(eol) = native.end_of_line {
            config.line_ending = match eol {
                IjEndOfLine::Crlf => LineEnding::Crlf,
                IjEndOfLine::Lf | IjEndOfLine::Cr => LineEnding::Lf,
            };
        }
        if let Some(final_newline) = native.insert_final_newline {
            config.insert_final_newline = final_newline;
        }
        if let Some(blank) = native.keep_blank_lines_in_code {
            config.max_blank_lines = blank;
        }
        if let Some(brace) = native.class_brace_style.or(native.method_brace_style) {
            config.brace_style = brace.brace_style();
        }
        if let Some(space) = native.space_before_colon {
            config.space_before_colon = space;
        }
        if let Some(space) = native.space_after_colon {
            config.space_after_colon = space;
        }
        if let Some(on_next_line) = native.binary_operation_sign_on_next_line {
            config.binop_separator = if on_next_line {
                BinopSeparator::Front
            } else {
                BinopSeparator::Back
            };
        }
        if let Some(wrap) = native.method_parameters_wrap {
            config.fn_params_layout = wrap.params_layout();
        }
        if let Some(wrap) = native
            .class_annotation_wrap
            .or(native.method_annotation_wrap)
        {
            config.annotation_placement = wrap.annotation_placement();
        }
        if let Some(layout) = &native.imports_layout {
            let groups = IntellijConfig::parse_imports_layout(layout);
            if !groups.is_empty() {
                config.group_imports = true;
                config.import_groups = groups;
            }
        }

        config
    }
}

impl IntellijConfig {
    /// Parse IntelliJ's `ij_java_imports_layout` mini-list into jals import-group prefixes.
    ///
    /// The value is a comma-separated list of entries: `$name` = a static-import group, `|` = a
    /// blank line (dropped — jals blanks every group), a wildcard like `java.**` = the prefix
    /// `java.`, and a bare `*` = the catch-all. Example: `$*, |, java.**, |, *` →
    /// `["static", "java.", "*"]`.
    fn parse_imports_layout(value: &str) -> Vec<String> {
        let mut groups = Vec::new();
        for raw in value.split(',') {
            let entry = raw.trim();
            if entry.is_empty() || entry == "|" {
                continue;
            }
            if entry.starts_with('$') {
                // A static-import group (`$*` = every static import). jals collapses these to one
                // `"static"` group regardless of the pattern.
                if !groups.iter().any(|g| g == "static") {
                    groups.push("static".to_owned());
                }
                continue;
            }
            let prefix = entry
                .strip_suffix("**")
                .or_else(|| entry.strip_suffix('*'))
                .unwrap_or(entry);
            // An entry that was nothing but a wildcard is the catch-all group.
            groups.push(if prefix.is_empty() { "*" } else { prefix }.to_owned());
        }
        groups
    }
}

/// Importer for the `.editorconfig` (`ij_java_*`) form (portable).
#[derive(Debug, Clone, Copy, Default)]
pub struct IntellijEditorConfig;

impl ConfigImporter for IntellijEditorConfig {
    type Native = IntellijConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        Kv::from_pairs(super::text::EditorConfig::parse(src))
    }
}

/// Importer for the `.idea/codeStyles/Project.xml` / exported scheme form. Needs the XML reader,
/// so it is gated behind the `std` feature.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct IntellijXmlScheme;

#[cfg(feature = "std")]
impl ConfigImporter for IntellijXmlScheme {
    type Native = IntellijConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        Kv::from_pairs(super::xml::IntellijSchemeReader::parse(src)?)
    }
}
