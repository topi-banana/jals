//! `[correctness]` — the code is wrong.
//!
//! Every rule here reports something a reader would call a bug rather than a preference: a name
//! that resolves to nothing, a value that cannot inhabit the slot it is written into, a checked
//! exception no enclosing clause admits, a `null` written where the declaration said there would
//! never be one. They are the rules a project is least likely to want off.
//!
//! Three of the four read `jals-hir`'s type inference. [`NullnessMismatch`] reads the *tree* —
//! annotations — and belongs here all the same, for the reason the other three do rather than
//! despite it: nullness is not a property inference answers, it is a **contract the annotations
//! state**, and a value contradicting it is a value the slot cannot take. That is the defect class
//! this section already names.
//!
//! # Why three of the four default to `warn`
//!
//! Because a false positive here is expensive and inference over an incomplete classpath is
//! approximate. `cannot-resolve` fires on a fact the index settles — the name is indexed or it is
//! not, and where the index cannot settle it (a supertype outside the project, a partial stub, a
//! name that binds against something other than a scope) it stands down rather than guesses — so
//! it defaults to [`Error`](super::LintLevel::Error). `type-mismatch` and `unreported-exception`
//! both narrow through the stdlib hierarchy and stand down where it is unknown, so they default to
//! [`Warn`](super::LintLevel::Warn): a project that trusts its classpath raises them, and one
//! linting a partial tree is not stopped by them.
//!
//! `nullness-mismatch` is [`Warn`](super::LintLevel::Warn) for a third reason. Its built-in
//! [`default`](NullnessMismatch::default) is [`NonNull`](Nullness::NonNull), so it speaks about
//! declarations nobody annotated — which is the strictness that makes it worth having, and also
//! why it must not be an `error` a build stops on. A project that wants only its *annotated*
//! declarations checked writes `default = "unspecified"`; that is the key, not the level.

// The annotation vendors (JSpecify, JSR-305, IntelliJ, AndroidX, SpotBugs, …) are named as prose
// in this module's docs, not as Rust items — `nullness-mismatch`'s two lists are theirs.
#![allow(clippy::doc_markdown)]

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use super::{LintOptions, NoOptions};

/// How a declaration's nullness reads.
///
/// The third state is not an absent key dressed up: "the declaration says nothing and this project
/// has not said what silence means" is a reachable, useful answer — it is what checks the
/// annotated half of a codebase while leaving the rest alone — and it is a different thing from
/// both of the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Nullness {
    /// Nothing is claimed, so nothing is reported about this declaration. What a codebase that
    /// annotates only part of itself wants.
    Unspecified,
    /// The declaration may hold `null`. A value flowing *out* of it is nullable; nothing flowing
    /// *into* it is ever reported.
    Nullable,
    /// The declaration never holds `null`, so writing one into it is the finding.
    NonNull,
}

/// `nullness-mismatch` options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct NullnessMismatch {
    /// How a declaration carrying neither a [`nullable`](Self::nullable) nor a
    /// [`non_null`](Self::non_null) annotation reads.
    ///
    /// Nothing to do with the rule's built-in **level**, which this crate's prose elsewhere calls
    /// its default: this key is about the *code*, and says what silence in a declaration means.
    ///
    /// [`NonNull`](Nullness::NonNull) out of the box, which is the strict reading — an unannotated
    /// slot rejects `null` — and applies to every kind of declaration alike: field, parameter,
    /// return type, **and local variable**. JSpecify exempts locals from `@NullMarked` on the
    /// grounds that a local's nullness is inferred from its initializer; jals does not, because a
    /// project that asked for the strict reading asked for it about the code it writes, and
    /// `String s = null;` is that code.
    pub default: Nullness,
    /// The annotation types read as `@Nullable`, fully qualified.
    ///
    /// Setting the key **replaces** this list rather than adding to it, so a project narrowing to
    /// one family writes just that family. A name is matched against the annotation as written,
    /// qualified through the file's single-type imports; an annotation whose simple name no import
    /// resolves falls back to matching the last segment of an entry here.
    pub nullable: Vec<String>,
    /// The annotation types read as `@NonNull`, fully qualified. Replaces rather than extends,
    /// exactly like [`nullable`](Self::nullable).
    ///
    /// `lombok.NonNull` is in the built-in list because a codebase that writes it means it — but
    /// it generates a runtime check rather than declaring a static contract, so a project that
    /// reads it as an implementation detail rather than as a claim can drop it. That is what these
    /// two keys are for.
    pub non_null: Vec<String>,
}

impl NullnessMismatch {
    /// The `@Nullable` families recognized out of the box: JSpecify, JSR-305 and its Jakarta
    /// rename, IntelliJ, AndroidX, the Checker Framework, Eclipse JDT, SpotBugs, and Spring.
    const NULLABLE: &'static [&'static str] = &[
        "org.jspecify.annotations.Nullable",
        "javax.annotation.Nullable",
        "javax.annotation.CheckForNull",
        "jakarta.annotation.Nullable",
        "org.jetbrains.annotations.Nullable",
        "androidx.annotation.Nullable",
        "org.checkerframework.checker.nullness.qual.Nullable",
        "org.eclipse.jdt.annotation.Nullable",
        "edu.umd.cs.findbugs.annotations.Nullable",
        "org.springframework.lang.Nullable",
    ];

    /// The `@NonNull` families, one per entry of [`NULLABLE`](Self::NULLABLE) — the three spellings
    /// (`NonNull`, `Nonnull`, `NotNull`) are the vendors', not a choice made here — plus Lombok's,
    /// which has no `@Nullable` counterpart to pair with.
    const NON_NULL: &'static [&'static str] = &[
        "org.jspecify.annotations.NonNull",
        "javax.annotation.Nonnull",
        "jakarta.annotation.Nonnull",
        "org.jetbrains.annotations.NotNull",
        "androidx.annotation.NonNull",
        "org.checkerframework.checker.nullness.qual.NonNull",
        "org.eclipse.jdt.annotation.NonNull",
        "edu.umd.cs.findbugs.annotations.NonNull",
        "org.springframework.lang.NonNull",
        "lombok.NonNull",
    ];
}

impl Default for NullnessMismatch {
    fn default() -> Self {
        Self {
            default: Nullness::NonNull,
            nullable: Self::NULLABLE.iter().copied().map(String::from).collect(),
            non_null: Self::NON_NULL.iter().copied().map(String::from).collect(),
        }
    }
}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for NullnessMismatch {}

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
        /// `nullness-mismatch` — a `null`, or a value a `@Nullable` declaration produced, written
        /// into a slot declared never to hold one. The nullness vocabulary is
        /// [`nullable`](NullnessMismatch::nullable) / [`non_null`](NullnessMismatch::non_null) and
        /// what an unannotated declaration means is [`default`](NullnessMismatch::default).
        "nullness-mismatch" => nullness_mismatch: NullnessMismatch = Warn,
    }
}
