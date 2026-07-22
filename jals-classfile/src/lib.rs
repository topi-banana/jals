#![cfg_attr(not(test), no_std)]
// This crate is a byte-exact binary codec: reinterpreting between signed and unsigned integers of
// the same width (e.g. a `u16` branch offset as an `i16`, two's-complement) and narrowing a
// derived-on-write count to its `u8`/`u16` field are the codec's intended semantics, so the
// value-losing cast lints are allowed crate-wide rather than papered over with `as`-site attributes.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
//! `jals-classfile`: a complete, round-trippable model of the JVM `.class` file format (JVMS ch. 4).
//!
//! Reads and writes Java class files through a full struct/enum model. The binary codec
//! ([`ClassFile::read`] / [`ClassFile::write`]) is hand-written and big-endian; it does **not** use
//! serde. Every model type *additionally* derives [`serde::Serialize`] / [`serde::Deserialize`] so
//! the model can be carried through a self-describing format (e.g. `serde_json`) — serde is the
//! struct⇄JSON medium, never the binary codec.
//!
//! The codec round-trips byte-for-byte: for any class file `b`, awaiting
//! `ClassFile::read(b.as_slice())` and calling `.write()` reproduces `b` exactly. Unrecognised
//! attributes are preserved verbatim, and counts / byte-length fields are derived from the
//! contents on write rather than stored, so the invariant holds even after the model is edited.
//!
//! Pure and `wasm32`-compatible: [`ClassFile::read`] is an `async fn` parsing from any portable
//! byte source ([`jals_storage::io::Read`]) — a `&[u8]` slice (always ready), or a host reader
//! bridged through `jals_storage::io::StdReader` (e.g. a `BufReader` over a file, or a
//! decompressing JAR member stream). The parse yields cooperatively inside its bulk loops;
//! [`ClassFile::write`] is pure in-memory `Vec` construction and stays synchronous. Opening
//! files or archives remains the host's job (the CLI / LSP).
//!
//! # Example
//!
//! ```no_run
//! # fn load() -> Vec<u8> { Vec::new() }
//! let bytes = load(); // e.g. read a `.class` from disk (host side)
//! let class = jals_exec::block_on_inline(jals_classfile::ClassFile::read(bytes.as_slice()))
//!     .expect("valid class file");
//! assert_eq!(class.write(), bytes); // byte-exact round-trip
//! ```

extern crate alloc;

mod annotation;
mod attribute;
mod bytes;
mod class_file;
mod constant_pool;
mod descriptor;
mod error;
mod field;
mod flags;
mod instruction;
mod method;
mod signature;
mod stackmap;

pub use annotation::{
    Annotation, ElementValue, ElementValuePair, LocalVarTargetEntry, TargetInfo, TypeAnnotation,
    TypePathEntry,
};
pub use attribute::{
    Attribute, AttributeBody, BootstrapMethod, CodeAttribute, ExceptionTableEntry, InnerClassEntry,
    LineNumberEntry, LocalVariableEntry, LocalVariableTypeEntry, MethodParameterEntry,
    ModuleAttribute, ModuleExport, ModuleOpen, ModuleProvide, ModuleRequire, RecordComponentInfo,
};
pub use class_file::ClassFile;
pub use constant_pool::{ConstantPool, ConstantPoolEntry};
pub use descriptor::{BaseType, FieldType, MethodDescriptor, ReturnType};
pub use error::{ClassfileError, Result};
pub use field::FieldInfo;
pub use flags::{ClassAccessFlags, FieldAccessFlags, MethodAccessFlags};
pub use instruction::{Instruction, WideInstruction};
pub use method::MethodInfo;
pub use signature::{
    ClassSignature, ClassTypeSignature, MethodSignature, ResultSignature, SimpleClassTypeSignature,
    ThrowsSignature, TypeArgument, TypeParameter, TypeSignature,
};
pub use stackmap::{StackMapFrame, VerificationType};
