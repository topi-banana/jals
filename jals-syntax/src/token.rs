//! Terminal token kinds for Java 26, produced by the hand-written lexer.
//!
//! Only terminal tokens are defined here (no payload-carrying variants). Node kinds and
//! sentinels such as `ERROR`/`EOF` live in [`crate::SyntaxKind`].
//!
//! Design notes:
//! - Trivia (whitespace, newlines, comments) are real tokens. This keeps lexing lossless.
//! - Context-sensitive keywords (`var` / `record` / `sealed` / `when` / module words, ...)
//!   are lexed as [`IDENT`](TokenKind::IDENT); promotion is the parser's job.
//! - For generics (`Map<K, List<V>>`) the lexer only ever emits a single
//!   [`GT`](TokenKind::GT); the parser fuses adjacent `GT`s into `>=` / `>>` / `>>>` /
//!   `>>=` / `>>>=`. `<<` / `<<=` are not ambiguous with a generics opener and lex
//!   greedily.
//! - Literals keep their raw slices (escape interpretation is later semantic work).

/// A Java 26 terminal token, as produced by [`crate::lexer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum TokenKind {
    // ===== Trivia =====
    /// Whitespace (spaces, tabs, form feeds; never line breaks).
    WHITESPACE,
    /// Line break (CRLF is one token).
    NEWLINE,
    /// Line comment `// ...`, up to (not including) the line break.
    LINE_COMMENT,
    /// Block comment `/* ... */` (including Javadoc `/** ... */`; the distinction is made
    /// by [`SyntaxKind::from_token`](crate::SyntaxKind::from_token)).
    BLOCK_COMMENT,

    // ===== Identifiers =====
    /// Identifier. Full Unicode (per `Character.isJavaIdentifierStart/Part`).
    /// Context-sensitive keywords are also lexed as this; the parser promotes them.
    IDENT,
    /// `_`. Reserved since Java 9; unnamed variables / patterns since Java 21.
    UNDERSCORE,

    // ===== Reserved keywords (50) =====
    ABSTRACT_KW,
    ASSERT_KW,
    BOOLEAN_KW,
    BREAK_KW,
    BYTE_KW,
    CASE_KW,
    CATCH_KW,
    CHAR_KW,
    CLASS_KW,
    CONST_KW,
    CONTINUE_KW,
    DEFAULT_KW,
    DO_KW,
    DOUBLE_KW,
    ELSE_KW,
    ENUM_KW,
    EXTENDS_KW,
    FINAL_KW,
    FINALLY_KW,
    FLOAT_KW,
    FOR_KW,
    GOTO_KW,
    IF_KW,
    IMPLEMENTS_KW,
    IMPORT_KW,
    INSTANCEOF_KW,
    INT_KW,
    INTERFACE_KW,
    LONG_KW,
    NATIVE_KW,
    NEW_KW,
    PACKAGE_KW,
    PRIVATE_KW,
    PROTECTED_KW,
    PUBLIC_KW,
    RETURN_KW,
    SHORT_KW,
    STATIC_KW,
    STRICTFP_KW,
    SUPER_KW,
    SWITCH_KW,
    SYNCHRONIZED_KW,
    THIS_KW,
    THROW_KW,
    THROWS_KW,
    TRANSIENT_KW,
    TRY_KW,
    VOID_KW,
    VOLATILE_KW,
    WHILE_KW,

    // ===== Literal keywords (literals per the JLS, but kept as distinct kinds) =====
    TRUE_KW,
    FALSE_KW,
    NULL_KW,

    // ===== Literals =====
    /// Integer literal (decimal / hex / octal / binary, `_` separators, `l`/`L` suffix).
    /// Radix reclassification happens later.
    INT_LITERAL,
    /// Floating-point literal (decimal / hex float, exponents, `f`/`F`/`d`/`D` suffix).
    FLOAT_LITERAL,
    /// Char literal `'x'` (raw slice kept).
    CHAR_LITERAL,
    /// String literal `"..."` (raw slice kept).
    STRING_LITERAL,
    /// Text block `""" ... """` (Java 15+, multi-line; raw slice kept).
    TEXT_BLOCK,

    // ===== Delimiters =====
    LPAREN,
    RPAREN,
    LBRACE,
    RBRACE,
    LBRACK,
    RBRACK,
    SEMICOLON,
    COMMA,
    DOT,
    ELLIPSIS,
    AT,
    COLON_COLON,

    // ===== Operators =====
    EQ,
    LT,
    /// `>`. The only `>`-family token the lexer emits (the parser fuses `>=` / `>>` / ...).
    GT,
    BANG,
    TILDE,
    QUESTION,
    COLON,
    ARROW,
    EQ_EQ,
    LT_EQ,
    BANG_EQ,
    AMP_AMP,
    PIPE_PIPE,
    PLUS_PLUS,
    MINUS_MINUS,
    PLUS,
    MINUS,
    STAR,
    SLASH,
    AMP,
    PIPE,
    CARET,
    PERCENT,
    LSHIFT,
    LSHIFT_EQ,
    PLUS_EQ,
    MINUS_EQ,
    STAR_EQ,
    SLASH_EQ,
    AMP_EQ,
    PIPE_EQ,
    CARET_EQ,
    PERCENT_EQ,
}

/// Maps reserved words (the 50 keywords plus the `true` / `false` / `null` literal
/// keywords) to their token kinds. Contextual keywords (`var`, `record`, `sealed`, ...)
/// are not included: they lex as [`IDENT`](TokenKind::IDENT) and the parser promotes them.
pub(crate) fn keyword_kind(text: &str) -> Option<TokenKind> {
    use TokenKind::{
        ABSTRACT_KW, ASSERT_KW, BOOLEAN_KW, BREAK_KW, BYTE_KW, CASE_KW, CATCH_KW, CHAR_KW,
        CLASS_KW, CONST_KW, CONTINUE_KW, DEFAULT_KW, DO_KW, DOUBLE_KW, ELSE_KW, ENUM_KW,
        EXTENDS_KW, FALSE_KW, FINAL_KW, FINALLY_KW, FLOAT_KW, FOR_KW, GOTO_KW, IF_KW,
        IMPLEMENTS_KW, IMPORT_KW, INSTANCEOF_KW, INT_KW, INTERFACE_KW, LONG_KW, NATIVE_KW, NEW_KW,
        NULL_KW, PACKAGE_KW, PRIVATE_KW, PROTECTED_KW, PUBLIC_KW, RETURN_KW, SHORT_KW, STATIC_KW,
        STRICTFP_KW, SUPER_KW, SWITCH_KW, SYNCHRONIZED_KW, THIS_KW, THROW_KW, THROWS_KW,
        TRANSIENT_KW, TRUE_KW, TRY_KW, VOID_KW, VOLATILE_KW, WHILE_KW,
    };
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
