//! `naming-convention`: flag declarations whose name breaks the project's naming table.
//!
//! One rule with one key per kind of declaration ([`NamingTable`]), defaulting to the
//! conventional Java casing: types `UpperCamelCase`, methods / fields / statics / parameters /
//! locals `lowerCamelCase`, and a `static final` field — Java's spelling of a constant —
//! `SCREAMING_SNAKE_CASE`. A kind is exempted by setting it to [`Case::Any`], which is a value and
//! not an absent key, so the table stays total and this rule never has to interpret a missing
//! entry.
//!
//! A field declaration picks one of three keys from its own modifiers, most specific first:
//! `static final` is `constants`, a `static` that is not `final` is `statics`, and everything else
//! is `fields`. The middle cell is what rustc's `non_upper_case_globals` covers beyond a constant;
//! its built-in is `lowerCamelCase` because that is what Java writes a mutable global in, so a
//! project wanting the rustc reading sets `statics = "screaming-snake-case"` rather than losing
//! the distinction. The three cells are read off the modifiers the declaration *writes*, so an
//! interface field — `public static final` with none of those tokens spelled — reads as `fields`.
//! Recovering the implicit set is an ancestor check and not a resolution (`jals-hir` does exactly
//! that with `is_static |= in_interface`), so this is a change that could be made and has not
//! been: it would move every interface constant into the `constants` cell, which is a different
//! question from the one this key answers.
//!
//! Constructors and enum constants are not checked at all, and neither is a configuration
//! question. A constructor's name *is* its type's, so a wrong case is already reported once,
//! against the type; and both `SCREAMING_SNAKE_CASE` and `UpperCamelCase` enum constants are
//! attested across the ecosystem, so neither is a convention to enforce.
//!
//! Only plain ASCII identifiers are checked; names with `$` or non-ASCII letters are left alone to
//! avoid false positives.

use alloc::format;
use alloc::vec::Vec;

use jals_syntax::SyntaxKind::{
    self, ANNOTATION_TYPE_DECL, CLASS_DECL, ENUM_DECL, FIELD_DECL, FINAL_KW, IDENT, INTERFACE_DECL,
    LOCAL_VAR_DECL, METHOD_DECL, MODIFIERS, PARAM, RECORD_DECL, STATIC_KW,
};
use jals_syntax::{SyntaxElement, SyntaxNode, SyntaxToken};

use jals_exec::{LocalBoxFuture, Yielder};

use jals_config::Category;
use jals_config::lint::{Case, Config, NamingConvention as NamingTable};

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "naming-convention",
    category: Category::Naming,
    level: |config| config.naming.naming_convention.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(NamingConvention::check),
};

/// The `naming-convention` rule.
struct NamingConvention;

impl NamingConvention {
    /// The table-edge shim: boxes the async rule body once per file.
    fn check<'a>(root: &'a SyntaxNode, config: &'a Config) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(Self::check_impl(root, config))
    }

    async fn check_impl(root: &SyntaxNode, config: &Config) -> Vec<Finding> {
        let table = &config.naming.naming_convention.options;
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL => {
                    if let Some(tok) = Self::first_name_ident(&node) {
                        Self::push_if_bad(&tok, table.types, "type", &mut out);
                    }
                }
                METHOD_DECL => {
                    if let Some(tok) = Self::first_name_ident(&node) {
                        Self::push_if_bad(&tok, table.methods, "method", &mut out);
                    }
                }
                PARAM => {
                    for tok in Self::name_idents(&node) {
                        Self::push_if_bad(&tok, table.parameters, "parameter", &mut out);
                    }
                }
                LOCAL_VAR_DECL => {
                    for tok in Self::name_idents(&node) {
                        Self::push_if_bad(&tok, table.locals, "local variable", &mut out);
                    }
                }
                FIELD_DECL => {
                    let (case, what) = Self::field_cell(&node, table);
                    for tok in Self::name_idents(&node) {
                        Self::push_if_bad(&tok, case, what, &mut out);
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn push_if_bad(tok: &SyntaxToken, case: Case, what: &str, out: &mut Vec<Finding>) {
        let name = tok.text();
        let Some(label) = Expected::label(case) else {
            // `Case::Any` — this kind is not checked at all.
            return;
        };
        if !Self::is_checkable(name) || Expected::accepts(case, name) {
            return;
        }
        out.push(Finding::at_token(
            tok,
            format!("{what} name `{name}` should be {label}"),
        ));
    }

    /// Whether `name` is a plain ASCII identifier worth checking: it starts with an ASCII letter
    /// and contains only ASCII letters, digits, and underscores (so `_`, `$name`, and Unicode
    /// names are skipped).
    fn is_checkable(name: &str) -> bool {
        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Which cell of the table a `FIELD_DECL` is checked against, and what a finding calls it.
    ///
    /// Read off the modifiers the declaration actually writes, most specific first: `static final`
    /// is a constant, a `static` without `final` is one of the class's mutable globals, and
    /// anything else is an ordinary field. The three are exclusive, so a field is reported once.
    fn field_cell(field: &SyntaxNode, table: &NamingTable) -> (Case, &'static str) {
        let modifiers = field.children().find(|c| c.kind() == MODIFIERS);
        let has = |kind| modifiers.as_ref().is_some_and(|m| Self::has_token(m, kind));
        if has(STATIC_KW) {
            if has(FINAL_KW) {
                (table.constants, "constant")
            } else {
                (table.statics, "static field")
            }
        } else {
            (table.fields, "field")
        }
    }

    fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .any(|t| t.kind() == kind)
    }

    /// The first directly-declared name (`IDENT`) of `node`, e.g. a type or method name.
    fn first_name_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| t.kind() == IDENT)
    }

    /// Every directly-declared name (`IDENT`) of `node`, e.g. each variable of `int a, b;`.
    fn name_idents(node: &SyntaxNode) -> Vec<SyntaxToken> {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|t| t.kind() == IDENT)
            .collect()
    }
}

/// What a configured [`Case`] accepts, and what it is called in a diagnostic.
///
/// The predicate lives here rather than on the config enum because it is the rule's reading of the
/// convention, not part of the schema — `jals-config` stays a data model with no behaviour.
struct Expected;

impl Expected {
    /// Whether `name` is spelled in `case`. [`Case::Any`] accepts everything, but the caller has
    /// already stood down on it via [`label`](Self::label), so this is never asked.
    fn accepts(case: Case, name: &str) -> bool {
        match case {
            Case::UpperCamelCase => {
                name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('_')
            }
            Case::LowerCamelCase => {
                name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && !name.contains('_')
            }
            Case::ScreamingSnakeCase => {
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    && name.chars().any(|c| c.is_ascii_uppercase())
            }
            Case::Any => true,
        }
    }

    /// How `case` is named in a diagnostic, or `None` for [`Case::Any`] — which is also how the
    /// caller learns the kind is exempt, so the two facts cannot disagree.
    const fn label(case: Case) -> Option<&'static str> {
        match case {
            Case::UpperCamelCase => Some("UpperCamelCase"),
            Case::LowerCamelCase => Some("lowerCamelCase"),
            Case::ScreamingSnakeCase => Some("UPPER_SNAKE_CASE"),
            Case::Any => None,
        }
    }
}
