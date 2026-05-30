//! error-resilient な再帰下降パーサ。文法はイベント列を吐き([`event`])、[`sink`] が
//! `rowan` の緑木を組み立てる。壊れた入力でも panic せず木を返すことを不変条件とする。

mod event;
mod grammar;
mod input;
mod marker;
mod sink;
mod token_set;

use std::cell::Cell;

use rowan::GreenNode;

use crate::language::SyntaxNode;
use crate::syntax_error::SyntaxError;
use crate::syntax_kind::SyntaxKind;
use event::Event;
use input::Input;
use marker::Marker;
use token_set::TokenSet;

/// 同じ位置で前進せずに先読みを繰り返す無限ループを検出する燃料。
const PARSER_FUEL: u32 = 256;

/// パーサ本体。significant トークン列を位置 `pos` で走査し、イベント列を組み立てる。
pub(crate) struct Parser<'a> {
    input: &'a Input<'a>,
    pos: usize,
    pub(crate) events: Vec<Event>,
    fuel: Cell<u32>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a Input<'a>) -> Self {
        Parser {
            input,
            pos: 0,
            events: Vec::new(),
            fuel: Cell::new(PARSER_FUEL),
        }
    }

    /// 新しいノードを開く。
    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::tombstone());
        Marker::new(pos)
    }

    pub(crate) fn push_event(&mut self, e: Event) {
        self.events.push(e);
    }

    /// 現在の significant トークン位置。ループの前進保証に使う(値が変わらなければ未消費)。
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// `n` 個先の significant トークンの種別。
    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        assert_ne!(
            self.fuel.get(),
            0,
            "parser fuel exhausted(無限ループの可能性)"
        );
        self.fuel.set(self.fuel.get() - 1);
        self.input.kind(self.pos + n)
    }

    /// 現在の significant トークンの種別。
    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.nth(0) == kind
    }

    pub(crate) fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        self.nth(n) == kind
    }

    pub(crate) fn at_ts(&self, set: TokenSet) -> bool {
        set.contains(self.nth(0))
    }

    pub(crate) fn at_eof(&self) -> bool {
        self.at(SyntaxKind::EOF)
    }

    /// 現在が `IDENT` で、そのテキストが `kw` に一致するか(文脈依存キーワード判定)。
    pub(crate) fn at_contextual_kw(&self, kw: &str) -> bool {
        self.at(SyntaxKind::IDENT) && self.current_text() == kw
    }

    /// `n` 個先の significant トークンが次の significant トークンと隣接するか
    /// (間にトリビアなし)。`>>` などの合成に使う。
    pub(crate) fn nth_adjacent(&self, n: usize) -> bool {
        self.input.adjacent(self.pos + n)
    }

    /// 現在の significant トークンのテキスト(文脈依存キーワード判定用)。
    pub(crate) fn current_text(&self) -> &'a str {
        self.input.text(self.pos)
    }

    pub(crate) fn nth_text(&self, n: usize) -> &'a str {
        self.input.text(self.pos + n)
    }

    fn do_bump(&mut self, remap: Option<SyntaxKind>) {
        self.pos += 1;
        self.fuel.set(PARSER_FUEL);
        self.events.push(Event::Token { remap });
    }

    /// 現在のトークンを消費する(EOF では何もしない)。
    pub(crate) fn bump_any(&mut self) {
        if self.at_eof() {
            return;
        }
        self.do_bump(None);
    }

    /// `kind` であることを確認して消費する。
    pub(crate) fn bump(&mut self, kind: SyntaxKind) {
        assert!(
            self.at(kind),
            "{kind:?} を bump しようとしたが現在は {:?}",
            self.current()
        );
        self.do_bump(None);
    }

    /// 現在のトークンを `kind` に付け替えて消費する(文脈依存キーワードの昇格)。
    pub(crate) fn bump_remap(&mut self, kind: SyntaxKind) {
        self.do_bump(Some(kind));
    }

    /// `kind` なら消費して `true`。
    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.do_bump(None);
            true
        } else {
            false
        }
    }

    /// エラーを記録する(消費しない)。
    pub(crate) fn error(&mut self, msg: impl Into<String>) {
        self.events.push(Event::Error { msg: msg.into() });
    }

    /// `kind` を期待。あれば消費して `true`、なければエラーを記録して `false`(消費しない)。
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error(format!("{kind:?} を期待しました"));
            false
        }
    }

    /// 現在のトークンを `ERROR` ノードで包んで1つ消費する(回復しつつ前進を保証)。
    pub(crate) fn err_and_bump(&mut self, msg: impl Into<String>) {
        let m = self.start();
        self.error(msg);
        self.bump_any();
        m.complete(self, SyntaxKind::ERROR);
    }

    /// エラーを記録し、回復集合 `recovery` に当たらないトークンを `ERROR` で包んで1つ消費する。
    /// 回復集合に当たる(=上位が処理できる)トークンや EOF では消費しない。
    pub(crate) fn err_recover(&mut self, msg: impl Into<String>, recovery: TokenSet) {
        if self.at_eof() || self.at_ts(recovery) {
            self.error(msg);
            return;
        }
        self.err_and_bump(msg);
    }

    fn finish(self) -> Vec<Event> {
        self.events
    }
}

/// ソースをパースして [`Parse`] を返す。
pub fn parse(src: &str) -> Parse {
    let input = Input::new(src);
    let mut p = Parser::new(&input);
    grammar::root(&mut p);
    let events = p.finish();
    let (green, errors) = sink::build(&input, events);
    Parse { green, errors }
}

/// パース結果。緑木と構文エラー一覧を持つ。
pub struct Parse {
    green: GreenNode,
    errors: Vec<SyntaxError>,
}

impl Parse {
    /// 構文木のルートノード。
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    /// 構文エラー一覧。
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    /// 緑木への参照。
    pub fn green(&self) -> &GreenNode {
        &self.green
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use expect_test::{Expect, expect};

    /// パース結果の構文木を、種別と範囲つきでインデント表示する(テスト用)。
    /// トークンはテキストも併記する。末尾のエラーは `error: ...` 行で続ける。
    fn debug_tree(parse: &Parse) -> String {
        use std::fmt::Write;
        let mut buf = String::new();
        let mut indent = 0;
        for event in parse.syntax().preorder_with_tokens() {
            use rowan::WalkEvent::{Enter, Leave};
            match event {
                Enter(elem) => {
                    let kind = elem.kind();
                    let range = elem.text_range();
                    match elem {
                        rowan::NodeOrToken::Node(_) => {
                            let _ = writeln!(buf, "{:indent$}{kind:?}@{range:?}", "");
                        }
                        rowan::NodeOrToken::Token(t) => {
                            let _ =
                                writeln!(buf, "{:indent$}{kind:?}@{range:?} {:?}", "", t.text());
                        }
                    }
                    indent += 2;
                }
                Leave(_) => indent -= 2,
            }
        }
        for err in parse.errors() {
            let _ = writeln!(buf, "error {:?}: {}", err.range(), err.message());
        }
        buf
    }

    /// 木のダンプがスナップショットに一致し、かつ lossless であることを確認する。
    fn check(src: &str, expected: Expect) {
        let parse = parse(src);
        expected.assert_eq(&debug_tree(&parse));
        assert_eq!(
            parse.syntax().text().to_string(),
            src,
            "lossless 不変条件の違反"
        );
    }

    /// lossless: 構文木のテキストは入力に一致する。
    fn assert_lossless(src: &str) {
        let parse = parse(src);
        assert_eq!(parse.syntax().text().to_string(), src);
    }

    #[test]
    fn package_and_imports() {
        check(
            "package a.b.c;\nimport java.util.List;\nimport static a.B.c;\nimport a.b.*;\n",
            expect![[r#"
                SOURCE_FILE@0..73
                  PACKAGE_DECL@0..14
                    PACKAGE_KW@0..7 "package"
                    QUALIFIED_NAME@7..13
                      WHITESPACE@7..8 " "
                      IDENT@8..9 "a"
                      DOT@9..10 "."
                      IDENT@10..11 "b"
                      DOT@11..12 "."
                      IDENT@12..13 "c"
                    SEMICOLON@13..14 ";"
                  IMPORT_DECL@14..37
                    NEWLINE@14..15 "\n"
                    IMPORT_KW@15..21 "import"
                    QUALIFIED_NAME@21..36
                      WHITESPACE@21..22 " "
                      IDENT@22..26 "java"
                      DOT@26..27 "."
                      IDENT@27..31 "util"
                      DOT@31..32 "."
                      IDENT@32..36 "List"
                    SEMICOLON@36..37 ";"
                  IMPORT_DECL@37..58
                    NEWLINE@37..38 "\n"
                    IMPORT_KW@38..44 "import"
                    WHITESPACE@44..45 " "
                    STATIC_KW@45..51 "static"
                    QUALIFIED_NAME@51..57
                      WHITESPACE@51..52 " "
                      IDENT@52..53 "a"
                      DOT@53..54 "."
                      IDENT@54..55 "B"
                      DOT@55..56 "."
                      IDENT@56..57 "c"
                    SEMICOLON@57..58 ";"
                  IMPORT_DECL@58..72
                    NEWLINE@58..59 "\n"
                    IMPORT_KW@59..65 "import"
                    QUALIFIED_NAME@65..71
                      WHITESPACE@65..66 " "
                      IDENT@66..67 "a"
                      DOT@67..68 "."
                      IDENT@68..69 "b"
                      DOT@69..70 "."
                      STAR@70..71 "*"
                    SEMICOLON@71..72 ";"
                  NEWLINE@72..73 "\n"
            "#]],
        );
    }

    #[test]
    fn class_with_field_and_method() {
        check(
            "public final class Foo<T> extends Bar implements I {\n  private int x = 1;\n  void m(int a) { return; }\n}\n",
            expect![[r#"
                SOURCE_FILE@0..104
                  CLASS_DECL@0..103
                    MODIFIERS@0..12
                      PUBLIC_KW@0..6 "public"
                      WHITESPACE@6..7 " "
                      FINAL_KW@7..12 "final"
                    WHITESPACE@12..13 " "
                    CLASS_KW@13..18 "class"
                    WHITESPACE@18..19 " "
                    IDENT@19..22 "Foo"
                    TYPE_PARAMS@22..25
                      LT@22..23 "<"
                      TYPE_PARAM@23..24
                        IDENT@23..24 "T"
                      GT@24..25 ">"
                    EXTENDS_CLAUSE@25..37
                      WHITESPACE@25..26 " "
                      EXTENDS_KW@26..33 "extends"
                      TYPE@33..37
                        WHITESPACE@33..34 " "
                        IDENT@34..37 "Bar"
                    IMPLEMENTS_CLAUSE@37..50
                      WHITESPACE@37..38 " "
                      IMPLEMENTS_KW@38..48 "implements"
                      TYPE@48..50
                        WHITESPACE@48..49 " "
                        IDENT@49..50 "I"
                    CLASS_BODY@50..103
                      WHITESPACE@50..51 " "
                      LBRACE@51..52 "{"
                      FIELD_DECL@52..73
                        MODIFIERS@52..62
                          NEWLINE@52..53 "\n"
                          WHITESPACE@53..55 "  "
                          PRIVATE_KW@55..62 "private"
                        TYPE@62..66
                          WHITESPACE@62..63 " "
                          INT_KW@63..66 "int"
                        WHITESPACE@66..67 " "
                        IDENT@67..68 "x"
                        WHITESPACE@68..69 " "
                        EQ@69..70 "="
                        LITERAL@70..72
                          WHITESPACE@70..71 " "
                          INT_LITERAL@71..72 "1"
                        SEMICOLON@72..73 ";"
                      METHOD_DECL@73..101
                        MODIFIERS@73..73
                        TYPE@73..80
                          NEWLINE@73..74 "\n"
                          WHITESPACE@74..76 "  "
                          VOID_KW@76..80 "void"
                        WHITESPACE@80..81 " "
                        IDENT@81..82 "m"
                        PARAM_LIST@82..89
                          LPAREN@82..83 "("
                          PARAM@83..88
                            MODIFIERS@83..83
                            TYPE@83..86
                              INT_KW@83..86 "int"
                            WHITESPACE@86..87 " "
                            IDENT@87..88 "a"
                          RPAREN@88..89 ")"
                        BLOCK@89..101
                          WHITESPACE@89..90 " "
                          LBRACE@90..91 "{"
                          RETURN_STMT@91..99
                            WHITESPACE@91..92 " "
                            RETURN_KW@92..98 "return"
                            SEMICOLON@98..99 ";"
                          WHITESPACE@99..100 " "
                          RBRACE@100..101 "}"
                      NEWLINE@101..102 "\n"
                      RBRACE@102..103 "}"
                  NEWLINE@103..104 "\n"
            "#]],
        );
    }

    #[test]
    fn generics_nested_close() {
        check(
            "class C { Map<K, List<V>> m; }",
            expect![[r#"
                SOURCE_FILE@0..30
                  CLASS_DECL@0..30
                    MODIFIERS@0..0
                    CLASS_KW@0..5 "class"
                    WHITESPACE@5..6 " "
                    IDENT@6..7 "C"
                    CLASS_BODY@7..30
                      WHITESPACE@7..8 " "
                      LBRACE@8..9 "{"
                      FIELD_DECL@9..28
                        MODIFIERS@9..9
                        TYPE@9..25
                          WHITESPACE@9..10 " "
                          IDENT@10..13 "Map"
                          TYPE_ARGS@13..25
                            LT@13..14 "<"
                            TYPE@14..15
                              IDENT@14..15 "K"
                            COMMA@15..16 ","
                            TYPE@16..24
                              WHITESPACE@16..17 " "
                              IDENT@17..21 "List"
                              TYPE_ARGS@21..24
                                LT@21..22 "<"
                                TYPE@22..23
                                  IDENT@22..23 "V"
                                GT@23..24 ">"
                            GT@24..25 ">"
                        WHITESPACE@25..26 " "
                        IDENT@26..27 "m"
                        SEMICOLON@27..28 ";"
                      WHITESPACE@28..29 " "
                      RBRACE@29..30 "}"
            "#]],
        );
    }

    #[test]
    fn expr_precedence_and_shifts() {
        check(
            "class C { int f() { return a + b * c >> d && e; } }",
            expect![[r#"
                SOURCE_FILE@0..51
                  CLASS_DECL@0..51
                    MODIFIERS@0..0
                    CLASS_KW@0..5 "class"
                    WHITESPACE@5..6 " "
                    IDENT@6..7 "C"
                    CLASS_BODY@7..51
                      WHITESPACE@7..8 " "
                      LBRACE@8..9 "{"
                      METHOD_DECL@9..49
                        MODIFIERS@9..9
                        TYPE@9..13
                          WHITESPACE@9..10 " "
                          INT_KW@10..13 "int"
                        WHITESPACE@13..14 " "
                        IDENT@14..15 "f"
                        PARAM_LIST@15..17
                          LPAREN@15..16 "("
                          RPAREN@16..17 ")"
                        BLOCK@17..49
                          WHITESPACE@17..18 " "
                          LBRACE@18..19 "{"
                          RETURN_STMT@19..47
                            WHITESPACE@19..20 " "
                            RETURN_KW@20..26 "return"
                            BINARY_EXPR@26..46
                              BINARY_EXPR@26..41
                                BINARY_EXPR@26..36
                                  NAME_REF@26..28
                                    WHITESPACE@26..27 " "
                                    IDENT@27..28 "a"
                                  WHITESPACE@28..29 " "
                                  PLUS@29..30 "+"
                                  BINARY_EXPR@30..36
                                    NAME_REF@30..32
                                      WHITESPACE@30..31 " "
                                      IDENT@31..32 "b"
                                    WHITESPACE@32..33 " "
                                    STAR@33..34 "*"
                                    NAME_REF@34..36
                                      WHITESPACE@34..35 " "
                                      IDENT@35..36 "c"
                                WHITESPACE@36..37 " "
                                GT@37..38 ">"
                                GT@38..39 ">"
                                NAME_REF@39..41
                                  WHITESPACE@39..40 " "
                                  IDENT@40..41 "d"
                              WHITESPACE@41..42 " "
                              AMP_AMP@42..44 "&&"
                              NAME_REF@44..46
                                WHITESPACE@44..45 " "
                                IDENT@45..46 "e"
                            SEMICOLON@46..47 ";"
                          WHITESPACE@47..48 " "
                          RBRACE@48..49 "}"
                      WHITESPACE@49..50 " "
                      RBRACE@50..51 "}"
            "#]],
        );
    }

    #[test]
    fn non_sealed_modifier() {
        check(
            "non-sealed class C { }",
            expect![[r#"
                SOURCE_FILE@0..22
                  CLASS_DECL@0..22
                    MODIFIERS@0..10
                      NON_SEALED_KW@0..10
                        IDENT@0..3 "non"
                        MINUS@3..4 "-"
                        IDENT@4..10 "sealed"
                    WHITESPACE@10..11 " "
                    CLASS_KW@11..16 "class"
                    WHITESPACE@16..17 " "
                    IDENT@17..18 "C"
                    CLASS_BODY@18..22
                      WHITESPACE@18..19 " "
                      LBRACE@19..20 "{"
                      WHITESPACE@20..21 " "
                      RBRACE@21..22 "}"
            "#]],
        );
    }

    #[test]
    fn error_recovery_in_member() {
        check(
            "class C { @ int ; void m() { } }",
            expect![[r#"
                SOURCE_FILE@0..32
                  CLASS_DECL@0..32
                    MODIFIERS@0..0
                    CLASS_KW@0..5 "class"
                    WHITESPACE@5..6 " "
                    IDENT@6..7 "C"
                    CLASS_BODY@7..32
                      WHITESPACE@7..8 " "
                      LBRACE@8..9 "{"
                      FIELD_DECL@9..17
                        MODIFIERS@9..11
                          ANNOTATION@9..11
                            WHITESPACE@9..10 " "
                            AT@10..11 "@"
                            QUALIFIED_NAME@11..11
                        TYPE@11..15
                          WHITESPACE@11..12 " "
                          INT_KW@12..15 "int"
                        WHITESPACE@15..16 " "
                        SEMICOLON@16..17 ";"
                      METHOD_DECL@17..30
                        MODIFIERS@17..17
                        TYPE@17..22
                          WHITESPACE@17..18 " "
                          VOID_KW@18..22 "void"
                        WHITESPACE@22..23 " "
                        IDENT@23..24 "m"
                        PARAM_LIST@24..26
                          LPAREN@24..25 "("
                          RPAREN@25..26 ")"
                        BLOCK@26..30
                          WHITESPACE@26..27 " "
                          LBRACE@27..28 "{"
                          WHITESPACE@28..29 " "
                          RBRACE@29..30 "}"
                      WHITESPACE@30..31 " "
                      RBRACE@31..32 "}"
                error 11..11: IDENT を期待しました
                error 15..15: IDENT を期待しました
            "#]],
        );
    }

    #[test]
    fn error_recovery_unclosed_block() {
        check(
            "class C { void m() { ",
            expect![[r#"
                SOURCE_FILE@0..21
                  CLASS_DECL@0..20
                    MODIFIERS@0..0
                    CLASS_KW@0..5 "class"
                    WHITESPACE@5..6 " "
                    IDENT@6..7 "C"
                    CLASS_BODY@7..20
                      WHITESPACE@7..8 " "
                      LBRACE@8..9 "{"
                      METHOD_DECL@9..20
                        MODIFIERS@9..9
                        TYPE@9..14
                          WHITESPACE@9..10 " "
                          VOID_KW@10..14 "void"
                        WHITESPACE@14..15 " "
                        IDENT@15..16 "m"
                        PARAM_LIST@16..18
                          LPAREN@16..17 "("
                          RPAREN@17..18 ")"
                        BLOCK@18..20
                          WHITESPACE@18..19 " "
                          LBRACE@19..20 "{"
                  WHITESPACE@20..21 " "
                error 20..20: RBRACE を期待しました
                error 20..20: RBRACE を期待しました
            "#]],
        );
    }

    #[test]
    fn instanceof_and_var_local() {
        check(
            "class C { void m() { var s = o; if (o instanceof String) return; } }",
            expect![[r#"
                SOURCE_FILE@0..68
                  CLASS_DECL@0..68
                    MODIFIERS@0..0
                    CLASS_KW@0..5 "class"
                    WHITESPACE@5..6 " "
                    IDENT@6..7 "C"
                    CLASS_BODY@7..68
                      WHITESPACE@7..8 " "
                      LBRACE@8..9 "{"
                      METHOD_DECL@9..66
                        MODIFIERS@9..9
                        TYPE@9..14
                          WHITESPACE@9..10 " "
                          VOID_KW@10..14 "void"
                        WHITESPACE@14..15 " "
                        IDENT@15..16 "m"
                        PARAM_LIST@16..18
                          LPAREN@16..17 "("
                          RPAREN@17..18 ")"
                        BLOCK@18..66
                          WHITESPACE@18..19 " "
                          LBRACE@19..20 "{"
                          LOCAL_VAR_DECL@20..31
                            TYPE@20..24
                              WHITESPACE@20..21 " "
                              VAR_KW@21..24 "var"
                            WHITESPACE@24..25 " "
                            IDENT@25..26 "s"
                            WHITESPACE@26..27 " "
                            EQ@27..28 "="
                            NAME_REF@28..30
                              WHITESPACE@28..29 " "
                              IDENT@29..30 "o"
                            SEMICOLON@30..31 ";"
                          IF_STMT@31..64
                            WHITESPACE@31..32 " "
                            IF_KW@32..34 "if"
                            WHITESPACE@34..35 " "
                            LPAREN@35..36 "("
                            BINARY_EXPR@36..55
                              NAME_REF@36..37
                                IDENT@36..37 "o"
                              WHITESPACE@37..38 " "
                              INSTANCEOF_KW@38..48 "instanceof"
                              TYPE@48..55
                                WHITESPACE@48..49 " "
                                IDENT@49..55 "String"
                            RPAREN@55..56 ")"
                            RETURN_STMT@56..64
                              WHITESPACE@56..57 " "
                              RETURN_KW@57..63 "return"
                              SEMICOLON@63..64 ";"
                          WHITESPACE@64..65 " "
                          RBRACE@65..66 "}"
                      WHITESPACE@66..67 " "
                      RBRACE@67..68 "}"
            "#]],
        );
    }

    #[test]
    fn lossless_on_various_inputs() {
        for src in [
            "",
            "   ",
            "\n\n",
            "// only a comment",
            "package a.b.c;",
            "int x = 0xFF; /* c */ var y = \"\"\"\nt\n\"\"\";",
            "@#$%^",
            "class\u{00A0}名前 { }",
            "class C{int a=b>>>c;var d=e<<2;}",
            "class C { void m() throws E, F { x[0].y(1, 2); } }",
            "class C { C() { this.x = new int[3]; } }",
        ] {
            assert_lossless(src);
        }
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// 任意の UTF-8 文字列でも panic せず、構文木のテキストが入力に一致する(lossless)。
        #[test]
        fn parse_is_lossless_and_never_panics(src in any::<String>()) {
            let parse = parse(&src);
            prop_assert_eq!(parse.syntax().text().to_string(), src);
        }

        /// Java らしいトークンを並べた入力でも lossless かつ非 panic。
        /// (ASCII 記号と識別子・キーワードを混ぜ、文法経路をより広く踏む。)
        #[test]
        fn parse_is_lossless_on_javaish(
            src in proptest::collection::vec(
                prop_oneof![
                    Just("class"), Just("void"), Just("int"), Just("return"), Just("if"),
                    Just("var"), Just("new"), Just("instanceof"), Just("non-sealed"),
                    Just("x"), Just("Foo"), Just("0"), Just("\"s\""),
                    Just("{"), Just("}"), Just("("), Just(")"), Just("<"), Just(">"),
                    Just(";"), Just(","), Just("."), Just("="), Just("+"), Just(">>"),
                    Just(" "), Just("\n"),
                ],
                0..40,
            ).prop_map(|parts| parts.concat())
        ) {
            let parse = parse(&src);
            prop_assert_eq!(parse.syntax().text().to_string(), src);
        }
    }
}
