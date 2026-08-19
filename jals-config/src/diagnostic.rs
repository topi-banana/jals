//! The presented severity of a diagnostic.
//!
//! The counterpart to [`lint::LintLevel`](crate::lint::LintLevel): that one is *configured* — what
//! a `jalslint.toml` sets a rule to — while this one is *presented*, what a destination draws. They
//! are not the same vocabulary, which is why `Allow` has no arm here (a rule set to `Allow` never
//! reaches a destination) and `Hint` has no arm there (nothing configures a `cfg`-disabled region).
//!
//! It lives beside the configured severity rather than in `jals-editor` because more than one crate
//! produces diagnostics and each has to state its own presentation. `jals-editor` assembles a
//! file's, `jals-project` assembles a project assembly's; neither depends on the other, and neither
//! should — so a crate that produces diagnostics says how they present by converting into this,
//! instead of every host restating the same table. `jals-editor` re-exports the name, so a host that
//! only knows the editor still spells it `jals_editor::DiagnosticSeverity`.

/// How a destination should present a diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    /// A definite problem (syntax error, unresolvable symbol, error-severity rule).
    Error,
    /// A warn-severity finding.
    Warning,
    /// Supplementary information kept out of the problems list: a `cfg`-disabled region, the faded
    /// dead-branch range of a constant condition, an advisory about the project's own state.
    Hint,
}

/// The presentation severity for a lint finding's configured level.
///
/// `Allow` rules are skipped inside the engine and never reach a destination, so its arm is
/// unreachable by construction; it is written out rather than left to a wildcard so that a future
/// [`LintLevel`](crate::lint::LintLevel) variant has to be placed here instead of being swallowed.
impl From<crate::lint::LintLevel> for DiagnosticSeverity {
    fn from(level: crate::lint::LintLevel) -> Self {
        match level {
            crate::lint::LintLevel::Error => Self::Error,
            crate::lint::LintLevel::Warn | crate::lint::LintLevel::Allow => Self::Warning,
        }
    }
}
