#![cfg_attr(not(test), no_std)]
//! `jals-native`: what a Java package is, and how one is found.
//!
//! Java has had a word for "this method's body lives outside the class file" since version 1.0,
//! and it is `native`. This crate is what that word means on the WebAssembly target: a **package**
//! is one value holding the Java it publishes and the Rust behind that Java's `native` methods, and
//! the compiler turns each of those methods into a wasm **import** the runner links against the
//! same value.
//!
//! (The `native` in the crate's name is Java's keyword. It is not the Cargo feature several other
//! crates in this workspace use to gate host I/O — this crate has no features at all, and no
//! dependencies, so that a package author's crate depends on this one and on nothing else.)
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
//! # This crate ships no Java
//!
//! Not even `java.lang`. The platform library is a package like any other (`jals-platform`), found
//! through the same [`ResolverChain`] as anything a third party writes, and that is the point: a
//! standard library privileged into the crate that defines what a package *is* would be a second
//! way to publish Java, and the second way is always the one that drifts.
//!
//! It is also why nothing here has an opinion about how an index should *read* a package's
//! declarations. A package states [`SourceKind`] — whether a file carries bodies — and stops. What
//! that means for analysis depends on what the build links, which is `jals-hir`'s
//! `LibraryFidelity` and somebody else's question.
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
//! [`java_package!`] is the declaration form, and takes both halves at once. By hand, for a package
//! whose Java is not a file on disk:
//!
//! ```
//! use jals_native::{
//!     Args, JavaPackage, NativeError, NativeHost, NativeValue, Results, SourceKind,
//! };
//!
//! const JAVA: &str = r"
//! package demo;
//! public final class Answer {
//!     public static native int compute();
//! }
//! ";
//!
//! let mut package = JavaPackage::new("demo", 1);
//! package.source("demo/Answer.java", JAVA, SourceKind::Implementation);
//! package.bind(
//!     "demo/Answer",
//!     "compute()I",
//!     |_host: &mut dyn NativeHost, _args: Args<'_>, mut results: Results<'_>| {
//!         results.set(0, NativeValue::I32(42));
//!         Ok::<(), NativeError>(())
//!     },
//! );
//! assert_eq!(package.sources().len(), 1);
//! assert_eq!(package.binding_count(), 1);
//! ```
//!
//! # Where the pieces are used
//!
//! - `jals-config` reads the manifest keys that name packages, and decides whether this build links
//!   any — a `native` method's implementation is a host function supplied to a WebAssembly module,
//!   and no other backend emits one for it to be supplied to.
//! - `jals-build` compiles a selection's [`link_sources`](PackageSelection::link_sources) into the
//!   module beside the project's own, folds its
//!   [`provenance`](PackageSelection::provenance) into the backend's cache key, and links its
//!   [`bindings`](PackageSelection::bindings) when the module is instantiated.
//! - `jals-editor` indexes a selection's [`analysis_sources`](PackageSelection::analysis_sources),
//!   so the project's own source resolves against the same Java.

extern crate alloc;

mod host;
mod macros;
mod package;
mod resolver;
mod selection;
mod value;

pub use host::NativeHost;
pub use package::{JavaPackage, JavaSource, NativeFn, SourceKind};
pub use resolver::{PackageResolver, ResolveError, ResolverChain, SourceResolver, StaticResolver};
pub use selection::{NativeBindings, PackageSelection};
pub use value::{Args, NativeError, NativeValue, Provenance, RefSlot, Results};

/// Paths [`java_package!`] expands to, so a caller needs no `extern crate alloc` of its own.
///
/// Not a public API: the macro names these, nothing else may.
#[doc(hidden)]
pub mod __private {
    pub use alloc::borrow::Cow;
    pub use alloc::rc::Rc;
}
