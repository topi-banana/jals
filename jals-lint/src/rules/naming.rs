//! `naming-convention`: flag declarations whose name breaks the conventional Java casing.
//!
//! - Type declarations (class / interface / enum / record / annotation type) → `UpperCamelCase`.
//! - Methods → `lowerCamelCase`.
//! - Parameters and local variables → `lowerCamelCase`.
//! - Fields → `lowerCamelCase`, unless `static final` (a constant) → `UPPER_SNAKE_CASE`.
//!
//! Constructors and enum constants are intentionally not checked. Only plain ASCII identifiers
//! are checked; names with `$` or non-ASCII letters are left alone to avoid false positives.

use alloc::format;
use alloc::vec::Vec;

use jals_syntax::SyntaxKind::{self, *};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::diagnostic::Severity;
use crate::rules::{Checker, Finding, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "naming-convention",
    default: Severity::Warn,
    check: Checker::Syntactic(check),
};

fn check(root: &SyntaxNode) -> Vec<Finding> {
    let mut out = Vec::new();
    for node in root.descendants() {
        match node.kind() {
            CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL => {
                if let Some(tok) = first_name_ident(&node) {
                    push_if_bad(&tok, Case::Pascal, "type", &mut out);
                }
            }
            METHOD_DECL => {
                if let Some(tok) = first_name_ident(&node) {
                    push_if_bad(&tok, Case::Camel, "method", &mut out);
                }
            }
            PARAM | LOCAL_VAR_DECL => {
                for tok in name_idents(&node) {
                    push_if_bad(&tok, Case::Camel, "variable", &mut out);
                }
            }
            FIELD_DECL => {
                let (case, what) = if is_constant_field(&node) {
                    (Case::Screaming, "constant")
                } else {
                    (Case::Camel, "field")
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
    if !is_checkable(name) || case.accepts(name) {
        return;
    }
    out.push(Finding::at_token(
        tok,
        format!("{what} name `{name}` should be {}", case.label()),
    ));
}

/// The expected casing for a kind of name.
#[derive(Clone, Copy)]
enum Case {
    /// `UpperCamelCase`.
    Pascal,
    /// `lowerCamelCase`.
    Camel,
    /// `UPPER_SNAKE_CASE`.
    Screaming,
}

impl Case {
    fn accepts(self, name: &str) -> bool {
        match self {
            Case::Pascal => {
                name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('_')
            }
            Case::Camel => {
                name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && !name.contains('_')
            }
            Case::Screaming => {
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    && name.chars().any(|c| c.is_ascii_uppercase())
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            Case::Pascal => "UpperCamelCase",
            Case::Camel => "lowerCamelCase",
            Case::Screaming => "UPPER_SNAKE_CASE",
        }
    }
}

/// Whether `name` is a plain ASCII identifier worth checking: it starts with an ASCII letter and
/// contains only ASCII letters, digits, and underscores (so `_`, `$name`, and Unicode names are
/// skipped).
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
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == kind)
}

/// The first directly-declared name (`IDENT`) of `node`, e.g. a type or method name.
fn first_name_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == IDENT)
}

/// Every directly-declared name (`IDENT`) of `node`, e.g. each variable of `int a, b;`.
fn name_idents(node: &SyntaxNode) -> Vec<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == IDENT)
        .collect()
}
