//! `logos` をラップした lossless な字句解析器。
//!
//! [`tokenize`] / [`Lexer`] は入力の全バイトをちょうど 1 トークンに対応させ(各トークンの
//! `text` を連結すると入力に一致する)、いかなる入力でも panic しない。未一致バイトは
//! [`SyntaxKind::ERROR`] になる。

use logos::Logos;
use text_size::{TextRange, TextSize};

use crate::syntax_kind::SyntaxKind;
use crate::token::{TokenKind, keyword_kind};

/// 字句解析で得られる 1 トークン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexedToken<'a> {
    /// トークン種別(`ERROR` を含む統一種別)。
    pub kind: SyntaxKind,
    /// このトークンに対応する元テキスト。
    pub text: &'a str,
    /// 元テキスト内のバイト範囲。
    pub range: TextRange,
}

/// 入力全体を字句解析してトークン列を返す。
pub fn tokenize(src: &str) -> Vec<LexedToken<'_>> {
    Lexer::new(src).collect()
}

/// `logos` をラップしたトークンイテレータ。トリビアも含め全トークンを返す。
pub struct Lexer<'a> {
    inner: logos::Lexer<'a, TokenKind>,
}

impl<'a> Lexer<'a> {
    /// 入力を受け取って字句解析器を作る。
    pub fn new(src: &'a str) -> Self {
        Lexer {
            inner: TokenKind::lexer(src),
        }
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = LexedToken<'a>;

    fn next(&mut self) -> Option<LexedToken<'a>> {
        let result = self.inner.next()?;
        let span = self.inner.span();
        let text = self.inner.slice();
        let kind = match result {
            Ok(token) => SyntaxKind::from_token(token, text),
            Err(()) => SyntaxKind::ERROR,
        };
        let range = TextRange::new(
            TextSize::from(span.start as u32),
            TextSize::from(span.end as u32),
        );
        Some(LexedToken { kind, text, range })
    }
}

/// Hand-written replacement for the `logos` path (Phase A: differential-tested side by
/// side; Phase B swaps it in). Tokenizes the whole input.
#[allow(dead_code)]
fn tokenize_new(src: &str) -> Vec<LexedToken<'_>> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < src.len() {
        let (token, end) = scan_token(src, pos);
        debug_assert!(end > pos, "scan_token must always make progress");
        let text = &src[pos..end];
        let kind = match token {
            Ok(token) => SyntaxKind::from_token(token, text),
            Err(()) => SyntaxKind::ERROR,
        };
        let range = TextRange::new(TextSize::from(pos as u32), TextSize::from(end as u32));
        tokens.push(LexedToken { kind, text, range });
        pos = end;
    }
    tokens
}

// === Hand-written scanner ===

/// A char cursor over the source text.
struct Cursor<'a> {
    src: &'a str,
    pos: usize,
}

impl Cursor<'_> {
    /// Peeks the next char without consuming it.
    fn first(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// Consumes and returns the next char.
    fn bump(&mut self) -> Option<char> {
        let c = self.first()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    /// Consumes the next char if it equals `c`.
    fn eat_if(&mut self, c: char) -> bool {
        if self.first() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    /// Consumes chars while `pred` holds.
    fn eat_while(&mut self, mut pred: impl FnMut(char) -> bool) {
        while let Some(c) = self.first() {
            if !pred(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
    }
}

/// Scans one token starting at `start` (which must be `< src.len()` and on a char
/// boundary). Returns the token kind (`Err(())` for unmatched input) and the end offset;
/// the end is always `> start` and on a char boundary.
fn scan_token(src: &str, start: usize) -> (Result<TokenKind, ()>, usize) {
    use TokenKind::*;

    let mut cursor = Cursor { src, pos: start };
    let Some(c) = cursor.bump() else {
        // Unreachable: the caller checks `start < src.len()`.
        return (Err(()), src.len());
    };
    let kind = match c {
        // Trivia. A line break is its own token, separate from blank trivia.
        ' ' | '\t' | '\x0C' => {
            cursor.eat_while(|c| matches!(c, ' ' | '\t' | '\x0C'));
            WHITESPACE
        }
        '\n' => NEWLINE,
        '\r' => {
            // CRLF is one token.
            cursor.eat_if('\n');
            NEWLINE
        }

        // Comments and the `/` operators.
        '/' => match cursor.first() {
            Some('/') => {
                // Up to (not including) the next line break, or to end of input.
                cursor.bump();
                cursor.eat_while(|c| !matches!(c, '\r' | '\n'));
                LINE_COMMENT
            }
            Some('*') => {
                cursor.bump();
                // Through the closing `*/`, or to end of input if unterminated.
                // (`/**` Javadoc is reclassified by `SyntaxKind::from_token`.)
                match src[cursor.pos..].find("*/") {
                    Some(i) => cursor.pos += i + 2,
                    None => cursor.pos = src.len(),
                }
                BLOCK_COMMENT
            }
            Some('=') => {
                cursor.bump();
                SLASH_EQ
            }
            _ => SLASH,
        },

        // Literals.
        '"' => return scan_string_or_text_block(src, start),
        '\'' => return scan_char_literal(src, start),
        '0'..='9' => {
            let (kind, end) = scan_number(src.as_bytes(), start);
            return (Ok(kind), end);
        }

        // `.` leads `...`, a fraction-first float (`.5`), or a plain `.`.
        '.' => {
            let bytes = src.as_bytes();
            if bytes.get(start + 1) == Some(&b'.') && bytes.get(start + 2) == Some(&b'.') {
                cursor.pos = start + 3;
                ELLIPSIS
            } else if bytes.get(start + 1).is_some_and(u8::is_ascii_digit) {
                return (Ok(FLOAT_LITERAL), scan_fraction_float(bytes, start));
            } else {
                DOT
            }
        }

        // Identifiers, keywords, and `_`: scan the full run, then look the slice up.
        c if is_ident_start(c) => {
            cursor.eat_while(is_ident_continue);
            match &src[start..cursor.pos] {
                "_" => UNDERSCORE,
                text => keyword_kind(text).unwrap_or(IDENT),
            }
        }

        // Delimiters.
        '(' => LPAREN,
        ')' => RPAREN,
        '{' => LBRACE,
        '}' => RBRACE,
        '[' => LBRACK,
        ']' => RBRACK,
        ';' => SEMICOLON,
        ',' => COMMA,
        '@' => AT,

        // Operators (maximal munch).
        '~' => TILDE,
        '?' => QUESTION,
        ':' => {
            if cursor.eat_if(':') {
                COLON_COLON
            } else {
                COLON
            }
        }
        '=' => {
            if cursor.eat_if('=') {
                EQ_EQ
            } else {
                EQ
            }
        }
        '!' => {
            if cursor.eat_if('=') {
                BANG_EQ
            } else {
                BANG
            }
        }
        '<' => {
            if cursor.eat_if('<') {
                if cursor.eat_if('=') { LSHIFT_EQ } else { LSHIFT }
            } else if cursor.eat_if('=') {
                LT_EQ
            } else {
                LT
            }
        }
        // Always a single `>`: the parser fuses adjacent `GT`s for generics, so the lexer
        // never emits `>=` / `>>` / `>>>` / `>>=` / `>>>=`.
        '>' => GT,
        '+' => {
            if cursor.eat_if('+') {
                PLUS_PLUS
            } else if cursor.eat_if('=') {
                PLUS_EQ
            } else {
                PLUS
            }
        }
        '-' => {
            if cursor.eat_if('>') {
                ARROW
            } else if cursor.eat_if('-') {
                MINUS_MINUS
            } else if cursor.eat_if('=') {
                MINUS_EQ
            } else {
                MINUS
            }
        }
        '*' => {
            if cursor.eat_if('=') {
                STAR_EQ
            } else {
                STAR
            }
        }
        '&' => {
            if cursor.eat_if('&') {
                AMP_AMP
            } else if cursor.eat_if('=') {
                AMP_EQ
            } else {
                AMP
            }
        }
        '|' => {
            if cursor.eat_if('|') {
                PIPE_PIPE
            } else if cursor.eat_if('=') {
                PIPE_EQ
            } else {
                PIPE
            }
        }
        '^' => {
            if cursor.eat_if('=') {
                CARET_EQ
            } else {
                CARET
            }
        }
        '%' => {
            if cursor.eat_if('=') {
                PERCENT_EQ
            } else {
                PERCENT
            }
        }

        // Anything else is a one-char error token.
        _ => return (Err(()), cursor.pos),
    };
    (Ok(kind), cursor.pos)
}

/// Scans a token starting with `"`: a text block (`"""` opener), a string literal, or an
/// error token if the string is unterminated. On failure mid-input the error spans only
/// the chars consumed before the failing one, which is then re-lexed on its own; at end
/// of input it spans everything consumed.
fn scan_string_or_text_block(src: &str, start: usize) -> (Result<TokenKind, ()>, usize) {
    let bytes = src.as_bytes();
    if bytes.get(start + 1) == Some(&b'"') && bytes.get(start + 2) == Some(&b'"') {
        return (Ok(TokenKind::TEXT_BLOCK), scan_text_block(bytes, start + 3));
    }
    let mut cursor = Cursor {
        src,
        pos: start + 1,
    };
    loop {
        match cursor.bump() {
            // Unterminated at end of input.
            None => return (Err(()), cursor.pos),
            Some('"') => return (Ok(TokenKind::STRING_LITERAL), cursor.pos),
            Some('\\') => match cursor.first() {
                // An escape covers any char except `\n` (which stays its own token).
                Some('\n') => return (Err(()), cursor.pos),
                Some(_) => {
                    cursor.bump();
                }
                None => return (Err(()), cursor.pos),
            },
            // A bare line break never belongs to a string literal (1 byte; not consumed).
            Some('\r' | '\n') => return (Err(()), cursor.pos - 1),
            Some(_) => {}
        }
    }
}

/// Scans the rest of a text block whose opening `"""` ends at `pos`. The block runs
/// through the first closing `"""` preceded by an even-length backslash run, or to the
/// end of input if unterminated. Returns the end offset.
fn scan_text_block(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"\"\"\"" {
            // This `"""` closes the block iff the run of backslashes directly before it
            // (within the block body) has even length.
            let mut j = i;
            while j > pos && bytes[j - 1] == b'\\' {
                j -= 1;
            }
            if (i - j) % 2 == 0 {
                return i + 3;
            }
        }
        i += 1;
    }
    bytes.len()
}

/// Scans a token starting with `'`: a char literal (`'x'` or `'\x'`), or an error token
/// on malformed input. The error span follows the same rule as for strings: the failing
/// char (if any) is excluded and re-lexed on its own.
fn scan_char_literal(src: &str, start: usize) -> (Result<TokenKind, ()>, usize) {
    let mut cursor = Cursor {
        src,
        pos: start + 1,
    };
    // The single element: a plain char or a `\` escape (covering any char except `\n`).
    match cursor.first() {
        None | Some('\'' | '\r' | '\n') => return (Err(()), cursor.pos),
        Some('\\') => {
            cursor.bump();
            match cursor.first() {
                None | Some('\n') => return (Err(()), cursor.pos),
                Some(_) => {
                    cursor.bump();
                }
            }
        }
        Some(_) => {
            cursor.bump();
        }
    }
    // The closing quote.
    if cursor.eat_if('\'') {
        (Ok(TokenKind::CHAR_LITERAL), cursor.pos)
    } else {
        (Err(()), cursor.pos)
    }
}

// === Numeric literals ===
//
// Reproduces the combined longest match over Java's integer and floating-point literal
// forms: for each candidate form the longest accepting end is computed, and the longest
// one wins. An integer form and a float form never accept the same length (integers end
// in a digit or `l`/`L`; floats end in `.`, an exponent digit, or `f`/`F`/`d`/`D`), so
// the winner is unambiguous.

/// Scans a numeric literal starting at `start` (`bytes[start]` is an ASCII digit).
fn scan_number(bytes: &[u8], start: usize) -> (TokenKind, usize) {
    if bytes[start] == b'0' && matches!(bytes.get(start + 1), Some(b'x' | b'X')) {
        scan_hex_number(bytes, start)
    } else {
        scan_decimal_number(bytes, start)
    }
}

/// Scans a decimal (or `0`-prefixed octal/binary) literal starting at `start`.
fn scan_decimal_number(bytes: &[u8], start: usize) -> (TokenKind, usize) {
    // Integer candidates: `0`, octal `0(_*[0-7])+`, binary `0[bB][01](_*[01])*`, or a
    // decimal run `[1-9](_*[0-9])*` — each with an optional `l`/`L` suffix.
    let int_body = if bytes[start] == b'0' {
        if matches!(bytes.get(start + 1), Some(b'b' | b'B')) {
            let run = digit_run(bytes, start + 2, |b| matches!(b, b'0' | b'1'));
            // Without a binary digit the form fails; fall back to the bare `0`.
            if run > start + 2 { run } else { start + 1 }
        } else {
            // The octal run includes the leading `0`; a lone `0` is also accepted.
            digit_run(bytes, start, |b| matches!(b, b'0'..=b'7'))
        }
    } else {
        digit_run(bytes, start, |b| b.is_ascii_digit())
    };
    let int_end = int_suffix(bytes, int_body);

    // Float candidates all build on the full decimal digit run (so `089.5` lexes as one
    // float even though `089` alone falls back to `0` + `89`).
    let digits = digit_run(bytes, start, |b| b.is_ascii_digit());
    let mut float_end = None;
    // `digits . [digits] [exponent] [suffix]`
    if bytes.get(digits) == Some(&b'.') {
        let mut end = digit_run(bytes, digits + 1, |b| b.is_ascii_digit());
        end = decimal_exponent(bytes, end).unwrap_or(end);
        float_end = Some(float_suffix(bytes, end));
    }
    // `digits exponent [suffix]`
    if let Some(end) = decimal_exponent(bytes, digits) {
        float_end = float_end.max(Some(float_suffix(bytes, end)));
    }
    // `digits suffix`
    if matches!(bytes.get(digits), Some(b'f' | b'F' | b'd' | b'D')) {
        float_end = float_end.max(Some(digits + 1));
    }

    match float_end {
        Some(end) if end > int_end => (TokenKind::FLOAT_LITERAL, end),
        _ => (TokenKind::INT_LITERAL, int_end),
    }
}

/// Scans a hexadecimal literal starting at `start` (`bytes[start..start + 2]` is
/// `0x`/`0X`): a hex integer, or a hex float — which always requires a `p` exponent.
/// Falls back to the bare `0` when no hex digit follows the prefix.
fn scan_hex_number(bytes: &[u8], start: usize) -> (TokenKind, usize) {
    let is_hex = |b: u8| b.is_ascii_hexdigit();
    let digits = digit_run(bytes, start + 2, is_hex);

    // Integer candidate: `0x digits [lL]`, or the bare `0` when no digit follows.
    let int_end = if digits > start + 2 {
        int_suffix(bytes, digits)
    } else {
        start + 1
    };

    let mut float_end = None;
    if digits > start + 2 {
        // `0x digits [.] p-exponent [suffix]` (with a dot, `p` must follow immediately).
        let after_dot = if bytes.get(digits) == Some(&b'.') {
            digits + 1
        } else {
            digits
        };
        if let Some(end) = hex_exponent(bytes, after_dot) {
            float_end = Some(float_suffix(bytes, end));
        }
    }
    if bytes.get(digits) == Some(&b'.') {
        // `0x [digits] . digits p-exponent [suffix]`
        let frac = digit_run(bytes, digits + 1, is_hex);
        if frac > digits + 1
            && let Some(end) = hex_exponent(bytes, frac)
        {
            float_end = float_end.max(Some(float_suffix(bytes, end)));
        }
    }

    match float_end {
        Some(end) if end > int_end => (TokenKind::FLOAT_LITERAL, end),
        _ => (TokenKind::INT_LITERAL, int_end),
    }
}

/// Scans a fraction-first float (`. digits [exponent] [suffix]`) whose `.` sits at
/// `start` (`bytes[start + 1]` is an ASCII digit). Returns the end offset.
fn scan_fraction_float(bytes: &[u8], start: usize) -> usize {
    let digits = digit_run(bytes, start + 1, |b| b.is_ascii_digit());
    let end = decimal_exponent(bytes, digits).unwrap_or(digits);
    float_suffix(bytes, end)
}

/// Scans a `digit (_* digit)*` run at `start` using `is_digit`, returning the position
/// after the last digit (trailing underscores are not part of the run). Returns `start`
/// if there is no digit at `start`.
fn digit_run(bytes: &[u8], start: usize, is_digit: impl Fn(u8) -> bool) -> usize {
    if !bytes.get(start).is_some_and(|&b| is_digit(b)) {
        return start;
    }
    let mut end = start + 1;
    loop {
        let mut i = end;
        while bytes.get(i) == Some(&b'_') {
            i += 1;
        }
        if bytes.get(i).is_some_and(|&b| is_digit(b)) {
            end = i + 1;
        } else {
            return end;
        }
    }
}

/// Scans a complete decimal exponent (`[eE][+-]?digits`) at `start`.
fn decimal_exponent(bytes: &[u8], start: usize) -> Option<usize> {
    exponent(bytes, start, b'e', b'E')
}

/// Scans a complete hex-float exponent (`[pP][+-]?digits`; the digits are decimal) at
/// `start`.
fn hex_exponent(bytes: &[u8], start: usize) -> Option<usize> {
    exponent(bytes, start, b'p', b'P')
}

/// Scans a complete exponent (`marker [+-] digits`) at `start`, returning its end.
/// An exponent is all-or-nothing: with no digit after the marker (and optional sign),
/// `None` is returned and the caller keeps its shorter accepting prefix.
fn exponent(bytes: &[u8], start: usize, lower: u8, upper: u8) -> Option<usize> {
    if !matches!(bytes.get(start), Some(&b) if b == lower || b == upper) {
        return None;
    }
    let mut i = start + 1;
    if matches!(bytes.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let end = digit_run(bytes, i, |b| b.is_ascii_digit());
    (end > i).then_some(end)
}

/// Extends past an optional integer suffix (`l`/`L`) at `end`.
fn int_suffix(bytes: &[u8], end: usize) -> usize {
    if matches!(bytes.get(end), Some(b'l' | b'L')) {
        end + 1
    } else {
        end
    }
}

/// Extends past an optional float suffix (`f`/`F`/`d`/`D`) at `end`.
fn float_suffix(bytes: &[u8], end: usize) -> usize {
    if matches!(bytes.get(end), Some(b'f' | b'F' | b'd' | b'D')) {
        end + 1
    } else {
        end
    }
}

// === Identifier character classes ===
//
// These match the identifier definition the previous lexer used:
// start = `\p{L}\p{Nl}\p{Sc}\p{Pc}`, continue = start plus `\p{Nd}\p{Mn}\p{Mc}\p{Cf}`.

/// Whether `c` can start an identifier.
fn is_ident_start(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic() || c == '_' || c == '$';
    }
    use unicode_properties::{GeneralCategory as GC, UnicodeGeneralCategory as _};
    matches!(
        c.general_category(),
        GC::UppercaseLetter
            | GC::LowercaseLetter
            | GC::TitlecaseLetter
            | GC::ModifierLetter
            | GC::OtherLetter
            | GC::LetterNumber
            | GC::CurrencySymbol
            | GC::ConnectorPunctuation
    )
}

/// Whether `c` can continue an identifier.
fn is_ident_continue(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_' || c == '$';
    }
    use unicode_properties::{GeneralCategory as GC, UnicodeGeneralCategory as _};
    matches!(
        c.general_category(),
        GC::UppercaseLetter
            | GC::LowercaseLetter
            | GC::TitlecaseLetter
            | GC::ModifierLetter
            | GC::OtherLetter
            | GC::LetterNumber
            | GC::CurrencySymbol
            | GC::ConnectorPunctuation
            | GC::DecimalNumber
            | GC::NonspacingMark
            | GC::SpacingMark
            | GC::Format
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use SyntaxKind::*;

    /// `(kind, text)` の列に落とす。
    fn lexed(src: &str) -> Vec<(SyntaxKind, &str)> {
        tokenize(src)
            .into_iter()
            .map(|t| (t.kind, t.text))
            .collect()
    }

    /// 種別の列に落とす。
    fn kinds(src: &str) -> Vec<SyntaxKind> {
        tokenize(src).into_iter().map(|t| t.kind).collect()
    }

    /// 連結すると入力に一致する(lossless)。
    fn roundtrip(src: &str) -> String {
        tokenize(src).iter().map(|t| t.text).collect()
    }

    #[test]
    fn keywords_vs_contextual_keywords() {
        // 予約語はキーワードトークン。
        assert_eq!(kinds("class"), vec![CLASS_KW]);
        assert_eq!(kinds("int"), vec![INT_KW]);
        assert_eq!(kinds("instanceof"), vec![INSTANCEOF_KW]);
        // 文脈依存キーワードは IDENT(parser が昇格する)。
        for kw in [
            "var", "yield", "record", "sealed", "permits", "when", "module", "with",
        ] {
            assert_eq!(lexed(kw), vec![(IDENT, kw)], "{kw} は IDENT であるべき");
        }
        // 予約語の前方一致はより長い IDENT。
        assert_eq!(lexed("classes"), vec![(IDENT, "classes")]);
    }

    #[test]
    fn underscore_vs_identifier() {
        assert_eq!(kinds("_"), vec![UNDERSCORE]);
        assert_eq!(lexed("__"), vec![(IDENT, "__")]);
        assert_eq!(lexed("_x"), vec![(IDENT, "_x")]);
    }

    #[test]
    fn unicode_identifiers() {
        // CJK(Lo)、通貨記号(Sc: $ / €)、接続句読点(Pc: _)。
        assert_eq!(lexed("名前"), vec![(IDENT, "名前")]);
        assert_eq!(lexed("$x"), vec![(IDENT, "$x")]);
        assert_eq!(lexed("€uro"), vec![(IDENT, "€uro")]);
    }

    #[test]
    fn generics_angle_brackets() {
        // `>` は常に単一 GT(複合は parser が合成)。
        assert_eq!(kinds(">"), vec![GT]);
        assert_eq!(kinds(">>"), vec![GT, GT]);
        assert_eq!(kinds(">>>"), vec![GT, GT, GT]);
        assert_eq!(kinds(">="), vec![GT, EQ]);
        assert_eq!(kinds(">>="), vec![GT, GT, EQ]);
        assert_eq!(kinds(">>>="), vec![GT, GT, GT, EQ]);
        // `<` 側はジェネリクス開きと曖昧でないため複合を字句化。
        assert_eq!(kinds("<"), vec![LT]);
        assert_eq!(kinds("<="), vec![LT_EQ]);
        assert_eq!(kinds("<<"), vec![LSHIFT]);
        assert_eq!(kinds("<<="), vec![LSHIFT_EQ]);
        // ネストしたジェネリクスの閉じは GT 2 つ。
        assert_eq!(
            kinds("Map<K,List<V>>"),
            vec![IDENT, LT, IDENT, COMMA, IDENT, LT, IDENT, GT, GT]
        );
    }

    #[test]
    fn non_sealed_is_three_tokens() {
        assert_eq!(
            lexed("non-sealed"),
            vec![(IDENT, "non"), (MINUS, "-"), (IDENT, "sealed")]
        );
    }

    #[test]
    fn integer_literals() {
        for s in [
            "0",
            "0L",
            "123",
            "1_000_000",
            "0xCAFE_babeL",
            "0b1010",
            "0777",
            "0XFFl",
        ] {
            assert_eq!(kinds(s), vec![INT_LITERAL], "{s} は INT であるべき");
        }
    }

    #[test]
    fn float_literals() {
        for s in [
            "3.14", "3.14f", ".5", "1e10", "1.5e-3", "2.0d", "100f", "0x1.8p3", "0x1p-2",
        ] {
            assert_eq!(kinds(s), vec![FLOAT_LITERAL], "{s} は FLOAT であるべき");
        }
    }

    #[test]
    fn char_string_text_block() {
        assert_eq!(kinds("'a'"), vec![CHAR_LITERAL]);
        assert_eq!(kinds(r"'\n'"), vec![CHAR_LITERAL]);
        assert_eq!(kinds(r#""hello""#), vec![STRING_LITERAL]);
        assert_eq!(kinds(r#""a\"b""#), vec![STRING_LITERAL]);
        assert_eq!(kinds("\"\"\"\nabc\n\"\"\""), vec![TEXT_BLOCK]);
    }

    #[test]
    fn comments_and_javadoc() {
        assert_eq!(kinds("// hi"), vec![LINE_COMMENT]);
        assert_eq!(kinds("/* b */"), vec![BLOCK_COMMENT]);
        assert_eq!(kinds("/**/"), vec![BLOCK_COMMENT]); // 空コメントは Javadoc ではない
        assert_eq!(kinds("/** d */"), vec![DOC_COMMENT]);
        assert_eq!(kinds("/***/"), vec![DOC_COMMENT]);
    }

    #[test]
    fn operators_and_separators() {
        assert_eq!(
            kinds("a += b ? c :: d -> e"),
            vec![
                IDENT,
                WHITESPACE,
                PLUS_EQ,
                WHITESPACE,
                IDENT,
                WHITESPACE,
                QUESTION,
                WHITESPACE,
                IDENT,
                WHITESPACE,
                COLON_COLON,
                WHITESPACE,
                IDENT,
                WHITESPACE,
                ARROW,
                WHITESPACE,
                IDENT
            ]
        );
        assert_eq!(kinds("..."), vec![ELLIPSIS]);
        assert_eq!(kinds(".."), vec![DOT, DOT]);
    }

    #[test]
    fn unterminated_inputs_are_lossless() {
        for s in [
            "/* unterminated",
            "\"unterminated",
            "'a",
            "\"\"\"unterminated",
            "0x",
        ] {
            assert_eq!(roundtrip(s), s, "{s:?} の round-trip 失敗");
        }
    }

    #[test]
    fn error_bytes_are_lossless() {
        let src = "a # b ` c";
        assert!(kinds(src).contains(&ERROR), "未一致バイトは ERROR になる");
        assert_eq!(roundtrip(src), src);
    }

    #[test]
    fn roundtrip_realistic_class() {
        let src = concat!(
            "package com.example;\n\n",
            "import java.util.List;\n\n",
            "/** Doc. */\n",
            "public final class Foo<T> {\n",
            "    private int x = 0xFF_00;\n",
            "    // line comment\n",
            "    String s = \"hi\\n\";\n",
            "    var t = \"\"\"\n        text\n        \"\"\";\n",
            "    int shift = x >> 2;\n",
            "    boolean b = p >= q && r <= s;\n",
            "}\n",
        );
        assert_eq!(roundtrip(src), src);
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// 任意の UTF-8 文字列で round-trip が成り立ち、panic しない。
        /// これが lossless 保証(全バイトをちょうど 1 トークンに対応)の中核。
        #[test]
        fn roundtrip_any_string(src in any::<String>()) {
            let joined: String = tokenize(&src).iter().map(|t| t.text).collect();
            prop_assert_eq!(joined, src);
        }
    }
}

/// TEMPORARY (Phase A only): differential tests asserting that the hand-written scanner
/// produces exactly the `logos` token stream. Deleted when the new scanner is swapped in.
#[cfg(test)]
mod differential {
    use super::*;
    use proptest::prelude::*;

    fn assert_same(src: &str) {
        let old = tokenize(src);
        let new = tokenize_new(src);
        assert_eq!(old, new, "token streams differ for input: {src:?}");
    }

    #[test]
    fn curated_edges() {
        #[rustfmt::skip]
        let cases: &[&str] = &[
            // Keywords, identifiers, underscore.
            "class", "classes", "class名", "int", "instanceof", "var", "record", "_", "__",
            "_x", "1_x", "non-sealed", "$x", "€uro", "名前", "true", "false", "null",
            "truely", "nullx", "a\u{200D}b", "a\u{0301}", "\u{0301}", "\u{2160}", "x\u{FEFF}",
            "\u{FEFF}", "µ", "क्ष", "_$", "$",
            // Integer literals.
            "0", "0L", "0l", "00", "08", "089", "0779", "0_7", "07_l", "0_8", "0_8f", "123",
            "1_000_000", "1__2", "1_", "1_x", "0x", "0X", "0xFF", "0xCAFE_babeL", "0XFFl",
            "0x_1", "0b1010", "0b2", "0B0", "0b", "0b1_", "0bl", "0lb", "0777", "0788",
            // Float literals.
            "3.14", "3.14f", ".5", ".5f", ".5e2", "1e10", "1e+10", "1e-10", "1.5e-3", "2.0d",
            "100f", "1.", "1.f", "1.e5", "1..2", "1e", "1e+", "1.5L", "1.5e", "1.5e_3", "0d",
            "0f", "089f", "08e1", "089.5", "09.5", "0.5", "..5", "...5", ".5...", ".e5",
            "1.0e5.4", "0_9.5",
            // Hex floats.
            "0x1p3", "0x1P3", "0x1p-2", "0x1p+2", "0x1.8p3", "0x1.p3", "0x.8p-2f", "0x1.8",
            "0x1.", "0x1p", "0x1p+", "0x1.p", "0x.8", "0x.p3", "0xp3", "0x1_p3", "0x1.8p",
            "0x1p3f", "0x1p3d", "0x1p3l", "0xfp3", "0x1.8p1_0", "0X.Ap2",
            // Char literals and errors.
            "'a'", r"'\n'", r"'\''", "''", "'''", "'ab'", "'abc'", "'a", "'", "'\n", "'\r\n",
            r"'\", "'\\\n", r"'\x", r"'\xy", "'名'", "'名", "'名x", "'\\\r'", "'\\'",
            // String literals and errors.
            r#""""#, r#""hello""#, r#""a\"b""#, "\"abc", "\"abc\ndef", "\"abc\rdef", "\"a\\",
            "\"a\\\nx", "\"a\\\rx", "\"a\"b\"", "\"\\\"", "\"名\"", "\"名",
            // Text blocks.
            "\"\"\"\nabc\n\"\"\"", "\"\"\"\"\"\"", "\"\"\"", "\"\"\"x", "\"\"\"\"",
            "\"\"\"\\\"\"\"x", "\"\"\"\\\\\"\"\"x", "\"\"\"a\"\"x\"\"\"b",
            // Comments.
            "// hi", "//", "// a\nb", "//\r\n", "/* b */", "/**/", "/***/", "/** d */",
            "/*/", "/* unterminated", "/**", "/*a*b*/c",
            // Operators and separators.
            ">", ">>", ">>>", ">=", ">>=", ">>>=", "<", "<=", "<<", "<<=", "<<<", "...",
            "..", ".", "::", ":::", "->", "--", "-=", "-", "&&", "&=", "&", "||", "|=",
            "++", "+=", "==", "!=", "/=", "a/=b", "a/b", "*=", "^=", "%=", "@", "~", "?",
            // Whitespace and newlines.
            " \t\x0C", "\r\n", "\r", "\n", "\r\r\n\n", " \n ", "\x0B",
            // Unmatched bytes.
            "#", "`", "\\", "\\\\", "a # b ` c", "\u{00A0}", "😀", "\0",
            // Realistic snippets.
            "Map<K,List<V>>",
            "class A{int x=0xFF;void m(){var s=\"\"\"\n hi\n\"\"\";}}",
            "a += b ? c :: d -> e",
        ];
        for src in cases {
            assert_same(src);
        }
    }

    /// Pins Unicode-table parity: every char, alone and embedded in an identifier-ish
    /// context, lexes identically in both implementations.
    #[test]
    fn exhaustive_chars() {
        for c in '\0'..=char::MAX {
            assert_same(&format!("{c}"));
            assert_same(&format!("a{c}b"));
        }
    }

    /// Differential over the OpenJDK corpus, if checked out (skips silently otherwise).
    #[test]
    fn corpus() {
        fn visit(dir: &std::path::Path) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path);
                } else if path.extension().is_some_and(|e| e == "java")
                    && let Ok(src) = std::fs::read_to_string(&path)
                {
                    assert_same(&src);
                }
            }
        }
        visit(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../jals-tests/sources"
        )));
    }

    /// A Java-biased fragment soup: heavy in quotes, backslashes, digits, operators, and
    /// the suffix/exponent letters that drive the numeric scanner.
    fn java_biased() -> impl Strategy<Value = String> {
        let fragment = prop_oneof![
            Just("\"".to_string()),
            Just("\"\"\"".to_string()),
            Just("'".to_string()),
            Just("\\".to_string()),
            Just("\n".to_string()),
            Just("\r\n".to_string()),
            Just("\r".to_string()),
            Just(" ".to_string()),
            Just("0".to_string()),
            Just("1".to_string()),
            Just("9".to_string()),
            Just("_".to_string()),
            Just(".".to_string()),
            Just("x".to_string()),
            Just("b".to_string()),
            Just("p".to_string()),
            Just("e".to_string()),
            Just("f".to_string()),
            Just("d".to_string()),
            Just("L".to_string()),
            Just("l".to_string()),
            Just("+".to_string()),
            Just("-".to_string()),
            Just("/".to_string()),
            Just("*".to_string()),
            Just("*/".to_string()),
            Just("//".to_string()),
            Just("<".to_string()),
            Just(">".to_string()),
            Just("=".to_string()),
            Just("&".to_string()),
            Just("|".to_string()),
            Just(":".to_string()),
            Just("a".to_string()),
            Just("F".to_string()),
            Just("class".to_string()),
            Just("名".to_string()),
            Just("€".to_string()),
            Just("\u{200D}".to_string()),
            Just("\u{0301}".to_string()),
            Just("#".to_string()),
            proptest::char::any().prop_map(|c| c.to_string()),
        ];
        proptest::collection::vec(fragment, 0..48).prop_map(|v| v.concat())
    }

    proptest! {
        #[test]
        fn diff_any_string(src in any::<String>()) {
            assert_same(&src);
        }

        #[test]
        fn diff_java_biased(src in java_biased()) {
            assert_same(&src);
        }
    }
}
