//! `[layout]` — indentation, the column limit, and the line-level output shape.
//!
//! The settings every target agrees *exist* but disagrees on: indent unit and width, the
//! continuation indent, the column limit, the line terminator, and the formatter on/off region
//! markers. See `jals-fmt/MAPPING.md` §5.1 for the per-vendor correspondence.

use alloc::borrow::ToOwned;
use alloc::string::String;

use serde::Deserialize;

/// How one indentation level is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndentStyle {
    /// `indent-width` spaces per level. Eclipse `tabulation.char = space`.
    Space,
    /// One tab per level. Eclipse `tabulation.char = tab`.
    Tab,
    /// Tabs up to the last full tab stop, spaces for the remainder — Eclipse
    /// `tabulation.char = mixed` / IntelliJ `SMART_TABS`. Indentation is measured in columns and
    /// only the *rendering* differs from [`Space`](Self::Space).
    Mixed,
}

/// The line terminator emitted by the formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
    /// Detect from the input: the first line break decides, falling back to
    /// [`Native`](Self::Native) for a source with no break.
    Auto,
    /// The host platform's terminator (`\r\n` on Windows, `\n` elsewhere).
    Native,
}

impl LineEnding {
    /// Resolve to a concrete terminator, consulting `src` for [`Auto`](Self::Auto).
    #[must_use]
    pub fn resolve(self, src: &str) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
            Self::Native => Self::native(),
            Self::Auto => Self::detect(src),
        }
    }

    /// The host platform's terminator. A compile-time `cfg`, so `wasm32` resolves to `\n` with
    /// no platform IO.
    const fn native() -> &'static str {
        if cfg!(windows) { "\r\n" } else { "\n" }
    }

    /// Auto-detect from `src`: the first `\n` decides (`\r\n` ⇒ Windows, a bare `\n` ⇒ Unix).
    fn detect(src: &str) -> &'static str {
        match src.find('\n') {
            Some(pos) if src.as_bytes()[..pos].last() == Some(&b'\r') => "\r\n",
            Some(_) => "\n",
            None => Self::native(),
        }
    }
}

/// The default text marking the start of a formatter-disabled region.
const DEFAULT_OFF_TAG: &str = "@formatter:off";
/// The default text marking the end of a formatter-disabled region.
const DEFAULT_ON_TAG: &str = "@formatter:on";

/// Indentation, width, and line-level output settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
#[allow(clippy::struct_excessive_bools)]
pub struct Layout {
    /// How one indentation level is rendered.
    pub indent_style: IndentStyle,
    /// Columns per indentation level. Eclipse `tabulation.size` (or `indentation.size` under
    /// `mixed`) / IntelliJ `INDENT_SIZE` / EditorConfig `indent_size`.
    pub indent_width: usize,
    /// Display width of a literal tab character. Distinct from
    /// [`indent_width`](Self::indent_width): under [`IndentStyle::Mixed`] one level is
    /// `indent-width` columns rendered with `tab-width`-wide tabs. Eclipse `tabulation.size` /
    /// IntelliJ `TAB_SIZE` / EditorConfig `tab_width`.
    pub tab_width: usize,
    /// Columns to indent a continuation line (the extra lines an expression or statement
    /// produces when it wraps). `None` falls back to [`indent_width`](Self::indent_width).
    /// Eclipse `continuation_indentation` (counted in *levels*) / IntelliJ
    /// `CONTINUATION_INDENT_SIZE` (columns).
    pub continuation_indent: Option<usize>,
    /// Columns to indent a labeled statement's label. IntelliJ `LABEL_INDENT_SIZE`; a negative
    /// IntelliJ `LABEL_INDENT_ABSOLUTE` (label at column 0) is not representable and clamps here.
    pub label_indent: usize,
    /// Target line width for code. Eclipse `lineSplit` / IntelliJ `RIGHT_MARGIN` /
    /// EditorConfig `max_line_length`.
    pub max_width: usize,
    /// Line terminator to emit. IntelliJ `LINE_SEPARATOR` / EditorConfig `end_of_line`.
    pub line_ending: LineEnding,
    /// Ensure the output ends with exactly one newline. Eclipse
    /// `insert_new_line_at_end_of_file_if_missing` / Spotless `endWithNewline()`.
    pub insert_final_newline: bool,
    /// Strip trailing spaces and tabs from every line. Spotless `trimTrailingWhitespace()` /
    /// EditorConfig `trim_trailing_whitespace`.
    pub trim_trailing_whitespace: bool,
    /// Keep indentation on an otherwise blank line. Eclipse `indent_empty_lines` / IntelliJ
    /// `KEEP_INDENTS_ON_EMPTY_LINES`. Interacts with
    /// [`trim_trailing_whitespace`](Self::trim_trailing_whitespace), which wins when both are on.
    pub indent_empty_lines: bool,
    /// Indent `case` / `default` labels one level from their `switch`. Eclipse
    /// `indent_switchstatements_compare_to_switch` / IntelliJ `INDENT_CASE_FROM_SWITCH`.
    pub indent_switch_labels: bool,
    /// Indent a legacy (colon-form) `case` group's statements one level from their label.
    /// Eclipse `indent_switchstatements_compare_to_cases` / IntelliJ `INDENT_BREAK_FROM_CASE`.
    pub indent_switch_case_body: bool,
    /// Indent a type's members one level from its header. Eclipse
    /// `indent_body_declarations_compare_to_type_header` / IntelliJ
    /// `DO_NOT_INDENT_TOP_LEVEL_CLASS_MEMBERS` (inverted).
    pub indent_type_members: bool,
    /// Honor the formatter on/off markers below, leaving the enclosed region byte-identical.
    /// Eclipse `use_on_off_tags` / IntelliJ `FORMATTER_TAGS_ENABLED` / Spotless `toggleOffOn()`.
    pub formatter_tags: bool,
    /// Comment text that starts a formatter-disabled region. Eclipse `disabling_tag` /
    /// IntelliJ `FORMATTER_OFF_TAG`.
    pub formatter_off_tag: String,
    /// Comment text that ends a formatter-disabled region. Eclipse `enabling_tag` /
    /// IntelliJ `FORMATTER_ON_TAG`.
    pub formatter_on_tag: String,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            indent_style: IndentStyle::Space,
            indent_width: 4,
            tab_width: 4,
            continuation_indent: None,
            label_indent: 0,
            max_width: 100,
            line_ending: LineEnding::Lf,
            insert_final_newline: true,
            trim_trailing_whitespace: true,
            indent_empty_lines: false,
            indent_switch_labels: true,
            indent_switch_case_body: true,
            indent_type_members: true,
            formatter_tags: false,
            formatter_off_tag: DEFAULT_OFF_TAG.to_owned(),
            formatter_on_tag: DEFAULT_ON_TAG.to_owned(),
        }
    }
}

impl Layout {
    /// The number of display columns one indentation level occupies.
    #[must_use]
    pub fn indent_cols(&self) -> usize {
        self.indent_width.max(1)
    }

    /// The number of display columns one *continuation* indent occupies, falling back to
    /// [`indent_width`](Self::indent_width) when unset.
    #[must_use]
    pub fn continuation_cols(&self) -> usize {
        self.continuation_indent.unwrap_or(self.indent_width).max(1)
    }

    /// The resolved line terminator for input `src`, honoring
    /// [`Auto`](LineEnding::Auto) / [`Native`](LineEnding::Native).
    #[must_use]
    pub fn newline(&self, src: &str) -> &'static str {
        self.line_ending.resolve(src)
    }
}
