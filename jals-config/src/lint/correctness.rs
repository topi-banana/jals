//! `[correctness]` — the code is wrong.
//!
//! Every rule here reports something a reader would call a bug rather than a preference: a name
//! that resolves to nothing, a value that cannot inhabit the slot it is written into, a checked
//! exception no enclosing clause admits. They are the rules a project is least likely to want off,
//! and the only rules whose findings are produced by `jals-hir`'s type inference rather than by
//! reading the tree.
//!
//! # Why two of the three default to `warn`
//!
//! Because a false positive here is expensive and inference over an incomplete classpath is
//! approximate. `cannot-resolve` fires on a fact the index settles — the name is indexed or it is
//! not, and where the index cannot settle it (a supertype outside the project, a partial stub, a
//! name that binds against something other than a scope) it stands down rather than guesses — so
//! it defaults to [`Error`](super::LintLevel::Error). `type-mismatch` and `unreported-exception`
//! both narrow through the stdlib hierarchy and stand down where it is unknown, so they default to
//! [`Warn`](super::LintLevel::Warn): a project that trusts its classpath raises them, and one
//! linting a partial tree is not stopped by them.

use super::NoOptions;

lint_section! {
    /// `[correctness]` — findings a reader would call a bug.
    Correctness: Correctness {
        /// `cannot-resolve` — a type, variable or method that the project index does not define:
        /// javac's "cannot find symbol", in all three name-spaces. Reports nothing without a
        /// project (a file linted on its own has no index to miss from) and nothing on a broken
        /// parse.
        "cannot-resolve" => cannot_resolve: NoOptions = Error,
        /// `type-mismatch` — a value written into a slot its type cannot inhabit: a narrowing
        /// assignment, an incompatible `return`, an argument the selected overload does not take.
        /// The one rule that still reports without a project index, and therefore the one that
        /// stands itself down on a broken parse.
        "type-mismatch" => type_mismatch: NoOptions = Warn,
        /// `unreported-exception` — a checked exception thrown where no enclosing `catch` admits
        /// it and no `throws` clause declares it (JLS §11.2). Needs the project index to classify
        /// a thrown type as checked, so it reports nothing without one.
        "unreported-exception" => unreported_exception: NoOptions = Warn,
    }
}
