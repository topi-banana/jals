//! What a literal token means.
//!
//! Reading `0xFF`, `1_000`, `'A'`, and `"a\\nb"` is a source fact: the answer is the same
//! whichever backend asked, and reading them twice was two chances to disagree about one of them.
//! The `case`-label evaluator reads them here. Both lowerings' *expression* paths still reach into
//! the JVM backend's own expression module for the same job, which is where those readers live only
//! because that is where they were needed first; folding them in is the rest of this move.
//!
//! A suffix is part of the token, so it is stripped **here** rather than by each caller. That is
//! not tidiness: the two `case`-label paths passed the text untrimmed while the expression paths
//! trimmed it, so `case 1L:` failed in both backends for a reason neither stated.

use alloc::string::String;

use super::{FactError, Result};

/// How wide an integer literal's value is, from its suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Width {
    /// No suffix: an `int` unless the context widens it.
    Int,
    /// An `l` / `L` suffix: a `long`.
    Long,
}

/// The value a literal token spells.
pub(crate) struct Literal;

impl Literal {
    /// An integer literal's value and width, in whichever base its prefix names, with `_` separators
    /// removed.
    pub(crate) fn integer(text: &str) -> Result<(i64, Width)> {
        let (body, width) = text
            .strip_suffix(['l', 'L'])
            .map_or((text, Width::Int), |body| (body, Width::Long));
        let cleaned = body.replace('_', "");
        let (digits, radix) = match cleaned.get(..2).map(str::to_ascii_lowercase).as_deref() {
            Some("0x") => (&cleaned[2..], 16),
            Some("0b") => (&cleaned[2..], 2),
            _ if cleaned.len() > 1 && cleaned.starts_with('0') => (&cleaned[1..], 8),
            _ => (cleaned.as_str(), 10),
        };
        // Parsing as unsigned first accepts `0x8000_0000_0000_0000`, which is a legal `long` literal
        // whose value is negative — the source spells the bit pattern, not the number.
        i64::from_str_radix(digits, radix)
            .or_else(|_| u64::from_str_radix(digits, radix).map(u64::cast_signed))
            .map(|value| (value, width))
            .map_err(|_| FactError::Unsupported("an integer literal this lowering cannot read"))
    }

    /// A floating-point literal's value, and whether its suffix makes it a `float`.
    pub(crate) fn floating(text: &str) -> Result<(f64, bool)> {
        let (body, is_float) = text.strip_suffix(['f', 'F']).map_or_else(
            || (text.strip_suffix(['d', 'D']).unwrap_or(text), false),
            |body| (body, true),
        );
        body.replace('_', "")
            .parse::<f64>()
            .map(|value| (value, is_float))
            .map_err(|_| {
                FactError::Unsupported("a floating-point literal this lowering cannot read")
            })
    }

    /// The text between a literal's delimiters: exactly **one** quote comes off each end.
    ///
    /// `trim_end_matches` took every trailing quote, so `"a\""` — whose last two characters are an
    /// escaped quote and the closing one — lost both and compiled to `a`. An unterminated literal the
    /// lexer recovered still yields its text rather than nothing.
    fn unquote(text: &str) -> &str {
        let open = text
            .strip_prefix('"')
            .or_else(|| text.strip_prefix('\''))
            .unwrap_or(text);
        open.strip_suffix('"')
            .or_else(|| open.strip_suffix('\''))
            .unwrap_or(open)
    }

    /// A string / char literal's value, with its quotes stripped and escapes resolved.
    ///
    /// An escape this does not know is reported rather than approximated. Pushing the character after
    /// the backslash — the old fallback — turned `A` into `u0041` and `\101` into `101`, which is a
    /// string constant that is simply wrong, in a class file nothing downstream checks.
    pub(crate) fn text(source: &str) -> Result<String> {
        let inner = Self::unquote(source);
        let unknown = || FactError::Unsupported("an escape sequence this lowering cannot read");
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '\\' {
                out.push(character);
                continue;
            }
            match chars.next().ok_or_else(unknown)? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                's' => out.push(' '),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\\' => out.push('\\'),
                // JLS §3.3: a unicode escape may carry any number of `u`s, and the four hex digits
                // after the last one name one UTF-16 code unit. A lone surrogate is a code unit Rust's
                // `char` cannot hold, so it is reported rather than silently replaced.
                'u' => {
                    while chars.peek() == Some(&'u') {
                        chars.next();
                    }
                    let mut digits = String::with_capacity(4);
                    for _ in 0..4 {
                        digits.push(chars.next().ok_or_else(unknown)?);
                    }
                    let unit = u32::from_str_radix(&digits, 16).map_err(|_| unknown())?;
                    out.push(char::from_u32(unit).ok_or_else(unknown)?);
                }
                // JLS §3.10.7: one to three octal digits, and at most `\377` — so a leading digit above
                // `3` takes only one more.
                first @ '0'..='7' => {
                    let mut value = u32::from(first as u8 - b'0');
                    let remaining = if first <= '3' { 2 } else { 1 };
                    for _ in 0..remaining {
                        let Some(&digit @ '0'..='7') = chars.peek() else {
                            break;
                        };
                        chars.next();
                        value = value * 8 + u32::from(digit as u8 - b'0');
                    }
                    out.push(char::from_u32(value).ok_or_else(unknown)?);
                }
                _ => return Err(unknown()),
            }
        }
        Ok(out)
    }
}
