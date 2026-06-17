//! `hex-literal-case`: normalize the case of a hex literal's mantissa digits.

use jals_syntax::SyntaxKind as S;

use crate::config::HexLiteralCase;
use crate::rules::LiteralRule;

/// The `hex-literal-case` rule, holding its resolved (non-`Preserve`) case.
pub(crate) struct HexCase {
    case: HexLiteralCase,
}

impl HexCase {
    /// The rule for `case`, or `None` under `Preserve` (a no-op that is never registered).
    pub(crate) fn new(case: HexLiteralCase) -> Option<Self> {
        (case != HexLiteralCase::Preserve).then_some(Self { case })
    }
}

impl LiteralRule for HexCase {
    fn rewrite(&self, text: &str, _kind: S) -> Option<String> {
        map_hex_case(text, self.case)
    }
}

/// Normalize the case of the hexadecimal digit letters (`a`–`f` / `A`–`F`) of `lit` per `case`,
/// returning the rewritten text — or `None` when nothing should change (the policy is
/// [`Preserve`](HexLiteralCase::Preserve), or `lit` is not a hex literal).
///
/// Only the hex *mantissa* digits are remapped. The `0x` / `0X` prefix is kept verbatim; for a hex
/// float the mantissa stops at the `p` / `P` exponent marker (the marker, its sign and decimal
/// digits, and any `f` / `F` / `d` / `D` suffix follow unchanged); for a hex integer it stops
/// before a trailing `l` / `L` suffix. The mantissa of a well-formed literal holds only hex digits,
/// `.`, and `_`, so an ASCII case map touches exactly the `a`–`f` letters.
fn map_hex_case(lit: &str, case: HexLiteralCase) -> Option<String> {
    if case == HexLiteralCase::Preserve {
        return None;
    }
    let bytes = lit.as_bytes();
    // A hex literal: `0x` / `0X` prefix. (The lexer only emits such a token with at least one
    // hex digit after the prefix, and a numeric token's text is pure ASCII.)
    if bytes.len() < 2 || bytes[0] != b'0' || !matches!(bytes[1], b'x' | b'X') {
        return None;
    }
    let mantissa_end = match bytes[2..].iter().position(|b| matches!(b, b'p' | b'P')) {
        // Hex float: the mantissa ends at the `p` / `P` exponent marker.
        Some(i) => i + 2,
        // Hex integer: the mantissa ends before a trailing `l` / `L` suffix, if any.
        None if matches!(bytes.last(), Some(b'l' | b'L')) => lit.len() - 1,
        None => lit.len(),
    };
    let mantissa = &lit[2..mantissa_end];
    let mapped = match case {
        HexLiteralCase::Upper => mantissa.to_ascii_uppercase(),
        HexLiteralCase::Lower => mantissa.to_ascii_lowercase(),
        HexLiteralCase::Preserve => unreachable!("handled above"),
    };
    Some(format!("{}{}{}", &lit[..2], mapped, &lit[mantissa_end..]))
}

#[cfg(test)]
mod tests {
    use super::map_hex_case;
    use crate::config::HexLiteralCase::{Lower, Preserve, Upper};

    #[test]
    fn preserve_is_a_no_op() {
        assert_eq!(map_hex_case("0xFf", Preserve), None);
    }

    #[test]
    fn non_hex_literals_are_untouched() {
        // Decimal, octal, and binary literals have no hex digits and no `0x` prefix.
        for lit in ["123", "0", "0777", "0b1010", "1_000L", "3.14f", "1e10"] {
            assert_eq!(map_hex_case(lit, Upper), None, "{lit}");
            assert_eq!(map_hex_case(lit, Lower), None, "{lit}");
        }
    }

    #[test]
    fn maps_hex_integer_digits() {
        assert_eq!(map_hex_case("0xff", Upper).as_deref(), Some("0xFF"));
        assert_eq!(map_hex_case("0xFF", Lower).as_deref(), Some("0xff"));
        assert_eq!(
            map_hex_case("0xCafeBabe", Upper).as_deref(),
            Some("0xCAFEBABE")
        );
        assert_eq!(
            map_hex_case("0xCafeBabe", Lower).as_deref(),
            Some("0xcafebabe")
        );
    }

    #[test]
    fn keeps_the_radix_prefix_case() {
        // The `0x` / `0X` prefix is never rewritten — only the digits after it.
        assert_eq!(map_hex_case("0Xff", Upper).as_deref(), Some("0XFF"));
        assert_eq!(map_hex_case("0XFF", Lower).as_deref(), Some("0Xff"));
    }

    #[test]
    fn keeps_the_integer_suffix_case() {
        // The `l` / `L` suffix is outside the mantissa and keeps its case.
        assert_eq!(map_hex_case("0xabl", Upper).as_deref(), Some("0xABl"));
        assert_eq!(map_hex_case("0xABL", Lower).as_deref(), Some("0xabL"));
        // `_` separators in the mantissa are preserved.
        assert_eq!(
            map_hex_case("0xDEAD_beefL", Lower).as_deref(),
            Some("0xdead_beefL")
        );
    }

    #[test]
    fn maps_hex_float_mantissa_only() {
        // The mantissa (before `p`) is mapped; the `p` exponent, its decimal digits, and any
        // `f` / `d` suffix keep their case.
        assert_eq!(map_hex_case("0xA.Bp1f", Lower).as_deref(), Some("0xa.bp1f"));
        assert_eq!(map_hex_case("0xa.bP1F", Upper).as_deref(), Some("0xA.BP1F"));
        // `f` / `d` are valid hex digits in the mantissa, but a suffix after the exponent is not.
        assert_eq!(map_hex_case("0xFp2d", Lower).as_deref(), Some("0xfp2d"));
        assert_eq!(map_hex_case("0X1P-2D", Lower).as_deref(), Some("0X1P-2D"));
    }
}
