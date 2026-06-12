//! Java 26 の終端トークン種別。`logos` で字句解析する。
//!
//! ここでは終端トークンのみを定義する(属性なし variant を持たない方針)。ノードや
//! `ERROR`/`EOF` などのセンチネルは [`crate::SyntaxKind`] が持つ。
//!
//! 設計上の要点:
//! - トリビア(空白・改行・コメント)も実トークンとして出す(`#[logos(skip)]` は使わない)。
//!   これにより字句解析が lossless になる。
//! - 文脈依存キーワード(`var` / `record` / `sealed` / `when` / モジュール系など)は
//!   [`IDENT`](TokenKind::IDENT) として字句化し、昇格は parser に委ねる。
//! - ジェネリクス(`Map<K, List<V>>`)のため `>` 系は単一 [`GT`](TokenKind::GT) のみを出し、
//!   `>=` / `>>` / `>>>` / `>>=` / `>>>=` は parser が隣接 `GT` から合成する。`<<` / `<<=` は
//!   ジェネリクス開きと曖昧でないため貪欲に字句化する。
//! - リテラルは生スライスのまま保持する(エスケープ解釈は後段の意味処理)。

use logos::Logos;

/// `logos` が生成する Java 26 の終端トークン。
#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum TokenKind {
    // ===== トリビア =====
    /// 空白(空白・タブ・フォームフィード。改行は含まない)。
    #[regex(r"[ \t\f]+")]
    WHITESPACE,
    /// 改行(CRLF は 1 トークン)。
    #[regex(r"\r\n|\r|\n")]
    NEWLINE,
    /// 行コメント `// ...`。改行の手前まで。
    /// 注: `//[^\r\n]*` は logos の貪欲ドットガードに抵触するためコールバックで実装する。
    #[token("//", lex_line_comment)]
    LINE_COMMENT,
    /// ブロックコメント `/* ... */`(Javadoc `/** ... */` も含む。区別は
    /// [`SyntaxKind::from_token`](crate::SyntaxKind::from_token) が行う)。
    #[token("/*", lex_block_comment)]
    BLOCK_COMMENT,

    // ===== 識別子 =====
    /// 識別子。完全 Unicode(`Character.isJavaIdentifierStart/Part` 準拠)。
    /// 文脈依存キーワードもここに含まれ、parser が昇格する。
    #[regex(
        r"[\p{L}\p{Nl}\p{Sc}\p{Pc}][\p{L}\p{Nl}\p{Sc}\p{Pc}\p{Nd}\p{Mn}\p{Mc}\p{Cf}]*",
        priority = 1
    )]
    IDENT,
    /// `_`。Java 9 以降の予約語、21 以降の unnamed variable / pattern。
    #[token("_")]
    UNDERSCORE,

    // ===== 予約語(50) =====
    #[token("abstract")]
    ABSTRACT_KW,
    #[token("assert")]
    ASSERT_KW,
    #[token("boolean")]
    BOOLEAN_KW,
    #[token("break")]
    BREAK_KW,
    #[token("byte")]
    BYTE_KW,
    #[token("case")]
    CASE_KW,
    #[token("catch")]
    CATCH_KW,
    #[token("char")]
    CHAR_KW,
    #[token("class")]
    CLASS_KW,
    #[token("const")]
    CONST_KW,
    #[token("continue")]
    CONTINUE_KW,
    #[token("default")]
    DEFAULT_KW,
    #[token("do")]
    DO_KW,
    #[token("double")]
    DOUBLE_KW,
    #[token("else")]
    ELSE_KW,
    #[token("enum")]
    ENUM_KW,
    #[token("extends")]
    EXTENDS_KW,
    #[token("final")]
    FINAL_KW,
    #[token("finally")]
    FINALLY_KW,
    #[token("float")]
    FLOAT_KW,
    #[token("for")]
    FOR_KW,
    #[token("goto")]
    GOTO_KW,
    #[token("if")]
    IF_KW,
    #[token("implements")]
    IMPLEMENTS_KW,
    #[token("import")]
    IMPORT_KW,
    #[token("instanceof")]
    INSTANCEOF_KW,
    #[token("int")]
    INT_KW,
    #[token("interface")]
    INTERFACE_KW,
    #[token("long")]
    LONG_KW,
    #[token("native")]
    NATIVE_KW,
    #[token("new")]
    NEW_KW,
    #[token("package")]
    PACKAGE_KW,
    #[token("private")]
    PRIVATE_KW,
    #[token("protected")]
    PROTECTED_KW,
    #[token("public")]
    PUBLIC_KW,
    #[token("return")]
    RETURN_KW,
    #[token("short")]
    SHORT_KW,
    #[token("static")]
    STATIC_KW,
    #[token("strictfp")]
    STRICTFP_KW,
    #[token("super")]
    SUPER_KW,
    #[token("switch")]
    SWITCH_KW,
    #[token("synchronized")]
    SYNCHRONIZED_KW,
    #[token("this")]
    THIS_KW,
    #[token("throw")]
    THROW_KW,
    #[token("throws")]
    THROWS_KW,
    #[token("transient")]
    TRANSIENT_KW,
    #[token("try")]
    TRY_KW,
    #[token("void")]
    VOID_KW,
    #[token("volatile")]
    VOLATILE_KW,
    #[token("while")]
    WHILE_KW,

    // ===== リテラルキーワード(JLS 上はリテラルだが種別を分ける) =====
    #[token("true")]
    TRUE_KW,
    #[token("false")]
    FALSE_KW,
    #[token("null")]
    NULL_KW,

    // ===== リテラル =====
    /// 整数リテラル(10 進 / 16 進 / 8 進 / 2 進、桁間 `_`、`l`/`L` 接尾辞)。
    /// 基数の再分類は後段で行う。
    #[regex(r"0[lL]?")]
    #[regex(r"[1-9](_*[0-9])*[lL]?")]
    #[regex(r"0[xX][0-9A-Fa-f](_*[0-9A-Fa-f])*[lL]?")]
    #[regex(r"0[bB][01](_*[01])*[lL]?")]
    #[regex(r"0(_*[0-7])+[lL]?")]
    INT_LITERAL,
    /// 浮動小数点リテラル(10 進 / 16 進 float、指数、`f`/`F`/`d`/`D` 接尾辞)。
    #[regex(r"[0-9](_*[0-9])*\.([0-9](_*[0-9])*)?([eE][+-]?[0-9](_*[0-9])*)?[fFdD]?")]
    #[regex(r"\.[0-9](_*[0-9])*([eE][+-]?[0-9](_*[0-9])*)?[fFdD]?")]
    #[regex(r"[0-9](_*[0-9])*[eE][+-]?[0-9](_*[0-9])*[fFdD]?")]
    #[regex(r"[0-9](_*[0-9])*[fFdD]")]
    #[regex(r"0[xX][0-9A-Fa-f](_*[0-9A-Fa-f])*\.?[pP][+-]?[0-9](_*[0-9])*[fFdD]?")]
    #[regex(
        r"0[xX]([0-9A-Fa-f](_*[0-9A-Fa-f])*)?\.[0-9A-Fa-f](_*[0-9A-Fa-f])*[pP][+-]?[0-9](_*[0-9])*[fFdD]?"
    )]
    FLOAT_LITERAL,
    /// 文字リテラル `'x'`(生スライス保持)。
    #[regex(r"'([^'\\\r\n]|\\.)'")]
    CHAR_LITERAL,
    /// 文字列リテラル `"..."`(生スライス保持)。
    #[regex(r#""([^"\\\r\n]|\\.)*""#)]
    STRING_LITERAL,
    /// テキストブロック `""" ... """`(Java 15+、複数行。生スライス保持)。
    #[token("\"\"\"", lex_text_block)]
    TEXT_BLOCK,

    // ===== 区切り子 =====
    #[token("(")]
    LPAREN,
    #[token(")")]
    RPAREN,
    #[token("{")]
    LBRACE,
    #[token("}")]
    RBRACE,
    #[token("[")]
    LBRACK,
    #[token("]")]
    RBRACK,
    #[token(";")]
    SEMICOLON,
    #[token(",")]
    COMMA,
    #[token(".")]
    DOT,
    #[token("...")]
    ELLIPSIS,
    #[token("@")]
    AT,
    #[token("::")]
    COLON_COLON,

    // ===== 演算子 =====
    #[token("=")]
    EQ,
    #[token("<")]
    LT,
    /// `>`。lexer が出す唯一の `>` 系(`>=` / `>>` 等は parser が合成する)。
    #[token(">")]
    GT,
    #[token("!")]
    BANG,
    #[token("~")]
    TILDE,
    #[token("?")]
    QUESTION,
    #[token(":")]
    COLON,
    #[token("->")]
    ARROW,
    #[token("==")]
    EQ_EQ,
    #[token("<=")]
    LT_EQ,
    #[token("!=")]
    BANG_EQ,
    #[token("&&")]
    AMP_AMP,
    #[token("||")]
    PIPE_PIPE,
    #[token("++")]
    PLUS_PLUS,
    #[token("--")]
    MINUS_MINUS,
    #[token("+")]
    PLUS,
    #[token("-")]
    MINUS,
    #[token("*")]
    STAR,
    #[token("/")]
    SLASH,
    #[token("&")]
    AMP,
    #[token("|")]
    PIPE,
    #[token("^")]
    CARET,
    #[token("%")]
    PERCENT,
    #[token("<<")]
    LSHIFT,
    #[token("<<=")]
    LSHIFT_EQ,
    #[token("+=")]
    PLUS_EQ,
    #[token("-=")]
    MINUS_EQ,
    #[token("*=")]
    STAR_EQ,
    #[token("/=")]
    SLASH_EQ,
    #[token("&=")]
    AMP_EQ,
    #[token("|=")]
    PIPE_EQ,
    #[token("^=")]
    CARET_EQ,
    #[token("%=")]
    PERCENT_EQ,
}

/// Maps reserved words (the 50 keywords plus the `true` / `false` / `null` literal
/// keywords) to their token kinds. Contextual keywords (`var`, `record`, `sealed`, ...)
/// are not included: they lex as [`IDENT`](TokenKind::IDENT) and the parser promotes them.
pub(crate) fn keyword_kind(text: &str) -> Option<TokenKind> {
    use TokenKind::*;
    let kind = match text {
        "abstract" => ABSTRACT_KW,
        "assert" => ASSERT_KW,
        "boolean" => BOOLEAN_KW,
        "break" => BREAK_KW,
        "byte" => BYTE_KW,
        "case" => CASE_KW,
        "catch" => CATCH_KW,
        "char" => CHAR_KW,
        "class" => CLASS_KW,
        "const" => CONST_KW,
        "continue" => CONTINUE_KW,
        "default" => DEFAULT_KW,
        "do" => DO_KW,
        "double" => DOUBLE_KW,
        "else" => ELSE_KW,
        "enum" => ENUM_KW,
        "extends" => EXTENDS_KW,
        "final" => FINAL_KW,
        "finally" => FINALLY_KW,
        "float" => FLOAT_KW,
        "for" => FOR_KW,
        "goto" => GOTO_KW,
        "if" => IF_KW,
        "implements" => IMPLEMENTS_KW,
        "import" => IMPORT_KW,
        "instanceof" => INSTANCEOF_KW,
        "int" => INT_KW,
        "interface" => INTERFACE_KW,
        "long" => LONG_KW,
        "native" => NATIVE_KW,
        "new" => NEW_KW,
        "package" => PACKAGE_KW,
        "private" => PRIVATE_KW,
        "protected" => PROTECTED_KW,
        "public" => PUBLIC_KW,
        "return" => RETURN_KW,
        "short" => SHORT_KW,
        "static" => STATIC_KW,
        "strictfp" => STRICTFP_KW,
        "super" => SUPER_KW,
        "switch" => SWITCH_KW,
        "synchronized" => SYNCHRONIZED_KW,
        "this" => THIS_KW,
        "throw" => THROW_KW,
        "throws" => THROWS_KW,
        "transient" => TRANSIENT_KW,
        "try" => TRY_KW,
        "void" => VOID_KW,
        "volatile" => VOLATILE_KW,
        "while" => WHILE_KW,
        "true" => TRUE_KW,
        "false" => FALSE_KW,
        "null" => NULL_KW,
        _ => return None,
    };
    Some(kind)
}

/// `//` 以降を改行(`\r` または `\n`)の手前まで消費する。改行自体は `NEWLINE` トークンに残す。
fn lex_line_comment(lex: &mut logos::Lexer<TokenKind>) {
    let rest = lex.remainder();
    let len = rest.find(['\r', '\n']).unwrap_or(rest.len());
    lex.bump(len);
}

/// `/*` 以降を `*/`(無ければ EOF)まで消費する。戻り値 `()` で `BLOCK_COMMENT` を発行。
fn lex_block_comment(lex: &mut logos::Lexer<TokenKind>) {
    let rest = lex.remainder();
    match rest.find("*/") {
        Some(i) => lex.bump(i + 2),
        None => lex.bump(rest.len()),
    }
}

/// `"""` 以降を、未エスケープの閉じ `"""`(無ければ EOF)まで消費する。
fn lex_text_block(lex: &mut logos::Lexer<TokenKind>) {
    let rest = lex.remainder();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        if &bytes[i..i + 3] == b"\"\"\"" {
            // 直前の連続バックスラッシュ数が偶数なら、この `"""` は閉じデリミタ。
            let mut backslashes = 0usize;
            let mut j = i;
            while j > 0 && bytes[j - 1] == b'\\' {
                backslashes += 1;
                j -= 1;
            }
            if backslashes.is_multiple_of(2) {
                lex.bump(i + 3);
                return;
            }
        }
        i += 1;
    }
    lex.bump(rest.len());
}
