//! The declaration a package author writes.
//!
//! [`java_package!`] is this crate's answer to [rhai's `def_package!`][rhai] and [deno_core's
//! `extension!`][deno]: **one declaration carries both halves**. deno's is the closer model — an
//! extension names its JavaScript (`esm`), its Rust (`ops`) and its host state (`state`) in a
//! single macro invocation — and the reason is the same in both languages. A package whose text
//! list lives in one file and whose binding table lives in another is two lists that a reviewer
//! has to read together, and one of them is always the one nobody updated.
//!
//! [rhai]: https://rhai.rs/book/rust/packages/create.html
//! [deno]: https://docs.rs/deno_core/latest/deno_core/macro.extension.html
//!
//! What the macro deliberately does **not** take is a glob. The two source lists are written out,
//! and a project that ships many files generates them — `jals-platform` does, through
//! `cargo run -p xtask -- codegen`, which CI runs with `--check`. A file nobody listed then fails
//! the build rather than silently not being part of the package, and moving one from `signatures`
//! to `implementation` is the diff that says somebody implemented it.

/// Declare a Java package: its name, version, the Java it publishes, and the Rust behind it.
///
/// ```ignore
/// // `ignore`, because `include_str!` in a doctest resolves against a synthetic path rather
/// // than against this file. `jals-native/tests/package.rs` drives the macro for real, over
/// // fixtures in `tests/java/`.
/// use alloc::rc::Rc;
/// use jals_native::{JavaPackage, java_package};
///
/// pub trait Clock {
///     fn millis(&self) -> i64;
/// }
///
/// java_package! {
///     /// A package with one host function.
///     pub Demo {
///         name: "demo.clock",
///         version: 1,
///         host: dyn Clock,
///         root: "../tests/java",
///         signatures: [],
///         implementation: ["demo/Clock.java"],
///         bind: Demo::bind,
///     }
/// }
///
/// impl Demo {
///     fn bind(package: &mut JavaPackage, host: &Rc<dyn Clock>) {
///         let clock = Rc::clone(host);
///         package.bind("demo/Clock", "now()J", move |_, _, mut out| {
///             out.set(0, jals_native::NativeValue::I64(clock.millis()));
///             Ok(())
///         });
///     }
/// }
/// ```
///
/// The generated type carries:
///
/// - `NAME` and `VERSION`, the two constants a manifest and a cache key read;
/// - `SOURCES`, every published unit as a `'static` slice — **host-free**, so an index can be built
///   from it with no state constructed at all, which is what a language server that instantiates
///   nothing needs;
/// - `package(host)`, the whole thing, for a build that will link and run it.
#[macro_export]
macro_rules! java_package {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident {
            name: $package_name:literal,
            version: $version:literal,
            host: dyn $host:path,
            root: $root:literal,
            signatures: [ $($signature:literal),* $(,)? ],
            implementation: [ $($implementation:literal),* $(,)? ],
            bind: $bind:path $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name;

        impl $name {
            /// The package's name, as a manifest spells it.
            pub const NAME: &'static str = $package_name;

            /// The package author's version — see [`JavaPackage::new`]($crate::JavaPackage::new).
            pub const VERSION: u32 = $version;

            /// Every published unit, signature-only ones first.
            ///
            /// `'static`, and reachable without constructing a host: indexing this package needs
            /// the Java and nothing else.
            pub const SOURCES: &'static [$crate::JavaSource] = &[
                $($crate::JavaSource {
                    // `Cow::Borrowed`, which is `const`: a package compiled into the binary must not
                    // copy its own text, and the platform is fifty files an editor re-reads on
                    // every rebuild.
                    path: $crate::__private::Cow::Borrowed($signature),
                    text: $crate::__private::Cow::Borrowed(include_str!(concat!($root, "/", $signature))),
                    kind: $crate::SourceKind::Signatures,
                },)*
                $($crate::JavaSource {
                    path: $crate::__private::Cow::Borrowed($implementation),
                    text: $crate::__private::Cow::Borrowed(include_str!(concat!($root, "/", $implementation))),
                    kind: $crate::SourceKind::Implementation,
                },)*
            ];

            /// The package, over the host state its bindings write through.
            pub fn package(host: $crate::__private::Rc<dyn $host>) -> $crate::JavaPackage {
                let mut package = $crate::JavaPackage::new(Self::NAME, Self::VERSION);
                for source in Self::SOURCES {
                    package.source(source.path.clone(), source.text.clone(), source.kind);
                }
                $bind(&mut package, &host);
                package
            }
        }
    };
}
