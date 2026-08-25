//! The lint rules and the registry that drives them.
//!
//! Each rule is a pure checker paired with metadata ([`RuleMeta`]): a kebab-case name, the
//! [`Category`] whose `jalslint.toml` section declares it, and an accessor that reads its
//! configured [`LintLevel`] out of that section. The library walks the parsed CST, runs every
//! enabled rule, and stamps each [`Finding`] with the rule name and the level. Rules never mutate
//! the tree and never panic.
//!
//! **A rule's built-in level is not written here.** It is the value the section's [`Default`] impl
//! in `jals_config::lint` gives the rule's key, and [`RuleMeta::level`] reads whatever the config
//! holds — so the default has one home and cannot drift from the schema that documents it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_config::lint::Config;
use jals_config::{Category, Feature, LintLevel};
use jals_exec::{LocalBoxFuture, Yielder};
use jals_hir::{Def, FileAnalysis, FileSemantics};
use jals_syntax::{SyntaxNode, SyntaxToken};

mod attribute;
mod boxed_primitive_constructor;
mod cannot_resolve;
mod collapsible_if;
mod compact_source_file;
mod constant_condition;
mod dead_code;
mod empty_catch;
mod empty_javadoc;
mod grouped_import;
mod implicit_this;
mod missing_braces;
mod module_import;
mod naming;
mod print_to_console;
mod type_mismatch;
mod unreported_exception;
mod unused_imports;
mod unused_variables;
mod wildcard_import;

/// A potential problem reported by a rule, before it is tagged with a rule name / severity.
#[derive(Default)]
pub(crate) struct Finding {
    /// Byte range in the original source.
    pub range: Range<usize>,
    /// Human-readable message.
    pub message: String,
    /// Whether the finding's own range is unnecessary code (e.g. an unused local) — a consumer may
    /// render it faded in place. `false` for nearly every rule.
    pub unnecessary: bool,
    /// A secondary unnecessary-code range with its own message — e.g. the dead branch of a
    /// constant `if` (the LSP renders it as a faded hint diagnostic). `None` for nearly every
    /// rule.
    pub unnecessary_range: Option<(Range<usize>, String)>,
}

impl Finding {
    /// A finding spanning `node`.
    pub(crate) fn at_node(node: &SyntaxNode, message: impl Into<String>) -> Self {
        let range = node.text_range();
        Self {
            range: usize::from(range.start())..usize::from(range.end()),
            message: message.into(),
            ..Self::default()
        }
    }

    /// A finding spanning an explicit byte range, for a span no single node or token covers —
    /// e.g. one member of a jals grouped import without the leading trivia rowan parks inside the
    /// member's own node.
    fn at_range(range: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            range,
            message: message.into(),
            ..Self::default()
        }
    }

    /// A finding whose own range is unnecessary code — an unused binding's name, an unused import
    /// declaration — so a consumer fades it in place. What every `unused`-group finding has in
    /// common: each points at something that could simply go. Reached from [`UnusedDefs`] for the
    /// two binding rules and directly by `unused-imports`.
    fn unnecessary_at(range: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            unnecessary: true,
            ..Self::at_range(range, message)
        }
    }

    /// A finding spanning `token`.
    fn at_token(token: &SyntaxToken, message: impl Into<String>) -> Self {
        let range = token.text_range();
        Self {
            range: usize::from(range.start())..usize::from(range.end()),
            message: message.into(),
            ..Self::default()
        }
    }
}

/// Shared message builder for the feature-gated ([`Checker::Gated`]) rules. The feature-gating
/// itself lives in the rule driver ([`crate::LintOutput::lint`]), which runs a gated rule's
/// `find` only when the guarded [`Feature`] is absent from the project's feature set, and stamps
/// this message on each flagged node — so a rule need only carry the detector, not the gate or
/// the message.
pub(crate) struct FeatureGate;

impl FeatureGate {
    /// The diagnostic message for a use of the gated `feature`: `subject` names the flagged
    /// construct (a plural noun phrase, [`Checker::Gated`]'s `subject`), the stabilizing release
    /// preset comes from [`Feature::stabilized_in`] — the single place that fact lives — and the
    /// fix names the two `[package] features` opt-ins (the whole release preset, or just this
    /// feature). The driver phrases every gated rule's message identically with this, built once
    /// per file that has findings.
    pub(crate) fn preview_message(feature: Feature, subject: &str) -> String {
        let name = feature.config_name();
        feature.stabilized_in().map_or_else(
            || {
                format!(
                    "{subject} are a jals dialect feature; to use them, add `\"{name}\"` to \
                     `[package] features`"
                )
            },
            |preset| {
                let preset = preset.config_name();
                format!(
                    "{subject} are a preview feature before `{preset}`; to use them, add \
                     `\"{preset}\"` or `\"{name}\"` to `[package] features`"
                )
            },
        )
    }
}

/// The byte range a node's **significant** tokens span, for the rules that need a node's extent
/// rather than its subtree — and for the suppression map, which ranges an annotated declaration
/// the same way.
///
/// The two are not the same, and the difference is a bug the first time each rule meets it: rowan
/// parks a node's leading trivia *inside* the node, so a statement written on its own line begins,
/// as far as `text_range` is concerned, at the newline that ended the previous one. A rule reading
/// that range then sees a newline every rule-relevant construct has, and a comment written before a
/// node reads as a comment inside it.
pub(crate) struct Significant;

impl Significant {
    /// `node`'s first significant token's start through its last significant token's end, or
    /// `None` when it holds no significant token at all (error recovery).
    pub(crate) fn range(node: &SyntaxNode) -> Option<Range<usize>> {
        let mut ranges = node
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia())
            .map(|token| token.text_range());
        let first = ranges.next()?;
        let last = ranges.last().unwrap_or(first);
        Some(usize::from(first.start())..usize::from(last.end()))
    }
}

/// Shared walk over `jals-hir`'s unused-binding signal ([`FileAnalysis::unused_defs`]) for the two
/// rules that split it: `unused-variables` takes the bindings one file scopes, `dead-code` the
/// `private` members. They are two rules so that they suppress independently (each rule's module
/// docs say why), not because they ask different questions — so the walk and the sentence around
/// the name live here, and a rule contributes only its own naming policy: a `subject` that names
/// the kinds it reports and answers `None` for every kind that is not its.
struct UnusedDefs;

impl UnusedDefs {
    /// Every unused [`Def`] `subject` names, ranged over the binding's own name. Also the
    /// table-edge shim: the async body is boxed once per file here, so a rule's entry in
    /// [`RULES`] is `subject` and nothing else.
    fn findings<'a>(
        analysis: &'a FileAnalysis,
        config: &'a Config,
        subject: fn(&Def, &Config) -> Option<&'static str>,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(async move {
            let mut yielder = Yielder::new();
            let mut out = Vec::new();
            for def in analysis.unused_defs() {
                yielder.tick().await;
                let Some(subject) = subject(def, config) else {
                    continue;
                };
                out.push(Finding::unnecessary_at(
                    def.name_range.clone(),
                    format!("unused {subject} `{}`", def.name),
                ));
            }
            out
        })
    }
}

/// How a rule is invoked, and — because the driver reads it before calling — what a rule is
/// allowed to make the library compute.
///
/// A [`Syntactic`](Checker::Syntactic) rule costs nothing beyond the walk. An
/// [`Analyzed`](Checker::Analyzed) one takes the file's [`FileAnalysis`], which the library
/// computes at most once per lint (or takes from [`crate::LintRequest::file`]) and shares. A
/// [`Semantic`](Checker::Semantic) one additionally receives the project binding, and is therefore
/// the only kind that can reach [`FileSemantics::typed`](jals_hir::FileSemantics::typed) — so the
/// variant is what keeps a resolution-only rule from paying for type inference.
///
/// Rule bodies are `async` (their walks tick cooperatively), so each checker is a plain `fn`
/// pointer returning the boxed future — one box per rule per file, at the table edge.
#[derive(Clone, Copy)]
pub(crate) enum Checker {
    /// A pure syntactic rule: given the CST root, return every finding.
    Syntactic(for<'a> fn(&'a SyntaxNode, &'a Config) -> LocalBoxFuture<'a, Vec<Finding>>),
    /// A rule over the file's own analysis: its name resolution, and the analyses that need no
    /// project. The root comes with it, so this takes the analysis rather than the root.
    Analyzed(for<'a> fn(&'a FileAnalysis, &'a Config) -> LocalBoxFuture<'a, Vec<Finding>>),
    /// A rule that additionally reads the project when the caller supplied one: it resolves
    /// reference types across files and may run type inference. With `None` it either reports
    /// nothing (`cannot-resolve`, `unreported-exception`) or falls back to the file-local analysis
    /// (`type-mismatch`). The basis for cross-file type checking.
    Semantic(
        for<'a> fn(
            &'a FileAnalysis,
            Option<&'a FileSemantics<'a>>,
            &'a Config,
        ) -> LocalBoxFuture<'a, Vec<Finding>>,
    ),
    /// A syntactic rule gated on the project's language [`FeatureSet`](jals_config::FeatureSet): it
    /// names the [`Feature`] it guards, and the driver runs `find` only when the set does not
    /// [`permit`](jals_config::FeatureSet::permits) that feature (threaded from the host via
    /// [`Config::features`](crate::Config::features)) — so for a Java feature an empty set (no
    /// `[package] features` declared) never fires, while a
    /// [`dialect`](jals_config::Feature::is_dialect) feature fires until it is explicitly listed,
    /// because nothing but jals can report its syntax. The driver builds the shared gate message
    /// ([`FeatureGate::preview_message`] from `feature` + `subject`) and stamps it on each flagged
    /// node, so the detector is pure syntax location.
    Gated {
        /// The language feature this rule guards; its findings are reported only when it is disabled.
        feature: Feature,
        /// The flagged construct as a plural noun phrase, spliced into the gate message.
        subject: &'static str,
        /// The detector: the flagged syntax nodes. Run only when `feature` is disabled.
        find: fn(&SyntaxNode) -> Vec<SyntaxNode>,
    },
}

/// A rule: its identity, where it is configured, and its checker.
pub(crate) struct RuleMeta {
    /// Stable kebab-case name. It is both the key inside the rule's `jalslint.toml` section and
    /// the name every [`Diagnostic`](crate::Diagnostic) it produces carries, so a reported finding
    /// names the key that silences it. Unique across sections
    /// (`jals-lint/tests/registry.rs`).
    pub name: &'static str,
    /// The defect class this rule reports, which is also the section it is configured under. See
    /// `jals_config::lint`'s module docs for why the vocabulary is defect classes rather than
    /// clippy's groups.
    pub category: Category,
    /// The rule's configured level, read out of its own section.
    ///
    /// An accessor and not a stored default: the built-in level is the one the section's
    /// [`Default`] gives this key, so a second copy here would be a second thing to keep in step
    /// with the schema. Applying `Config::default()` to this is therefore *the* built-in level,
    /// which is what [`crate::rules()`] reports and what pins the default set in
    /// `jals-lint/tests/registry.rs`.
    pub level: fn(&Config) -> LintLevel,
    /// Whether this rule must not run at all when the file has syntax errors.
    ///
    /// The criterion is **findings derived from type inference**, which a half-parsed tree turns into
    /// noise: a recovered declaration gets a wrong type, and every value written into it then looks
    /// incompatible. It is *not* "needs the project index" — the driver already withholds the project
    /// from a broken parse, which silences every [`Checker::Semantic`] rule on its own — and it is
    /// *not* "reads the resolution": `unused-variables` and `constant-condition` do, but a missing
    /// reference and a literal condition both survive recovery, so they keep reporting.
    ///
    /// Set on `type-mismatch` alone, the one rule that still reports without an index and so is not
    /// covered by withholding one.
    pub needs_clean_parse: bool,
    /// The checker, syntactic or resolution-based.
    pub check: Checker,
}

/// Every rule, grouped by [`Category`] in the order the sections are declared on the config.
pub(crate) const RULES: &[RuleMeta] = &[
    // [correctness]
    cannot_resolve::RULE,
    type_mismatch::RULE,
    unreported_exception::RULE,
    // [compatibility]
    compact_source_file::RULE,
    module_import::RULE,
    grouped_import::RULE,
    attribute::RULE,
    // [suspicious]
    constant_condition::RULE,
    empty_catch::RULE,
    // [unused]
    unused_variables::RULE,
    unused_imports::RULE,
    dead_code::RULE,
    // [complexity]
    collapsible_if::RULE,
    // [performance]
    boxed_primitive_constructor::RULE,
    // [style]
    wildcard_import::RULE,
    missing_braces::RULE,
    // [naming]
    naming::RULE,
    // [documentation]
    empty_javadoc::RULE,
    // [restriction]
    print_to_console::RULE,
    implicit_this::RULE,
];
