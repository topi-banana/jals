//! `[suspicious]` — the code compiles and is probably not what was meant.
//!
//! The distinction from `[correctness]` is evidence, not seriousness: a `[correctness]` rule
//! reports something the language rules settle, a rule here reports a shape that is legal and
//! almost always a mistake.
//!
//! `constant-condition` deliberately has no options: what a project would want to exempt —
//! `while (true)`, `do … while (false)` — the analysis already never examines, because those are
//! how Java spells an intentionally unbounded loop rather than a folded condition. An option to
//! turn an exemption off that is not an exemption would be a key that reaches nothing.

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use super::{LintOptions, NoOptions};

/// What `empty-catch` accepts as a statement that the exception is ignored on purpose.
///
/// A comment is the conventional Java spelling (`catch (E e) { /* unreachable */ }`), and it is
/// what the rule has always accepted; a project that wants the intent expressed in the parameter's
/// *name* instead sets [`allowed_names`](EmptyCatch::allowed_names) and turns this to
/// [`Reject`](Self::Reject).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IgnoredCatch {
    /// A comment anywhere in the block means the emptiness is deliberate. The default.
    #[default]
    Accept,
    /// A comment proves nothing; only a name from [`allowed_names`](EmptyCatch::allowed_names)
    /// does.
    Reject,
}

/// `empty-catch` options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct EmptyCatch {
    /// Whether a comment in the block is accepted as the explanation.
    pub commented: IgnoredCatch,
    /// Exception parameter names whose emptiness is never reported — `["ignored", "expected"]` is
    /// the common pair. Empty by default, so the rule's only opt-out out of the box is a comment.
    pub allowed_names: Vec<String>,
}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for EmptyCatch {}

lint_section! {
    /// `[suspicious]` — legal code that is almost always a mistake.
    Suspicious: Suspicious {
        /// `constant-condition` — an `if` whose condition folds to a constant, so one of its
        /// branches is dead. Conservative by construction: a condition that cannot be *proven*
        /// constant is never reported. The dead branch travels as a secondary unnecessary-code
        /// range, which an editor fades in place.
        "constant-condition" => constant_condition: NoOptions = Warn,
        /// `empty-catch` — a `catch` block that swallows its exception with no statement and no
        /// stated reason.
        "empty-catch" => empty_catch: EmptyCatch = Warn,
    }
}
