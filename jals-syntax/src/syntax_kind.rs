//! Node/token kinds for the syntax tree. A unified kind that maps to `rowan`'s `SyntaxKind`(u16).
//!
//! Milestone A contains only terminal token kinds, `DOC_COMMENT` (Javadoc), and `ERROR`
//! (a catch-all for unmatched bytes). Node kinds and `EOF` are added in Milestone B,
//! where the parser builds the tree (to avoid `dead_code` from unconstructed variants).

use num_derive::{FromPrimitive, ToPrimitive};

use crate::token::TokenKind;

/// Lexical and syntactic kind. Converted to `rowan`'s `SyntaxKind`(u16) via `num-derive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, FromPrimitive, ToPrimitive)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum SyntaxKind {
    // ===== Trivia =====
    WHITESPACE,
    NEWLINE,
    LINE_COMMENT,
    BLOCK_COMMENT,
    /// Javadoc comment `/** ... */` (excluding `/**/`).
    DOC_COMMENT,

    // ===== Identifiers =====
    IDENT,
    UNDERSCORE,

    // ===== Keywords (50) =====
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

    // ===== Literal keywords =====
    TRUE_KW,
    FALSE_KW,
    NULL_KW,

    // ===== Literals =====
    INT_LITERAL,
    FLOAT_LITERAL,
    CHAR_LITERAL,
    STRING_LITERAL,
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

    // ===== Sentinels =====
    /// Unmatched bytes from lexing (a catch-all to preserve losslessness).
    /// Also used as a node kind in the parser to wrap unexpected tokens.
    ERROR,
    /// End of input. An internal sentinel in the parser; does not appear in the syntax tree.
    EOF,

    // ===== Promoted keywords (lexed as IDENT, reclassified by context in the parser) =====
    VAR_KW,
    YIELD_KW,
    RECORD_KW,
    SEALED_KW,
    PERMITS_KW,
    WHEN_KW,
    MODULE_KW,
    OPEN_KW,
    OPENS_KW,
    REQUIRES_KW,
    TRANSITIVE_KW,
    EXPORTS_KW,
    TO_KW,
    PROVIDES_KW,
    USES_KW,
    WITH_KW,

    // ===== Nodes =====
    /// Compilation unit (the whole file).
    SOURCE_FILE,
    PACKAGE_DECL,
    IMPORT_DECL,
    /// Dotted name (`a.b.c`, including `a.b.*` in imports).
    QUALIFIED_NAME,
    CLASS_DECL,
    MODIFIERS,
    ANNOTATION,
    TYPE_PARAMS,
    TYPE_PARAM,
    EXTENDS_CLAUSE,
    IMPLEMENTS_CLAUSE,
    PERMITS_CLAUSE,
    THROWS_CLAUSE,
    /// `non-sealed` (a node that re-joins `IDENT("non") MINUS IDENT("sealed")`).
    NON_SEALED_KW,
    CLASS_BODY,
    FIELD_DECL,
    METHOD_DECL,
    CONSTRUCTOR_DECL,
    PARAM_LIST,
    PARAM,
    BLOCK,
    LOCAL_VAR_DECL,
    EXPR_STMT,
    RETURN_STMT,
    IF_STMT,
    WHILE_STMT,
    TYPE,
    TYPE_ARGS,
    /// Name reference in an expression (single identifier, `this`, or `super`).
    NAME_REF,
    LITERAL,
    BINARY_EXPR,
    UNARY_EXPR,
    POSTFIX_EXPR,
    PAREN_EXPR,
    CALL_EXPR,
    FIELD_ACCESS,
    INDEX_EXPR,
    NEW_EXPR,
    ARG_LIST,

    // ===== Nodes (Milestone B extensions) =====
    // --- Type declarations and members ---
    INTERFACE_DECL,
    ENUM_DECL,
    RECORD_DECL,
    /// `@interface` (annotation type declaration).
    ANNOTATION_TYPE_DECL,
    /// enum body (constants + optional members).
    ENUM_BODY,
    /// enum constant (`NAME(args) { body }`).
    ENUM_CONSTANT,
    /// record header (`(components)`).
    RECORD_HEADER,
    RECORD_COMPONENT,
    /// Initializer block (`{ ... }` / `static { ... }`).
    INITIALIZER,
    /// Default value of an annotation element (`default value`).
    ANNOTATION_DEFAULT,

    // --- Annotation arguments ---
    /// Argument list of an annotation use (`(...)`).
    ANNOTATION_ARG_LIST,
    /// Element = value pair (`name = value`).
    ANNOTATION_PAIR,

    // --- Statements ---
    FOR_STMT,
    FOR_EACH_STMT,
    DO_WHILE_STMT,
    BREAK_STMT,
    CONTINUE_STMT,
    THROW_STMT,
    YIELD_STMT,
    ASSERT_STMT,
    SYNCHRONIZED_STMT,
    TRY_STMT,
    /// The resource list of a try-with-resources (`(...)`).
    RESOURCE_LIST,
    RESOURCE,
    CATCH_CLAUSE,
    FINALLY_CLAUSE,
    SWITCH_STMT,
    /// switch body (`{ ... }`).
    SWITCH_BLOCK,
    /// Arrow-form rule (`case L -> ...`).
    SWITCH_RULE,
    /// Colon-form group (`case L: stmts`).
    SWITCH_GROUP,
    /// `case ...` / `default` label.
    SWITCH_LABEL,
    /// Labeled statement (`label: stmt`).
    LABELED_STMT,
    /// Empty statement (`;`).
    EMPTY_STMT,

    // --- Expressions ---
    ASSIGNMENT_EXPR,
    /// Ternary conditional expression (`c ? a : b`).
    TERNARY_EXPR,
    LAMBDA_EXPR,
    LAMBDA_PARAMS,
    /// Method reference (`Type::method` / `expr::method` / `Type::new`).
    METHOD_REF_EXPR,
    CAST_EXPR,
    /// Array initializer (`{ a, b, c }`).
    ARRAY_INIT,
    SWITCH_EXPR,
    /// Class literal (`Type.class`).
    CLASS_LITERAL,

    // --- Patterns ---
    /// Type pattern (`Type id`).
    TYPE_PATTERN,
    /// Record pattern (`Type(subpatterns)`).
    RECORD_PATTERN,
    /// Pattern guard (`when expr`).
    GUARD,
}

impl SyntaxKind {
    /// Determines a `SyntaxKind` from a `TokenKind` and the original source text.
    ///
    /// `BLOCK_COMMENT` tokens that are Javadoc comments (starting with `/**` but not `/**/`)
    /// are classified as `DOC_COMMENT`. Everything else behaves the same as [`From<TokenKind>`].
    pub(crate) fn from_token(token: TokenKind, text: &str) -> SyntaxKind {
        match token {
            TokenKind::BLOCK_COMMENT if is_doc_comment(text) => SyntaxKind::DOC_COMMENT,
            other => SyntaxKind::from(other),
        }
    }

    /// Returns whether this kind is trivia (whitespace, newlines, or comments).
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::WHITESPACE
                | SyntaxKind::NEWLINE
                | SyntaxKind::LINE_COMMENT
                | SyntaxKind::BLOCK_COMMENT
                | SyntaxKind::DOC_COMMENT
        )
    }
}

impl From<TokenKind> for SyntaxKind {
    fn from(token: TokenKind) -> SyntaxKind {
        match token {
            TokenKind::WHITESPACE => SyntaxKind::WHITESPACE,
            TokenKind::NEWLINE => SyntaxKind::NEWLINE,
            TokenKind::LINE_COMMENT => SyntaxKind::LINE_COMMENT,
            TokenKind::BLOCK_COMMENT => SyntaxKind::BLOCK_COMMENT,
            TokenKind::IDENT => SyntaxKind::IDENT,
            TokenKind::UNDERSCORE => SyntaxKind::UNDERSCORE,
            TokenKind::ABSTRACT_KW => SyntaxKind::ABSTRACT_KW,
            TokenKind::ASSERT_KW => SyntaxKind::ASSERT_KW,
            TokenKind::BOOLEAN_KW => SyntaxKind::BOOLEAN_KW,
            TokenKind::BREAK_KW => SyntaxKind::BREAK_KW,
            TokenKind::BYTE_KW => SyntaxKind::BYTE_KW,
            TokenKind::CASE_KW => SyntaxKind::CASE_KW,
            TokenKind::CATCH_KW => SyntaxKind::CATCH_KW,
            TokenKind::CHAR_KW => SyntaxKind::CHAR_KW,
            TokenKind::CLASS_KW => SyntaxKind::CLASS_KW,
            TokenKind::CONST_KW => SyntaxKind::CONST_KW,
            TokenKind::CONTINUE_KW => SyntaxKind::CONTINUE_KW,
            TokenKind::DEFAULT_KW => SyntaxKind::DEFAULT_KW,
            TokenKind::DO_KW => SyntaxKind::DO_KW,
            TokenKind::DOUBLE_KW => SyntaxKind::DOUBLE_KW,
            TokenKind::ELSE_KW => SyntaxKind::ELSE_KW,
            TokenKind::ENUM_KW => SyntaxKind::ENUM_KW,
            TokenKind::EXTENDS_KW => SyntaxKind::EXTENDS_KW,
            TokenKind::FINAL_KW => SyntaxKind::FINAL_KW,
            TokenKind::FINALLY_KW => SyntaxKind::FINALLY_KW,
            TokenKind::FLOAT_KW => SyntaxKind::FLOAT_KW,
            TokenKind::FOR_KW => SyntaxKind::FOR_KW,
            TokenKind::GOTO_KW => SyntaxKind::GOTO_KW,
            TokenKind::IF_KW => SyntaxKind::IF_KW,
            TokenKind::IMPLEMENTS_KW => SyntaxKind::IMPLEMENTS_KW,
            TokenKind::IMPORT_KW => SyntaxKind::IMPORT_KW,
            TokenKind::INSTANCEOF_KW => SyntaxKind::INSTANCEOF_KW,
            TokenKind::INT_KW => SyntaxKind::INT_KW,
            TokenKind::INTERFACE_KW => SyntaxKind::INTERFACE_KW,
            TokenKind::LONG_KW => SyntaxKind::LONG_KW,
            TokenKind::NATIVE_KW => SyntaxKind::NATIVE_KW,
            TokenKind::NEW_KW => SyntaxKind::NEW_KW,
            TokenKind::PACKAGE_KW => SyntaxKind::PACKAGE_KW,
            TokenKind::PRIVATE_KW => SyntaxKind::PRIVATE_KW,
            TokenKind::PROTECTED_KW => SyntaxKind::PROTECTED_KW,
            TokenKind::PUBLIC_KW => SyntaxKind::PUBLIC_KW,
            TokenKind::RETURN_KW => SyntaxKind::RETURN_KW,
            TokenKind::SHORT_KW => SyntaxKind::SHORT_KW,
            TokenKind::STATIC_KW => SyntaxKind::STATIC_KW,
            TokenKind::STRICTFP_KW => SyntaxKind::STRICTFP_KW,
            TokenKind::SUPER_KW => SyntaxKind::SUPER_KW,
            TokenKind::SWITCH_KW => SyntaxKind::SWITCH_KW,
            TokenKind::SYNCHRONIZED_KW => SyntaxKind::SYNCHRONIZED_KW,
            TokenKind::THIS_KW => SyntaxKind::THIS_KW,
            TokenKind::THROW_KW => SyntaxKind::THROW_KW,
            TokenKind::THROWS_KW => SyntaxKind::THROWS_KW,
            TokenKind::TRANSIENT_KW => SyntaxKind::TRANSIENT_KW,
            TokenKind::TRY_KW => SyntaxKind::TRY_KW,
            TokenKind::VOID_KW => SyntaxKind::VOID_KW,
            TokenKind::VOLATILE_KW => SyntaxKind::VOLATILE_KW,
            TokenKind::WHILE_KW => SyntaxKind::WHILE_KW,
            TokenKind::TRUE_KW => SyntaxKind::TRUE_KW,
            TokenKind::FALSE_KW => SyntaxKind::FALSE_KW,
            TokenKind::NULL_KW => SyntaxKind::NULL_KW,
            TokenKind::INT_LITERAL => SyntaxKind::INT_LITERAL,
            TokenKind::FLOAT_LITERAL => SyntaxKind::FLOAT_LITERAL,
            TokenKind::CHAR_LITERAL => SyntaxKind::CHAR_LITERAL,
            TokenKind::STRING_LITERAL => SyntaxKind::STRING_LITERAL,
            TokenKind::TEXT_BLOCK => SyntaxKind::TEXT_BLOCK,
            TokenKind::LPAREN => SyntaxKind::LPAREN,
            TokenKind::RPAREN => SyntaxKind::RPAREN,
            TokenKind::LBRACE => SyntaxKind::LBRACE,
            TokenKind::RBRACE => SyntaxKind::RBRACE,
            TokenKind::LBRACK => SyntaxKind::LBRACK,
            TokenKind::RBRACK => SyntaxKind::RBRACK,
            TokenKind::SEMICOLON => SyntaxKind::SEMICOLON,
            TokenKind::COMMA => SyntaxKind::COMMA,
            TokenKind::DOT => SyntaxKind::DOT,
            TokenKind::ELLIPSIS => SyntaxKind::ELLIPSIS,
            TokenKind::AT => SyntaxKind::AT,
            TokenKind::COLON_COLON => SyntaxKind::COLON_COLON,
            TokenKind::EQ => SyntaxKind::EQ,
            TokenKind::LT => SyntaxKind::LT,
            TokenKind::GT => SyntaxKind::GT,
            TokenKind::BANG => SyntaxKind::BANG,
            TokenKind::TILDE => SyntaxKind::TILDE,
            TokenKind::QUESTION => SyntaxKind::QUESTION,
            TokenKind::COLON => SyntaxKind::COLON,
            TokenKind::ARROW => SyntaxKind::ARROW,
            TokenKind::EQ_EQ => SyntaxKind::EQ_EQ,
            TokenKind::LT_EQ => SyntaxKind::LT_EQ,
            TokenKind::BANG_EQ => SyntaxKind::BANG_EQ,
            TokenKind::AMP_AMP => SyntaxKind::AMP_AMP,
            TokenKind::PIPE_PIPE => SyntaxKind::PIPE_PIPE,
            TokenKind::PLUS_PLUS => SyntaxKind::PLUS_PLUS,
            TokenKind::MINUS_MINUS => SyntaxKind::MINUS_MINUS,
            TokenKind::PLUS => SyntaxKind::PLUS,
            TokenKind::MINUS => SyntaxKind::MINUS,
            TokenKind::STAR => SyntaxKind::STAR,
            TokenKind::SLASH => SyntaxKind::SLASH,
            TokenKind::AMP => SyntaxKind::AMP,
            TokenKind::PIPE => SyntaxKind::PIPE,
            TokenKind::CARET => SyntaxKind::CARET,
            TokenKind::PERCENT => SyntaxKind::PERCENT,
            TokenKind::LSHIFT => SyntaxKind::LSHIFT,
            TokenKind::LSHIFT_EQ => SyntaxKind::LSHIFT_EQ,
            TokenKind::PLUS_EQ => SyntaxKind::PLUS_EQ,
            TokenKind::MINUS_EQ => SyntaxKind::MINUS_EQ,
            TokenKind::STAR_EQ => SyntaxKind::STAR_EQ,
            TokenKind::SLASH_EQ => SyntaxKind::SLASH_EQ,
            TokenKind::AMP_EQ => SyntaxKind::AMP_EQ,
            TokenKind::PIPE_EQ => SyntaxKind::PIPE_EQ,
            TokenKind::CARET_EQ => SyntaxKind::CARET_EQ,
            TokenKind::PERCENT_EQ => SyntaxKind::PERCENT_EQ,
        }
    }
}

/// Returns whether the text is a Javadoc comment (starts with `/**` and is not the empty comment `/**/`).
fn is_doc_comment(text: &str) -> bool {
    text.starts_with("/**") && text != "/**/"
}
