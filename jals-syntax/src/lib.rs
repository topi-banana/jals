//! `jals-syntax`: Java 26 の lossless な字句解析器(`logos`)と CST parser(`rowan`)。
//!
//! `jals-fmt` / `jals-lint` / `jals-lsp` の共通基盤。CLI を除き
//! `wasm32-unknown-unknown` でのビルドを必須とする。
//!
//! マイルストーン B 進行中: `logos` による lexer + `rowan` による CST parser。
//!
//! # 例
//!
//! ```
//! use jals_syntax::{tokenize, SyntaxKind};
//!
//! let tokens = tokenize("int x = 1;");
//! assert_eq!(tokens[0].kind, SyntaxKind::INT_KW);
//! // 各トークンの text を連結すると入力に一致する(lossless)。
//! let joined: String = tokens.iter().map(|t| t.text).collect();
//! assert_eq!(joined, "int x = 1;");
//! ```

pub mod language;
pub mod lexer;
mod parser;
pub mod syntax_error;
pub mod syntax_kind;
pub mod token;

pub use language::{JavaLanguage, SyntaxElement, SyntaxNode, SyntaxToken};
pub use lexer::{LexedToken, Lexer, tokenize};
pub use parser::{Parse, parse};
pub use syntax_error::SyntaxError;
pub use syntax_kind::SyntaxKind;
pub use token::TokenKind;
