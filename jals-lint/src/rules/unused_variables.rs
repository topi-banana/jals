//! `unused-variables`: flag every binding one file scopes and never uses.
//!
//! This rule consumes `jals-hir`'s file-local analysis — [`unused_defs`](FileAnalysis::unused_defs)
//! — and does no searching of its own. What it owns is the *policy*: which of those facts one file
//! is entitled to call unused, and how each one is worded. The same signal's `private` members are
//! `dead-code`'s to report, and imports are `unused-imports`', for the reason rustc splits the
//! three: they are suppressed independently, and one `allow` should not silence the other two.
//!
//! The line is **whether one file settles the question**. A local, a parameter, a type parameter, a
//! `catch` parameter, a pattern variable and a lambda parameter are all scoped to the file that
//! declares them, so a file that never uses one has seen every use there can be. Everything wider
//! belongs to another rule or to no rule at all.
//!
//! Two exclusions inside that line, each because non-use is not evidence there:
//!
//! - A **resource** (`try (var in = open())`) exists for its `close()`. The declaration *is* the
//!   use, and the syntax makes the name mandatory, so flagging one asks for a change that cannot
//!   be made.
//! - A name beginning with `_` is the author's own opt-out: it says the binding is deliberately
//!   unused where the syntax still demands one — an `@Override` parameter, a `catch` clause whose
//!   exception is genuinely ignored. It is honoured here and not on a `private` member, where a
//!   leading `_` is a naming *style* rather than a statement of intent (`dead-code`'s module docs
//!   give that reason in full).
//!
//! `@Override` / interface-implementation parameters remain a known source of false positives —
//! the signature is not the method's to choose — so suppress the rule via `jalslint.toml` where
//! that matters.

use alloc::vec::Vec;

use jals_exec::LocalBoxFuture;
use jals_hir::{Def, DefKind, FileAnalysis};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta, UnusedDefs};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unused-variables",
    default: Severity::Warn,
    needs_clean_parse: false,
    check: Checker::Analyzed(UnusedVariables::check),
};

/// The `unused-variables` rule.
struct UnusedVariables;

impl UnusedVariables {
    /// The table edge: [`UnusedDefs`] walks the signal, [`subject`](Self::subject) is this rule's
    /// share of it.
    fn check(analysis: &FileAnalysis) -> LocalBoxFuture<'_, Vec<Finding>> {
        UnusedDefs::findings(analysis, Self::subject)
    }

    /// How to name `def` in the diagnostic, or `None` when this rule does not report its kind.
    ///
    /// The whole reporting policy, in one table: a binding one file scopes is named, and its own
    /// name may still opt out. Every wider kind yields `None` here — the members among them are
    /// `dead-code`'s — and the match stays exhaustive so a new [`DefKind`] has to be placed rather
    /// than silently ignored. Java 22's unnamed variable (`_` on its own) binds nothing and reaches
    /// no [`Def`], so the opt-out only ever decides `_name`; `naming-convention` already leaves a
    /// name that does not start with an ASCII letter alone, so it trades no warning for another.
    fn subject(def: &Def) -> Option<&'static str> {
        let subject = match def.kind {
            DefKind::Local => "local variable",
            DefKind::Param => "parameter",
            DefKind::LambdaParam => "lambda parameter",
            DefKind::TypeParam => "type parameter",
            DefKind::CatchParam => "exception parameter",
            DefKind::PatternVar => "pattern variable",
            DefKind::Resource
            | DefKind::Field
            | DefKind::Method
            | DefKind::Constructor
            | DefKind::Class
            | DefKind::Interface
            | DefKind::Enum
            | DefKind::Record
            | DefKind::AnnotationType
            | DefKind::EnumConstant => return None,
        };
        (!def.name.starts_with('_')).then_some(subject)
    }
}
