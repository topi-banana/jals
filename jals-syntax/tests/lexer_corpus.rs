//! Lexer edge-case regression corpus.
//!
//! Freezes the exact token boundaries (kind, text, and span) produced by the
//! current lexer through its public API ([`jals_syntax::Lexer::tokenize`]), so that a
//! future lexer rewrite can be verified token-for-token against today's output.
//!
//! Unlike the in-crate unit tests (which mostly assert token kinds, or only
//! round-trip unterminated/error inputs), every case here snapshots the full
//! `KIND "text"` sequence — in particular the exact extent of `ERROR` tokens.
//!
//! To regenerate after an *intentional* lexer change:
//! `UPDATE_EXPECT=1 cargo test -p jals-syntax --test lexer_corpus`

use std::fmt::Write as _;

use expect_test::{Expect, expect};
use jals_syntax::Lexer;

// ===== Corpus inputs =====
//
// Every input is listed in `CORPUS` below so the blanket losslessness test
// covers each one. Keep the two in sync when adding cases.

/// Unicode identifier starts: CJK (Lo), `€` (Sc, U+20AC), `₿` (Sc, U+20BF),
/// and `Ⅻ` (Nl, U+216B).
const UNICODE_IDENTS: &str = "名前x1 €uro ₿coin Ⅻab";
/// Identifier continue chars: combining acute (Mn, U+0301) and ZWJ (Cf, U+200D).
const MARK_AND_FORMAT_IDENTS: &str = "e\u{301}s a\u{200D}b";
/// Emoji (So) is not a Java identifier char.
const EMOJI: &str = "🙂";
/// NBSP (U+00A0, Zs) is not Java whitespace.
const NBSP: &str = "\u{A0}";
/// Vertical tab (U+000B) is not Java whitespace.
const VERTICAL_TAB: &str = "\u{B}";
/// Integer literal forms: hex/binary/octal, digit separators, suffix case.
const INT_FORMS: &str = "0xCAFE_babeL 0b1010_10L 0777 0_7 0XFFl";
/// Inputs that look like integer literals but split: bad octal digit, missing
/// hex/binary digits, `_` in the wrong place, trailing `_`.
const INT_FALLBACK_SPLITS: &str = "089 0x 0b2 0_8 1_";
/// Decimal float forms: bare dot, suffixes, exponents, digit separators.
const FLOAT_FORMS: &str = "1. 1.f 1.e5 .5f 1e+10 1E-3_0 0f 1_0.0_1e1_0d";
/// Hexadecimal float forms (binary `p` exponent required).
const HEX_FLOAT_FORMS: &str = "0x1p3 0x1.p3 0x.8p-2f 0X1_2.A_Bp+0d";
/// Hex-float lookalikes without a valid `p` exponent split into several tokens.
const HEX_FLOAT_SPLITS: &str = "0x1.8 0x1p";
/// Exponents without digits force backtracking to a shorter literal.
const EXPONENT_BACKTRACKING: &str = "1e 1.5e+ 1e_5";
/// `...` vs `..` vs dots adjacent to digits.
const DOT_FAMILY: &str = "... .. ..5 1..2";
/// `<` is munched maximally while every `>` stays a single `GT`.
const SHIFT_ASSIGN_MUNCH: &str = "a<<=b>>>=c";
/// Nested generics close with two plain `GT` tokens.
const NESTED_GENERICS_CLOSE: &str = "Map<String,List<Integer>>x";
/// Greedy operator runs and their leftovers.
const OPERATOR_RUNS: &str = "&&& ||| ^= %= ->-- ::: ++ +=";
/// Line comment terminated by end of input rather than a newline.
const LINE_COMMENT_AT_EOF: &str = "// eof-no-newline";
/// Line comment stops before `\r`; the whole CRLF is one NEWLINE token.
const LINE_COMMENT_BEFORE_CRLF: &str = "// x\r\n";
/// `/*/` has no closing `*/`: one unterminated block comment to end of input.
const UNTERMINATED_BLOCK_COMMENT: &str = "/*/";
/// Empty comment vs star-only doc vs regular doc comment.
const BLOCK_AND_DOC_COMMENTS: &str = "/**/ /***/ /** d */";
/// Text block whose body contains lone `"` quotes: `"""a"b"""`.
const TEXT_BLOCK_INNER_QUOTES: &str = r#""""a"b""""#;
/// `"""\""" x`: an odd backslash run escapes the close, so the block stays
/// open and runs to the end of input (swallowing ` x`).
const TEXT_BLOCK_ODD_BACKSLASHES: &str = r#""""\""" x"#;
/// `"""\\"""`: an even backslash run does not escape the close.
const TEXT_BLOCK_EVEN_BACKSLASHES: &str = r#""""\\""""#;
/// Four quotes: a text-block opener followed by one `"`, unterminated.
const TEXT_BLOCK_FOUR_QUOTES: &str = "\"\"\"\"";
/// Six quotes: one immediately-closed text block.
const TEXT_BLOCK_SIX_QUOTES: &str = "\"\"\"\"\"\"";
/// String literal forms: empty, escaped quote, trailing escaped backslash,
/// and an empty string glued to `+x`.
const STRING_FORMS: &str = r#""" "a\"b" "a\\" ""+x"#;
/// String opened at end of input.
const UNTERMINATED_STRING_AT_EOF: &str = r#""abc"#;
/// String literals cannot span a raw newline.
const STRING_BROKEN_BY_NEWLINE: &str = "\"abc\ndef\"";
/// Char literal opened at end of input.
const CHAR_UNTERMINATED_AT_EOF: &str = "'a";
/// Char literal with two chars never matches.
const CHAR_TWO_CHARS: &str = "'ab'";
/// Empty char literal never matches.
const CHAR_EMPTY: &str = "''";
/// Backslash escape cut off by end of input.
const CHAR_BACKSLASH_AT_EOF: &str = "'\\";
/// Char literal quote followed directly by a newline.
const CHAR_BROKEN_BY_NEWLINE: &str = "'\n";
/// Octal escape (1-3 octal digits) is a single char literal.
const CHAR_OCTAL_ESCAPE: &str = r"'\033'";
/// The largest in-range octal escape (`\377` == 255).
const CHAR_OCTAL_ESCAPE_MAX: &str = r"'\377'";
/// Unicode escape `\uXXXX` is a single char literal.
const CHAR_UNICODE_ESCAPE: &str = r"'\u00ff'";
/// A 4th octal digit overruns the escape and defeats the closing quote.
const CHAR_OCTAL_TOO_LONG: &str = r"'\0000'";
/// A `\u` escape with zero hex digits: the next `'` closes it, so the malformed escape
/// still forms a single (raw) char literal rather than derailing.
const CHAR_UNICODE_NO_HEX: &str = r"'\u'";
/// CRLF between identifiers is a single NEWLINE token.
const CRLF_BETWEEN_IDENTS: &str = "a\r\nb";
/// Lone CR followed by CRLF: two NEWLINE tokens.
const CR_THEN_CRLF: &str = "\r\r\n";
/// A lone CR is a NEWLINE on its own.
const LONE_CR: &str = "\r";
/// `#` is not a Java token.
const HASH_BETWEEN_IDENTS: &str = "a#b";
/// Backtick is not a Java token.
const LONE_BACKTICK: &str = "`";
/// A lone backslash is not a Java token.
const LONE_BACKSLASH: &str = "\\";

/// Every corpus input, for the blanket losslessness test.
const CORPUS: &[&str] = &[
    UNICODE_IDENTS,
    MARK_AND_FORMAT_IDENTS,
    EMOJI,
    NBSP,
    VERTICAL_TAB,
    INT_FORMS,
    INT_FALLBACK_SPLITS,
    FLOAT_FORMS,
    HEX_FLOAT_FORMS,
    HEX_FLOAT_SPLITS,
    EXPONENT_BACKTRACKING,
    DOT_FAMILY,
    SHIFT_ASSIGN_MUNCH,
    NESTED_GENERICS_CLOSE,
    OPERATOR_RUNS,
    LINE_COMMENT_AT_EOF,
    LINE_COMMENT_BEFORE_CRLF,
    UNTERMINATED_BLOCK_COMMENT,
    BLOCK_AND_DOC_COMMENTS,
    TEXT_BLOCK_INNER_QUOTES,
    TEXT_BLOCK_ODD_BACKSLASHES,
    TEXT_BLOCK_EVEN_BACKSLASHES,
    TEXT_BLOCK_FOUR_QUOTES,
    TEXT_BLOCK_SIX_QUOTES,
    STRING_FORMS,
    UNTERMINATED_STRING_AT_EOF,
    STRING_BROKEN_BY_NEWLINE,
    CHAR_UNTERMINATED_AT_EOF,
    CHAR_TWO_CHARS,
    CHAR_EMPTY,
    CHAR_BACKSLASH_AT_EOF,
    CHAR_BROKEN_BY_NEWLINE,
    CHAR_OCTAL_ESCAPE,
    CHAR_OCTAL_ESCAPE_MAX,
    CHAR_UNICODE_ESCAPE,
    CHAR_OCTAL_TOO_LONG,
    CHAR_UNICODE_NO_HEX,
    CRLF_BETWEEN_IDENTS,
    CR_THEN_CRLF,
    LONE_CR,
    HASH_BETWEEN_IDENTS,
    LONE_BACKTICK,
    LONE_BACKSLASH,
];

/// Renders one token per line as `KIND "escaped text"`, asserting along the
/// way that the tokens tile the input exactly (lossless, contiguous spans).
fn render(src: &str) -> String {
    let tokens = Lexer::tokenize(src);
    let joined: String = tokens.iter().map(|t| t.text).collect();
    assert_eq!(joined, src, "lexer is not lossless for {src:?}");
    let mut offset = 0usize;
    let mut out = String::new();
    for token in &tokens {
        assert_eq!(
            usize::from(token.range.start()),
            offset,
            "non-contiguous span for {:?} in {src:?}",
            token.text
        );
        assert_eq!(
            usize::from(token.range.len()),
            token.text.len(),
            "span/text length mismatch for {:?} in {src:?}",
            token.text
        );
        offset += token.text.len();
        writeln!(out, "{:?} {:?}", token.kind, token.text).unwrap();
    }
    out
}

/// Compares the rendered token stream for `src` with an inline snapshot.
#[allow(clippy::needless_pass_by_value)]
fn check(src: &str, expect: Expect) {
    expect.assert_eq(&render(src));
}

/// Losslessness over the whole corpus: concatenating every token's text
/// reproduces the input byte for byte.
#[test]
fn corpus_is_lossless() {
    for src in CORPUS {
        let joined: String = Lexer::tokenize(src).iter().map(|t| t.text).collect();
        assert_eq!(&joined, src, "lexer is not lossless for {src:?}");
    }
}

#[test]
fn unicode_identifiers() {
    check(
        UNICODE_IDENTS,
        expect![[r#"
        IDENT "名前x1"
        WHITESPACE " "
        IDENT "€uro"
        WHITESPACE " "
        IDENT "₿coin"
        WHITESPACE " "
        IDENT "Ⅻab"
    "#]],
    );
}

#[test]
fn mark_and_format_continue_chars() {
    check(
        MARK_AND_FORMAT_IDENTS,
        expect![[r#"
        IDENT "e\u{301}s"
        WHITESPACE " "
        IDENT "a\u{200d}b"
    "#]],
    );
}

#[test]
fn non_identifier_unicode_is_error() {
    check(
        EMOJI,
        expect![[r#"
        ERROR "🙂"
    "#]],
    );
    check(
        NBSP,
        expect![[r#"
        ERROR "\u{a0}"
    "#]],
    );
    check(
        VERTICAL_TAB,
        expect![[r#"
        ERROR "\u{b}"
    "#]],
    );
}

#[test]
fn integer_forms_and_suffix_case() {
    check(
        INT_FORMS,
        expect![[r#"
        INT_LITERAL "0xCAFE_babeL"
        WHITESPACE " "
        INT_LITERAL "0b1010_10L"
        WHITESPACE " "
        INT_LITERAL "0777"
        WHITESPACE " "
        INT_LITERAL "0_7"
        WHITESPACE " "
        INT_LITERAL "0XFFl"
    "#]],
    );
}

#[test]
fn integer_fallback_splits() {
    check(
        INT_FALLBACK_SPLITS,
        expect![[r#"
        INT_LITERAL "0"
        INT_LITERAL "89"
        WHITESPACE " "
        INT_LITERAL "0"
        IDENT "x"
        WHITESPACE " "
        INT_LITERAL "0"
        IDENT "b2"
        WHITESPACE " "
        INT_LITERAL "0"
        IDENT "_8"
        WHITESPACE " "
        INT_LITERAL "1"
        UNDERSCORE "_"
    "#]],
    );
}

#[test]
fn float_forms() {
    check(
        FLOAT_FORMS,
        expect![[r#"
        FLOAT_LITERAL "1."
        WHITESPACE " "
        FLOAT_LITERAL "1.f"
        WHITESPACE " "
        FLOAT_LITERAL "1.e5"
        WHITESPACE " "
        FLOAT_LITERAL ".5f"
        WHITESPACE " "
        FLOAT_LITERAL "1e+10"
        WHITESPACE " "
        FLOAT_LITERAL "1E-3_0"
        WHITESPACE " "
        FLOAT_LITERAL "0f"
        WHITESPACE " "
        FLOAT_LITERAL "1_0.0_1e1_0d"
    "#]],
    );
}

#[test]
fn hex_float_forms() {
    check(
        HEX_FLOAT_FORMS,
        expect![[r#"
        FLOAT_LITERAL "0x1p3"
        WHITESPACE " "
        FLOAT_LITERAL "0x1.p3"
        WHITESPACE " "
        FLOAT_LITERAL "0x.8p-2f"
        WHITESPACE " "
        FLOAT_LITERAL "0X1_2.A_Bp+0d"
    "#]],
    );
}

#[test]
fn hex_float_splits() {
    check(
        HEX_FLOAT_SPLITS,
        expect![[r#"
        INT_LITERAL "0x1"
        FLOAT_LITERAL ".8"
        WHITESPACE " "
        INT_LITERAL "0x1"
        IDENT "p"
    "#]],
    );
}

#[test]
fn exponent_backtracking() {
    check(
        EXPONENT_BACKTRACKING,
        expect![[r#"
        INT_LITERAL "1"
        IDENT "e"
        WHITESPACE " "
        FLOAT_LITERAL "1.5"
        IDENT "e"
        PLUS "+"
        WHITESPACE " "
        INT_LITERAL "1"
        IDENT "e_5"
    "#]],
    );
}

#[test]
fn dot_family() {
    check(
        DOT_FAMILY,
        expect![[r#"
        ELLIPSIS "..."
        WHITESPACE " "
        DOT "."
        DOT "."
        WHITESPACE " "
        DOT "."
        FLOAT_LITERAL ".5"
        WHITESPACE " "
        FLOAT_LITERAL "1."
        FLOAT_LITERAL ".2"
    "#]],
    );
}

#[test]
fn angle_bracket_munching() {
    check(
        SHIFT_ASSIGN_MUNCH,
        expect![[r#"
        IDENT "a"
        LSHIFT_EQ "<<="
        IDENT "b"
        GT ">"
        GT ">"
        GT ">"
        EQ "="
        IDENT "c"
    "#]],
    );
    check(
        NESTED_GENERICS_CLOSE,
        expect![[r#"
        IDENT "Map"
        LT "<"
        IDENT "String"
        COMMA ","
        IDENT "List"
        LT "<"
        IDENT "Integer"
        GT ">"
        GT ">"
        IDENT "x"
    "#]],
    );
}

#[test]
fn operator_runs() {
    check(
        OPERATOR_RUNS,
        expect![[r#"
        AMP_AMP "&&"
        AMP "&"
        WHITESPACE " "
        PIPE_PIPE "||"
        PIPE "|"
        WHITESPACE " "
        CARET_EQ "^="
        WHITESPACE " "
        PERCENT_EQ "%="
        WHITESPACE " "
        ARROW "->"
        MINUS_MINUS "--"
        WHITESPACE " "
        COLON_COLON "::"
        COLON ":"
        WHITESPACE " "
        PLUS_PLUS "++"
        WHITESPACE " "
        PLUS_EQ "+="
    "#]],
    );
}

#[test]
fn line_comment_boundaries() {
    check(
        LINE_COMMENT_AT_EOF,
        expect![[r#"
        LINE_COMMENT "// eof-no-newline"
    "#]],
    );
    check(
        LINE_COMMENT_BEFORE_CRLF,
        expect![[r#"
        LINE_COMMENT "// x"
        NEWLINE "\r\n"
    "#]],
    );
}

#[test]
fn block_and_doc_comment_forms() {
    check(
        UNTERMINATED_BLOCK_COMMENT,
        expect![[r#"
        BLOCK_COMMENT "/*/"
    "#]],
    );
    check(
        BLOCK_AND_DOC_COMMENTS,
        expect![[r#"
        BLOCK_COMMENT "/**/"
        WHITESPACE " "
        DOC_COMMENT "/***/"
        WHITESPACE " "
        DOC_COMMENT "/** d */"
    "#]],
    );
}

#[test]
fn text_block_inner_quotes() {
    check(
        TEXT_BLOCK_INNER_QUOTES,
        expect![[r#"
        TEXT_BLOCK "\"\"\"a\"b\"\"\""
    "#]],
    );
}

#[test]
fn text_block_escape_parity() {
    check(
        TEXT_BLOCK_ODD_BACKSLASHES,
        expect![[r#"
        TEXT_BLOCK "\"\"\"\\\"\"\" x"
    "#]],
    );
    check(
        TEXT_BLOCK_EVEN_BACKSLASHES,
        expect![[r#"
        TEXT_BLOCK "\"\"\"\\\\\"\"\""
    "#]],
    );
}

#[test]
fn text_block_quote_runs() {
    check(
        TEXT_BLOCK_FOUR_QUOTES,
        expect![[r#"
        TEXT_BLOCK "\"\"\"\""
    "#]],
    );
    check(
        TEXT_BLOCK_SIX_QUOTES,
        expect![[r#"
        TEXT_BLOCK "\"\"\"\"\"\""
    "#]],
    );
}

#[test]
fn string_forms() {
    check(
        STRING_FORMS,
        expect![[r#"
        STRING_LITERAL "\"\""
        WHITESPACE " "
        STRING_LITERAL "\"a\\\"b\""
        WHITESPACE " "
        STRING_LITERAL "\"a\\\\\""
        WHITESPACE " "
        STRING_LITERAL "\"\""
        PLUS "+"
        IDENT "x"
    "#]],
    );
}

#[test]
fn unterminated_string_error_spans() {
    check(
        UNTERMINATED_STRING_AT_EOF,
        expect![[r#"
        ERROR "\"abc"
    "#]],
    );
    check(
        STRING_BROKEN_BY_NEWLINE,
        expect![[r#"
        ERROR "\"abc"
        NEWLINE "\n"
        IDENT "def"
        ERROR "\""
    "#]],
    );
}

#[test]
fn char_literal_error_spans() {
    check(
        CHAR_UNTERMINATED_AT_EOF,
        expect![[r#"
        ERROR "'a"
    "#]],
    );
    check(
        CHAR_TWO_CHARS,
        expect![[r#"
        ERROR "'a"
        IDENT "b"
        ERROR "'"
    "#]],
    );
    check(
        CHAR_EMPTY,
        expect![[r#"
        ERROR "'"
        ERROR "'"
    "#]],
    );
    check(
        CHAR_BACKSLASH_AT_EOF,
        expect![[r#"
        ERROR "'\\"
    "#]],
    );
    check(
        CHAR_BROKEN_BY_NEWLINE,
        expect![[r#"
        ERROR "'"
        NEWLINE "\n"
    "#]],
    );
    // A 4th octal digit overruns `\000` and defeats the close; the excluded `0` and the
    // quote re-lex on their own.
    check(
        CHAR_OCTAL_TOO_LONG,
        expect![[r#"
        ERROR "'\\000"
        INT_LITERAL "0"
        ERROR "'"
    "#]],
    );
    // `\u` with zero hex digits: the `'` right after closes it, so it stays one raw char
    // literal (a malformed escape, kept verbatim) instead of derailing.
    check(
        CHAR_UNICODE_NO_HEX,
        expect![[r#"
        CHAR_LITERAL "'\\u'"
    "#]],
    );
}

#[test]
fn char_literal_escape_spans() {
    check(
        CHAR_OCTAL_ESCAPE,
        expect![[r#"
        CHAR_LITERAL "'\\033'"
    "#]],
    );
    check(
        CHAR_OCTAL_ESCAPE_MAX,
        expect![[r#"
        CHAR_LITERAL "'\\377'"
    "#]],
    );
    check(
        CHAR_UNICODE_ESCAPE,
        expect![[r#"
        CHAR_LITERAL "'\\u00ff'"
    "#]],
    );
}

#[test]
fn newline_variants() {
    check(
        CRLF_BETWEEN_IDENTS,
        expect![[r#"
        IDENT "a"
        NEWLINE "\r\n"
        IDENT "b"
    "#]],
    );
    check(
        CR_THEN_CRLF,
        expect![[r#"
        NEWLINE "\r"
        NEWLINE "\r\n"
    "#]],
    );
    check(
        LONE_CR,
        expect![[r#"
        NEWLINE "\r"
    "#]],
    );
}

#[test]
fn error_bytes_mixed_with_code() {
    check(
        HASH_BETWEEN_IDENTS,
        expect![[r##"
        IDENT "a"
        ERROR "#"
        IDENT "b"
    "##]],
    );
    check(
        LONE_BACKTICK,
        expect![[r#"
        ERROR "`"
    "#]],
    );
    check(
        LONE_BACKSLASH,
        expect![[r#"
        ERROR "\\"
    "#]],
    );
}
