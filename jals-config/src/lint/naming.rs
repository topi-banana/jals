//! `[naming]` — a declaration's name breaks the project's convention.
//!
//! One rule with one option per kind of declaration, rather than one rule per kind. The kinds are
//! not independent policies a project mixes and matches: they are the cells of a single table
//! every Java style guide states as a table, and a project that changes one cell has not changed
//! rules, it has changed the table. Splitting them into seven rules would also make the level
//! seven-valued for no gain — nobody wants `field` names to be an error and `local` names a
//! warning.
//!
//! # Why a field is three cells, and why two of them share a built-in
//!
//! [`fields`](NamingConvention::fields), [`constants`](NamingConvention::constants) and
//! [`statics`](NamingConvention::statics) partition the field declarations. The built-in table
//! writes both `static` cells `SCREAMING_SNAKE_CASE`, which is rustc's reading in
//! `non_upper_case_globals`: what a global's name has to carry is that the binding belongs to the
//! class and outlives every object, and whether it can be reassigned does not change that.
//!
//! This is deliberately stricter than the guide the rest of the table follows. Google Java Style
//! §5.2.4 says *every constant is a `static final` field, but not all `static final` fields are
//! constants*, and writes every non-constant field in `lowerCamelCase` however it is scoped — so
//! under the letter of that guide a `static` without `final` is `lowerCamelCase`. jals takes the
//! stricter reading as its built-in and leaves the other one a config line:
//! `statics = "lower-camel-case"`.
//!
//! That is also the answer to why `constants` and `statics` are two keys when their built-ins
//! agree. They agree *here*; they are exactly where the two readings come apart, so folding them
//! into one cell would leave a project no way to take Google's line on mutable globals without
//! also unspelling its constants.
//!
//! # Why `any` and not an absent key
//!
//! Turning off the check for one kind is [`Case::Any`], a value, and not the absence of a value.
//! An `Option<Case>` would have spelled the same thing with two states for "unset" and "set to the
//! default" that no reader can tell apart, and every consumer would have had to re-derive which
//! default an absent key meant.

use serde::{Deserialize, Serialize};

use super::LintOptions;

/// The expected casing of a name.
///
/// [`Any`](Self::Any) is how a kind is exempted; it is a case that accepts everything, so the
/// rule's table stays total and no consumer has to interpret a missing entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Case {
    /// `UpperCamelCase` — an initial capital and no underscore.
    UpperCamelCase,
    /// `lowerCamelCase` — an initial lowercase letter and no underscore.
    LowerCamelCase,
    /// `SCREAMING_SNAKE_CASE` — capitals, digits and underscores, with at least one letter.
    ScreamingSnakeCase,
    /// Accept any spelling: this kind of declaration is not checked.
    Any,
}

/// `naming-convention` options: the expected case of each kind of declaration.
///
/// Only plain ASCII identifiers are checked whatever these say — a name containing `$` or a
/// non-ASCII letter is left alone, because the conventions are stated over ASCII and guessing at
/// the case of a name written in another script produces false positives rather than findings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct NamingConvention {
    /// Class, interface, enum, record and annotation type declarations.
    pub types: Case,
    /// Method declarations. A constructor is never checked: its name *is* the type's, so a wrong
    /// case is already reported once, against the type.
    pub methods: Case,
    /// Instance field declarations — a field declared without `static`.
    pub fields: Case,
    /// `static final` field declarations — the Java spelling of a constant.
    pub constants: Case,
    /// `static` field declarations that are not `final` — a class's mutable globals. The built-in
    /// is `SCREAMING_SNAKE_CASE`, the reading rustc's `non_upper_case_globals` takes, and it is
    /// stricter than Google Java Style §5.2.4, which writes these in `lowerCamelCase`. A project
    /// holding to that guide sets `statics = "lower-camel-case"` — which is the whole reason this
    /// is its own cell rather than part of `constants`.
    pub statics: Case,
    /// Method, constructor and lambda parameters.
    pub parameters: Case,
    /// Local variable declarations.
    pub locals: Case,
}

impl Default for NamingConvention {
    fn default() -> Self {
        Self {
            types: Case::UpperCamelCase,
            methods: Case::LowerCamelCase,
            fields: Case::LowerCamelCase,
            constants: Case::ScreamingSnakeCase,
            statics: Case::ScreamingSnakeCase,
            parameters: Case::LowerCamelCase,
            locals: Case::LowerCamelCase,
        }
    }
}

/// See [`LintOptions`]: this rule takes options, so it always serializes as a table.
impl LintOptions for NamingConvention {}

lint_section! {
    /// `[naming]` — names against the conventional Java casing table.
    Naming: Naming {
        /// `naming-convention` — a declaration whose name breaks the case its kind is
        /// conventionally written in. Enum constants are not checked at all: both
        /// `SCREAMING_SNAKE_CASE` and `UpperCamelCase` are attested across the ecosystem, so
        /// neither answer is a convention to enforce.
        "naming-convention" => naming_convention: NamingConvention = Warn,
    }
}
