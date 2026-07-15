//! Rendering constant-pool constants as valid Java literals. Shared by the attribute readers
//! (`ConstantValue`) and the bytecode body decompiler (`ldc` / `iconst` / …). Every result parses,
//! including the awkward cases (NaN / infinity, control characters).

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use crate::types::JavaType;

/// Namespace for rendering constant-pool constants as valid Java literals.
pub(crate) struct Literal;

impl Literal {
    /// Render a `float` constant as a valid Java literal (finite values get an `f` suffix; NaN /
    /// infinity map to the `Float` constants).
    pub(crate) fn float_literal(v: f32) -> String {
        if v.is_nan() {
            "Float.NaN".to_owned()
        } else if v.is_infinite() {
            if v.is_sign_positive() {
                "Float.POSITIVE_INFINITY".to_owned()
            } else {
                "Float.NEGATIVE_INFINITY".to_owned()
            }
        } else {
            format!("{v}f")
        }
    }

    /// Render a `double` constant as a valid Java literal (finite values get a `d` suffix; NaN /
    /// infinity map to the `Double` constants).
    pub(crate) fn double_literal(v: f64) -> String {
        if v.is_nan() {
            "Double.NaN".to_owned()
        } else if v.is_infinite() {
            if v.is_sign_positive() {
                "Double.POSITIVE_INFINITY".to_owned()
            } else {
                "Double.NEGATIVE_INFINITY".to_owned()
            }
        } else {
            format!("{v}d")
        }
    }

    /// Render a `char` constant as an escaped Java character literal (quotes included).
    pub(crate) fn char_literal(c: char) -> String {
        let mut out = String::from("'");
        if c == '\'' {
            out.push_str("\\'");
        } else {
            Self::push_escaped(c, &mut out);
        }
        out.push('\'');
        out
    }

    /// Render a `String` constant as an escaped Java string literal (quotes included).
    pub(crate) fn string_literal(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' {
                out.push_str("\\\"");
            } else {
                Self::push_escaped(c, &mut out);
            }
        }
        out.push('"');
        out
    }

    /// Push one character of a `char` / `String` literal body, applying the escapes the two kinds
    /// share (`\\`, `\n`, `\r`, `\t`, `\b`, `\f`, and `\uXXXX` for any other control character).
    /// The delimiting quote character each kind must additionally escape is the caller's job.
    fn push_escaped(c: char, out: &mut String) {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }

    /// Render a `Class` constant (internal name) as a Java class literal (`java/lang/String` →
    /// `java.lang.String.class`).
    pub(crate) fn class_literal(internal: &str) -> String {
        format!("{}.class", JavaType::internal_to_java(internal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_and_double_specials_are_valid_java() {
        assert_eq!(Literal::float_literal(1.5), "1.5f");
        assert_eq!(Literal::float_literal(1.0), "1f");
        assert_eq!(Literal::float_literal(f32::NAN), "Float.NaN");
        assert_eq!(
            Literal::float_literal(f32::INFINITY),
            "Float.POSITIVE_INFINITY"
        );
        assert_eq!(Literal::double_literal(2.5), "2.5d");
        assert_eq!(
            Literal::double_literal(f64::NEG_INFINITY),
            "Double.NEGATIVE_INFINITY"
        );
        assert_eq!(
            Literal::double_literal(f64::INFINITY),
            "Double.POSITIVE_INFINITY"
        );
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(Literal::string_literal("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(Literal::string_literal("\u{01}"), "\"\\u0001\"");
    }

    #[test]
    fn class_literals_are_dotted() {
        assert_eq!(
            Literal::class_literal("java/lang/String"),
            "java.lang.String.class"
        );
    }
}
