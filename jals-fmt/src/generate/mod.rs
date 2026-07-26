//! **Config generation** — render a jals [`Config`] back out as a `jalsfmt.toml`.
//!
//! The counterpart of [`import`](super::import): where an importer lowers a native Java-formatter
//! config onto jals's option surface, this module writes that surface back out as the file jals
//! itself discovers. Together they are `DESIGN.md` §15's "jalsfmt.toml 自動生成" — a host
//! (`jals-cli`) finds a native config, `import` projects it, and [`Provenance::jalsfmt_toml`]
//! renders it.
//!
//! # What is written
//!
//! **Only the keys that differ from [`Config::default`]** (`DESIGN.md` §15 P-gen-6): a native
//! option left at its vendor default has no business pinning a jals key, and a smaller file stays
//! readable as the schema grows. A config that projects to nothing therefore renders as a header
//! and nothing else — still a real file, so the next run discovers it and stops rather than
//! re-detecting forever.
//!
//! The header records **where the config came from** and, when the native file declared one, its
//! version. That is what §15 asks for in prose; the per-key `# lineSplit` annotations in its
//! illustrative block would need a provenance side-channel threaded through every `From` impl and
//! are deliberately not attempted here.
//!
//! # Why serde and not 174 hand-written comparisons
//!
//! [`Config`] is exactly two levels deep — eight section tables of scalars, strings, string lists
//! and unit-variant enums, with no scalar at the root — so diffing it against its default through
//! `serde_json::Value` and walking the survivors is both shorter than a hand-written emitter and
//! automatically correct when a section gains a key. The two-level shape is not an assumption
//! left to rot: `generate::tests` asserts it, and the emitter refuses to guess if it ever breaks.
//!
//! It also makes the TOML ordering constraint vanish. "Every scalar of a table must precede its
//! sub-tables" cannot be violated by a document whose root holds only tables.

// The warning prose names native products and settings (EditorConfig, Javadoc, `WRAP_LONG_LINES`).
#![allow(clippy::doc_markdown)]

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::fmt::Write as _;

use jals_config::fmt::{Braces, Config, KeepOnOneLine, ParenPositions, Wrapping};

mod prune;
mod toml_out;

use prune::Pruned;
use toml_out::Toml;

#[cfg(test)]
mod tests;

/// Where a generated `jalsfmt.toml` came from, written into its header.
///
/// Only for tracking a regeneration — jals has one layout engine, so nothing here changes how the
/// formatter behaves (`DESIGN.md` §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The native file, relative to the directory the config was detected in — e.g.
    /// `".settings/org.eclipse.jdt.core.prefs"`.
    pub source: String,
    /// The product family: `"eclipse"` or `"intellij"`.
    pub tool: &'static str,
    /// The version the file itself declares, when it declares one (an Eclipse profile's
    /// `version="23"`, an IntelliJ `<code_scheme version=…>`). *Not* the Eclipse preference
    /// store's `eclipse.preferences.version`, which counts something else entirely.
    pub version: Option<String>,
}

/// Why a migrated key deserves a note. Private: [`MigrationWarning`] is opaque, so there is no
/// way to obtain one of these from outside the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationWarningKind {
    /// The native value is a function of the input's existing line breaks, which the single
    /// engine does not read. It is carried into the config verbatim but the engine rounds it to
    /// a canonical value (`DESIGN.md` §17).
    Rounded,
    /// Several files matched one detection row, or one file carried several profiles, so the
    /// chosen source is not the only candidate.
    Ambiguous,
}

impl MigrationWarningKind {
    /// The lowercase word used in the rendered `# warning: …` line.
    const fn label(self) -> &'static str {
        match self {
            Self::Rounded => "rounded",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// One note about a migration, rendered both into the generated file's header and onto stderr.
///
/// Opaque: built through the constructors below and consumed through [`Display`](fmt::Display),
/// which is the whole surface a host needs and keeps the rendered wording in one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationWarning {
    /// What kind of note this is.
    kind: MigrationWarningKind,
    /// The dotted jals key (`wrapping.join-wrapped-lines`) or the native file it concerns.
    subject: String,
    /// One sentence, with **no trailing period and no newline** — it is rendered inside a `#`
    /// comment line, where a newline would silently produce uncommented TOML.
    detail: String,
}

impl MigrationWarning {
    /// A [`Rounded`](MigrationWarningKind::Rounded) note about `subject`.
    fn rounded(subject: &str, detail: &str) -> Self {
        Self {
            kind: MigrationWarningKind::Rounded,
            subject: subject.to_owned(),
            detail: detail.to_owned(),
        }
    }

    /// A note that the chosen source was not the only candidate: several files matched one
    /// detection row, or one file carried several profiles.
    pub fn ambiguous(subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind: MigrationWarningKind::Ambiguous,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for MigrationWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}: {}",
            self.kind.label(),
            self.subject,
            self.detail
        )
    }
}

impl Provenance {
    /// Render `config` as the text of a `jalsfmt.toml`, under this provenance and a warning
    /// header.
    ///
    /// Only the keys that differ from [`Config::default`] are written (`DESIGN.md` §15 P-gen-6),
    /// so a config equal to the defaults renders as the header alone. The result always ends in a
    /// newline.
    ///
    /// Infallible by construction: the derived schema cannot fail to serialize, and a shape the
    /// emitter does not recognize produces an explanatory comment instead of a panic or a
    /// half-written table.
    #[must_use]
    pub fn jalsfmt_toml(&self, config: &Config, warnings: &[MigrationWarning]) -> String {
        let mut out = String::new();

        let version = self
            .version
            .as_ref()
            .map_or_else(String::new, |version| format!(" {version}"));
        // Deliberately not starting `# jalsfmt.toml`: `jals-tests` selects the *documented
        // defaults* sample out of a Markdown page by that exact string, so a generated example
        // pasted into the README would otherwise be mistaken for it.
        let _ = writeln!(
            out,
            "# Generated by jals from {} ({}{version}).",
            self.source, self.tool
        );
        out.push_str(
            "# Only the keys that differ from the jals defaults are written \
             (jals-fmt/DESIGN.md §15).\n",
        );
        out.push_str(
            "# Native options that were not projected are listed in jals-fmt/MAPPING.md \
             §7 and §18.\n",
        );
        for warning in warnings {
            let _ = writeln!(out, "#\n# warning: {warning}");
        }

        let Some(sections) = Pruned::non_default(config) else {
            // Unreachable while `Config` stays two levels deep (pinned by
            // `the_schema_is_two_levels_deep`). Say so in the file rather than emitting a table
            // that would not round-trip.
            out.push_str(
                "#\n# warning: this jals version could not render its own config schema; \
                 no keys were written\n",
            );
            return out;
        };

        for section in Toml::SECTIONS {
            let Some(keys) = sections.get(section).and_then(serde_json::Value::as_object) else {
                continue;
            };
            let _ = writeln!(out, "\n[{section}]");
            for (key, value) in keys {
                let _ = match Toml::scalar(value) {
                    Some(rendered) => writeln!(out, "{key} = {rendered}"),
                    // A value with no TOML spelling (only `null` can be one, and only if a key's
                    // default ever stops being `None`). Record it instead of writing broken TOML.
                    None => writeln!(out, "# unresolved: {section}.{key} has no TOML form"),
                };
            }
        }

        out
    }
}

impl MigrationWarning {
    /// The `DESIGN.md` §17 rounding notes `config` implies.
    ///
    /// A pure function of the config, because §17 puts the rounding in the **engine**, not in the
    /// projection: an importer stores the native value verbatim and it is the formatter that later
    /// declines to read input whitespace. So this needs no importer cooperation and works just as
    /// well on a hand-written `jalsfmt.toml`.
    ///
    /// A row is reported only when its value is both the one §17 rounds *away* **and** different
    /// from [`Config::default`] — the same test [`Provenance::jalsfmt_toml`] applies when deciding
    /// what to write, so this warns about exactly the keys that get written. Without the default
    /// check, `wrapping.wrap-long-lines` (whose default `false` *is* §17's rounded-away value)
    /// would warn on every config in existence, including an empty one.
    #[must_use]
    pub fn rounding(config: &Config) -> Vec<Self> {
        /// The canonical value §17 rounds a whitespace-reading `keep-*` to: "what was written on one
        /// line stays on one line" is approximated structurally, not by reading the input.
        const KEEP_ROUNDING: &str =
            "preserves the input's line breaks; the formatter uses `if-single-item` instead";
        const PAREN_ROUNDING: &str = "preserves the input's parenthesis positions; the formatter uses `common-lines` instead";

        // Each row names one field *once*, as an accessor read from both the config and the
        // default. A hand-paired `(actual, default)` table would let the two halves of a row drift
        // onto different fields without any error.
        let keeps: [(&str, fn(&Braces) -> KeepOnOneLine); 8] = [
            ("braces.keep-type-body-on-one-line", |b| {
                b.keep_type_body_on_one_line
            }),
            ("braces.keep-method-body-on-one-line", |b| {
                b.keep_method_body_on_one_line
            }),
            ("braces.keep-block-on-one-line", |b| {
                b.keep_block_on_one_line
            }),
            ("braces.keep-lambda-body-on-one-line", |b| {
                b.keep_lambda_body_on_one_line
            }),
            ("braces.keep-switch-body-on-one-line", |b| {
                b.keep_switch_body_on_one_line
            }),
            ("braces.keep-enum-declaration-on-one-line", |b| {
                b.keep_enum_declaration_on_one_line
            }),
            ("braces.keep-record-declaration-on-one-line", |b| {
                b.keep_record_declaration_on_one_line
            }),
            ("braces.keep-annotation-declaration-on-one-line", |b| {
                b.keep_annotation_declaration_on_one_line
            }),
        ];
        let parens: [(&str, fn(&Wrapping) -> ParenPositions); 6] = [
            ("wrapping.paren-method-declaration", |w| {
                w.paren_method_declaration
            }),
            ("wrapping.paren-method-invocation", |w| {
                w.paren_method_invocation
            }),
            ("wrapping.paren-control", |w| w.paren_control),
            ("wrapping.paren-annotation", |w| w.paren_annotation),
            ("wrapping.paren-lambda", |w| w.paren_lambda),
            ("wrapping.paren-record", |w| w.paren_record),
        ];

        let default = Config::default();
        let mut out = Vec::new();

        for (key, field) in keeps {
            let actual = field(&config.braces);
            if actual == KeepOnOneLine::Preserve && actual != field(&default.braces) {
                out.push(Self::rounded(key, KEEP_ROUNDING));
            }
        }

        for (key, field) in parens {
            let actual = field(&config.wrapping);
            if actual == ParenPositions::Preserve && actual != field(&default.wrapping) {
                out.push(Self::rounded(key, PAREN_ROUNDING));
            }
        }

        let wrapping = &config.wrapping;
        let wrapping_default = &default.wrapping;

        // The three boolean rows. They are spelled out rather than tabulated because each names its
        // own rounded-away value and lives in a different section. `wrapping.wrap-long-lines` is
        // currently unreachable — `false` is both what §17 rounds away and the default — and only
        // becomes live if that default flips; `rounding_warnings_name_every_section_17_row` pins it.
        for (key, is_rounded_away, differs, detail) in [
            (
                "wrapping.join-wrapped-lines",
                !wrapping.join_wrapped_lines,
                wrapping.join_wrapped_lines != wrapping_default.join_wrapped_lines,
                "off would keep the source's line breaks; the formatter always rejoins",
            ),
            (
                "wrapping.wrap-long-lines",
                !wrapping.wrap_long_lines,
                wrapping.wrap_long_lines != wrapping_default.wrap_long_lines,
                "off would leave over-long lines alone; the formatter always wraps them",
            ),
            (
                "comments.preserve-line-breaks",
                config.comments.preserve_line_breaks,
                config.comments.preserve_line_breaks != default.comments.preserve_line_breaks,
                "on would keep the source's line breaks in comment prose; the formatter always refills",
            ),
        ] {
            if is_rounded_away && differs {
                out.push(Self::rounded(key, detail));
            }
        }

        out
    }
}
