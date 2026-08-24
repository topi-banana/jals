//! `[naming]` — a declaration's name breaks the project's convention.
//!
//! One rule with one option per kind of declaration, rather than one rule per kind. The kinds are
//! not independent policies a project mixes and matches: they are the cells of a single table
//! every Java style guide states as a table, and a project that changes one cell has not changed
//! rules, it has changed the table. Splitting them into seven rules would also make the level
//! seven-valued for no gain — nobody wants `field` names to be an error and `local` names a
//! warning.
//!
//! # Why a field is three cells and not one
//!
//! [`fields`](NamingConvention::fields), [`constants`](NamingConvention::constants) and
//! [`statics`](NamingConvention::statics) partition the field declarations, because the case Java
//! conventionally writes one in is not the case it writes another in: a `static final` field is
//! the language's spelling of a constant and is `SCREAMING_SNAKE_CASE`, while every other field —
//! `static` or not — is `lowerCamelCase`. Google Java Style §5.2.4 states exactly that split:
//! *every constant is a `static final` field, but not all `static final` fields are constants*.
//! A project that reads rustc's `non_upper_case_globals` as covering mutable globals too sets
//! `statics = "screaming-snake-case"`; the cell exists so that is a config change rather than a
//! second rule.
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
    /// Instance field declarations: every field that is neither `static final` nor `static`.
    pub fields: Case,
    /// `static final` field declarations — the Java spelling of a constant.
    pub constants: Case,
    /// `static` field declarations that are not `final` — a class's mutable globals, which Java
    /// writes in an instance field's case rather than a constant's. This is the cell rustc's
    /// `non_upper_case_globals` would read as `SCREAMING_SNAKE_CASE`; the built-in is the Java
    /// convention, so a project wanting that parity asks for it.
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
            statics: Case::LowerCamelCase,
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
