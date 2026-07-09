//! `literal-suffix-case`: normalize the case of a numeric literal's trailing type suffix.

use alloc::format;
use alloc::string::String;

use jals_syntax::SyntaxKind as S;

use crate::config::LiteralSuffixCase;
use crate::rules::LiteralRule;

/// The `literal-suffix-case` rule, holding its resolved (non-`Preserve`) case.
pub(crate) struct LiteralSuffix {
    case: LiteralSuffixCase,
}

impl LiteralSuffix {
    /// The rule for `case`, or `None` under `Preserve` (a no-op that is never registered).
    pub(crate) fn new(case: LiteralSuffixCase) -> Option<Self> {
        (case != LiteralSuffixCase::Preserve).then_some(Self { case })
    }
}

impl LiteralRule for LiteralSuffix {
    fn rewrite(&self, text: &str, kind: S) -> Option<String> {
        map_literal_suffix(text, kind, self.case)
    }
}

/// Normalize the case of the trailing type suffix of the numeric literal `lit` (whose token is
/// `kind`) per `case`, returning the rewritten text — or `None` when nothing should change (the
/// policy is [`Preserve`](LiteralSuffixCase::Preserve), the literal carries no suffix, or it is
/// already in the requested case).
///
/// The suffix is always the literal's final character: the `l` / `L` `long` suffix of an
/// [`INT_LITERAL`](S::INT_LITERAL), or the `f` / `F` / `d` / `D` `float` / `double` suffix of a
/// [`FLOAT_LITERAL`](S::FLOAT_LITERAL). The token kind disambiguates the otherwise ambiguous
/// trailing letters: a final `f` / `d` on an integer literal is a hex digit (`0xabcdef`), not a
/// suffix, and a float literal never ends in `l` / `L`. Only that one letter is remapped; the
/// value, radix prefix, mantissa, and exponent are kept verbatim. The literal's text is pure
/// ASCII, so the final byte is the final character.
fn map_literal_suffix(lit: &str, kind: S, case: LiteralSuffixCase) -> Option<String> {
    if case == LiteralSuffixCase::Preserve {
        return None;
    }
    let last = *lit.as_bytes().last()?;
    let is_suffix = match kind {
        S::INT_LITERAL => matches!(last, b'l' | b'L'),
        S::FLOAT_LITERAL => matches!(last, b'f' | b'F' | b'd' | b'D'),
        _ => false,
    };
    if !is_suffix {
        return None;
    }
    let mapped = match case {
        LiteralSuffixCase::Upper => last.to_ascii_uppercase(),
        LiteralSuffixCase::Lower => last.to_ascii_lowercase(),
        LiteralSuffixCase::Preserve => unreachable!("handled above"),
    };
    if mapped == last {
        return None;
    }
    Some(format!("{}{}", &lit[..lit.len() - 1], mapped as char))
}

#[cfg(test)]
mod tests {
    use jals_syntax::SyntaxKind;

    use super::map_literal_suffix;
    use crate::config::LiteralSuffixCase::{
        Lower as SufLower, Preserve as SufPreserve, Upper as SufUpper,
    };

    const INT: SyntaxKind = SyntaxKind::INT_LITERAL;
    const FLOAT: SyntaxKind = SyntaxKind::FLOAT_LITERAL;

    #[test]
    fn literal_suffix_preserve_is_a_no_op() {
        for (lit, kind) in [("123l", INT), ("1.5f", FLOAT), ("2.0D", FLOAT)] {
            assert_eq!(map_literal_suffix(lit, kind, SufPreserve), None, "{lit}");
        }
    }

    #[test]
    fn literal_suffix_maps_the_integer_long_suffix() {
        assert_eq!(
            map_literal_suffix("123l", INT, SufUpper).as_deref(),
            Some("123L")
        );
        assert_eq!(
            map_literal_suffix("123L", INT, SufLower).as_deref(),
            Some("123l")
        );
        // The suffix is the only thing touched — hex digits, `_` separators, and the radix prefix
        // are all preserved.
        assert_eq!(
            map_literal_suffix("0xCAFEl", INT, SufUpper).as_deref(),
            Some("0xCAFEL")
        );
        assert_eq!(
            map_literal_suffix("0b1010L", INT, SufLower).as_deref(),
            Some("0b1010l")
        );
    }

    #[test]
    fn literal_suffix_maps_the_float_and_double_suffix() {
        for (lit, want) in [("1.5f", "1.5F"), ("1.5d", "1.5D"), ("1e10f", "1e10F")] {
            assert_eq!(
                map_literal_suffix(lit, FLOAT, SufUpper).as_deref(),
                Some(want)
            );
        }
        for (lit, want) in [("1.5F", "1.5f"), ("2.0D", "2.0d")] {
            assert_eq!(
                map_literal_suffix(lit, FLOAT, SufLower).as_deref(),
                Some(want)
            );
        }
    }

    #[test]
    fn literal_suffix_leaves_an_integer_literals_trailing_hex_digit_alone() {
        // A final `f` / `d` on an *integer* literal is a hex digit, never a suffix, so it must
        // never be rewritten — the token kind is what tells the two apart.
        for lit in ["0xabcdef", "0xff", "0xFD", "0xabcd"] {
            assert_eq!(map_literal_suffix(lit, INT, SufUpper), None, "{lit}");
            assert_eq!(map_literal_suffix(lit, INT, SufLower), None, "{lit}");
        }
    }

    #[test]
    fn literal_suffix_maps_only_the_hex_floats_final_letter() {
        // The `f` inside the hex-float mantissa is left alone; only the trailing `d` suffix flips.
        assert_eq!(
            map_literal_suffix("0x1.fp3d", FLOAT, SufUpper).as_deref(),
            Some("0x1.fp3D")
        );
        assert_eq!(
            map_literal_suffix("0x1p3f", FLOAT, SufUpper).as_deref(),
            Some("0x1p3F")
        );
    }

    #[test]
    fn literal_suffix_skips_unsuffixed_literals_and_already_correct_case() {
        // No trailing suffix letter to normalize.
        for (lit, kind) in [("123", INT), ("1.5", FLOAT), ("1e10", FLOAT), ("0xff", INT)] {
            assert_eq!(map_literal_suffix(lit, kind, SufUpper), None, "{lit}");
            assert_eq!(map_literal_suffix(lit, kind, SufLower), None, "{lit}");
        }
        // Already in the requested case: no change (returns `None`).
        assert_eq!(map_literal_suffix("123L", INT, SufUpper), None);
        assert_eq!(map_literal_suffix("1.5f", FLOAT, SufLower), None);
    }
}
