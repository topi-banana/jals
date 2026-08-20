//! `dead-code`: flag every `private` member the file that declares it never uses.
//!
//! This rule consumes `jals-hir`'s file-local analysis — [`unused_defs`](FileAnalysis::unused_defs)
//! — and does no searching of its own. It reports the half of that signal `unused-variables` leaves
//! alone: the members. What it owns is the *policy*: which of those facts one file is entitled to
//! call dead, and how each one is worded.
//!
//! The line is **whether one file settles the question**. A `private` member is the widest thing
//! that qualifies: JLS §6.6.1 confines it to the body of its top-level class, which is one
//! compilation unit, so a file that never uses one has seen every use there can be. Everything
//! wider is left alone, because another file may be the one that uses it.
//!
//! Three exclusions inside that line, each because non-use is not evidence there:
//!
//! - A **constructor** is never a recorded reference at all: `new C()` names the *type*, so the
//!   analysis has nothing to be silent about. A private constructor is also the standard way to say
//!   a utility class is not instantiable, which is a use with no call site by design.
//! - An **annotated** private member is routinely assigned or invoked by something no source names
//!   — `@Inject`, `@Autowired`, `@Mock` — and is spelled exactly like one nobody uses. A project
//!   whose annotations never inject turns that exemption off with
//!   [`api::annotated`](jals_config::lint::DeadCode::annotated).
//! - The **serialization** members are called by `ObjectOutputStream`/`ObjectInputStream` through
//!   reflection, by name, and are `private` precisely so nothing else calls them.
//!
//! The leading-`_` opt-out `unused-variables` honours is deliberately *not* honoured here: on a
//! `private` member a leading `_` is a naming *style* rather than a statement of intent, and
//! reading it as one would drop the finding for every codebase that spells its fields that way.

use alloc::vec::Vec;

use jals_exec::LocalBoxFuture;
use jals_hir::{Def, DefKind, FileAnalysis};

use jals_config::Category;
use jals_config::lint::Config;

use jals_config::lint::AnnotatedMembers;

use crate::rules::unused_defs;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "dead-code",
    category: Category::Unused,
    level: |config| config.unused.dead_code.level,
    needs_clean_parse: false,
    check: Checker::Analyzed(api::check),
};

/// The `dead-code` rule.
mod api {
    use super::{
        AnnotatedMembers, Config, Def, DefKind, FileAnalysis, Finding, LocalBoxFuture, Vec,
        unused_defs,
    };

    /// The members `java.io.Serializable` reaches by name, through reflection, rather than through
    /// a call site any analysis could see (JLS-adjacent: `java.io.ObjectOutputStream` /
    /// `ObjectInputStream` specify these by signature). Declaring them `private` is what the
    /// serialization contract asks for, so their non-use is the contract being honoured.
    const SERIALIZATION_MEMBERS: &[&str] = &[
        "serialVersionUID",
        "serialPersistentFields",
        "writeObject",
        "readObject",
        "readObjectNoData",
        "writeReplace",
        "readResolve",
    ];

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
    /// The whole reporting policy, in one table: a member whose disuse this file is entitled to
    /// report is named, everything else yields `None`. The locally-scoped kinds are
    /// `unused-variables`', and the match stays exhaustive so a new [`DefKind`] has to be placed
    /// rather than silently ignored. See the module docs for why each absent member kind is absent;
    /// an enum constant is additionally one the `private` gate below could never admit, since
    /// [`is_private`](Def::is_private) reads an explicit `private` keyword and JLS §8.9.3 makes an
    /// enum constant implicitly `public`, so no source can write one.
    ///
    /// Two gates on the kinds that remain. The member must be `private`, because anything wider is
    /// another file's question; and nothing must be reaching it without naming it, which is what an
    /// annotation and a serialization member each say in their own way.
    fn subject(def: &Def, config: &Config) -> Option<&'static str> {
        let subject = match def.kind {
            DefKind::Field => "private field",
            DefKind::Method => "private method",
            DefKind::Class => "private class",
            DefKind::Interface => "private interface",
            DefKind::Enum => "private enum",
            DefKind::Record => "private record",
            DefKind::AnnotationType => "private annotation type",
            DefKind::Constructor
            | DefKind::EnumConstant
            | DefKind::Resource
            | DefKind::Local
            | DefKind::Param
            | DefKind::LambdaParam
            | DefKind::TypeParam
            | DefKind::CatchParam
            | DefKind::PatternVar => return None,
        };
        let annotated_injects = matches!(
            config.unused.dead_code.options.annotated,
            AnnotatedMembers::Skip
        );
        let reached_unnamed = (def.is_annotated && annotated_injects)
            || SERIALIZATION_MEMBERS.contains(&def.name.as_str());
        (def.is_private && !reached_unnamed).then_some(subject)
    }
}
