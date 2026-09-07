//! The WebAssembly backend: a project's typed files in, one module out.
//!
//! Java's memory model lands on the [garbage-collection
//! proposal](https://github.com/WebAssembly/gc): a class becomes a `struct` type, inheritance
//! becomes declared subtyping, and `new` becomes `struct.new`. Nothing in this backend traces,
//! marks, sweeps, or frees — allocation hands the object to the embedder's collector, which is the
//! whole point of targeting GC rather than linear memory.
//!
//! # What sits at the top
//!
//! wasm's declared subtyping is single-inheritance, so an *interface* cannot be a supertype of two
//! unrelated classes and gets no struct type: a value of interface type is held as `anyref` and
//! narrowed with `ref.cast` at each use. `java.lang.Object` sits in exactly the same place and for
//! exactly the same reason — it is the root of Java's reference hierarchy and `anyref` is wasm's —
//! which makes it the one library type this backend needs no `java.base` to represent. A **type
//! variable** joins them: JLS §4.6 erases it to its bound and to `Object` with none, and a field of
//! type `T` is one field whatever a use instantiates it at.
//!
//! A value comes back down with the `ref.cast` the JVM backend spells `checkcast` wherever the
//! *declaration* says what type is wanted: a receiver at its owner, an argument at its parameter, a
//! `return` at its result, and any store at what it was declared to hold. What erasure cannot reach
//! is the other conversion Java does at those places — a **boxing** one, whose result is a wrapper
//! this host has no `java.base` to supply, and which is reported as the library type it needs.
//!
//! Unlike the JVM backend, this one lowers from the syntax tree directly. wasm's control flow is
//! structured (`block` / `loop` / `if`), so the nesting the source already has is the nesting the
//! output needs; going through the other backend's `goto`s would mean recovering it again.
//!
//! # A `native` method is an import
//!
//! Java has had a word for "the body is not in this class file" since 1.0. Here it means a
//! WebAssembly **import**: the module declares what it needs, the embedder supplies it, and no
//! engine instantiates a module whose needs are unmet. Nothing else in the lowering changes — the
//! member gets a function index like any other, so a call site emits the same `call` and never
//! learns which kind of function it reached.
//!
//! The two names an import is spelled with are derived from the declaration alone: the declaring
//! class's internal name, and the method's name with its JVM descriptor. That is what lets the
//! host key its implementation table on the same two strings without either side restating the
//! other's type mapping — and it makes a signature the two halves disagree about an *unresolved
//! import*, refused at instantiation with both spellings in hand, rather than a mismatch somebody
//! has to notice. `jals-native` is where the other half of that arrangement lives.
//!
//! One consequence is structural and easy to lose: an import's function type is **its own
//! type-section entry**, outside the single `rec` group every declared type shares. An engine
//! canonicalises a host function alone, so a signature allocated inside that group matches
//! nothing — see [`Import`].
//!
//! # What is exported, and why
//!
//! [`CompileWasm::project`] hands back bytes, which is all a build needs. Everything else here is
//! the layer *beneath* it: [`Insn`] records a body as [`Instr`] values and [`Module`] holds the
//! declared types, functions, and exports until [`Module::finish`] encodes them.
//!
//! A **library** input — the Java a native package publishes — is compiled into the module exactly
//! as the project's own sources are, and is the one thing that is *not* exported. Otherwise a
//! library's `static` methods would fill the list an `--invoke` offers, and, because the first
//! export of a name wins and the second is dropped, a package method could silently take a project
//! method's export away from it.
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
    CompType, ExportKind, FieldType, Func, Global, HeapType, Import, Module, RefType, StorageType,
    SubType, ValType,
};
pub use insn::{Insn, Instr, NumOp};
pub use lower::{CompileWasm, WasmError, WasmOptions};
