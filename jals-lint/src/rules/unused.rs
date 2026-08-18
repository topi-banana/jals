//! `unused`: flag every binding and every import the file never uses.
//!
//! This rule consumes `jals-hir`'s file-local analysis — [`unused_defs`](FileAnalysis::unused_defs)
//! for bindings, [`unused_imports`](FileAnalysis::unused_imports) for imports — and does no
//! searching of its own. What it owns is the *policy*: which of those facts one file is entitled to
//! call unused, and how each one is worded.
//!
//! The line is **whether one file settles the question**. A local, a parameter, a type parameter, a
//! `catch` parameter, a pattern variable and a lambda parameter are all scoped to the file that
//! declares them, so a file that never uses one has seen every use there can be. So is a `private`
//! member: JLS §6.6.1 confines it to the body of its top-level class, which is one compilation
//! unit. Everything wider is left alone, because another file may be the one that uses it.
//!
//! Four exclusions inside that line, each because non-use is not evidence there:
//!
//! - A **resource** (`try (var in = open())`) exists for its `close()`. The declaration *is* the
//!   use, and the syntax makes the name mandatory, so flagging one asks for a change that cannot
//!   be made.
//! - A **constructor** is never a recorded reference at all: `new C()` names the *type*, so the
//!   analysis has nothing to be silent about. A private constructor is also the standard way to say
//!   a utility class is not instantiable, which is a use with no call site by design.
//! - An **annotated** private member is routinely assigned or invoked by something no source names
//!   — `@Inject`, `@Autowired`, `@Mock` — and is spelled exactly like one nobody uses.
//! - The **serialization** members are called by `ObjectOutputStream`/`ObjectInputStream` through
//!   reflection, by name, and are `private` precisely so nothing else calls them.
//!
//! A fifth exclusion is the author's own to make: a name beginning with `_` says the binding is
//! deliberately unused where the syntax still demands one. It is honoured for the locally-scoped
//! kinds only — on a `private` member a leading `_` is a naming *style* rather than a statement of
//! intent, and reading it as one would drop the finding for every codebase that spells its fields
//! that way. An import is not this file's name to choose at all.
//!
//! `@Override` / interface-implementation parameters remain a known source of false positives —
//! the signature is not the method's to choose — so suppress the rule via `jalslint.toml` where
//! that matters.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{Def, DefKind, FileAnalysis};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "unused",
    default: Severity::Warn,
    needs_clean_parse: false,
    check: Checker::Analyzed(Unused::check),
};

/// The `unused` rule.
struct Unused;

impl Unused {
    /// The members `java.io.Serializable` reaches by name, through reflection, rather than through
    /// a call site any analysis could see (JLS-adjacent: `java.io.ObjectOutputStream` /
    /// `ObjectInputStream` specify these by signature). Declaring them `private` is what the
    /// serialization contract asks for, so their non-use is the contract being honoured.
    const SERIALIZATION_MEMBERS: &'static [&'static str] = &[
        "serialVersionUID",
        "serialPersistentFields",
        "writeObject",
        "readObject",
        "readObjectNoData",
        "writeReplace",
        "readResolve",
    ];

    /// The table-edge shim: boxes the async rule body once per file.
    fn check(analysis: &FileAnalysis) -> LocalBoxFuture<'_, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(analysis))
    }

    async fn check_impl(analysis: &FileAnalysis) -> Vec<Finding> {
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for def in analysis.unused_defs() {
            yielder.tick().await;
            let Some(subject) = Self::subject(def) else {
                continue;
            };
            out.push(Self::finding(
                def.name_range.clone(),
                format!("unused {subject} `{}`", def.name),
            ));
        }
        for import in analysis.unused_imports().await {
            let modifier = if import.is_static { "static " } else { "" };
            out.push(Self::finding(
                import.range,
                format!("unused {modifier}import `{}`", import.name),
            ));
        }
        out
    }

    /// Every finding this rule makes: whatever it points at — a binding's name, an import
    /// declaration — is code that could simply go, so the span is its own unnecessary range and a
    /// consumer fades it in place.
    fn finding(range: Range<usize>, message: String) -> Finding {
        Finding {
            range,
            message,
            unnecessary: true,
            ..Finding::default()
        }
    }

    /// How to name `def` in the diagnostic, or `None` when this rule does not report its kind.
    ///
    /// The whole reporting policy, in one table: a binding one file settles is named, everything
    /// else yields `None`. See the module docs for why each absent kind is absent.
    fn subject(def: &Def) -> Option<&'static str> {
        match def.kind {
            DefKind::Local => Self::local_subject(def, "local variable"),
            DefKind::Param => Self::local_subject(def, "parameter"),
            DefKind::LambdaParam => Self::local_subject(def, "lambda parameter"),
            DefKind::TypeParam => Self::local_subject(def, "type parameter"),
            DefKind::CatchParam => Self::local_subject(def, "exception parameter"),
            DefKind::PatternVar => Self::local_subject(def, "pattern variable"),
            DefKind::Field => Self::member_subject(def, "private field"),
            DefKind::Method => Self::member_subject(def, "private method"),
            DefKind::Class => Self::member_subject(def, "private class"),
            DefKind::Interface => Self::member_subject(def, "private interface"),
            DefKind::Enum => Self::member_subject(def, "private enum"),
            DefKind::Record => Self::member_subject(def, "private record"),
            DefKind::AnnotationType => Self::member_subject(def, "private annotation type"),
            DefKind::Resource | DefKind::Constructor | DefKind::EnumConstant => None,
        }
    }

    /// `subject` for a binding one file scopes, `None` when its own name opts out.
    ///
    /// A leading `_` is the established way to write a name the author is required to give and
    /// does not intend to use — an `@Override` parameter, a `catch` clause whose exception is
    /// genuinely ignored — so honouring it costs a diagnostic nobody could act on. It is confined
    /// to these kinds for the reason the module docs give; `naming-convention` already leaves a
    /// name that does not start with an ASCII letter alone, so the opt-out trades no warning for
    /// another. Java 22's unnamed variable (`_` on its own) binds nothing and reaches no [`Def`],
    /// so this only ever decides `_name`.
    fn local_subject(def: &Def, subject: &'static str) -> Option<&'static str> {
        (!def.name.starts_with('_')).then_some(subject)
    }

    /// `subject` for a member whose disuse this file is entitled to report, `None` otherwise.
    ///
    /// Two gates. The member must be `private`, because anything wider is another file's question;
    /// and nothing must be reaching it without naming it, which is what an annotation and a
    /// serialization member each say in their own way.
    fn member_subject(def: &Def, subject: &'static str) -> Option<&'static str> {
        let reached_unnamed =
            def.is_annotated || Self::SERIALIZATION_MEMBERS.contains(&def.name.as_str());
        (def.is_private && !reached_unnamed).then_some(subject)
    }
}
