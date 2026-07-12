#![cfg_attr(not(test), no_std)]
//! `jals-decompile`: reconstructing Java source from a compiled `jals_classfile::ClassFile`.
//!
//! This crate turns the byte-exact model produced by [`jals_classfile`] into readable Java: the type
//! vocabulary shared with the signature-skeleton renderer ([`types`]), the attribute readers a
//! skeleton needs ([`attrs`] — `ConstantValue` initializers, declared `throws`, real parameter
//! names), and, in later phases, method-body decompilation from bytecode.
//!
//! Pure and `wasm32`-compatible: it only reads the already-parsed class model and never panics. Every
//! reconstruction is conservative — when a construct cannot be rendered as something a Java parser
//! accepts, the function reports that (`None` / empty) so the host emits a safe fallback and the
//! output stays valid Java. The host owns all I/O (reading `.class` bytes, writing `.java`).

extern crate alloc;

mod attrs;
mod body;
mod cfg;
mod expr;
mod literal;
mod types;

pub use attrs::Attrs;
pub use body::MethodBody;
pub use types::JavaType;
