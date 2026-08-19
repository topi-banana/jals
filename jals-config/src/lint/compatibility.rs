//! `[compatibility]` — the code is legal here and not everywhere it has to run.
//!
//! Every rule here is **gated on the project's `[package] features`**: it reports a construct only
//! while the feature that makes the construct legal is *absent* from the set. The engine, not the
//! rule, applies that gate — a rule names the [`Feature`](crate::Feature) it guards and the
//! construct it looks for, and the driver phrases every one of them identically (`jals-lint`'s
//! `FeatureGate`), so the four cannot drift apart in wording.
//!
//! The consequence for a plain Java project is that the section is silent: a source with no
//! manifest declares no features, and a feature set that declares nothing permits every *Java*
//! feature. What stays reportable in that state is the jals **dialect** syntax, because nothing
//! but jals compiles it.
//!
//! All four default to [`Error`](super::LintLevel::Error): unlike every other section, the finding
//! is not a judgement about the code but a statement that the compiler will reject it.

use super::NoOptions;

lint_section! {
    /// `[compatibility]` — constructs the project's declared features do not allow.
    Compatibility: Compatibility {
        /// `compact-source-file` — a top-level `main`, method or field written outside any type
        /// declaration (JEP 512), while `compact-source-files` is not enabled.
        "compact-source-file" => compact_source_file: NoOptions = Error,
        /// `module-import` — `import module java.base;` (JEP 511), while `module-imports` is not
        /// enabled.
        "module-import" => module_import: NoOptions = Error,
        /// `grouped-import` — the jals dialect's `import java.util.{List, Map};`, while
        /// `grouped-imports` is not enabled. A dialect feature: no Java release stabilizes it, so
        /// this reports until the project asks for it.
        "grouped-import" => grouped_import: NoOptions = Error,
        /// `attribute` — the jals dialect's `#[cfg(...)]` attributes, while `attributes` is not
        /// enabled. Distinct from the fixed `cfg` diagnostic, which reports a *malformed*
        /// attribute in a project that does have the feature and is not configurable.
        "attribute" => attribute: NoOptions = Error,
    }
}
