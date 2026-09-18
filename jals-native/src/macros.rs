//! The declaration macro: both halves of a package in one place.
//!
//! A text list in one file and a binding table in another are two lists a reviewer has to read
//! together, and one of them is always the one nobody updated. `java_package!` takes the Java
//! (through `include_str!`, so the text is part of the binary) and the Rust that implements its
//! `native` methods at once, and generates:
//!
//! - `NAME` and `VERSION`, the two facts a manifest and a cache key name;
//! - `SOURCES`, the published Java, reachable with **no host constructed at all** — which is what
//!   a language server that instantiates nothing needs;
//! - `package(host)`, a [`JavaPackage`](crate::JavaPackage) with the sources and the bindings
//!   installed by the named function.
//!
//! Paths in the two lists are relative to the file that invokes the macro, because `include_str!`
//! resolves there. Signature units are emitted first whatever order the invocation wrote them,
//! which is the order an index reads them in.

/// Declare a package's Java and Rust together.
///
/// ```ignore
/// java_package! {
///     /// A package with one host function.
///     pub Demo {
///         name: "demo.clock",
///         version: 1,
///         host: dyn Clock,
///         root: "java",
///         signatures: ["demo/Marker.java"],
///         implementation: ["demo/Clock.java"],
///         bind: Demo::install,
///     }
/// }
/// ```
///
/// `bind` is called as `bind(&mut package, &host)`; a package with no host state at all still
/// declares a marker trait, so the shape never changes under it.
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
            /// The package's name, which is what a manifest names it by.
            pub const NAME: &'static str = $package_name;

            /// The package author's version — the one input a cache key cannot observe.
            pub const VERSION: u32 = $version;

            /// Every published unit, signatures first.
            pub const SOURCES: &'static [$crate::JavaSource] = &[
                $($crate::JavaSource {
                    path: $crate::__private::Cow::Borrowed($signature),
                    text: $crate::__private::Cow::Borrowed(::core::include_str!(
                        ::core::concat!($root, "/", $signature)
                    )),
                    kind: $crate::SourceKind::Signatures,
                },)*
                $($crate::JavaSource {
                    path: $crate::__private::Cow::Borrowed($implementation),
                    text: $crate::__private::Cow::Borrowed(::core::include_str!(
                        ::core::concat!($root, "/", $implementation)
                    )),
                    kind: $crate::SourceKind::Implementation,
                },)*
            ];

            /// Build the package over `host`.
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
