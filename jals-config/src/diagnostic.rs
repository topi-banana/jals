//! The presented severity of a diagnostic.
//!
//! The counterpart to [`lint::Severity`](crate::lint::Severity): that one is *configured* — what a
//! `jalslint.toml` sets a rule to — while this one is *presented*, what a destination draws. They
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

/// The presentation severity for a lint finding's configured severity.
///
/// `Allow` rules are skipped inside the engine and never reach a destination; the arm exists so the
/// match is exhaustive without a wildcard that would swallow a future variant.
impl From<crate::lint::Severity> for DiagnosticSeverity {
    fn from(severity: crate::lint::Severity) -> Self {
        match severity {
            crate::lint::Severity::Error => Self::Error,
            crate::lint::Severity::Warn | crate::lint::Severity::Allow => Self::Warning,
        }
    }
}
