//! `[unused]` — something is declared and nothing reads it.
//!
//! One `jals-hir` signal ([`FileAnalysis::unused_defs`]) feeds all three rules, and they are three
//! rules rather than one because they are **suppressed independently**: a project that must keep
//! unavoidable `@Override` parameters is not thereby a project that wants dead private members
//! kept. rustc splits them the same way and for the same reason, so the names are rustc's.
//!
//! The signal is a *negative* fact, so it over-approximates use: a member named where the
//! file-local pass cannot bind it (`this.x`, `Outer.Inner`, `X.class`, `@Anno`, JLS §6.5.2's
//! ambiguous-name qualifier, anything inside a `cfg`-disabled host) counts as used, and a method's
//! evidence is its *name* rather than its declaration, because the scope chain binds a call to
//! *an* overload rather than to the one the arguments select.
//!
//! [`FileAnalysis::unused_defs`]: https://docs.rs/jals-hir

use alloc::borrow::ToOwned;
use alloc::string::String;

use serde::{Deserialize, Serialize};

use super::{LintOptions, NoOptions};

/// What `dead-code` does with a `private` member carrying an annotation.
///
/// `@Inject`, `@Autowired`, `@Mock` and their kind are assigned or invoked by something no source
/// names, and are spelled exactly like a member nobody uses — so an annotation is read as evidence
/// of an invisible use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotatedMembers {
    /// An annotated private member is never reported. The default.
    #[default]
    Skip,
    /// Report it like any other, for a project whose annotations never inject.
    Report,
}

/// `dead-code` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct DeadCode {
    /// Whether an annotation exempts a private member.
    pub annotated: AnnotatedMembers,
}

/// `unused-variables` options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct UnusedVariables {
    /// The name prefix that opts a binding out — `"_"` by default, so `_e` is the spelling for a
    /// name the syntax demands and the code does not want (an `@Override` parameter, a genuinely
    /// ignored `catch` clause).
    ///
    /// An empty string disables the opt-out entirely rather than exempting every name: a prefix
    /// every name carries would silence the whole rule, which is what setting its level to
    /// `allow` already says, more plainly.
    ///
    /// Deliberately not honoured by `dead-code`: on a `private` member a leading `_` is a naming
    /// *style* rather than a statement of intent, and reading it as one would drop the finding for
    /// every codebase that spells its fields that way.
    pub ignore_prefix: String,
}

impl Default for UnusedVariables {
    fn default() -> Self {
        Self {
            ignore_prefix: "_".to_owned(),
        }
    }
}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for DeadCode {}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for UnusedVariables {}

lint_section! {
    /// `[unused]` — declared and never read.
    Unused: Unused {
        /// `unused-variables` — a binding the file itself scopes and never uses: a local, a
        /// parameter, a lambda parameter, a type parameter, a `catch` parameter, a pattern
        /// variable. A `try` resource is never reported (the declaration *is* the use).
        "unused-variables" => unused_variables: UnusedVariables = Warn,
        /// `unused-imports` — an `import` no name in the file resolves through. A wildcard import
        /// is never reported: nothing in the file names what it would have to miss.
        "unused-imports" => unused_imports: NoOptions = Warn,
        /// `dead-code` — a `private` member the declaring file never uses. `private` is the widest
        /// visibility one file settles (JLS §6.6.1), so nothing wider is reported. A constructor
        /// is never reported: `new C()` names the *type*, so non-use is not evidence.
        "dead-code" => dead_code: DeadCode = Warn,
    }
}
