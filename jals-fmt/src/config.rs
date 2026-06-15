//! Formatting configuration, deserialized from `jalsfmt.toml`.
//!
//! Every key is optional; omitted keys fall back to [`Config::default`]. Keys use
//! kebab-case (e.g. `indent-style`, `max-blank-lines`).

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// How to render a single indentation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndentStyle {
    /// Indent with spaces (`indent-width` spaces per level).
    Space,
    /// Indent with a single tab per level.
    Tab,
}

/// The line terminator emitted by the formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
    /// Detect from the input: the first line break in the source decides (`\r\n` ⇒ Windows,
    /// a bare `\n` ⇒ Unix). A source with no line break falls back to
    /// [`Native`](Self::Native). Mirrors rustfmt's `newline_style = "Auto"`.
    Auto,
    /// The host platform's native terminator (`\r\n` on Windows, `\n` elsewhere). Mirrors
    /// rustfmt's `newline_style = "Native"`.
    Native,
}

impl LineEnding {
    /// Resolve to a concrete terminator string, consulting `src` for [`Auto`](Self::Auto).
    pub(crate) fn resolve(self, src: &str) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Native => Self::native(),
            LineEnding::Auto => Self::detect(src),
        }
    }

    /// The host platform's native terminator. Compile-time `cfg`, so `wasm32` (which is not
    /// Windows) resolves to `\n` without any platform IO.
    fn native() -> &'static str {
        if cfg!(windows) { "\r\n" } else { "\n" }
    }

    /// Auto-detect from `src`: the first `\n` decides (`\r\n` ⇒ Windows, a bare `\n` ⇒ Unix).
    /// A source with no `\n` falls back to the platform native terminator.
    fn detect(src: &str) -> &'static str {
        match src.find('\n') {
            Some(pos) if src.as_bytes()[..pos].last() == Some(&b'\r') => "\r\n",
            Some(_) => "\n",
            None => Self::native(),
        }
    }
}

/// Where the opening brace of a declaration body is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BraceStyle {
    /// K&R: the opening brace stays on the header's line (`class Foo {`). The default.
    SameLine,
    /// Allman: the opening brace goes on its own line, aligned under the header
    /// (`class Foo` then `{`). Mirrors rustfmt's `brace_style = "AlwaysNextLine"`.
    NextLine,
}

/// How control-flow brace styling is laid out — the complement of [`BraceStyle`], covering
/// everything `brace-style` deliberately leaves alone. It governs two coupled junctions:
/// the opening brace of a control-flow block (`if`/`for`/`while`/`do`/`try`/`catch`/`finally`/
/// `synchronized`), a `switch` block, a lambda body, or a bare block; and the continuation
/// keyword that follows a closing brace (`} else`, `} catch`, `} finally`, `} while`).
/// Mirrors rustfmt's `control_brace_style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlBraceStyle {
    /// K&R: opening braces hug the header and continuations cuddle the closing brace
    /// (`if (x) {`, `} else {`). The default.
    SameLine,
    /// Allman: opening braces and continuations each take their own line:
    ///
    /// ```text
    /// if (x)
    /// {
    ///     …
    /// }
    /// else
    /// {
    /// ```
    NextLine,
}

/// How the formatter treats the optional trailing comma of an array initializer (`{1, 2, 3,}`).
/// Mirrors rustfmt's `trailing_comma`, plus a [`Preserve`](Self::Preserve) default (rustfmt has
/// no such mode) that keeps the source comma exactly, so the strict significant-token invariant
/// holds unless this is opted into. Only array initializers are affected — the sole Java
/// delimited list, besides enum constants, where a trailing comma is legal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrailingComma {
    /// Keep the source's trailing comma exactly (present stays present, absent stays absent).
    /// The default; preserves the significant-token sequence.
    Preserve,
    /// Always emit a trailing comma, adding one when the source lacks it.
    Always,
    /// Never emit a trailing comma, dropping one the source has — unless it carries a comment,
    /// which is kept so no comment is lost.
    Never,
    /// Emit a trailing comma only when the initializer is laid out vertically (one element per
    /// line), and omit it when the initializer fits on one line. Mirrors rustfmt's default.
    Vertical,
}

/// Where the operator of a binary expression sits when the expression wraps across lines.
/// Mirrors rustfmt's `binop_separator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BinopSeparator {
    /// The operator starts the continuation line (`a` then `+ b`). The default
    /// (rustfmt's default; also the Google/Sun Java convention of breaking before
    /// an operator).
    Front,
    /// The operator ends the line being broken (`a +` then `b`).
    Back,
}

/// Layout of a method / constructor parameter list (`PARAM_LIST`). Mirrors rustfmt's
/// `fn_params_layout` (formerly `fn_args_layout`, which jals accepts as a deprecated alias);
/// it applies only to declaration parameter lists, never to call argument lists. Layout-only —
/// the significant-token sequence is preserved exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FnParamsLayout {
    /// All-or-nothing: keep the parameters on one line when they fit
    /// [`max_width`](Config::max_width), otherwise lay them out one per line. The default;
    /// matches the prior behavior.
    Tall,
    /// Pack as many parameters per line as fit [`max_width`](Config::max_width), wrapping to a
    /// new line only when the next parameter would overflow. Mirrors rustfmt's `Compressed`.
    Compressed,
    /// Always one parameter per line, even when the whole list would fit on one line. Mirrors
    /// rustfmt's `Vertical`.
    Vertical,
}

/// Density of spacing around the `&` of a Java intersection type — a type-parameter bound
/// (`<T extends A & B>`) or a cast intersection (`(A & B) x`). Mirrors rustfmt's
/// `type_punctuation_density`. The bitwise-AND operator `&` (an expression) is never affected.
/// Layout-only — the significant-token sequence is preserved exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypePunctuationDensity {
    /// Space around `&` (`A & B`). The default; matches the prior behavior.
    Wide,
    /// No space around `&` (`A&B`). Mirrors rustfmt's `Compressed`.
    Compressed,
}

/// Placement of a declaration's leading annotations (the annotations in the `MODIFIERS` node of
/// a type / method / constructor / field / initializer / local-variable declaration). A
/// Java-specific option with no rustfmt equivalent. Layout-only — the significant-token
/// sequence is preserved exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationPlacement {
    /// Keep a declaration's leading annotations inline with the modifiers / declaration
    /// (`@Override public void m()`). The default; matches the prior behavior.
    Compact,
    /// Break each leading annotation onto its own line above the declaration (`@Override`,
    /// then `public void m()`). The idiomatic Java convention. Parameter annotations and
    /// type-use / enum-constant / type-parameter annotations are never affected.
    Expanded,
}

/// Case of the hexadecimal digit letters (`a`–`f` / `A`–`F`) of an integer or floating-point
/// literal — `0xFF` vs. `0xff`. Mirrors rustfmt's `hex_literal_case`, plus a
/// [`Preserve`](Self::Preserve) default (rustfmt's is `Preserve` too) that keeps the source
/// case exactly, so the strict significant-token invariant holds unless this is opted into.
///
/// Only the hex *mantissa* digits are affected. The `0x` / `0X` radix prefix, the `p` / `P`
/// binary exponent marker of a hex float and its decimal digits, and any `l` / `L` integer or
/// `f` / `F` / `d` / `D` float suffix are all left exactly as written (suffix-letter case is a
/// separate, not-yet-implemented Java-specific concern). Decimal, octal, and binary literals
/// have no hex digits and are never touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HexLiteralCase {
    /// Keep the source's hex-digit case exactly. The default; preserves the significant-token
    /// sequence.
    Preserve,
    /// Force hex digits to upper case (`0xff` → `0xFF`).
    Upper,
    /// Force hex digits to lower case (`0xFF` → `0xff`).
    Lower,
}

/// Formatter style settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Spaces vs. tab for indentation.
    pub indent_style: IndentStyle,
    /// Number of columns per indentation level (and spaces emitted when `indent_style` is `Space`).
    pub indent_width: usize,
    /// Runs of blank lines are collapsed down to at most this many.
    pub max_blank_lines: usize,
    /// Line terminator to emit.
    pub line_ending: LineEnding,
    /// Ensure the output ends with exactly one newline.
    pub insert_final_newline: bool,
    /// Target line width for wrapping code.
    pub max_width: usize,
    /// Maximum width of a method chain (`a.b().c().d()`) kept on a single line. A chain with
    /// at least two calls whose flat width exceeds this is laid out one call per line (it also
    /// wraps when it would overflow [`max_width`](Config::max_width)). Mirrors rustfmt's
    /// `chain_width`.
    pub chain_width: usize,
    /// Maximum width of a function or method call's argument list (`f(a, b, c)`) kept on a
    /// single line. A call whose argument list's flat width exceeds this is laid out one
    /// argument per line (it also wraps when it would overflow [`max_width`](Config::max_width)).
    /// Mirrors rustfmt's `fn_call_width`.
    pub fn_call_width: usize,
    /// Maximum width of an array initializer (`{a, b, c}`) kept on a single line. An
    /// initializer whose flat width exceeds this is laid out one element per line (it also
    /// wraps when it would overflow [`max_width`](Config::max_width)). Mirrors rustfmt's
    /// `array_width`.
    pub array_width: usize,
    /// Placement of the opening brace of a declaration body (type, method, constructor, or
    /// initializer): same line (K&R) or next line (Allman). Control-flow blocks
    /// (`if`/`for`/`while`/`try`/…), `switch`, and lambda bodies are governed separately by
    /// [`control_brace_style`](Config::control_brace_style).
    pub brace_style: BraceStyle,
    /// Collapse an empty declaration body — a type body (`class` / `interface` / `@interface` /
    /// record) or the block of a method, constructor, or initializer — to `{}` on the header's
    /// line. On by default. When off, such a body expands to a two-line `{` … `}` (opening on
    /// its own line under [`brace_style`](Config::brace_style) `next-line`). Control-flow /
    /// `switch` / lambda / bare blocks are never affected and always keep `{}`; `enum` bodies
    /// (not block-formatted yet) are likewise unaffected. Layout-only — the significant-token
    /// sequence is preserved exactly. Mirrors rustfmt's `empty_item_single_line`.
    pub empty_item_single_line: bool,
    /// Keep a declaration body — the block of a method, constructor, or initializer — on the
    /// header's line when it holds exactly one statement and no comments, e.g.
    /// `int foo() { return 1; }`. Off by default. When on, such a body collapses onto one line
    /// if it fits [`max_width`](Config::max_width); a body with two or more statements, a
    /// comment, a nested block, or one that would overflow `max-width` stays multi-line. The
    /// one-liner is emitted regardless of [`brace_style`](Config::brace_style) (like
    /// [`empty_item_single_line`](Config::empty_item_single_line)); only when it does not fit
    /// does the brace open on its own line under `next-line`. Layout-only — the
    /// significant-token sequence is preserved exactly. Mirrors rustfmt's `fn_single_line`.
    pub fn_single_line: bool,
    /// Layout of control-flow brace styling: the opening brace of a control-flow / `switch` /
    /// lambda / bare block, and the `} else` / `} catch` / `} finally` / `} while`
    /// continuations. Same line (K&R) or next line (Allman).
    pub control_brace_style: ControlBraceStyle,
    /// Reflow comments so no line exceeds [`comment_width`](Config::comment_width).
    /// Off by default; [`comment_width`](Config::comment_width) has no effect unless this
    /// is enabled (mirrors rustfmt's `wrap_comments`).
    pub wrap_comments: bool,
    /// Target line width for reflowing comment / Javadoc prose, including indentation.
    /// Only consulted when [`wrap_comments`](Config::wrap_comments) is enabled.
    pub comment_width: usize,
    /// Sort `import` declarations: non-static imports first (alphabetical by qualified name),
    /// then static imports (alphabetical). Off by default; opt-in like
    /// [`wrap_comments`](Config::wrap_comments). When enabled the formatter's significant-token
    /// *sequence* may change (the multiset is preserved); mirrors rustfmt's `reorder_imports`.
    pub reorder_imports: bool,
    /// How to treat the trailing comma of an array initializer (`{1, 2, 3,}`). Defaults to
    /// [`Preserve`](TrailingComma::Preserve), which keeps the source comma exactly; the other
    /// modes may add or drop that single comma, weakening the strict significant-token invariant
    /// (a comma carrying a comment is never dropped). Mirrors rustfmt's `trailing_comma`.
    pub trailing_comma: TrailingComma,
    /// Group `import` declarations into prefix-defined blocks separated by a blank line, each
    /// block sorted alphabetically by qualified name. The groups and their order come from
    /// [`import_groups`](Config::import_groups). Off by default; opt-in like
    /// [`reorder_imports`](Config::reorder_imports), preserving the significant-token *multiset*
    /// while the *sequence* may change. Implies and overrides
    /// [`reorder_imports`](Config::reorder_imports) — grouping sorts within each block. Mirrors
    /// rustfmt's `group_imports`.
    pub group_imports: bool,
    /// The ordered import groups consulted when [`group_imports`](Config::group_imports) is on:
    /// a list of name prefixes. A non-static import joins the group of its *longest* matching
    /// prefix (ties broken by list order); `"*"` is the catch-all for non-static imports matching
    /// no prefix, and `"static"` is the group of every static import (regardless of its name).
    /// A missing `"*"` / `"static"` becomes an implicit trailing group (catch-all, then static).
    /// Consulted only when `group_imports` is enabled.
    pub import_groups: Vec<String>,
    /// Placement of a binary operator when a binary expression wraps across lines: at the
    /// start of the continuation line ([`Front`](BinopSeparator::Front), the default) or at
    /// the end of the broken line ([`Back`](BinopSeparator::Back)). The wrapping itself is
    /// driven by [`max_width`](Config::max_width) alone. Mirrors rustfmt's `binop_separator`.
    pub binop_separator: BinopSeparator,
    /// Let the last item of a call argument list or annotation argument list hang past the
    /// call line when it is a delimited expression — a block-bodied lambda, an
    /// anonymous-class / array-creating `new`, an array initializer, or a `name = {…}`
    /// annotation pair: the earlier arguments stay on the call line and only the trailing
    /// body breaks (`f(a, () -> {` … `});`). Off by default, keeping the all-or-nothing
    /// layout. Layout-only — the significant-token sequence is preserved exactly. Mirrors
    /// rustfmt's `overflow_delimited_expr`.
    pub overflow_delimited_expr: bool,
    /// Whether to emit a space *before* a colon (`:`). Applies uniformly to every Java colon
    /// context: a ternary (`a ? b : c`), an enhanced `for` (`for (T x : xs)`), a labeled
    /// statement (`label:`), an `assert` message (`assert c : m`), and a `switch` `case` /
    /// `default` label (`case x:`). Off by default (no space before), matching idiomatic
    /// label / `case` style. The `::` method-reference token is a distinct token and is never
    /// affected. Layout-only — the significant-token sequence is preserved exactly. Mirrors
    /// rustfmt's `space_before_colon`.
    pub space_before_colon: bool,
    /// Whether to emit a space *after* a colon (`:`), in the same contexts as
    /// [`space_before_colon`](Config::space_before_colon). On by default. The `::`
    /// method-reference token is never affected. Layout-only. Mirrors rustfmt's
    /// `space_after_colon`.
    pub space_after_colon: bool,
    /// Layout of a method / constructor parameter list (`PARAM_LIST`):
    /// [`Tall`](FnParamsLayout::Tall) (the default, all-or-nothing),
    /// [`Compressed`](FnParamsLayout::Compressed) (pack as many per line as fit), or
    /// [`Vertical`](FnParamsLayout::Vertical) (always one per line). Applies only to
    /// declaration parameter lists, never to call argument lists. Layout-only — the
    /// significant-token sequence is preserved exactly. Mirrors rustfmt's `fn_params_layout`;
    /// the deprecated key `fn-args-layout` is accepted as an alias.
    #[serde(alias = "fn-args-layout")]
    pub fn_params_layout: FnParamsLayout,
    /// Density of spacing around the `&` of a Java intersection type — a type-parameter bound
    /// (`<T extends A & B>`) or a cast intersection (`(A & B) x`): [`Wide`](TypePunctuationDensity::Wide)
    /// (the default, `A & B`) or [`Compressed`](TypePunctuationDensity::Compressed) (`A&B`). The
    /// bitwise-AND operator `&` (an expression) is never affected. Layout-only — the
    /// significant-token sequence is preserved exactly. Mirrors rustfmt's `type_punctuation_density`.
    pub type_punctuation_density: TypePunctuationDensity,
    /// Reorder the keyword modifiers of every declaration (`public`, `static`, `final`, …) into
    /// a fixed canonical order (JLS / Checkstyle: public, protected, private, abstract, default,
    /// static, sealed, non-sealed, final, transient, volatile, synchronized, native, strictfp),
    /// hoisting all annotations to the front (keeping their relative order). Off by default;
    /// opt-in like [`reorder_imports`](Config::reorder_imports). When enabled the formatter's
    /// significant-token *sequence* may change (the multiset is preserved, and each comment stays
    /// glued to its modifier). A Java-specific option with no rustfmt equivalent.
    pub reorder_modifiers: bool,
    /// Placement of a declaration's leading annotations (the `MODIFIERS` node of a type / method
    /// / constructor / field / initializer / local-variable declaration):
    /// [`Compact`](AnnotationPlacement::Compact) (the default, inline `@Override public void m()`)
    /// or [`Expanded`](AnnotationPlacement::Expanded) (each annotation on its own line above the
    /// declaration). Parameter annotations (a `PARAM`'s own `MODIFIERS`) and type-use /
    /// enum-constant / type-parameter annotations are never affected — they always stay inline.
    /// Layout-only — the significant-token sequence is preserved exactly. A Java-specific option
    /// with no rustfmt equivalent.
    pub annotation_placement: AnnotationPlacement,
    /// Case of the hexadecimal digit letters of an integer / floating-point literal (`0xFF` vs.
    /// `0xff`). Defaults to [`Preserve`](HexLiteralCase::Preserve), which keeps the source case
    /// exactly; the other modes rewrite the case of the hex *mantissa* digits, weakening the
    /// strict significant-token invariant (a literal token's text — but never its kind — may
    /// change). The `0x` prefix, `p` exponent, and any `l` / `f` / `d` suffix are left untouched.
    /// Mirrors rustfmt's `hex_literal_case`.
    pub hex_literal_case: HexLiteralCase,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            indent_style: IndentStyle::Space,
            indent_width: 4,
            max_blank_lines: 1,
            line_ending: LineEnding::Lf,
            insert_final_newline: true,
            max_width: 100,
            chain_width: 60,
            fn_call_width: 60,
            array_width: 60,
            brace_style: BraceStyle::SameLine,
            empty_item_single_line: true,
            fn_single_line: false,
            control_brace_style: ControlBraceStyle::SameLine,
            wrap_comments: false,
            comment_width: 80,
            reorder_imports: false,
            trailing_comma: TrailingComma::Preserve,
            group_imports: false,
            import_groups: vec![
                "java.".to_string(),
                "javax.".to_string(),
                "*".to_string(),
                "static".to_string(),
            ],
            binop_separator: BinopSeparator::Front,
            overflow_delimited_expr: false,
            space_before_colon: false,
            space_after_colon: true,
            fn_params_layout: FnParamsLayout::Tall,
            type_punctuation_density: TypePunctuationDensity::Wide,
            reorder_modifiers: false,
            annotation_placement: AnnotationPlacement::Compact,
            hex_literal_case: HexLiteralCase::Preserve,
        }
    }
}

impl Config {
    /// One indentation level rendered as a string.
    pub(crate) fn indent_unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Tab => "\t".to_string(),
            IndentStyle::Space => " ".repeat(self.indent_width),
        }
    }

    /// The number of display columns one indentation level occupies.
    pub(crate) fn indent_cols(&self) -> usize {
        self.indent_width.max(1)
    }

    /// The resolved line terminator for input `src`, honoring `Auto`/`Native`.
    pub(crate) fn newline(&self, src: &str) -> &'static str {
        self.line_ending.resolve(src)
    }

    /// Load and parse a specific `jalsfmt.toml` file.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Search upward from `start_dir` for `jalsfmt.toml`.
    ///
    /// Returns the parsed config if a file is found, otherwise [`Config::default`].
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a discovered file cannot be read or parsed.
    pub fn discover(start_dir: &Path) -> Result<Config, ConfigError> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = d.join("jalsfmt.toml");
            if candidate.is_file() {
                return Config::from_file(&candidate);
            }
            dir = d.parent();
        }
        Ok(Config::default())
    }
}

/// An error loading or parsing a config file.
#[derive(Debug)]
pub enum ConfigError {
    /// The file could not be read.
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The file contained invalid TOML.
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The underlying parse error.
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "failed to read config {}: {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "failed to parse config {}: {source}", path.display())
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.indent_width, 4);
        assert_eq!(c.max_width, 100);
        assert_eq!(c.chain_width, 60);
        assert_eq!(c.fn_call_width, 60);
        assert_eq!(c.comment_width, 80);
        assert_eq!(c.max_blank_lines, 1);
        assert!(c.insert_final_newline);
        // Comment reflow is opt-in, mirroring rustfmt's `wrap_comments`.
        assert!(!c.wrap_comments);
        // K&R braces by default, for both declaration and control-flow braces.
        assert_eq!(c.brace_style, BraceStyle::SameLine);
        assert_eq!(c.control_brace_style, ControlBraceStyle::SameLine);
        // Empty declaration bodies collapse to `{}` by default (rustfmt's `empty_item_single_line`).
        assert!(c.empty_item_single_line);
        // Import sorting is opt-in; off by default to preserve the significant-token sequence.
        assert!(!c.reorder_imports);
        // Trailing-comma handling defaults to preserve, keeping the source comma exactly.
        assert_eq!(c.trailing_comma, TrailingComma::Preserve);
        // Import grouping is opt-in; off by default, with a JDK / others / static default order.
        assert!(!c.group_imports);
        assert_eq!(c.import_groups, ["java.", "javax.", "*", "static"]);
        // Wrapped binary operators lead their continuation line by default.
        assert_eq!(c.binop_separator, BinopSeparator::Front);
        // Last-argument overflow is opt-in; off by default keeps the all-or-nothing layout.
        assert!(!c.overflow_delimited_expr);
        // Colon spacing defaults to idiomatic `label:` / `case x:` style: no space before,
        // one space after.
        assert!(!c.space_before_colon);
        assert!(c.space_after_colon);
        // Parameter lists default to the all-or-nothing Tall layout (the prior behavior).
        assert_eq!(c.fn_params_layout, FnParamsLayout::Tall);
        // Modifier reordering is opt-in; off by default to preserve the significant-token sequence.
        assert!(!c.reorder_modifiers);
        // Single-statement bodies are not collapsed onto one line by default (rustfmt's
        // `fn_single_line` is also off by default).
        assert!(!c.fn_single_line);
        // Annotation placement defaults to Compact (inline, the prior behavior).
        assert_eq!(c.annotation_placement, AnnotationPlacement::Compact);
        // Hex-literal case defaults to preserve, keeping the source case exactly.
        assert_eq!(c.hex_literal_case, HexLiteralCase::Preserve);
    }

    #[test]
    fn brace_style_parses_kebab_values() {
        let c: Config = toml::from_str("brace-style = \"next-line\"\n").unwrap();
        assert_eq!(c.brace_style, BraceStyle::NextLine);
        let c: Config = toml::from_str("brace-style = \"same-line\"\n").unwrap();
        assert_eq!(c.brace_style, BraceStyle::SameLine);
    }

    #[test]
    fn control_brace_style_parses_kebab_values() {
        let c: Config = toml::from_str("control-brace-style = \"next-line\"\n").unwrap();
        assert_eq!(c.control_brace_style, ControlBraceStyle::NextLine);
        let c: Config = toml::from_str("control-brace-style = \"same-line\"\n").unwrap();
        assert_eq!(c.control_brace_style, ControlBraceStyle::SameLine);
    }

    #[test]
    fn empty_item_single_line_parses() {
        let c: Config = toml::from_str("empty-item-single-line = false\n").unwrap();
        assert!(!c.empty_item_single_line);
        let c: Config = toml::from_str("empty-item-single-line = true\n").unwrap();
        assert!(c.empty_item_single_line);
    }

    #[test]
    fn fn_single_line_parses() {
        let c: Config = toml::from_str("fn-single-line = true\n").unwrap();
        assert!(c.fn_single_line);
        let c: Config = toml::from_str("fn-single-line = false\n").unwrap();
        assert!(!c.fn_single_line);
    }

    #[test]
    fn wrap_comments_parses() {
        let c: Config = toml::from_str("wrap-comments = true\ncomment-width = 60\n").unwrap();
        assert!(c.wrap_comments);
        assert_eq!(c.comment_width, 60);
    }

    #[test]
    fn reorder_imports_parses() {
        let c: Config = toml::from_str("reorder-imports = true\n").unwrap();
        assert!(c.reorder_imports);
    }

    #[test]
    fn reorder_modifiers_parses() {
        let c: Config = toml::from_str("reorder-modifiers = true\n").unwrap();
        assert!(c.reorder_modifiers);
    }

    #[test]
    fn trailing_comma_parses_kebab_values() {
        let c: Config = toml::from_str("trailing-comma = \"always\"\n").unwrap();
        assert_eq!(c.trailing_comma, TrailingComma::Always);
        let c: Config = toml::from_str("trailing-comma = \"never\"\n").unwrap();
        assert_eq!(c.trailing_comma, TrailingComma::Never);
        let c: Config = toml::from_str("trailing-comma = \"vertical\"\n").unwrap();
        assert_eq!(c.trailing_comma, TrailingComma::Vertical);
        let c: Config = toml::from_str("trailing-comma = \"preserve\"\n").unwrap();
        assert_eq!(c.trailing_comma, TrailingComma::Preserve);
    }

    #[test]
    fn binop_separator_parses_kebab_values() {
        let c: Config = toml::from_str("binop-separator = \"front\"\n").unwrap();
        assert_eq!(c.binop_separator, BinopSeparator::Front);
        let c: Config = toml::from_str("binop-separator = \"back\"\n").unwrap();
        assert_eq!(c.binop_separator, BinopSeparator::Back);
    }

    #[test]
    fn overflow_delimited_expr_parses() {
        let c: Config = toml::from_str("overflow-delimited-expr = true\n").unwrap();
        assert!(c.overflow_delimited_expr);
    }

    #[test]
    fn fn_params_layout_parses_kebab_values() {
        let c: Config = toml::from_str("fn-params-layout = \"tall\"\n").unwrap();
        assert_eq!(c.fn_params_layout, FnParamsLayout::Tall);
        let c: Config = toml::from_str("fn-params-layout = \"compressed\"\n").unwrap();
        assert_eq!(c.fn_params_layout, FnParamsLayout::Compressed);
        let c: Config = toml::from_str("fn-params-layout = \"vertical\"\n").unwrap();
        assert_eq!(c.fn_params_layout, FnParamsLayout::Vertical);
    }

    #[test]
    fn fn_args_layout_is_a_deprecated_alias() {
        // The rustfmt-era `fn-args-layout` key maps to the same field as `fn-params-layout`.
        let c: Config = toml::from_str("fn-args-layout = \"vertical\"\n").unwrap();
        assert_eq!(c.fn_params_layout, FnParamsLayout::Vertical);
    }

    #[test]
    fn colon_spacing_parses_kebab_keys() {
        let c: Config =
            toml::from_str("space-before-colon = true\nspace-after-colon = false\n").unwrap();
        assert!(c.space_before_colon);
        assert!(!c.space_after_colon);
    }

    #[test]
    fn type_punctuation_density_parses_kebab_values() {
        let c: Config = toml::from_str("type-punctuation-density = \"wide\"\n").unwrap();
        assert_eq!(c.type_punctuation_density, TypePunctuationDensity::Wide);
        let c: Config = toml::from_str("type-punctuation-density = \"compressed\"\n").unwrap();
        assert_eq!(
            c.type_punctuation_density,
            TypePunctuationDensity::Compressed
        );
    }

    #[test]
    fn annotation_placement_parses_kebab_values() {
        let c: Config = toml::from_str("annotation-placement = \"compact\"\n").unwrap();
        assert_eq!(c.annotation_placement, AnnotationPlacement::Compact);
        let c: Config = toml::from_str("annotation-placement = \"expanded\"\n").unwrap();
        assert_eq!(c.annotation_placement, AnnotationPlacement::Expanded);
    }

    #[test]
    fn hex_literal_case_parses_kebab_values() {
        let c: Config = toml::from_str("hex-literal-case = \"preserve\"\n").unwrap();
        assert_eq!(c.hex_literal_case, HexLiteralCase::Preserve);
        let c: Config = toml::from_str("hex-literal-case = \"upper\"\n").unwrap();
        assert_eq!(c.hex_literal_case, HexLiteralCase::Upper);
        let c: Config = toml::from_str("hex-literal-case = \"lower\"\n").unwrap();
        assert_eq!(c.hex_literal_case, HexLiteralCase::Lower);
    }

    #[test]
    fn group_imports_parses() {
        let c: Config = toml::from_str("group-imports = true\n").unwrap();
        assert!(c.group_imports);
    }

    #[test]
    fn import_groups_parses() {
        // The Vec<String> key parses from a TOML array (no other Vec field exists yet).
        let c: Config = toml::from_str("import-groups = [\"java.\", \"*\"]\n").unwrap();
        assert_eq!(c.import_groups, ["java.", "*"]);
    }

    #[test]
    fn chain_width_parses_kebab_key() {
        let c: Config = toml::from_str("chain-width = 40\n").unwrap();
        assert_eq!(c.chain_width, 40);
    }

    #[test]
    fn fn_call_width_parses_kebab_key() {
        let c: Config = toml::from_str("fn-call-width = 40\n").unwrap();
        assert_eq!(c.fn_call_width, 40);
    }

    #[test]
    fn array_width_parses_kebab_key() {
        let c: Config = toml::from_str("array-width = 40\n").unwrap();
        assert_eq!(c.array_width, 40);
    }

    #[test]
    fn max_blank_lines_parses_kebab_key() {
        let c: Config = toml::from_str("max-blank-lines = 2\n").unwrap();
        assert_eq!(c.max_blank_lines, 2);
    }

    #[test]
    fn partial_toml_falls_back_to_defaults() {
        let c: Config = toml::from_str("indent-width = 2\n").unwrap();
        assert_eq!(c.indent_width, 2);
        // untouched keys keep defaults
        assert_eq!(c.max_width, 100);
        assert_eq!(c.indent_style, IndentStyle::Space);
    }

    #[test]
    fn enums_parse_kebab_values() {
        let c: Config = toml::from_str("indent-style = \"tab\"\nline-ending = \"crlf\"\n").unwrap();
        assert_eq!(c.indent_style, IndentStyle::Tab);
        assert_eq!(c.line_ending, LineEnding::Crlf);
        assert_eq!(c.indent_unit(), "\t");
        // A fixed line ending ignores the source text.
        assert_eq!(c.newline("a\nb"), "\r\n");
    }

    #[test]
    fn auto_and_native_parse() {
        let c: Config = toml::from_str("line-ending = \"auto\"\n").unwrap();
        assert_eq!(c.line_ending, LineEnding::Auto);
        let c: Config = toml::from_str("line-ending = \"native\"\n").unwrap();
        assert_eq!(c.line_ending, LineEnding::Native);
    }

    #[test]
    fn auto_detects_from_first_line_break() {
        let auto = Config {
            line_ending: LineEnding::Auto,
            ..Config::default()
        };
        // The first line break decides: CRLF stays CRLF, a bare LF stays LF.
        assert_eq!(auto.newline("a\r\nb\nc"), "\r\n");
        assert_eq!(auto.newline("a\nb\r\nc"), "\n");
        assert_eq!(auto.newline("only one\nbreak"), "\n");
        assert_eq!(auto.newline("\r\n"), "\r\n");
        assert_eq!(auto.newline("\n"), "\n");
    }

    #[test]
    fn auto_without_line_break_falls_back_to_native() {
        let auto = Config {
            line_ending: LineEnding::Auto,
            ..Config::default()
        };
        let native = Config {
            line_ending: LineEnding::Native,
            ..Config::default()
        };
        // No `\n` anywhere ⇒ same answer as Native (platform-dependent, so compare the two).
        assert_eq!(auto.newline("no breaks here"), native.newline(""));
        assert_eq!(auto.newline(""), native.newline(""));
    }

    #[test]
    fn native_matches_platform() {
        let native = Config {
            line_ending: LineEnding::Native,
            ..Config::default()
        };
        let expected = if cfg!(windows) { "\r\n" } else { "\n" };
        assert_eq!(native.newline(""), expected);
    }

    #[test]
    fn space_indent_unit() {
        let c = Config {
            indent_width: 2,
            ..Config::default()
        };
        assert_eq!(c.indent_unit(), "  ");
    }
}
