//! Case / trailing-zero option enums for numeric literals — the opt-in rewrites the formatter's
//! literal rules carry out. Each defaults to `Preserve`, keeping the source exactly so the strict
//! significant-token invariant holds unless opted into.

use serde::Deserialize;

/// Case of the hexadecimal digit letters (`a`–`f` / `A`–`F`) of an integer or floating-point
/// literal — `0xFF` vs. `0xff`. Mirrors rustfmt's `hex_literal_case`, plus a
/// [`Preserve`](Self::Preserve) default (rustfmt's is `Preserve` too) that keeps the source
/// case exactly, so the strict significant-token invariant holds unless this is opted into.
///
/// Only the hex *mantissa* digits are affected. The `0x` / `0X` radix prefix, the `p` / `P`
/// binary exponent marker of a hex float and its decimal digits, and any `l` / `L` integer or
/// `f` / `F` / `d` / `D` float suffix are all left exactly as written (suffix-letter case is a
/// separate Java-specific concern handled by [`LiteralSuffixCase`]). Decimal, octal, and binary
/// literals have no hex digits and are never touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HexLiteralCase {
    /// Keep the source's hex-digit case exactly. The default; preserves the significant-token
    /// sequence.
    Preserve,
    /// Force hex digits to upper case (`0xff` → `0xFF`).
    Upper,
    /// Force hex digits to lower case (`0xFF` → `0xff`).
    Lower,
}

/// Whether a decimal floating-point literal carries a trailing zero — `1.0` vs. `1.`. Mirrors
/// rustfmt's `float_literal_trailing_zero`, plus a [`Preserve`](Self::Preserve) default that keeps
/// the source exactly, so the strict significant-token invariant holds unless this is opted into.
/// (rustfmt's Rust-only `IfNoPostfix` mode is intentionally omitted: in Java both `1.f` and `1.0f`
/// are legal, so it would be semantically empty.)
///
/// Only **decimal** float literals that contain a `.` are affected, and only the boundary between
/// an empty fraction (`1.`) and an all-zero one (`1.0`): a fraction with a non-zero digit (`1.50`),
/// a dotless float (`1e10`, `100f`), a leading-dot float (`.5`, `.0`), a hex float (`0x1.0p3`), and
/// every integer literal are all left exactly as written. The numeric value, the type suffix
/// (`f` / `F` / `d` / `D`), and any exponent are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FloatLiteralTrailingZero {
    /// Keep the source's trailing zero (or lack of one) exactly. The default; preserves the
    /// significant-token sequence.
    Preserve,
    /// Give every in-scope float literal a trailing zero (`1.` → `1.0`, `1.f` → `1.0f`).
    Always,
    /// Strip an all-zero trailing fraction (`1.0` → `1.`, `1.00` → `1.`, `1.0f` → `1.f`).
    Never,
}

/// Case of a numeric literal's trailing type suffix — `123l` vs. `123L`, `1.5f` vs. `1.5F`. A
/// Java-specific extension with no rustfmt equivalent (rustfmt's `hex_literal_case` covers only
/// the digits), plus a [`Preserve`](Self::Preserve) default that keeps the source exactly, so the
/// strict significant-token invariant holds unless this is opted into.
///
/// Only the single trailing suffix letter is affected: the `l` / `L` `long` suffix of an integer
/// literal, or the `f` / `F` / `d` / `D` `float` / `double` suffix of a floating-point literal.
/// The kind of the literal disambiguates: a trailing `f` / `d` on an *integer* literal is a hex
/// digit (`0xabcdef`), never a suffix, and a float literal never ends in `l` / `L`. The numeric
/// value, the radix prefix, the mantissa, and any exponent are all left exactly as written; a
/// literal with no suffix is untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiteralSuffixCase {
    /// Keep the source's suffix-letter case exactly. The default; preserves the significant-token
    /// sequence.
    Preserve,
    /// Force the suffix letter to upper case (`123l` → `123L`, `1.5f` → `1.5F`, `1.5d` → `1.5D`).
    Upper,
    /// Force the suffix letter to lower case (`123L` → `123l`, `1.5F` → `1.5f`, `1.5D` → `1.5d`).
    Lower,
}
