//! The source list, generated from the `java/` tree.
//!
//! `cargo run -p xtask -- codegen` writes this file, and CI runs the same command with `--check`.
//! That is what makes a `.java` file nobody listed a build failure rather than a file that silently
//! is not part of the package — and it makes moving one between the two tiers a visible diff that
//! says somebody implemented it.
//!
//! Which tier a file is in is decided by `xtask`'s own list, not by looking at the Java: "has a
//! body" is the wrong question, because an interface's methods have none and interfaces *are*
//! compiled, while `java.lang.Object` could be given bodies and must still never be lowered.

use jals_native::java_package;

use crate::bindings::Bindings;
use crate::host::PlatformHost;

java_package! {
    /// The Java platform library `jals` ships: `java.lang` and `java.io`, behind twelve host
    /// functions.
    ///
    /// See the crate docs for what is here and what deliberately is not.
    pub JavaBase {
        name: "java.base",
        version: 1,
        host: dyn PlatformHost,
        root: "../java",
        signatures: [
            "java/lang/Enum.java",
            "java/lang/Iterable.java",
            "java/lang/Object.java",
            "java/lang/Record.java",
            "java/util/ArrayList.java",
            "java/util/Collection.java",
            "java/util/HashMap.java",
            "java/util/HashSet.java",
            "java/util/Iterator.java",
            "java/util/List.java",
            "java/util/Map.java",
            "java/util/Objects.java",
            "java/util/Optional.java",
            "java/util/Set.java",
        ],
        implementation: [
            "java/io/Closeable.java",
            "java/io/FileNotFoundException.java",
            "java/io/IOException.java",
            "java/io/PrintStream.java",
            "java/io/UncheckedIOException.java",
            "java/lang/ArithmeticException.java",
            "java/lang/ArrayIndexOutOfBoundsException.java",
            "java/lang/AssertionError.java",
            "java/lang/AutoCloseable.java",
            "java/lang/Boolean.java",
            "java/lang/Byte.java",
            "java/lang/CharSequence.java",
            "java/lang/Character.java",
            "java/lang/Class.java",
            "java/lang/ClassCastException.java",
            "java/lang/ClassNotFoundException.java",
            "java/lang/CloneNotSupportedException.java",
            "java/lang/Comparable.java",
            "java/lang/Deprecated.java",
            "java/lang/Double.java",
            "java/lang/Error.java",
            "java/lang/Exception.java",
            "java/lang/Float.java",
            "java/lang/FunctionalInterface.java",
            "java/lang/IllegalAccessException.java",
            "java/lang/IllegalArgumentException.java",
            "java/lang/IllegalStateException.java",
            "java/lang/IndexOutOfBoundsException.java",
            "java/lang/InstantiationException.java",
            "java/lang/Integer.java",
            "java/lang/InterruptedException.java",
            "java/lang/Long.java",
            "java/lang/Math.java",
            "java/lang/NegativeArraySizeException.java",
            "java/lang/NoSuchFieldException.java",
            "java/lang/NoSuchMethodException.java",
            "java/lang/NullPointerException.java",
            "java/lang/Number.java",
            "java/lang/NumberFormatException.java",
            "java/lang/Override.java",
            "java/lang/ReflectiveOperationException.java",
            "java/lang/RuntimeException.java",
            "java/lang/SafeVarargs.java",
            "java/lang/Short.java",
            "java/lang/String.java",
            "java/lang/StringBuilder.java",
            "java/lang/StringIndexOutOfBoundsException.java",
            "java/lang/SuppressWarnings.java",
            "java/lang/System.java",
            "java/lang/Throwable.java",
            "java/lang/UnsupportedOperationException.java",
            "java/lang/Void.java",
        ],
        bind: Bindings::install,
    }
}

impl JavaBase {
    /// Every unit paired with the fidelity a build that does or does not link it reads it at.
    ///
    /// The tier rule, in one place, as a value: an implementation unit is the running code only
    /// where it is compiled into the artifact, and a signature unit is a record wherever it is.
    /// `links` is `jals_config::Manifest::links_packages`.
    ///
    /// A convenience over [`SOURCES`](Self::SOURCES), for a consumer that wants the platform and
    /// nothing else — every test in this workspace, and a host with no manifest to resolve from.
    /// A host that has one goes through `PackageSelection`, which answers the same way for the
    /// same reason.
    #[must_use]
    pub fn tiers(links: bool) -> alloc::vec::Vec<(&'static str, bool)> {
        Self::SOURCES
            .iter()
            .map(|source| {
                let running =
                    links && matches!(source.kind, jals_native::SourceKind::Implementation);
                (source.text.as_ref(), running)
            })
            .collect()
    }
}
