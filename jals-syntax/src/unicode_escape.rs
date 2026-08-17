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
    /// Where each untouched run *after* an escape begins, as `(translated offset, original
    /// offset)`, in increasing order.
    ///
    /// Sparse rather than per-byte: between two entries the two sources run in lockstep, so the map
    /// is `original = entry.1 + (offset - entry.0)` and the identity prefix before the first entry
    /// needs no entry at all. A per-byte `Vec<usize>` cost `8 * src.len()` bytes on a file with one
    /// escape in it, built one push per source byte.
    runs: Vec<(usize, usize)>,
}

impl Translation {
    /// Translates `src`, or `None` when it holds no eligible escape and the original *is* the
    /// translation.
    ///
    /// The common case by far, and worth its own answer: the scan allocates nothing until it
    /// reaches an escape, so a source that merely *mentions* `\\u` — a Windows path, a regex, a
    /// javadoc — costs one pass and no allocation at all, and the lexer keeps borrowing the
    /// caller's string with no map. `Lexer::tokenize` calls this on every parse, and `jals-editor`
    /// reparses per keystroke, so the pre-pass runs on a current-thread executor before the first
    /// yield.
    ///
    /// The backslash run's parity is carried forward across the one left-to-right pass rather than
    /// rescanned backwards at each `\`, which is what makes a run of *k* backslashes cost *k* steps
    /// instead of *k(k+1)/2*.
    pub(crate) fn of(src: &str) -> Option<Self> {
        let bytes = src.as_bytes();
        // Allocated at the first escape, so a source holding none pays nothing.
        let mut built: Option<(String, Vec<(usize, usize)>)> = None;
        let mut at = 0;
        // Whether the run of backslashes immediately before `at` is even in number, which is what
        // makes a backslash begin an escape (JLS §3.3): `\\u0041` is a backslash then `u0041`.
        let mut run_even = true;
        while at < src.len() {
            if run_even
                && bytes[at] == b'\\'
                && let Some((decoded, width)) = Self::escape_at(src, at)
            {
                let (text, runs) = built.get_or_insert_with(|| {
                    let mut text = String::with_capacity(src.len());
                    text.push_str(&src[..at]);
                    (text, Vec::new())
                });
                text.push(decoded);
                // The run after the escape resumes in lockstep: one translated character stands for
                // `width` original bytes, and a token boundary never falls inside it, because an
                // escape becomes exactly one character and a token is made of whole characters.
                runs.push((text.len(), at + width));
                at += width;
                // The escape's own trailing hex digit is not a backslash, so the next one starts a
                // fresh run.
                run_even = true;
                continue;
            }
            // Not an escape: copy one whole UTF-8 sequence.
            let width = Self::utf8_width(bytes[at]);
            if let Some((text, _)) = built.as_mut() {
                text.push_str(&src[at..at + width]);
            }
            run_even = bytes[at] != b'\\' || !run_even;
            at += width;
        }
        let (text, runs) = built?;
        Some(Self { text, runs })
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
        // The last run that begins at or before the offset; before the first escape the two sources
        // are the same string, so the identity is the answer and needs no stored entry.
        let partition = self
            .runs
            .partition_point(|&(start, _)| start <= translated_offset);
        let (translated_start, original_start) = partition
            .checked_sub(1)
            .map_or((0, 0), |index| self.runs[index]);
        original_start + (translated_offset - translated_start)
    }

    /// The escape starting at `at`, as `(character, original byte width)`.
    ///
    /// JLS §3.3 in full: a backslash begins an escape only when the run of backslashes before it has
    /// **even** length (so `"\\u0041"` is a backslash followed by the letters, not an escape), one or
    /// more `u`s may follow, and then exactly four hex digits. A surrogate pair written as two
    /// escapes is one character — `\ud801\udc00` is U+10400 — which is the only reason this returns a
    /// width rather than a fixed six.
    ///
    /// The eligibility half is the caller's: [`of`](Self::of) carries the backslash run's parity
    /// forward across its single left-to-right pass, and the low surrogate this reaches on its own
    /// is preceded by a hex digit, so it always begins a fresh run.
    ///
    /// An ill-formed escape (`\u` with fewer than four hex digits) is a compile-time error in Java.
    /// Nothing here checks, so it is left untranslated and reaches the lexer as the `\` it is; the
    /// error it becomes there is the same shape as any other malformed input.
    fn escape_at(src: &str, at: usize) -> Option<(char, usize)> {
        let bytes = src.as_bytes();
        let (unit, width) = Self::escape_unit(bytes, at)?;
        // A high surrogate pairs with a low one written as its own escape; anything else stands
        // alone. A lone surrogate has no `char`, so it stays untranslated rather than becoming one.
        if (0xD800..0xDC00).contains(&unit) {
            if let Some((low, low_width)) = Self::escape_unit(bytes, at + width)
                && (0xDC00..0xE000).contains(&low)
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

    /// A source that merely *mentions* an escape holds none. Every one of these matches a
    /// `contains("\\u")` fast-out, which is why that is not the eligibility test: each used to build
    /// a full copy of the source and a per-byte offset map before answering `None`.
    #[test]
    fn a_mention_of_an_escape_is_not_one() {
        for src in [
            r#"String path = "C:\\users";"#,
            r#"String regex = "\\\\u";"#,
            r"// a javadoc writing \\u0041 to mean an escape",
        ] {
            assert!(Translation::of(src).is_none(), "translating `{src}`");
        }
    }

    /// The run's parity is carried across the one left-to-right pass rather than rescanned backwards
    /// at every backslash — and still answers what counting the run would.
    #[test]
    fn a_backslash_run_keeps_its_parity() {
        for run in 0..12usize {
            let prefix = "\\".repeat(run);
            let src = format!("{prefix}\\u0041");
            match Translation::of(&src) {
                Some(translated) => {
                    assert!(
                        run.is_multiple_of(2),
                        "a run of {run} should leave it ineligible"
                    );
                    assert_eq!(translated.text(), format!("{prefix}A"));
                }
                None => assert!(
                    !run.is_multiple_of(2),
                    "a run of {run} should leave it eligible"
                ),
            }
        }
    }

    /// The offsets before the first escape map to themselves, with no stored entry — the identity
    /// prefix the sparse map leaves implicit.
    #[test]
    fn offsets_before_the_first_escape_map_to_themselves() {
        let src = "class C { int \\u0061; }";
        let translated = Translation::of(src).expect("holds an escape");
        let prefix = src.find('\\').expect("the escape is in there");
        for offset in 0..=prefix {
            assert_eq!(translated.origin(offset), offset, "at {offset}");
        }
        // And the tail after it runs in lockstep again, one character standing for six bytes.
        assert_eq!(translated.origin(prefix + 1), prefix + 6);
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
