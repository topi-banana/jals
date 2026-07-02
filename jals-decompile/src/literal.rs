//! Rendering constant-pool constants as valid Java literals. Shared by the attribute readers
//! (`ConstantValue`) and the bytecode body decompiler (`ldc` / `iconst` / …). Every result parses,
//! including the awkward cases (NaN / infinity, control characters).

use alloc::format;
use alloc::string::{String, ToString};

use crate::types::internal_to_java;

/// Render a `float` constant as a valid Java literal (finite values get an `f` suffix; NaN / infinity
/// map to the `Float` constants).
pub(crate) fn float_literal(v: f32) -> String {
    if v.is_nan() {
        "Float.NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 {
            "Float.POSITIVE_INFINITY".to_string()
        } else {
            "Float.NEGATIVE_INFINITY".to_string()
        }
    } else {
        format!("{v}f")
    }
}

/// Render a `double` constant as a valid Java literal (finite values get a `d` suffix; NaN / infinity
/// map to the `Double` constants).
pub(crate) fn double_literal(v: f64) -> String {
    if v.is_nan() {
        "Double.NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 {
            "Double.POSITIVE_INFINITY".to_string()
        } else {
            "Double.NEGATIVE_INFINITY".to_string()
        }
    } else {
        format!("{v}d")
    }
}

/// Render a `String` constant as an escaped Java string literal (quotes included).
pub(crate) fn string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a `Class` constant (internal name) as a Java class literal (`java/lang/String` →
/// `java.lang.String.class`).
pub(crate) fn class_literal(internal: &str) -> String {
    format!("{}.class", internal_to_java(internal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_and_double_specials_are_valid_java() {
        assert_eq!(float_literal(1.5), "1.5f");
        assert_eq!(float_literal(1.0), "1f");
        assert_eq!(float_literal(f32::NAN), "Float.NaN");
        assert_eq!(float_literal(f32::INFINITY), "Float.POSITIVE_INFINITY");
        assert_eq!(double_literal(2.5), "2.5d");
        assert_eq!(
            double_literal(f64::NEG_INFINITY),
            "Double.NEGATIVE_INFINITY"
        );
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(string_literal("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(string_literal("\u{01}"), "\"\\u0001\"");
    }

    #[test]
    fn class_literals_are_dotted() {
        assert_eq!(class_literal("java/lang/String"), "java.lang.String.class");
    }
}
