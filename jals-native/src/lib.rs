#![cfg_attr(not(test), no_std)]
//! `jals-native`: a Java package whose implementation is Rust.
//!
//! Java has had a word for "this method's body lives outside the class file" since version 1.0,
//! and it is `native`. This crate is what that word means on the WebAssembly target: a **native
//! package** is one value holding the Java it publishes and the Rust behind that Java's `native`
//! methods, and the compiler turns each of those methods into a wasm **import** the runner links
//! against the same value.
//!
//! (The `native` in the crate's name is Java's keyword. It is not the Cargo feature several other
//! crates in this workspace use to gate host I/O — this crate has no features at all.)
//!
//! # Why one value holds both halves
//!
//! The alternative is a Java library somewhere and a table of Rust functions registered somewhere
//! else, which are two artifacts that can disagree about a signature. Here they cannot: a binding
//! is keyed by the declaring class's internal name and the method's name-with-descriptor, which
//! are exactly the two strings the backend writes into the import section. A Rust half that spells
//! a signature differently produces an import nothing satisfies, refused when the module is
//! instantiated, with both spellings listed.
//!
//! That is the only agreement point, and it is structural. Nothing re-derives a wasm type from a
//! descriptor: the runner takes the `FuncType` the *module itself* declared for the import and
//! hands the binding to the engine under it.
//!
//! # What a binding can do
//!
//! Read and write Java arrays, call the module's own exports, and hold whatever host state its
//! closure captured. It cannot allocate a Java object — a wasm embedder has no `struct.new` of its
//! own — so a native method that must return one calls a `static` factory the package's Java
//! declares. See [`NativeHost`].
//!
//! # Writing one
//!
//! ```
//! use jals_native::{Args, NativeError, NativeHost, NativePackage, NativeValue, Results};
//!
//! const JAVA: &str = r"
//! package demo;
//! public final class Answer {
//!     public static native int compute();
//! }
//! ";
//!
//! let mut package = NativePackage::new("demo", 1);
//! package.source("demo/Answer.java", JAVA);
//! package.bind(
//!     "demo/Answer",
//!     "compute()I",
//!     |_host: &mut dyn NativeHost, _args: Args<'_>, mut results: Results<'_>| {
//!         results.set(0, NativeValue::I32(42));
//!         Ok::<(), NativeError>(())
//!     },
//! );
//! assert_eq!(package.sources().len(), 1);
//! ```
//!
//! # Where the pieces are used
//!
//! - `jals-config` reads `[build] native-packages`, and refuses a non-empty list under any backend
//!   but `jals-wasm` — a native package has exactly one producer that can take it in.
//! - `jals-build` selects a [`NativePackageSet`] out of a [`NativeRegistry`], compiles its
//!   [`sources`](NativePackageSet::sources) into the module beside the project's own, folds its
//!   [`provenance`](NativePackageSet::provenance) into the backend's cache key, and links its
//!   [`bindings`](NativePackageSet::bindings) when the module is instantiated.
//! - `jals-hir` indexes the same Java, so the project's own source resolves against it.
//!
//! # The two packages this crate ships
//!
//! [`packages::jals_io`] is the smallest thing this seam can be: three `native` methods, a
//! `char[]`, and a library written in Java on top of them.
//!
//! [`packages::java_base`] is the largest. `jals-hir` publishes signature-only stubs for
//! `java.lang` and `java.io` so a reference to `String` resolves; this is the same set of types
//! with the bodies those stubs do not have — `String`, `StringBuilder`, every wrapper, `Math`,
//! `System`, `PrintStream`, and the whole `Throwable` hierarchy — behind ten host functions.

extern crate alloc;

mod host;
mod package;
mod registry;
mod value;

pub mod packages;

pub use host::NativeHost;
pub use package::{NativeFn, NativePackage, NativeSource};
pub use registry::{NativeBindings, NativePackageSet, NativeRegistry, UnknownNativePackage};
pub use value::{Args, NativeError, NativeValue, Provenance, RefSlot, Results};
