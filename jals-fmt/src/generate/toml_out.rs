//! Rendering the pruned config tree as TOML text.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use core::fmt::Write as _;

use serde_json::Value;

/// TOML rendering of the pruned config tree.
pub(super) struct Toml;

impl Toml {
    /// The eight `[section]` tables, in `Config`'s **declaration** order.
    ///
    /// The order has to be stated here: `serde_json::Map` is a `BTreeMap` (the crate's
    /// `preserve_order` feature is off, and the workspace pins its features), so iterating the
    /// serialized config would give alphabetical sections. Key order *within* a section is that
    /// same alphabetical order — deterministic, which is what the workspace requires, just not
    /// declaration order.
    ///
    /// `tests::sections_covers_every_key` pins this list against the schema: a typo here would
    /// drop a whole section from every generated file without any error.
    pub(super) const SECTIONS: [&str; 8] = [
        "layout",
        "blank-lines",
        "braces",
        "wrapping",
        "spacing",
        "comments",
        "imports",
        "literals",
    ];

    /// Render one leaf as a TOML value, or `None` when it has no TOML spelling (`null`) or is not
    /// a leaf at all.
    pub(super) fn scalar(value: &Value) -> Option<String> {
        match value {
            Value::Bool(flag) => Some(if *flag { "true" } else { "false" }.to_owned()),
            Value::Number(number) => Some(number.to_string()),
            Value::String(text) => Some(Self::basic_string(text)),
            Value::Array(items) => {
                let mut out = String::from("[");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&Self::scalar(item)?);
                }
                out.push(']');
                Some(out)
            }
            Value::Null | Value::Object(_) => None,
        }
    }

    /// Quote `text` as a TOML basic string, escaping what the grammar requires.
    ///
    /// Every control character needs an escape — `formatter-off-tag` and `imports.groups` are
    /// user-supplied strings that reach here verbatim from a native config, so an unusual value
    /// must not be able to produce a file that no longer parses.
    fn basic_string(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                // The rest of C0, plus DEL, which TOML also forbids unescaped.
                control if control < '\u{20}' || control == '\u{7f}' => {
                    let _ = write!(out, "\\u{:04X}", control as u32);
                }
                other => out.push(other),
            }
        }
        out.push('"');
        out
    }
}
