#![cfg_attr(not(test), no_std)]
//! `jals-classfile`: a complete, round-trippable model of the JVM `.class` file format (JVMS ch. 4).
//!
//! Reads and writes Java class files through a full struct/enum model. The binary codec
//! ([`ClassFile::read`] / [`ClassFile::write`]) is hand-written and big-endian; it does **not** use
//! serde. Every model type *additionally* derives [`serde::Serialize`] / [`serde::Deserialize`] so
//! the model can be carried through a self-describing format (e.g. `serde_json`) — serde is the
//! struct⇄JSON medium, never the binary codec.
//!
//! The codec round-trips byte-for-byte: for any class file `b`,
//! `ClassFile::read(&b).unwrap().write() == b`. Unrecognised attributes are preserved verbatim, and
//! counts / byte-length fields are derived from the contents on write rather than stored, so the
//! invariant holds even after the model is edited.
//!
//! Pure and `wasm32`-compatible: no filesystem, process, or other I/O. Reading `.class` bytes from
//! disk or a JAR is the host's job (the CLI / LSP).
//!
//! # Example
//!
//! ```no_run
//! # fn load() -> Vec<u8> { Vec::new() }
//! let bytes = load(); // e.g. read a `.class` from disk (host side)
//! let class = jals_classfile::ClassFile::read(&bytes).expect("valid class file");
//! assert_eq!(class.write(), bytes); // byte-exact round-trip
//! ```

extern crate alloc;

use alloc::vec::Vec;

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
pub use descriptor::{
    BaseType, FieldType, MethodDescriptor, ReturnType, parse_field_descriptor,
    parse_method_descriptor,
};
pub use error::{ClassfileError, Result};
pub use field::FieldInfo;
pub use flags::{ClassAccessFlags, FieldAccessFlags, MethodAccessFlags};
pub use instruction::{Instruction, WideInstruction};
pub use method::MethodInfo;
pub use signature::{
    ClassSignature, ClassTypeSignature, MethodSignature, ResultSignature, SimpleClassTypeSignature,
    ThrowsSignature, TypeArgument, TypeParameter, TypeSignature, parse_class_signature,
    parse_field_signature, parse_method_signature,
};
pub use stackmap::{StackMapFrame, VerificationType};

/// Parse a class file from its raw bytes. A thin alias for [`ClassFile::read`].
pub fn read(bytes: &[u8]) -> Result<ClassFile> {
    ClassFile::read(bytes)
}

/// Serialise a class file back to bytes. A thin alias for [`ClassFile::write`].
pub fn write(class: &ClassFile) -> Vec<u8> {
    class.write()
}
