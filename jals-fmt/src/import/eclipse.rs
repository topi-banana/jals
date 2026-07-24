//! Eclipse JDT importer.
//!
//! Eclipse exposes ~400 settings under the `org.eclipse.jdt.core.formatter.` id namespace, shared
//! by both file forms — the `.settings/org.eclipse.jdt.core.prefs` properties file and the
//! exported XML profile (`<setting id=… value=…/>`). Both lower to the same `key → value` map, so
//! one [`EclipseConfig`] model serves both ([`EclipsePrefs`] / [`EclipseXmlProfile`]).
//!
//! Only the subset with a jals equivalent is modeled. The settings retained here are the common
//! rules — indentation, width, brace placement, blank lines, colon spacing, binary-operator wrap,
//! parameter-list wrap, annotation newlines, final newline — each typed to its native vocabulary
//! (DESIGN §12.2 / §A.3): brace positions are an enum, insert-space toggles are the
//! `insert` / `do not insert` enum (not a bool), and `alignment_for_*` is a **bitmask**
//! ([`Alignment`]) whose *bits* — not the integer as an opaque id — carry the wrap policy.

use jals_config::fmt::{
    BinopSeparator, BraceStyle, Config, ControlBraceStyle, FnParamsLayout, IndentStyle,
};
use serde::{Deserialize, Deserializer};

use super::serde_kv::Kv;
use super::{ConfigImporter, ImportError};

/// `org.eclipse.jdt.core.formatter.tabulation.char`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabChar {
    /// Indent with real tab characters.
    Tab,
    /// Indent with spaces.
    Space,
    /// Tabs for indentation, spaces for alignment. Mapped to tabs on the jals side.
    Mixed,
}

/// A brace-position setting (`brace_position_for_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracePosition {
    /// K&R — the brace hugs the header line.
    EndOfLine,
    /// Allman — the brace on its own line.
    NextLine,
    /// The brace on its own line, indented one level.
    NextLineShifted,
    /// On its own line only when the declaration wraps.
    NextLineOnWrap,
}

impl BracePosition {
    /// Whether this position puts the brace on the header line (K&R). Every next-line variant —
    /// including the wrap-conditional one — maps to jals's next-line, which has no conditional
    /// form.
    const fn is_same_line(self) -> bool {
        matches!(self, Self::EndOfLine | Self::NextLineOnWrap)
    }

    const fn brace_style(self) -> BraceStyle {
        if self.is_same_line() {
            BraceStyle::SameLine
        } else {
            BraceStyle::NextLine
        }
    }

    const fn control_brace_style(self) -> ControlBraceStyle {
        if self.is_same_line() {
            ControlBraceStyle::SameLine
        } else {
            ControlBraceStyle::NextLine
        }
    }
}

/// An `insert_space_*` / `insert_new_line_*` toggle. Eclipse spells these `insert` /
/// `do not insert` (note the interior spaces) — a two-valued enum, *not* a bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Insert {
    /// `insert`.
    #[serde(rename = "insert")]
    Insert,
    /// `do not insert`.
    #[serde(rename = "do not insert")]
    DoNotInsert,
}

impl Insert {
    const fn is_insert(self) -> bool {
        matches!(self, Self::Insert)
    }
}

/// A `true` / `false` toggle (`wrap_before_binary_operator` and friends are real booleans, unlike
/// the `insert` toggles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EclipseBool {
    /// `true`.
    True,
    /// `false`.
    False,
}

impl EclipseBool {
    const fn is_true(self) -> bool {
        matches!(self, Self::True)
    }
}

/// An `alignment_for_*` value: a decimal integer whose **bits** encode the wrap policy
/// (DESIGN §A.3.1). It is *not* an opaque id — the split mode lives in `SPLIT_MASK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment(pub u32);

impl Alignment {
    /// `M_FORCE` — wrapping is forced rather than only-if-too-long.
    pub const FORCE: u32 = 1;
    /// The bits (`0x70`) that hold the split mode.
    pub const SPLIT_MASK: u32 = 0x70;
    /// `M_COMPACT_SPLIT` — fill as many items per line as fit.
    pub const COMPACT: u32 = 16;
    /// `M_COMPACT_FIRST_BREAK_SPLIT` — like compact, breaking before the first element.
    pub const COMPACT_FIRST_BREAK: u32 = 32;
    /// `M_ONE_PER_LINE_SPLIT` — one item per line.
    pub const ONE_PER_LINE: u32 = 48;
    /// `M_NEXT_SHIFTED_SPLIT` — one per line, continuation shifted.
    pub const NEXT_SHIFTED: u32 = 64;
    /// `M_NEXT_PER_LINE_SPLIT` — every item after the first on its own line.
    pub const NEXT_PER_LINE: u32 = 80;
    /// `Integer.MAX_VALUE`, Eclipse's sentinel for "never wrap here".
    pub const NEVER: u32 = 0x7fff_ffff;

    /// The list layout this alignment implies for jals's `Tall` / `Compressed` / `Vertical`
    /// vocabulary. The never-wrap sentinel and the plain no-alignment value both keep the
    /// all-or-nothing `Tall` default.
    const fn params_layout(self) -> FnParamsLayout {
        if self.0 == Self::NEVER {
            return FnParamsLayout::Tall;
        }
        match self.0 & Self::SPLIT_MASK {
            Self::COMPACT | Self::COMPACT_FIRST_BREAK => FnParamsLayout::Compressed,
            Self::ONE_PER_LINE | Self::NEXT_SHIFTED | Self::NEXT_PER_LINE => {
                FnParamsLayout::Vertical
            }
            _ => FnParamsLayout::Tall,
        }
    }

    /// `deserialize_with` coercer: a decimal `alignment_for_*` string into `Option<Alignment>`.
    fn opt_deserialize<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Kv::opt_number(deserializer)?.map(Self))
    }
}

/// The modeled subset of an Eclipse JDT formatter profile.
///
/// Field names bind to the full setting id via `#[serde(rename)]`, so the same struct
/// deserializes from a `.prefs` map and an exported-XML map. Every field is optional: an absent
/// setting leaves the corresponding jals option at its default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct EclipseConfig {
    /// `tabulation.char`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.tabulation.char",
        deserialize_with = "Kv::opt_enum"
    )]
    pub tab_char: Option<TabChar>,
    /// `tabulation.size`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.tabulation.size",
        deserialize_with = "Kv::opt_number"
    )]
    pub tab_size: Option<usize>,
    /// `continuation_indentation` — in *indentation units*, not columns.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.continuation_indentation",
        deserialize_with = "Kv::opt_number"
    )]
    pub continuation_indentation: Option<usize>,
    /// `lineSplit` — the column limit (camelCase, unlike the rest).
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.lineSplit",
        deserialize_with = "Kv::opt_number"
    )]
    pub line_split: Option<usize>,
    /// `number_of_empty_lines_to_preserve`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.number_of_empty_lines_to_preserve",
        deserialize_with = "Kv::opt_number"
    )]
    pub empty_lines_to_preserve: Option<usize>,
    /// `brace_position_for_type_declaration`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_type_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub brace_type: Option<BracePosition>,
    /// `brace_position_for_method_declaration` (fallback for `brace_type`).
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_method_declaration",
        deserialize_with = "Kv::opt_enum"
    )]
    pub brace_method: Option<BracePosition>,
    /// `brace_position_for_block` — governs control-flow braces on the jals side.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.brace_position_for_block",
        deserialize_with = "Kv::opt_enum"
    )]
    pub brace_block: Option<BracePosition>,
    /// `insert_space_before_colon_in_conditional` (representative colon context).
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_before_colon_in_conditional",
        deserialize_with = "Kv::opt_enum"
    )]
    pub space_before_colon: Option<Insert>,
    /// `insert_space_after_colon_in_conditional`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_conditional",
        deserialize_with = "Kv::opt_enum"
    )]
    pub space_after_colon: Option<Insert>,
    /// `wrap_before_binary_operator` — `true` ⇒ operator leads the wrapped line.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.wrap_before_binary_operator",
        deserialize_with = "Kv::opt_enum"
    )]
    pub wrap_before_binary_operator: Option<EclipseBool>,
    /// `alignment_for_parameters_in_method_declaration` — the parameter-list wrap bitmask.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.alignment_for_parameters_in_method_declaration",
        deserialize_with = "Alignment::opt_deserialize"
    )]
    pub alignment_for_parameters: Option<Alignment>,
    /// `insert_new_line_after_annotation_on_type`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_after_annotation_on_type",
        deserialize_with = "Kv::opt_enum"
    )]
    pub newline_after_annotation_on_type: Option<Insert>,
    /// `insert_new_line_at_end_of_file_if_missing`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.insert_new_line_at_end_of_file_if_missing",
        deserialize_with = "Kv::opt_enum"
    )]
    pub final_newline: Option<Insert>,
}

impl From<EclipseConfig> for Config {
    fn from(native: EclipseConfig) -> Self {
        let mut config = Self::default();

        if let Some(tab) = native.tab_char {
            config.indent_style = match tab {
                TabChar::Space => IndentStyle::Space,
                TabChar::Tab | TabChar::Mixed => IndentStyle::Tab,
            };
        }
        if let Some(size) = native.tab_size {
            config.indent_width = size;
        }
        // Eclipse counts the continuation indent in indentation *units*; jals wants columns.
        // `saturating_mul` guards a pathological unit count (usize is 32-bit on wasm).
        if let Some(units) = native.continuation_indentation {
            config.continuation_indent = Some(units.saturating_mul(config.indent_width));
        }
        if let Some(width) = native.line_split {
            config.max_width = width;
        }
        if let Some(blank) = native.empty_lines_to_preserve {
            config.max_blank_lines = blank;
        }
        if let Some(brace) = native.brace_type.or(native.brace_method) {
            config.brace_style = brace.brace_style();
        }
        if let Some(brace) = native.brace_block {
            config.control_brace_style = brace.control_brace_style();
        }
        if let Some(space) = native.space_before_colon {
            config.space_before_colon = space.is_insert();
        }
        if let Some(space) = native.space_after_colon {
            config.space_after_colon = space.is_insert();
        }
        if let Some(wrap) = native.wrap_before_binary_operator {
            config.binop_separator = if wrap.is_true() {
                BinopSeparator::Front
            } else {
                BinopSeparator::Back
            };
        }
        if let Some(alignment) = native.alignment_for_parameters {
            config.fn_params_layout = alignment.params_layout();
        }
        if let Some(newline) = native.newline_after_annotation_on_type {
            config.annotation_placement = if newline.is_insert() {
                jals_config::fmt::AnnotationPlacement::Expanded
            } else {
                jals_config::fmt::AnnotationPlacement::Compact
            };
        }
        if let Some(final_newline) = native.final_newline {
            config.insert_final_newline = final_newline.is_insert();
        }

        config
    }
}

/// Importer for the `.settings/org.eclipse.jdt.core.prefs` properties file (portable).
#[derive(Debug, Clone, Copy, Default)]
pub struct EclipsePrefs;

impl ConfigImporter for EclipsePrefs {
    type Native = EclipseConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        Kv::from_pairs(super::text::Properties::parse(src))
    }
}

/// Importer for an exported Eclipse XML formatter profile. Needs the XML reader, so it is gated
/// behind the `std` feature.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct EclipseXmlProfile;

#[cfg(feature = "std")]
impl ConfigImporter for EclipseXmlProfile {
    type Native = EclipseConfig;

    fn parse(src: &str) -> Result<Self::Native, ImportError> {
        Kv::from_pairs(super::xml::EclipseProfileReader::parse(src)?)
    }
}
