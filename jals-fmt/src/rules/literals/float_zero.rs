//! `float-literal-trailing-zero`: normalize the trailing zero of a decimal float literal.

use alloc::format;
use alloc::string::String;

use jals_syntax::SyntaxKind as S;

use crate::config::FloatLiteralTrailingZero;
use crate::rules::LiteralRule;

/// The `float-literal-trailing-zero` rule, holding its resolved (non-`Preserve`) policy.
pub struct FloatTrailingZero {
    policy: FloatLiteralTrailingZero,
}

impl FloatTrailingZero {
    /// The rule for `policy`, or `None` under `Preserve` (a no-op that is never registered).
    pub(crate) fn new(policy: FloatLiteralTrailingZero) -> Option<Self> {
        (policy != FloatLiteralTrailingZero::Preserve).then_some(Self { policy })
    }
}

impl LiteralRule for FloatTrailingZero {
    fn rewrite(&self, text: &str, _kind: S) -> Option<String> {
        map_float_trailing_zero(text, self.policy)
    }
}

/// Normalize the trailing zero of a **decimal** floating-point literal `lit` per `policy`,
/// returning the rewritten text — or `None` when nothing should change (the policy is
/// [`Preserve`](FloatLiteralTrailingZero::Preserve), or `lit` is out of scope).
///
/// Out of scope (always `None`): a hex literal (`0x…`), a literal with no `.` (a dotless float
/// `1e10` / `100f`, or any integer), and — for [`Never`](FloatLiteralTrailingZero::Never) — a
/// leading-dot float (`.5` / `.0`, whose fraction can never be stripped without producing the
/// illegal bare `.`). The literal's text is pure ASCII, so byte indices are char indices.
///
/// Let `frac` be the digit/underscore run right after the `.` (it stops at an `e` / `E` exponent
/// marker or an `f` / `F` / `d` / `D` suffix). [`Always`](FloatLiteralTrailingZero::Always) inserts
/// a single `0` when `frac` is empty; [`Never`](FloatLiteralTrailingZero::Never) removes `frac`
/// when it is non-empty and consists solely of `0`s (so `1.50` and underscore fractions like
/// `1.0_0` are left intact). Both transforms preserve the numeric value, the suffix, and the
/// exponent, and are idempotent in a single pass.
fn map_float_trailing_zero(lit: &str, policy: FloatLiteralTrailingZero) -> Option<String> {
    if policy == FloatLiteralTrailingZero::Preserve {
        return None;
    }
    let bytes = lit.as_bytes();
    // Hex literals (`0x` / `0X`) are out of scope — left to `hex-literal-case`.
    if bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X') {
        return None;
    }
    // A dotless float (`1e10`, `100f`) or any integer literal has no fraction to normalize.
    let dot = bytes.iter().position(|&b| b == b'.')?;
    // The fraction is the digit/underscore run after the `.`; it ends at the exponent or suffix.
    let mut frac_end = dot + 1;
    while frac_end < bytes.len() && (bytes[frac_end].is_ascii_digit() || bytes[frac_end] == b'_') {
        frac_end += 1;
    }
    match policy {
        FloatLiteralTrailingZero::Always if frac_end == dot + 1 => {
            // Empty fraction: insert a single `0` right after the `.` (`1.` → `1.0`).
            Some(format!("{}0{}", &lit[..=dot], &lit[dot + 1..]))
        }
        FloatLiteralTrailingZero::Never
            // Non-empty integer part (so the bare `.` is never produced) and an all-zero fraction.
            if dot > 0
                && frac_end > dot + 1
                && bytes[dot + 1..frac_end].iter().all(|&b| b == b'0') =>
        {
            // Strip the whole zero run at once (`1.0` / `1.00` → `1.`) — required for idempotency.
            Some(format!("{}{}", &lit[..=dot], &lit[frac_end..]))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::map_float_trailing_zero;
    use crate::config::FloatLiteralTrailingZero::{self, Always, Never};

    #[test]
    fn float_trailing_zero_preserve_is_a_no_op() {
        for lit in ["1.0", "1.", "1.50", "0x1.0p3"] {
            assert_eq!(
                map_float_trailing_zero(lit, FloatLiteralTrailingZero::Preserve),
                None,
                "{lit}"
            );
        }
    }

    #[test]
    fn float_trailing_zero_skips_dotless_and_non_float_literals() {
        // No `.` to normalize: dotless floats and every integer literal are untouched.
        for lit in ["1e10", "100f", "1f", "123", "0xCafe", "0b1010", "0777"] {
            assert_eq!(map_float_trailing_zero(lit, Always), None, "{lit}");
            assert_eq!(map_float_trailing_zero(lit, Never), None, "{lit}");
        }
    }

    #[test]
    fn float_trailing_zero_skips_hex_floats() {
        // Hex floats are left to `hex-literal-case`; the `0x` prefix opts them out of both modes.
        for lit in ["0x1.0p3", "0x1.p3", "0X1.0P-2f"] {
            assert_eq!(map_float_trailing_zero(lit, Always), None, "{lit}");
            assert_eq!(map_float_trailing_zero(lit, Never), None, "{lit}");
        }
    }

    #[test]
    fn always_inserts_a_single_trailing_zero() {
        assert_eq!(
            map_float_trailing_zero("1.", Always).as_deref(),
            Some("1.0")
        );
        assert_eq!(
            map_float_trailing_zero("1.f", Always).as_deref(),
            Some("1.0f")
        );
        assert_eq!(
            map_float_trailing_zero("1.e10", Always).as_deref(),
            Some("1.0e10")
        );
        // A fraction that already has a digit (even another zero) is left as written.
        for lit in ["1.0", "1.00", "1.5", ".5", ".0"] {
            assert_eq!(map_float_trailing_zero(lit, Always), None, "{lit}");
        }
    }

    #[test]
    fn never_strips_an_all_zero_fraction() {
        assert_eq!(map_float_trailing_zero("1.0", Never).as_deref(), Some("1."));
        assert_eq!(
            map_float_trailing_zero("1.00", Never).as_deref(),
            Some("1.")
        );
        assert_eq!(map_float_trailing_zero("0.0", Never).as_deref(), Some("0."));
        assert_eq!(
            map_float_trailing_zero("1.0f", Never).as_deref(),
            Some("1.f")
        );
        assert_eq!(
            map_float_trailing_zero("1.0e10", Never).as_deref(),
            Some("1.e10")
        );
    }

    #[test]
    fn never_keeps_nonzero_underscore_empty_and_leading_dot_fractions() {
        // A non-zero digit, an underscore-grouped fraction, an already-empty fraction, and a
        // leading-dot float (stripping which would yield the illegal bare `.`) are all untouched.
        for lit in ["1.5", "1.50", "1.05", "1.0_0", "1.", ".0", ".5"] {
            assert_eq!(map_float_trailing_zero(lit, Never), None, "{lit}");
        }
    }
}
