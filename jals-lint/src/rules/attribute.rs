//! `attribute`: flag jals attributes (`#[cfg(feature = "x")]`) when the project's `attributes`
//! dialect feature is not enabled.
//!
//! Attributes are a jals-specific dialect construct — not valid Java at any release — so the
//! [`Feature::Attributes`] capability guards them. The rule fires when the project's resolved
//! feature set (from `[package] features`) does *not* include it, and reports nothing once the
//! feature is enabled. The rule driver applies the gate (see [`Checker::Gated`]) and stamps the
//! "add it to `[package] features`" hint; this rule only detects the syntax. The compile frontend
//! strips enabled attributes and applies `cfg` conditional compilation.

use alloc::vec::Vec;

use jals_config::Feature;
use jals_syntax::{SyntaxKind, SyntaxNode};

use crate::diagnostic::Severity;
use crate::rules::{Checker, RuleMeta};

pub(crate) const RULE: RuleMeta = RuleMeta {
    name: "attribute",
    default: Severity::Error,
    check: Checker::Gated {
        feature: Feature::Attributes,
        subject: "attributes (`#[cfg(...)]`)",
        find: Attr::find,
    },
};

/// The `attribute` rule.
struct Attr;

impl Attr {
    fn find(root: &SyntaxNode) -> Vec<SyntaxNode> {
        // The driver runs this only when `attributes` is disabled and stamps the gate message, so
        // here we just locate the syntax. Attributes attach at several depths (imports, any
        // declaration's modifiers, statements), so they are found by kind rather than through one
        // typed parent.
        root.descendants()
            .filter(|node| node.kind() == SyntaxKind::ATTRIBUTE)
            .collect()
    }
}
