//! `[literals]` — the opt-in numeric-literal rewrites.
//!
//! The one jals-native rule family: no native Java formatter rewrites a literal, so every key
//! defaults to `preserve` and `import` can never move one (`MAPPING.md` §5.6). A rewrite changes
//! a literal token's *text* and never its kind, so the significant-token invariant holds up to
//! spelling — which is why these are the only rules gated purely on the user asking for them.
//!
//! Applied where the token is emitted rather than as a tree pass: the rewrite is a pure function
//! of one token's text and kind, and routing it through the visitor keeps a single place where a
//! token's final text is decided.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;

use jals_config::fmt::{FloatLiteralTrailingZero, HexLiteralCase, LiteralSuffixCase, Literals};
use jals_syntax::SyntaxKind;

pub(crate) use api::{KINDS, apply, is_active};

/// The literal rewrites, as a namespace over `[literals]`.
pub(crate) mod api {
    use super::{
        Cow, FloatLiteralTrailingZero, HexLiteralCase, LiteralSuffixCase, Literals, String,
        SyntaxKind, format,
    };

    /// The token kinds `[literals]` can respell.
    ///
    /// Read by [`apply`](apply) and by this operation's row in
    /// [`OPERATIONS`](super::token_license::OPERATIONS), so the license can never come out narrower
    /// than the pass. Adding a rewrite over a third kind without adding it here would make the pass
    /// change a token no row claims — which the fail-safe answers by returning the whole file
    /// unformatted.
    pub(crate) const KINDS: &[SyntaxKind] = &[SyntaxKind::INT_LITERAL, SyntaxKind::FLOAT_LITERAL];

    /// The final text of a literal token, borrowing when nothing changes.
    ///
    /// The three rewrites compose in a fixed order — hex digits, then the trailing zero, then the
    /// suffix letter — and they operate on disjoint parts of a literal, so the order does not
    /// affect the result. It is fixed anyway, so the composition stays idempotent by inspection.
    pub(crate) fn apply(text: &str, kind: SyntaxKind, rules: Literals) -> Cow<'_, str> {
        if !KINDS.contains(&kind) {
            return Cow::Borrowed(text);
        }
        let mut current = Cow::Borrowed(text);
        if let Some(next) = hex_case(&current, rules.hex_case) {
            current = Cow::Owned(next);
        }
        if let Some(next) = float_trailing_zero(&current, rules.float_trailing_zero) {
            current = Cow::Owned(next);
        }
        if let Some(next) = suffix_case(&current, kind, rules.suffix_case) {
            current = Cow::Owned(next);
        }
        current
    }

    /// Whether any rule in `rules` can change a literal at all.
    pub(crate) fn is_active(rules: Literals) -> bool {
        rules.hex_case != HexLiteralCase::Preserve
            || rules.float_trailing_zero != FloatLiteralTrailingZero::Preserve
            || rules.suffix_case != LiteralSuffixCase::Preserve
    }

    /// Case of a hex literal's **mantissa** digits.
    ///
    /// The `0x` / `0X` prefix is kept verbatim. For a hex float the mantissa stops at the `p` /
    /// `P` exponent marker (marker, sign, decimal digits, and any `f` / `d` suffix follow
    /// unchanged); for a hex integer it stops before a trailing `l` / `L`. A well-formed
    /// mantissa holds only hex digits, `.`, and `_`, so an ASCII case map touches exactly the
    /// `a`–`f` letters.
    fn hex_case(lit: &str, case: HexLiteralCase) -> Option<String> {
        if case == HexLiteralCase::Preserve {
            return None;
        }
        let bytes = lit.as_bytes();
        if bytes.len() < 3 || bytes[0] != b'0' || !matches!(bytes[1], b'x' | b'X') {
            return None;
        }
        let mantissa_end = match bytes[2..].iter().position(|b| matches!(b, b'p' | b'P')) {
            Some(at) => at + 2,
            None if matches!(bytes.last(), Some(b'l' | b'L')) => lit.len() - 1,
            None => lit.len(),
        };
        let mantissa = &lit[2..mantissa_end];
        let mapped = match case {
            HexLiteralCase::Upper => mantissa.to_ascii_uppercase(),
            HexLiteralCase::Lower => mantissa.to_ascii_lowercase(),
            HexLiteralCase::Preserve => return None,
        };
        if mapped == mantissa {
            return None;
        }
        Some(format!("{}{}{}", &lit[..2], mapped, &lit[mantissa_end..]))
    }

    /// Whether a **decimal** float literal carries a trailing zero.
    ///
    /// Out of scope: a hex literal, a literal with no `.` (a dotless float `1e10` / `100f`, or
    /// any integer), and — for [`Never`](FloatLiteralTrailingZero::Never) — a leading-dot float
    /// (`.5`), whose fraction cannot be stripped without producing the illegal bare `.`.
    ///
    /// `Never` strips the whole zero run at once (`1.00` → `1.`), which is what makes it
    /// idempotent in one pass; a fraction with a non-zero digit or an underscore (`1.0_0`) is
    /// left intact.
    fn float_trailing_zero(lit: &str, policy: FloatLiteralTrailingZero) -> Option<String> {
        if policy == FloatLiteralTrailingZero::Preserve {
            return None;
        }
        let bytes = lit.as_bytes();
        if bytes.len() >= 2 && bytes[0] == b'0' && matches!(bytes[1], b'x' | b'X') {
            return None;
        }
        let dot = bytes.iter().position(|&b| b == b'.')?;
        let mut frac_end = dot + 1;
        while frac_end < bytes.len()
            && (bytes[frac_end].is_ascii_digit() || bytes[frac_end] == b'_')
        {
            frac_end += 1;
        }
        match policy {
            FloatLiteralTrailingZero::Always if frac_end == dot + 1 => {
                Some(format!("{}0{}", &lit[..=dot], &lit[dot + 1..]))
            }
            FloatLiteralTrailingZero::Never
                if dot > 0
                    && frac_end > dot + 1
                    && bytes[dot + 1..frac_end].iter().all(|&b| b == b'0') =>
            {
                Some(format!("{}{}", &lit[..=dot], &lit[frac_end..]))
            }
            _ => None,
        }
    }

    /// Case of a literal's trailing type-suffix letter.
    ///
    /// The token kind disambiguates the otherwise ambiguous trailing letters: a final `f` / `d`
    /// on an *integer* literal is a hex digit (`0xabcdef`), never a suffix, and a float literal
    /// never ends in `l` / `L`.
    fn suffix_case(lit: &str, kind: SyntaxKind, case: LiteralSuffixCase) -> Option<String> {
        if case == LiteralSuffixCase::Preserve {
            return None;
        }
        let last = *lit.as_bytes().last()?;
        let is_suffix = match kind {
            SyntaxKind::INT_LITERAL => matches!(last, b'l' | b'L'),
            SyntaxKind::FLOAT_LITERAL => matches!(last, b'f' | b'F' | b'd' | b'D'),
            _ => false,
        };
        if !is_suffix {
            return None;
        }
        let mapped = match case {
            LiteralSuffixCase::Upper => last.to_ascii_uppercase(),
            LiteralSuffixCase::Lower => last.to_ascii_lowercase(),
            LiteralSuffixCase::Preserve => return None,
        };
        if mapped == last {
            return None;
        }
        Some(format!("{}{}", &lit[..lit.len() - 1], mapped as char))
    }
}
