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
    /// Unnamed pattern (`_`), valid only as a record-pattern component.
    UNNAMED_PATTERN,
    /// Pattern guard (`when expr`).
    GUARD,

    // --- Module declarations (`module-info.java`) ---
    /// `{Annotation} [open] module Name { directives }`.
    MODULE_DECL,
    /// Module body (`{ directives }`).
    MODULE_BODY,
    /// `requires {transitive|static} ModuleName ;`.
    REQUIRES_DIRECTIVE,
    /// `exports PackageName [to ModuleName, ...] ;`.
    EXPORTS_DIRECTIVE,
    /// `opens PackageName [to ModuleName, ...] ;`.
    OPENS_DIRECTIVE,
    /// `uses TypeName ;`.
    USES_DIRECTIVE,
    /// `provides TypeName with TypeName, ... ;`.
    PROVIDES_DIRECTIVE,

    // --- jals dialect ---
    /// A jals grouped import (`.{ A, B }`).
    IMPORT_GROUP,
    /// `#` (begins a jals attribute; not a Java token).
    HASH,
    /// A jals attribute (`#[cfg(feature = "x")]`).
    ATTRIBUTE,
    /// The meta item of a jals attribute (`cfg(...)`, `feature = "x"`).
    ATTR_META,
    /// Argument list of a jals attribute meta (`(...)`).
    ATTR_ARG_LIST,
}

impl SyntaxKind {
    /// Determines a `SyntaxKind` from a `TokenKind` and the original source text.
    ///
    /// `BLOCK_COMMENT` tokens that are Javadoc comments (starting with `/**` but not `/**/`)
    /// are classified as `DOC_COMMENT`. Everything else behaves the same as [`From<TokenKind>`].
    pub(crate) fn from_token(token: TokenKind, text: &str) -> Self {
        /// Whether the text is a Javadoc comment (starts with `/**` and is not the empty comment `/**/`).
        fn is_doc_comment(text: &str) -> bool {
            text.starts_with("/**") && text != "/**/"
        }

        match token {
            TokenKind::BLOCK_COMMENT if is_doc_comment(text) => Self::DOC_COMMENT,
            other => Self::from(other),
        }
    }

    /// Returns whether this kind is trivia (whitespace, newlines, or comments).
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::WHITESPACE
                | Self::NEWLINE
                | Self::LINE_COMMENT
                | Self::BLOCK_COMMENT
                | Self::DOC_COMMENT
        )
    }
}

impl From<TokenKind> for SyntaxKind {
    fn from(token: TokenKind) -> Self {
        match token {
            TokenKind::WHITESPACE => Self::WHITESPACE,
            TokenKind::NEWLINE => Self::NEWLINE,
            TokenKind::LINE_COMMENT => Self::LINE_COMMENT,
            TokenKind::BLOCK_COMMENT => Self::BLOCK_COMMENT,
            TokenKind::IDENT => Self::IDENT,
            TokenKind::UNDERSCORE => Self::UNDERSCORE,
            TokenKind::ABSTRACT_KW => Self::ABSTRACT_KW,
            TokenKind::ASSERT_KW => Self::ASSERT_KW,
            TokenKind::BOOLEAN_KW => Self::BOOLEAN_KW,
            TokenKind::BREAK_KW => Self::BREAK_KW,
            TokenKind::BYTE_KW => Self::BYTE_KW,
            TokenKind::CASE_KW => Self::CASE_KW,
            TokenKind::CATCH_KW => Self::CATCH_KW,
            TokenKind::CHAR_KW => Self::CHAR_KW,
            TokenKind::CLASS_KW => Self::CLASS_KW,
            TokenKind::CONST_KW => Self::CONST_KW,
            TokenKind::CONTINUE_KW => Self::CONTINUE_KW,
            TokenKind::DEFAULT_KW => Self::DEFAULT_KW,
            TokenKind::DO_KW => Self::DO_KW,
            TokenKind::DOUBLE_KW => Self::DOUBLE_KW,
            TokenKind::ELSE_KW => Self::ELSE_KW,
            TokenKind::ENUM_KW => Self::ENUM_KW,
            TokenKind::EXTENDS_KW => Self::EXTENDS_KW,
            TokenKind::FINAL_KW => Self::FINAL_KW,
            TokenKind::FINALLY_KW => Self::FINALLY_KW,
            TokenKind::FLOAT_KW => Self::FLOAT_KW,
            TokenKind::FOR_KW => Self::FOR_KW,
            TokenKind::GOTO_KW => Self::GOTO_KW,
            TokenKind::IF_KW => Self::IF_KW,
            TokenKind::IMPLEMENTS_KW => Self::IMPLEMENTS_KW,
            TokenKind::IMPORT_KW => Self::IMPORT_KW,
            TokenKind::INSTANCEOF_KW => Self::INSTANCEOF_KW,
            TokenKind::INT_KW => Self::INT_KW,
            TokenKind::INTERFACE_KW => Self::INTERFACE_KW,
            TokenKind::LONG_KW => Self::LONG_KW,
            TokenKind::NATIVE_KW => Self::NATIVE_KW,
            TokenKind::NEW_KW => Self::NEW_KW,
            TokenKind::PACKAGE_KW => Self::PACKAGE_KW,
            TokenKind::PRIVATE_KW => Self::PRIVATE_KW,
            TokenKind::PROTECTED_KW => Self::PROTECTED_KW,
            TokenKind::PUBLIC_KW => Self::PUBLIC_KW,
            TokenKind::RETURN_KW => Self::RETURN_KW,
            TokenKind::SHORT_KW => Self::SHORT_KW,
            TokenKind::STATIC_KW => Self::STATIC_KW,
            TokenKind::STRICTFP_KW => Self::STRICTFP_KW,
            TokenKind::SUPER_KW => Self::SUPER_KW,
            TokenKind::SWITCH_KW => Self::SWITCH_KW,
            TokenKind::SYNCHRONIZED_KW => Self::SYNCHRONIZED_KW,
            TokenKind::THIS_KW => Self::THIS_KW,
            TokenKind::THROW_KW => Self::THROW_KW,
            TokenKind::THROWS_KW => Self::THROWS_KW,
            TokenKind::TRANSIENT_KW => Self::TRANSIENT_KW,
            TokenKind::TRY_KW => Self::TRY_KW,
            TokenKind::VOID_KW => Self::VOID_KW,
            TokenKind::VOLATILE_KW => Self::VOLATILE_KW,
            TokenKind::WHILE_KW => Self::WHILE_KW,
            TokenKind::TRUE_KW => Self::TRUE_KW,
            TokenKind::FALSE_KW => Self::FALSE_KW,
            TokenKind::NULL_KW => Self::NULL_KW,
            TokenKind::INT_LITERAL => Self::INT_LITERAL,
            TokenKind::FLOAT_LITERAL => Self::FLOAT_LITERAL,
            TokenKind::CHAR_LITERAL => Self::CHAR_LITERAL,
            TokenKind::STRING_LITERAL => Self::STRING_LITERAL,
            TokenKind::TEXT_BLOCK => Self::TEXT_BLOCK,
            TokenKind::LPAREN => Self::LPAREN,
            TokenKind::RPAREN => Self::RPAREN,
            TokenKind::LBRACE => Self::LBRACE,
            TokenKind::RBRACE => Self::RBRACE,
            TokenKind::LBRACK => Self::LBRACK,
            TokenKind::RBRACK => Self::RBRACK,
            TokenKind::SEMICOLON => Self::SEMICOLON,
            TokenKind::COMMA => Self::COMMA,
            TokenKind::DOT => Self::DOT,
            TokenKind::ELLIPSIS => Self::ELLIPSIS,
            TokenKind::AT => Self::AT,
            TokenKind::COLON_COLON => Self::COLON_COLON,
            TokenKind::HASH => Self::HASH,
            TokenKind::EQ => Self::EQ,
            TokenKind::LT => Self::LT,
            TokenKind::GT => Self::GT,
            TokenKind::BANG => Self::BANG,
            TokenKind::TILDE => Self::TILDE,
            TokenKind::QUESTION => Self::QUESTION,
            TokenKind::COLON => Self::COLON,
            TokenKind::ARROW => Self::ARROW,
            TokenKind::EQ_EQ => Self::EQ_EQ,
            TokenKind::LT_EQ => Self::LT_EQ,
            TokenKind::BANG_EQ => Self::BANG_EQ,
            TokenKind::AMP_AMP => Self::AMP_AMP,
            TokenKind::PIPE_PIPE => Self::PIPE_PIPE,
            TokenKind::PLUS_PLUS => Self::PLUS_PLUS,
            TokenKind::MINUS_MINUS => Self::MINUS_MINUS,
            TokenKind::PLUS => Self::PLUS,
            TokenKind::MINUS => Self::MINUS,
            TokenKind::STAR => Self::STAR,
            TokenKind::SLASH => Self::SLASH,
            TokenKind::AMP => Self::AMP,
            TokenKind::PIPE => Self::PIPE,
            TokenKind::CARET => Self::CARET,
            TokenKind::PERCENT => Self::PERCENT,
            TokenKind::LSHIFT => Self::LSHIFT,
            TokenKind::LSHIFT_EQ => Self::LSHIFT_EQ,
            TokenKind::PLUS_EQ => Self::PLUS_EQ,
            TokenKind::MINUS_EQ => Self::MINUS_EQ,
            TokenKind::STAR_EQ => Self::STAR_EQ,
            TokenKind::SLASH_EQ => Self::SLASH_EQ,
            TokenKind::AMP_EQ => Self::AMP_EQ,
            TokenKind::PIPE_EQ => Self::PIPE_EQ,
            TokenKind::CARET_EQ => Self::CARET_EQ,
            TokenKind::PERCENT_EQ => Self::PERCENT_EQ,
        }
    }
}
