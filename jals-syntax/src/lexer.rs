//! `logos` をラップした lossless な字句解析器。
//!
//! [`tokenize`] / [`Lexer`] は入力の全バイトをちょうど 1 トークンに対応させ(各トークンの
//! `text` を連結すると入力に一致する)、いかなる入力でも panic しない。未一致バイトは
//! [`SyntaxKind::ERROR`] になる。

use logos::Logos;
use text_size::{TextRange, TextSize};

use crate::syntax_kind::SyntaxKind;
use crate::token::TokenKind;

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
