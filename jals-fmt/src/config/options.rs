//! The style / layout option enums of [`Config`](super::Config).
//!
//! Each enum is `Deserialize` with kebab-case (or lowercase) variants and is re-exported from the
//! `config` module root. The numeric-literal case options live in [`literals`](super::literals).

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

/// How a same-precedence binary-operator run (`a + b + c …`) is laid out when it wraps across
/// lines, independent of [`BinopSeparator`] (which decides *where* the operator sits). A
/// Java-specific option with no rustfmt equivalent. Layout-only — the significant-token sequence
/// is preserved exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BinopLayout {
    /// All-or-nothing: keep the run on one line when it fits [`max_width`](super::Config::max_width),
    /// otherwise break at *every* operator (one operand per line). The default; matches the prior
    /// behavior.
    Tall,
    /// Pack as many operands per line as fit [`max_width`](super::Config::max_width), wrapping to a
    /// new line only when the next operand would overflow (a *fill*). Matches google-java-format's
    /// binary-expression wrapping. Mirrors the [`Compressed`](FnParamsLayout::Compressed) parameter
    /// layout.
    Compressed,
}

/// Layout of a method / constructor parameter list (`PARAM_LIST`). Mirrors rustfmt's
/// `fn_params_layout` (formerly `fn_args_layout`, which jals accepts as a deprecated alias);
/// it applies only to declaration parameter lists, never to call argument lists. Layout-only —
/// the significant-token sequence is preserved exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FnParamsLayout {
    /// All-or-nothing: keep the parameters on one line when they fit
    /// [`max_width`](super::Config::max_width), otherwise lay them out one per line. The default;
    /// matches the prior behavior.
    Tall,
    /// Pack as many parameters per line as fit [`max_width`](super::Config::max_width), wrapping to
    /// a new line only when the next parameter would overflow. Mirrors rustfmt's `Compressed`.
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
