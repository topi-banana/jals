//! `naming-convention`: flag declarations whose name breaks the project's naming table.
//!
//! One rule with one key per kind of declaration ([`NamingConvention`]), defaulting to the
//! conventional Java casing: types `UpperCamelCase`, methods / fields / parameters / locals
//! `lowerCamelCase`, and a `static final` field — Java's spelling of a constant —
//! `SCREAMING_SNAKE_CASE`. A kind is exempted by setting it to [`Case::Any`], which is a value and
//! not an absent key, so the table stays total and this rule never has to interpret a missing
//! entry.
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
use jals_config::lint::{Case, Config};

use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "naming-convention",
    category: Category::Naming,
    level: |config| config.naming.naming_convention.level,
    needs_clean_parse: false,
    check: Checker::Syntactic(naming_convention::check),
};

/// The `naming-convention` rule.
mod naming_convention {
    use super::{
        ANNOTATION_TYPE_DECL, CLASS_DECL, Case, Config, ENUM_DECL, FIELD_DECL, FINAL_KW, Finding,
        IDENT, INTERFACE_DECL, LOCAL_VAR_DECL, LocalBoxFuture, METHOD_DECL, MODIFIERS, PARAM,
        RECORD_DECL, STATIC_KW, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, Vec, Yielder,
        expected, format,
    };

    /// The table-edge shim: boxes the async rule body once per file.
    pub(crate) fn check<'a>(
        root: &'a SyntaxNode,
        config: &'a Config,
    ) -> LocalBoxFuture<'a, Vec<Finding>> {
        alloc::boxed::Box::pin(check_impl(root, config))
    }

    async fn check_impl(root: &SyntaxNode, config: &Config) -> Vec<Finding> {
        let table = &config.naming.naming_convention.options;
        let mut yielder = Yielder::new();
        let mut out = Vec::new();
        for node in root.descendants() {
            yielder.tick().await;
            match node.kind() {
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL => {
                    if let Some(tok) = first_name_ident(&node) {
                        push_if_bad(&tok, table.types, "type", &mut out);
                    }
                }
                METHOD_DECL => {
                    if let Some(tok) = first_name_ident(&node) {
                        push_if_bad(&tok, table.methods, "method", &mut out);
                    }
                }
                PARAM => {
                    for tok in name_idents(&node) {
                        push_if_bad(&tok, table.parameters, "parameter", &mut out);
                    }
                }
                LOCAL_VAR_DECL => {
                    for tok in name_idents(&node) {
                        push_if_bad(&tok, table.locals, "local variable", &mut out);
                    }
                }
                FIELD_DECL => {
                    let (case, what) = if is_constant_field(&node) {
                        (table.constants, "constant")
                    } else {
                        (table.fields, "field")
                    };
                    for tok in name_idents(&node) {
                        push_if_bad(&tok, case, what, &mut out);
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn push_if_bad(tok: &SyntaxToken, case: Case, what: &str, out: &mut Vec<Finding>) {
        let name = tok.text();
        let Some(label) = expected::label(case) else {
            // `Case::Any` — this kind is not checked at all.
            return;
        };
        if !is_checkable(name) || expected::accepts(case, name) {
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

    /// Whether a `FIELD_DECL` is a constant (`static final`).
    fn is_constant_field(field: &SyntaxNode) -> bool {
        field
            .children()
            .find(|c| c.kind() == MODIFIERS)
            .is_some_and(|m| has_token(&m, STATIC_KW) && has_token(&m, FINAL_KW))
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
mod expected {
    use super::Case;

    /// Whether `name` is spelled in `case`. [`Case::Any`] accepts everything, but the caller has
    /// already stood down on it via [`label`](label), so this is never asked.
    pub(crate) fn accepts(case: Case, name: &str) -> bool {
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
    pub(crate) const fn label(case: Case) -> Option<&'static str> {
        match case {
            Case::UpperCamelCase => Some("UpperCamelCase"),
            Case::LowerCamelCase => Some("lowerCamelCase"),
            Case::ScreamingSnakeCase => Some("UPPER_SNAKE_CASE"),
            Case::Any => None,
        }
    }
}
