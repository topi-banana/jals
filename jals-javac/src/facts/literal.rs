//! What a literal token means.
//!
//! Reading `0xFF`, `1_000`, `'A'`, and `"a\\nb"` is a source fact: the answer is the same
//! whichever backend asked, and reading them twice was two chances to disagree about one of them.
//! Every reader goes through here — both `case`-label evaluators, both expression paths, and the
//! JVM's annotation element-value folder.
//!
//! For a while only the `case`-label evaluator did. The expression paths kept a second copy in the
//! *JVM backend's* own module, and the wasm backend called into it across the seam — with comments
//! at both call sites saying the sharing was deliberate, which it was, at the wrong layer: a fact
//! reached through the other backend is one that moves when that backend is refactored. Floating
//! point never even got that far and was parsed inline in four places.
//!
//! A suffix is part of the token, so it is stripped **here** rather than by each caller. That is
//! not tidiness: the two `case`-label paths passed the text untrimmed while the expression paths
//! trimmed it, so `case 1L:` failed in both backends for a reason neither stated. Exactly one
//! suffix comes off, where a caller's `trim_end_matches` took every trailing letter.
//!
//! The [`Width`] a suffix names is reported, and every caller outside the constant evaluator drops
//! it: an expression's width comes from the inferred type, a wasm global's from the field, and an
//! annotation element's from the declared element type. Reading the type rather than re-reading the
//! suffix is what keeps the two from disagreeing — so the fact states what the source spells and
//! the caller states what it is being spelled *into*.

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

    /// The single character a `char` literal spells.
    ///
    /// Delegated to [`text`](Self::text) rather than written again, so an escape that cannot be read
    /// is reported in the *same* words whichever kind of literal held it — a wording both backends'
    /// error types carry verbatim and the integration tests match by exact text.
    ///
    /// Four call sites wrote `text(t).ok().and_then(|s| s.chars().next())` — the JVM expression path,
    /// the JVM annotation element-value folder, and the wasm global and expression paths — and each
    /// invented its own answer for the empty case, two of them silently falling back to a default.
    pub(crate) fn character(text: &str) -> Result<char> {
        Self::text(text)?
            .chars()
            .next()
            .ok_or(FactError::Unsupported("an empty character literal"))
    }
}

#[cfg(test)]
mod tests {
    use super::{Literal, Width};
    use crate::facts::FactError;

    /// These run where the end-to-end tests do not.
    ///
    /// Every value this file decodes is checked today by compiling Java and running it on a JVM,
    /// which `jals-javac/tests/compile.rs` stands down from when the host has no `java`. CI also
    /// runs this crate's tests as `wasm32-wasip1`, where there is no JVM at all — so on that cell,
    /// and on any machine without a JDK, the reader below ran unchecked. It needs no host: a
    /// literal's text is a `&str`, and its value is a fact about that `&str` alone.
    ///
    /// A number's base is named by its prefix and its `_` separators mean nothing, both of which
    /// are cheap. The one that is not: `0x8000_0000_0000_0000L` is a legal `long` whose value is
    /// negative, because the source spells the bit pattern rather than the number. Parsing signed
    /// first and falling back to unsigned is what accepts it.
    #[test]
    fn an_integer_literal_is_read_in_the_base_its_prefix_names() {
        assert_eq!(Literal::integer("10"), Ok((10, Width::Int)));
        assert_eq!(Literal::integer("0x1F"), Ok((31, Width::Int)));
        assert_eq!(Literal::integer("0b1010"), Ok((10, Width::Int)));
        assert_eq!(Literal::integer("017"), Ok((15, Width::Int)));
        assert_eq!(Literal::integer("1_000_000"), Ok((1_000_000, Width::Int)));
        assert_eq!(
            Literal::integer("0x8000_0000_0000_0000L"),
            Ok((i64::MIN, Width::Long))
        );
    }

    /// The suffix is part of the token, so it comes off **here** rather than at each caller.
    ///
    /// It used not to: the two `case`-label paths passed the text untrimmed while the expression
    /// paths trimmed it, so `case 1L:` failed in both backends for a reason neither stated.
    ///
    /// Exactly one suffix comes off, not every trailing letter. No legal literal ends in `LL`, so
    /// this only ever rejects something the lexer should not have produced.
    #[test]
    fn a_suffix_is_stripped_here_and_names_the_width() {
        assert_eq!(Literal::integer("1L"), Ok((1, Width::Long)));
        assert_eq!(Literal::integer("1l"), Ok((1, Width::Long)));
        assert_eq!(Literal::integer("1"), Ok((1, Width::Int)));
        assert_eq!(Literal::floating("1.5f"), Ok((1.5, true)));
        assert_eq!(Literal::floating("1.5F"), Ok((1.5, true)));
        assert_eq!(Literal::floating("1.5"), Ok((1.5, false)));
        assert_eq!(Literal::floating("1_0.5d"), Ok((10.5, false)));
    }

    /// Exactly **one** quote comes off each end.
    ///
    /// `trim_end_matches` took every trailing quote, so `"a\""` — whose last two characters are an
    /// escaped quote and the closing one — lost both and compiled to `a`. An unterminated literal
    /// the lexer recovered still yields its text rather than nothing, because a lossless parse
    /// hands this reader a token it already knows is broken.
    #[test]
    fn exactly_one_quote_comes_off_each_end() {
        assert_eq!(Literal::text(r#""a\"""#).as_deref(), Ok("a\""));
        assert_eq!(Literal::text(r#""""#).as_deref(), Ok(""));
        assert_eq!(Literal::text(r#""ab"#).as_deref(), Ok("ab"));
        assert_eq!(Literal::text("'a'").as_deref(), Ok("a"));
    }

    /// Every escape family, resolved rather than approximated.
    ///
    /// A unicode escape may carry any number of `u`s (JLS §3.3), and an octal one takes at most
    /// three digits and at most `\377` — so a leading digit above `3` takes only one more (§3.10.7).
    /// Both rules are easy to write down and easy to get subtly wrong, and neither has any
    /// observable effect until a string constant reaches a class file nothing downstream checks.
    #[test]
    fn every_escape_family_resolves() {
        assert_eq!(Literal::text(r#""a\nb""#).as_deref(), Ok("a\nb"));
        assert_eq!(
            Literal::text(r#""\t\r\b\f\s""#).as_deref(),
            Ok("\t\r\u{8}\u{c} ")
        );
        assert_eq!(Literal::text(r#""\\""#).as_deref(), Ok("\\"));
        assert_eq!(Literal::text(r"'\''").as_deref(), Ok("'"));
        assert_eq!(Literal::text(r"'A'").as_deref(), Ok("A"));
        assert_eq!(Literal::text(r"'\uuu0041'").as_deref(), Ok("A"));
        assert_eq!(Literal::text(r"'\101'").as_deref(), Ok("A"));
        assert_eq!(Literal::text(r"'\47'").as_deref(), Ok("'"));
        // A leading `4` cannot take *two* more digits and stay under `\377`, so `\477` is the two
        // characters `\47` and `7` rather than one escape.
        assert_eq!(Literal::text(r#""\477""#).as_deref(), Ok("'7"));
    }

    /// An escape this does not know is reported, not approximated.
    ///
    /// Pushing the character after the backslash — the old fallback — turned `A` into `u0041`
    /// and `\101` into `101`. The wording travels verbatim into both backends' error types and is
    /// matched by exact text in `jals-javac/tests/compile.rs`, which only runs where a JDK is
    /// installed. This is where it is pinned everywhere else.
    #[test]
    fn an_unknown_escape_is_reported_in_the_pinned_words() {
        let unknown = Err(FactError::Unsupported(
            "an escape sequence this lowering cannot read",
        ));
        assert_eq!(Literal::text(r#""\q""#), unknown);
        // A lone surrogate is a UTF-16 code unit Rust's `char` cannot hold.
        assert_eq!(Literal::text(r#""\ud800""#), unknown);
        // Four hex digits are required; `\u00` runs out of literal first.
        assert_eq!(Literal::text(r#""\u00""#), unknown);
    }

    /// A number the reader cannot make sense of is reported rather than approximated, for the same
    /// reason: this crate does not check, so a value it invents is one nothing downstream re-derives.
    #[test]
    fn an_unreadable_number_is_reported() {
        assert_eq!(
            Literal::integer("0xZZ"),
            Err(FactError::Unsupported(
                "an integer literal this lowering cannot read"
            ))
        );
        assert_eq!(
            Literal::floating("1.2.3"),
            Err(FactError::Unsupported(
                "a floating-point literal this lowering cannot read"
            ))
        );
    }
}
