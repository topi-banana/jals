//! JLS §3.3 unicode escapes: the translation that happens *before* anything is lexed.
//!
//! Java does not treat `\uXXXX` as a string escape. It replaces every eligible one with the
//! character it names over the whole input, and only then divides the result into tokens — so
//! a `\u0070ublic` is the keyword `public`, `\u002f\u002f` starts a comment, a `\u000a` ends one,
//! and a `\u0022` opens and closes a string literal. The escapes in `T8245153`, `FirstChar2`,
//! `UnicodeAtEOL`, and `UnicodeCommentDelimiter` are each one of those four, and none of them is
//! reachable by a lexer that looks for `\u` only inside a literal.
//!
//! # Why this is a separate pass rather than an escape-aware cursor
//!
//! This crate's parse is lossless: the tree is literally the caller's `&str`, reassembled from token
//! slices ([`crate::parser::sink`]), and four independent tests say so. Translating the source and
//! lexing *that* would put decoded text into the tree and break every one of them.
//!
//! So the translated text never leaves this module. [`Translation`] holds it beside a map back to
//! the original byte offsets; the lexer runs over the translation, and each token it produces is
//! then re-cut from the **original** source at the mapped offsets. Losslessness is structural rather
//! than checked: every translated byte comes from exactly one contiguous original span, the spans
//! tile the input in order, so token boundaries map to original boundaries and the slices still
//! concatenate to the input.
//!
//! # What it deliberately does not do
//!
//! A token's `text` stays the source's own spelling, escapes and all. That is what keeps the tree
//! lossless, and it means two spellings of one identifier — `a` and `\u0061` — are two names to
//! everything downstream, which keys on token text. No file in the corpus writes a name both ways,
//! and closing it needs a *decoded* spelling carried beside the raw one rather than in place of it.

use alloc::string::String;
use alloc::vec::Vec;

/// A source with its eligible `\uXXXX` escapes replaced, plus the map back to the original.
pub(crate) struct Translation {
    /// The source as the language defines it after §3.3.
    text: String,
    /// For each byte of [`text`](Self::text), the original byte offset it came from — plus one final
    /// entry for the end, so a translated range maps to an original range with no special case.
    origin: Vec<usize>,
}

impl Translation {
    /// Translates `src`, or `None` when it holds no eligible escape and the original *is* the
    /// translation.
    ///
    /// The common case by far, and worth its own answer: it costs one scan for `\` and lets the
    /// lexer keep borrowing the caller's string with no map at all.
    pub(crate) fn of(src: &str) -> Option<Self> {
        if !src.contains("\\u") {
            return None;
        }
        let mut text = String::with_capacity(src.len());
        let mut origin = Vec::with_capacity(src.len());
        let bytes = src.as_bytes();
        let mut at = 0;
        let mut translated = false;
        while at < src.len() {
            if let Some((decoded, width)) = Self::escape_at(src, at) {
                translated = true;
                // The whole escape collapses to one character, and every byte of that character
                // maps back to where the escape began — so a token boundary either side of it lands
                // on a real boundary in the original.
                text.push(decoded);
                for _ in 0..decoded.len_utf8() {
                    origin.push(at);
                }
                at += width;
                continue;
            }
            // Not an escape: copy one whole UTF-8 sequence, so `origin` stays byte-aligned.
            let width = Self::utf8_width(bytes[at]);
            text.push_str(&src[at..at + width]);
            for _ in 0..width {
                origin.push(at);
            }
            at += width;
        }
        origin.push(src.len());
        translated.then_some(Self { text, origin })
    }

    /// The translated text, for the lexer to divide into tokens.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The original byte offset a translated offset came from.
    ///
    /// Defined for every offset a token boundary can fall on, which is every translated byte
    /// boundary plus the end — a token never begins or ends inside a translated character, because
    /// an escape becomes exactly one character and a token is made of whole characters.
    pub(crate) fn origin(&self, translated_offset: usize) -> usize {
        self.origin
            .get(translated_offset)
            .copied()
            .unwrap_or_else(|| self.origin.last().copied().unwrap_or(0))
    }

    /// The escape starting at `at`, as `(character, original byte width)`.
    ///
    /// JLS §3.3 in full: a backslash begins an escape only when the run of backslashes before it has
    /// **even** length (so `"\\u0041"` is a backslash followed by the letters, not an escape), one or
    /// more `u`s may follow, and then exactly four hex digits. A surrogate pair written as two
    /// escapes is one character — `\ud801\udc00` is U+10400 — which is the only reason this returns a
    /// width rather than a fixed six.
    ///
    /// An ill-formed escape (`\u` with fewer than four hex digits) is a compile-time error in Java.
    /// Nothing here checks, so it is left untranslated and reaches the lexer as the `\` it is; the
    /// error it becomes there is the same shape as any other malformed input.
    fn escape_at(src: &str, at: usize) -> Option<(char, usize)> {
        let bytes = src.as_bytes();
        if bytes[at] != b'\\' || !Self::eligible(bytes, at) {
            return None;
        }
        let (unit, width) = Self::escape_unit(bytes, at)?;
        // A high surrogate pairs with a low one written as its own escape; anything else stands
        // alone. A lone surrogate has no `char`, so it stays untranslated rather than becoming one.
        if (0xD800..0xDC00).contains(&unit) {
            if let Some((low, low_width)) = Self::escape_unit(bytes, at + width)
                && (0xDC00..0xE000).contains(&low)
                && Self::eligible(bytes, at + width)
            {
                let combined =
                    0x1_0000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                return char::from_u32(combined).map(|c| (c, width + low_width));
            }
            return None;
        }
        char::from_u32(u32::from(unit)).map(|c| (c, width))
    }

    /// One `\` + `u`+ + four hex digits, as `(code unit, byte width)`. No eligibility check.
    fn escape_unit(bytes: &[u8], at: usize) -> Option<(u16, usize)> {
        if bytes.get(at) != Some(&b'\\') {
            return None;
        }
        let mut cursor = at + 1;
        let mut saw_u = false;
        while bytes.get(cursor) == Some(&b'u') {
            saw_u = true;
            cursor += 1;
        }
        if !saw_u {
            return None;
        }
        let digits = bytes.get(cursor..cursor + 4)?;
        let mut unit: u16 = 0;
        for digit in digits {
            let value = u16::try_from((*digit as char).to_digit(16)?).ok()?;
            unit = unit.checked_mul(16)?.checked_add(value)?;
        }
        Some((unit, cursor + 4 - at))
    }

    /// Whether the backslash at `at` begins an escape, i.e. the backslashes before it are even in
    /// number. `\\u0041` is a literal backslash then `u0041`; `\\A` is one then an escape.
    fn eligible(bytes: &[u8], at: usize) -> bool {
        let mut before = at;
        while before > 0 && bytes[before - 1] == b'\\' {
            before -= 1;
        }
        (at - before).is_multiple_of(2)
    }

    /// The byte width of the UTF-8 sequence a lead byte starts.
    const fn utf8_width(lead: u8) -> usize {
        match lead {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Translation;

    /// The four effects §3.3 has that a literal-only reader cannot reach.
    #[test]
    fn an_escape_changes_what_the_source_says() {
        let cases = [
            // A keyword spelled through an escape.
            ("\\u0070ublic class C {}", "public class C {}"),
            // A comment delimiter, and a line terminator that ends one.
            ("// x \\u000A int y;", "// x \n int y;"),
            ("\\u002f\\u002f x", "// x"),
            // String quotes, which is how an empty literal gets written as `\u0022\u0022`.
            ("String s = \\u0022\\u0022;", "String s = \"\";"),
            // Multiple `u`s are one escape.
            ("\\uuuu0041", "A"),
            // A surrogate pair written as two escapes is one character.
            ("int \\ud801\\udc00 = 1;", "int \u{10400} = 1;"),
        ];
        for (src, want) in cases {
            let translated = Translation::of(src).expect("holds an escape");
            assert_eq!(translated.text(), want, "translating `{src}`");
        }
    }

    /// An even run of backslashes before the `u` means the backslash is data, not an escape.
    #[test]
    fn an_escaped_backslash_is_not_an_escape() {
        // `\\u0041` inside a string is a backslash followed by `u0041`.
        assert!(Translation::of("\\\\u0041").is_none());
        // …and one more backslash makes the *third* one begin an escape again.
        let translated = Translation::of("\\\\\\u0041").expect("holds an escape");
        assert_eq!(translated.text(), "\\\\A");
    }

    /// A source with no escape needs no translation, and says so — the lexer then keeps borrowing
    /// the caller's own string.
    #[test]
    fn a_source_without_escapes_is_its_own_translation() {
        assert!(Translation::of("class C {}").is_none());
        // A `\u` that cannot be an escape still yields `None` rather than a copy.
        assert!(Translation::of("String s = \"\\\\u\";").is_none());
    }

    /// Every translated offset maps back into the original, and the map is monotonic — which is what
    /// makes the re-cut token slices tile the input.
    #[test]
    fn the_origin_map_is_monotonic_and_total() {
        let src = "int \\u0061 = \\u0031; // \\u000A x";
        let translated = Translation::of(src).expect("holds escapes");
        let mut previous = 0;
        for offset in 0..=translated.text().len() {
            let origin = translated.origin(offset);
            assert!(origin >= previous, "origin went backwards at {offset}");
            assert!(origin <= src.len(), "origin past the end at {offset}");
            previous = origin;
        }
        assert_eq!(translated.origin(translated.text().len()), src.len());
        let _ = src.to_owned();
    }
}
