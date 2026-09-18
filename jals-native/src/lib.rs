#![cfg_attr(not(test), no_std)]
//! `jals-native`: a Java package whose implementation is Rust.
//!
//! Java has had a word for "this method's body lives outside the class file" since version 1.0,
//! and it is `native`. This crate is what that word means on the WebAssembly target: a **package**
//! is one value holding the Java it publishes and the Rust behind that Java's `native` methods,
//! and the compiler turns each of those methods into a wasm **import** the runner links against
//! the same value.
//!
//! (The `native` in the crate's name is Java's keyword. It is not the Cargo feature several other
//! crates in this workspace use to gate host I/O — this crate has no features at all.)
//!
//! # One definition, two readers
//!
//! The alternative is a Java library somewhere and a table of Rust functions registered somewhere
//! else, which are two artifacts that can disagree about a signature. Here they cannot: a binding
//! is keyed by the declaring class's internal name and the method's name-with-descriptor, which
//! are exactly the two strings the backend writes into the import section. A Rust half that spells
//! a signature differently produces an import nothing satisfies, refused when the module is
//! instantiated, with both spellings listed.
//!
//! A package's Java is read by two consumers that want different things from it — an *index*
//! (`jals-hir`, and through it the linter and the language server) reads every unit as a
//! declaration, and a *linking compile* lowers the units that carry bodies — and both read the
//! same [`JavaSource`] values. A host cannot index one text and compile another, because there is
//! only one text. How faithfully an index should read it is the consumer's answer
//! (`jals_hir::LibraryFidelity`), never a second enum here.
//!
//! # What a binding can do
//!
//! Read and write Java arrays, call the module's own exports, and hold state the host's table
//! keeps for the run. It cannot allocate a Java object — a wasm embedder has no `struct.new` of
//! its own — so a native method that must hand one back calls a `static` factory the package's
//! Java declares, or returns an object it was handed. A Java instance *is* allowed to carry an
//! `int` handle naming a Rust object the host holds, which is how a container can have a `Vec`
//! behind it; see [`JavaPackage::native_class`].
//!
//! # Writing one
//!
//! ```
//! use jals_native::{JavaPackage, NativeValue, SourceKind};
//!
//! const JAVA: &str = r#"
//! package demo;
//! public final class Counter {
//!     private int handle;
//!     public Counter() { this.handle = allocate(); }
//!     public int next() { return advance(this.handle); }
//!     private static native int allocate();
//!     private static native int advance(int handle);
//! }
//! "#;
//!
//! let mut package = JavaPackage::new("demo", 1);
//! package.source("demo/Counter.java", JAVA, SourceKind::Implementation);
//! package
//!     .native_class::<u32>("demo/Counter")
//!     .allocate("allocate()I", |_| Ok(0u32))
//!     .method("advance(I)I", |count, _host, _args, mut results| {
//!         *count += 1;
//!         results.set(0, NativeValue::I32(*count as i32));
//!         Ok(())
//!     });
//! assert_eq!(package.binding_count(), 2);
//! ```
//!
//! # Where the pieces are used
//!
//! - `jals-config` reads `[build] platform` and `[build] native-packages`, and refuses a
//!   non-empty native-package list under any backend but `jals-wasm` — a native package has
//!   exactly one producer that can take it in.
//! - `jals-build` selects a [`PackageSelection`] out of a [`PackageRegistry`], compiles its
//!   implementation units into the module beside the project's own, folds its
//!   [`provenance`](PackageSelection::provenance) into the backend's cache key, and links its
//!   [`bindings`](PackageSelection::bindings) when the module is instantiated.
//! - `jals-hir` indexes the same Java, so the project's own source resolves against it.

extern crate alloc;

mod host;
mod macros;
mod package;
mod registry;
mod value;

pub use host::{HostObjects, NativeHost};
pub use package::{JavaPackage, JavaSource, NativeClass, NativeFn, SourceKind};
pub use registry::{NativeBindings, PackageRegistry, PackageSelection, UnknownPackage};
pub use value::{Args, HostId, HostValue, NativeError, NativeValue, Provenance, RefSlot, Results};

/// Names the declaration macro expands to, so a package author needs no `extern crate alloc`.
#[doc(hidden)]
pub mod __private {
    pub use alloc::borrow::Cow;
    pub use alloc::rc::Rc;
}
