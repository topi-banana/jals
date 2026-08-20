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
//! - A name beginning with the configured prefix — `_` out of the box
//!   ([`api::ignore_prefix`]) — is the author's own opt-out: it says the binding is
//!   deliberately unused where the syntax still demands one, as in an `@Override` parameter or a
//!   `catch` clause whose exception is genuinely ignored. It is honoured here and not on a
//!   `private` member, where a leading `_` is a naming *style* rather than a statement of intent
//!   (`dead-code`'s module docs give that reason in full). An empty prefix turns the opt-out off
//!   rather than exempting everything: a prefix every name carries would silence the whole rule,
//!   which `level = "allow"` already says more plainly.
//!
//! `@Override` / interface-implementation parameters remain a known source of false positives —
//! the signature is not the method's to choose — so rename them with the opt-out prefix, or set
//! `[unused] unused-variables = "allow"` where that is not practical.

use alloc::vec::Vec;

use jals_exec::LocalBoxFuture;
use jals_hir::{Def, DefKind, FileAnalysis};

use jals_config::Category;
use jals_config::lint::Config;

use jals_config::lint::UnusedVariables as Options;

use crate::rules::unused_defs;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unused-variables",
    category: Category::Unused,
    level: |config| config.unused.unused_variables.level,
    needs_clean_parse: false,
    check: Checker::Analyzed(api::check),
};

/// The `unused-variables` rule.
mod api {
    use super::{
        Config, Def, DefKind, FileAnalysis, Finding, LocalBoxFuture, Options, Vec, unused_defs,
    };

    /// The table edge: [`UnusedDefs`] walks the signal, [`subject`](subject) is this rule's
    /// share of it.
    pub(crate) fn check<'a>(
        analysis: &'a FileAnalysis,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        unused_defs::findings(analysis, config, subject)
    }

    /// How to name `def` in the diagnostic, or `None` when this rule does not report its kind.
    ///
    /// The whole reporting policy, in one table: a binding one file scopes is named, and its own
    /// name may still opt out. Every wider kind yields `None` here — the members among them are
    /// `dead-code`'s — and the match stays exhaustive so a new [`DefKind`] has to be placed rather
    /// than silently ignored. Java 22's unnamed variable (`_` on its own) binds nothing and reaches
    /// no [`Def`], so the opt-out only ever decides `_name`; `naming-convention` already leaves a
    /// name that does not start with an ASCII letter alone, so it trades no warning for another.
    pub(crate) fn subject(def: &Def, config: &Config) -> Option<&'static str> {
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
        let Options { ignore_prefix } = &config.unused.unused_variables.options;
        let opted_out = !ignore_prefix.is_empty() && def.name.starts_with(ignore_prefix.as_str());
        (!opted_out).then_some(subject)
    }
}
