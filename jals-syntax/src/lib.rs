//! `jals-syntax`: a lossless Java 26 lexer (`logos`) and CST parser (`rowan`).
//!
//! The shared foundation for `jals-fmt` / `jals-lint` / `jals-lsp`. Everything except the CLI must
//! build for `wasm32-unknown-unknown`.
//!
//! Layers: a `logos` lexer, a `rowan` CST parser, and a typed [`ast`] view over the CST.
//!
//! # Example
//!
//! ```
//! use jals_syntax::{tokenize, SyntaxKind};
//!
//! let tokens = tokenize("int x = 1;");
//! assert_eq!(tokens[0].kind, SyntaxKind::INT_KW);
//! // Concatenating each token's text reproduces the input (lossless).
//! let joined: String = tokens.iter().map(|t| t.text).collect();
//! assert_eq!(joined, "int x = 1;");
//! ```
//!
//! ```
//! use jals_syntax::ast::{AstNode, SourceFile};
//!
//! let parse = jals_syntax::parse("class Foo { }");
//! let file = SourceFile::cast(parse.syntax()).unwrap();
//! let class = file.decls().next().unwrap();
//! assert_eq!(class.syntax().text().to_string(), "class Foo { }");
//! ```

pub mod ast;
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
