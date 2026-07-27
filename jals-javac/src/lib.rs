#![cfg_attr(not(test), no_std)]
//! `jals-javac`: Java source to executable code.
//!
//! The compiler proper. It takes the shared CST plus the semantic index and emits either JVM class
//! files (one per declared type) or a single WebAssembly module for a whole project.
//!
//! Both targets read the same front end — the CST and [`jals_hir`]'s resolution — but there is no
//! compiler IR between it and them, and the two lowerings are separate on purpose. The JVM's
//! control flow is a `goto` stream and wasm's is structured (`block` / `loop` / `br`), so sharing
//! one lowering would mean recovering the tree shape the tree already has. An IR earns its place
//! where the targets agree; so far they mostly do not.
//!
//! # It does not check
//!
//! Diagnostics are [`jals-lint`](../jals_lint)'s job, over [`jals-hir`](../jals_hir)'s analysis.
//! This crate assumes its input is a well-formed program and never reports a type error. It does
//! still *resolve*: emitting one `invokevirtual` requires knowing which overload was selected, its
//! exact descriptor, and whether the owner is a class or an interface. That is code-generation
//! input, not checking, and it is read from the index rather than recomputed here.
//!
//! # Layers
//!
//! - [`desc`] — erasure: a resolved [`jals_hir::Ty`] to the class file's internal names and
//!   descriptors.
//! - [`jvm`] — the JVM backend: a label-based assembler over `jals_classfile::Instruction` that
//!   resolves branches, sizes the frame, and derives the `StackMapTable`.
//! - [`lower`] — the compiler proper: a parsed source file plus its semantic index in, class files
//!   out.
//! - [`wasm`] — the WebAssembly backend, where the host's garbage collector owns every object.
//!
//! Pure and `wasm32`-compatible: no filesystem, process, or network I/O. Reading sources and
//! writing artifacts stays with the host (the CLI, the LSP).

extern crate alloc;

pub mod desc;
pub mod jvm;
pub mod lower;
pub mod wasm;
