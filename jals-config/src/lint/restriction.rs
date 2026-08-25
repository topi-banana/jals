//! `[restriction]` — the code is correct, idiomatic and fast, and the project has banned it.
//!
//! The one section whose findings are not defects. It exists because clippy's `restriction` group
//! exists and is genuinely useful, and because folding those rules into `[style]` would have made
//! "the code does not read the way Java is written" untrue of half of that section.
//!
//! **A rule's built-in level here is its own**, and no longer a property of the category.
//! `print-to-console` is [`Allow`](super::LintLevel::Allow) because a restriction nobody asked for
//! is not a finding — the reading this section was created under. `implicit-this` is
//! [`Warn`](super::LintLevel::Warn): an unqualified field is a name a reader can mistake for a
//! local, which is a hazard before it is a policy, so it speaks unless a project says otherwise.
//!
//! The set of built-in levels is pinned in one place either way —
//! `jals-lint/tests/registry.rs`'s `the_default_config_enables_exactly_these_rules` — so a level
//! changing is an edit to that list rather than a silent drift.

use serde::{Deserialize, Serialize};

use super::LintOptions;

/// Which console stream `print-to-console` reports writes to.
///
/// clippy spells this as two lints, `print_stdout` and `print_stderr`, which a config can enable
/// in any combination — including the combination it has no name for. One key with three values
/// says the same thing with no unreachable state, and adds the answer the pair could not express
/// as a single setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleStreams {
    /// `System.out` and `System.err` alike. The default when the rule is enabled.
    #[default]
    Both,
    /// `System.out` only — for a program whose diagnostics on `System.err` are intended.
    Stdout,
    /// `System.err` only.
    Stderr,
}

/// `print-to-console` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PrintToConsole {
    /// Which streams are reported.
    pub streams: ConsoleStreams,
}

/// Which instance-field references `implicit-this` requires the `this.` qualifier on.
///
/// Checkstyle spells the same choice as `RequireThis`'s `validateOnlyOverlapping` flag, a boolean
/// whose `false` means "check more" — the wrong way round for a reader, and unreadable beside the
/// rule's other boolean. Naming both answers says which code each one is about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThisScope {
    /// Every instance-field reference in a non-static context. The default: a project adopts this
    /// rule for `this.` everywhere, not for `this.` only where the compiler would force it.
    #[default]
    Always,
    /// Only a reference inside an executable that also declares a local, parameter or pattern
    /// variable of the same name — the sites where the spelling alone no longer says which
    /// binding a reader is looking at.
    ShadowedOnly,
}

/// `implicit-this` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ImplicitThis {
    /// Which references require the qualifier.
    pub scope: ThisScope,
}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for PrintToConsole {}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for ImplicitThis {}

lint_section! {
    /// `[restriction]` — constructs a project has chosen to ban.
    Restriction: Restriction {
        /// `print-to-console` — a call on `System.out` or `System.err`, for a project that logs
        /// through a framework and wants the console left to the framework. Ports
        /// `clippy::print_stdout` and `clippy::print_stderr` as one rule with a
        /// [`streams`](PrintToConsole::streams) key.
        "print-to-console" => print_to_console: PrintToConsole = Allow,
        /// `implicit-this` — an instance field read or written by simple name where `this.` would
        /// qualify it, for a project that wants a field distinguishable from a local by the
        /// spelling alone. Checkstyle's `RequireThis`; neither rustc nor clippy carries an
        /// analogue, because Rust has no implicit receiver to leave out.
        "implicit-this" => implicit_this: ImplicitThis = Warn,
    }
}
