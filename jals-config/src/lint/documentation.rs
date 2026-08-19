//! `[documentation]` — a doc comment is missing, empty, or disagrees with what it documents.
//!
//! Javadoc is the only comment Java gives a defined meaning, so it is the only comment a rule here
//! reads. A finding never concerns the *prose*: whether a sentence is good is not a thing a linter
//! settles, and a rule that tried would fire on every codebase. What it settles is whether the
//! comment says anything at all, and whether what it says still matches the declaration beneath it.

use super::NoOptions;

lint_section! {
    /// `[documentation]` — Javadoc that is absent, empty, or out of step with its declaration.
    Documentation: Documentation {
        /// `empty-javadoc` — a `/** ... */` whose content is only whitespace and asterisks. Ports
        /// `clippy::empty_docs`: an empty doc comment is strictly worse than none, because tooling
        /// reads it as documentation that exists and a reader reads it as a promise nobody kept.
        "empty-javadoc" => empty_javadoc: NoOptions = Warn,
    }
}
