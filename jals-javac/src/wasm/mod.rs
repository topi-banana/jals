//! The WebAssembly backend: a project's typed files in, one module out.
//!
//! Java's memory model lands on the [garbage-collection
//! proposal](https://github.com/WebAssembly/gc): a class becomes a `struct` type, inheritance
//! becomes declared subtyping, and `new` becomes `struct.new`. Nothing in this backend traces,
//! marks, sweeps, or frees — allocation hands the object to the embedder's collector, which is the
//! whole point of targeting GC rather than linear memory.
//!
//! Unlike the JVM backend, this one lowers from the syntax tree directly. wasm's control flow is
//! structured (`block` / `loop` / `if`), so the nesting the source already has is the nesting the
//! output needs; going through the other backend's `goto`s would mean recovering it again.
//!
//! # What is exported, and why
//!
//! [`CompileWasm::project`] hands back bytes, which is all a build needs. Everything else here is
//! the layer *beneath* it: [`Insn`] records a body as [`Instr`] values and [`Module`] holds the
//! declared types, functions, and exports until [`Module::finish`] encodes them.
//!
//! That layer is public for the same reason [`jvm`](crate::jvm) publishes its assembler — a
//! generator's derivations deserve to be asserted apart from the lowering that feeds them, and a
//! module that only ever appears as bytes can be asked nothing at all. It matters more here than
//! there: the tests that run this backend end-to-end need a real engine, and the platform this
//! backend targets is exactly the one CI has no engine on.

mod encode;
mod insn;
mod lower;

/// Numeric promotion is a source fact, so the type lives in `crate::facts`; it is named here
/// because it is what [`Insn::convert`] takes. [`jvm`](crate::jvm) re-exports it for the same
/// reason, so that neither backend's seam sends a caller to the other one for a name it needs.
pub use crate::facts::Numeric;
pub use encode::{
    CompType, ExportKind, FieldType, Func, Global, HeapType, Module, RefType, StorageType, SubType,
    ValType,
};
pub use insn::{Insn, Instr, NumOp};
pub use lower::{CompileWasm, WasmError};
