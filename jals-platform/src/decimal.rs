//! Java's decimal layout for a `double` and a `float`.
//!
//! # What this is not
//!
//! It is **not** a float-to-string algorithm. Producing the shortest decimal that round-trips to a
//! given binary float is a genuinely hard problem — Steele and White, then Grisu, then Ryū — and
//! `core`'s own formatter already solves it correctly. Writing a second one here would be a second
//! implementation of a hard thing, held to a correctness bar this crate cannot check.
//!
//! So the digits come from `core`, through `{:e}`, and only the **layout** is ours. Java and Rust
//! disagree about where a decimal point goes and about what an exponent looks like, and that
//! disagreement is a rendering convention rather than a numeric fact:
//!
//! | value | Rust `{}` | Java |
//! | --- | --- | --- |
//! | `1.0` | `1` | `1.0` |
//! | `0.0001` | `0.0001` | `1.0E-4` |
//! | `1e7` | `10000000` | `1.0E7` |
//! | `f64::INFINITY` | `inf` | `Infinity` |
//!
//! # Why a `float` is not rendered as a `double`
//!
//! [`Decimal::of_f32`] formats from the `f32`, not from a widened `f64`. Widening first prints
//! `0.10000000149011612` where Java prints `0.1`: the shortest decimal that round-trips *at
//! `f32` width* is a different (shorter) string than the one that round-trips at `f64` width, and
//! there is no way to recover the first from the second. The same asymmetry is why parsing goes
//! through `f32::from_str` rather than parsing wide and narrowing, which rounds twice.

use alloc::format;
use alloc::string::String;

/// Java's rendering of a floating-point value.
pub(crate) struct Decimal;

impl Decimal {
    /// The exponent range Java renders without an `E`: `10^-3 <= |value| < 10^7`.
    const PLAIN: core::ops::RangeInclusive<i32> = -3..=6;

    /// `value` as `Double.toString` renders it.
    pub(crate) fn of_f64(value: f64) -> String {
        if value.is_nan() {
            return String::from("NaN");
        }
        if value.is_infinite() {
            return String::from(if value < 0.0 { "-Infinity" } else { "Infinity" });
        }
        if value == 0.0 {
            return String::from(if value.is_sign_negative() { "-0.0" } else { "0.0" });
        }
        Self::lay_out(&format!("{:e}", value.abs()), value.is_sign_negative())
    }

    /// `value` as `Float.toString` renders it — formatted at `f32` width; see the module docs.
    pub(crate) fn of_f32(value: f32) -> String {
        if value.is_nan() {
            return String::from("NaN");
        }
        if value.is_infinite() {
            return String::from(if value < 0.0 { "-Infinity" } else { "Infinity" });
        }
        if value == 0.0 {
            return String::from(if value.is_sign_negative() { "-0.0" } else { "0.0" });
        }
        Self::lay_out(&format!("{:e}", value.abs()), value.is_sign_negative())
    }

    /// Re-lay `core`'s scientific form (`d.dddde±ee`) into Java's.
    fn lay_out(scientific: &str, negative: bool) -> String {
        let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific, "0"));
        let exponent: i32 = exponent.parse().unwrap_or(0);
        let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        let sign = if negative { "-" } else { "" };
        if Self::PLAIN.contains(&exponent) {
            return format!("{sign}{}", Self::plain(&digits, exponent));
        }
        let (head, tail) = digits.split_at(1);
        let tail = if tail.is_empty() { "0" } else { tail };
        format!("{sign}{head}.{tail}E{exponent}")
    }

    /// `digits` with the point placed `exponent` positions in, padded so a digit stands on each
    /// side of it — Java writes `100.0` and `0.001`, never `100.` or `.001`.
    fn plain(digits: &str, exponent: i32) -> String {
        if exponent >= 0 {
            let point = exponent as usize + 1;
            let mut out = String::from(digits);
            while out.len() < point {
                out.push('0');
            }
            let (whole, fraction) = out.split_at(point);
            let fraction = if fraction.is_empty() { "0" } else { fraction };
            return format!("{whole}.{fraction}");
        }
        let zeros = "0".repeat((-exponent - 1) as usize);
        format!("0.{zeros}{digits}")
    }

    /// The `f64` `text` spells, accepting what `Double.parseDouble` accepts.
    pub(crate) fn parse(text: &str) -> Option<f64> {
        Self::admitted(text)?.parse().ok()
    }

    /// The `f32` `text` spells — parsed at `f32` width so it rounds once; see the module docs.
    pub(crate) fn parse_f32(text: &str) -> Option<f32> {
        Self::admitted(text)?.parse().ok()
    }

    /// `text` as Rust's parser should see it, or `None` when Java would not accept it at all.
    ///
    /// Rust's `from_str` is **more permissive than Java's** about the two non-numeric spellings: it
    /// takes `inf`, `infinity` and `nan` in any case, where Java takes exactly `Infinity` and
    /// exactly `NaN`. Delegating without this check would make `Double.parseDouble("inf")` answer
    /// infinity here and throw on a JVM — a divergence no test of the *numbers* would ever catch,
    /// since every numeric spelling agrees.
    fn admitted(text: &str) -> Option<&str> {
        let trimmed = Self::without_suffix(text);
        let magnitude = trimmed
            .strip_prefix(['+', '-'])
            .unwrap_or(trimmed);
        if magnitude.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return (magnitude == "NaN" || magnitude == "Infinity").then_some(trimmed);
        }
        Some(trimmed)
    }

    /// `text` with a trailing Java type suffix removed.
    ///
    /// `1.5f` and `1.5d` are numbers Java parses and Rust does not. The suffix is stripped only
    /// when a digit or a point precedes it, so `inf` and `Inf` keep their `f` — those are not
    /// spellings Java accepts either, and turning one into `in` would change which of the two
    /// rejects it.
    fn without_suffix(text: &str) -> &str {
        let mut chars = text.chars().rev();
        let last = chars.next();
        let previous = chars.next();
        match (last, previous) {
            (Some('d' | 'D' | 'f' | 'F'), Some(before))
                if before.is_ascii_digit() || before == '.' =>
            {
                &text[..text.len() - 1]
            }
            _ => text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Decimal;

    #[test]
    fn a_double_renders_the_way_java_renders_one() {
        for (value, expected) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (-1.0, "-1.0"),
            (100.0, "100.0"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (1.0e7, "1.0E7"),
            (9_999_999.0, "9999999.0"),
            (1.4142135623730951, "1.4142135623730951"),
            (f64::MAX, "1.7976931348623157E308"),
            (f64::MIN_POSITIVE, "2.2250738585072014E-308"),
            (1.0 / 3.0, "0.3333333333333333"),
        ] {
            assert_eq!(Decimal::of_f64(value), expected, "rendering {value}");
        }
        assert_eq!(Decimal::of_f64(f64::NAN), "NaN");
        assert_eq!(Decimal::of_f64(f64::INFINITY), "Infinity");
        assert_eq!(Decimal::of_f64(f64::NEG_INFINITY), "-Infinity");
    }

    /// The case that makes the two widths two bindings rather than one.
    #[test]
    fn a_float_renders_at_float_width() {
        assert_eq!(Decimal::of_f32(0.1_f32), "0.1");
        assert_eq!(Decimal::of_f64(f64::from(0.1_f32)), "0.10000000149011612");
        assert_eq!(Decimal::of_f32(1.0_f32), "1.0");
        assert_eq!(Decimal::of_f32(f32::MAX), "3.4028235E38");
    }

    /// The Java half allocates a fixed-width array before it calls across, so a rendering that did
    /// not fit would be a refusal at run time rather than a compile error. Pin the bound.
    #[test]
    fn every_rendering_fits_the_array_the_java_side_allocates() {
        const LIMIT: usize = 32;
        for value in [
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            5.0e-324,
            1.0 / 3.0,
            -1.0 / 3.0,
        ] {
            let rendered = Decimal::of_f64(value);
            assert!(
                rendered.len() <= LIMIT,
                "`{rendered}` is {} characters, past the {LIMIT} the Java half allocates",
                rendered.len()
            );
        }
    }

    #[test]
    fn parsing_accepts_what_java_accepts() {
        assert_eq!(Decimal::parse("1.5"), Some(1.5));
        assert_eq!(Decimal::parse("1.5d"), Some(1.5));
        assert_eq!(Decimal::parse("1.5f"), Some(1.5));
        assert_eq!(Decimal::parse("-2"), Some(-2.0));
        assert_eq!(Decimal::parse("1e3"), Some(1000.0));
        assert_eq!(Decimal::parse("12x"), None);
        assert_eq!(Decimal::parse(""), None);
        assert_eq!(Decimal::parse_f32("0.1"), Some(0.1_f32));
    }

    /// `inf` is not a spelling Java accepts, and stripping its `f` would make it `in` — still
    /// rejected, but by the wrong side and with a different reason.
    #[test]
    fn a_suffix_is_stripped_only_after_a_digit_or_a_point() {
        assert_eq!(Decimal::without_suffix("inf"), "inf");
        assert_eq!(Decimal::without_suffix("2.f"), "2.");
        assert_eq!(Decimal::without_suffix("1.5d"), "1.5");
        assert_eq!(Decimal::parse("2.f"), Some(2.0));
    }

    /// Rust's parser takes three spellings Java does not, and every one of them would have gone
    /// through unnoticed: no test of the numbers can catch a disagreement about `inf`.
    #[test]
    fn only_javas_spellings_of_the_non_numeric_values_are_accepted() {
        assert_eq!(Decimal::parse("NaN").map(f64::is_nan), Some(true));
        assert_eq!(Decimal::parse("Infinity"), Some(f64::INFINITY));
        assert_eq!(Decimal::parse("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(Decimal::parse("+Infinity"), Some(f64::INFINITY));
        for rejected in ["inf", "Inf", "INF", "infinity", "nan", "NAN", "-inf"] {
            assert_eq!(Decimal::parse(rejected), None, "Java rejects `{rejected}`");
            assert_eq!(Decimal::parse_f32(rejected), None, "Java rejects `{rejected}`");
        }
        assert_eq!(Decimal::parse_f32("Infinity"), Some(f32::INFINITY));
    }
}
