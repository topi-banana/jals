#![no_std]
//! `java.base` for the wasm target, as the package a compiled program links against.
//!
//! Java has been able to name `java.lang.String` since it had a `main`, and until this crate there
//! was nothing behind the name on this target: the standard-library stubs declared the surface so
//! that a program would *check*, and the backend then refused every use, because there was no
//! `String` and no host to ask for one. This crate is the answer. It is a real `java.base`, written
//! in Java, compiled to wasm at build time, and shipped as one artifact.
//!
//! # Why a package and not a `classpath`
//!
//! The classes here are not read by a host JVM at run time; they *are* the run time. Every method
//! is lowered into the module that links them, so the platform is a wasm **library** — exactly the
//! shape [`jals_native::NativePackage`] exists for. A package ships either its Java or a compiled
//! module, and a platform may not ship both without being two platforms, so this one ships the
//! module; the Java it was built from travels inside that module's `jals.library` section, which is
//! what a consumer indexes to resolve `new String(chars)` and the rest.
//!
//! The two halves cannot drift. The module's ABI section states the Java that produced it, so a
//! consumer that runs the module and a reader that resolves against its Java are reading one
//! artifact.
//!
//! # Why the build compiles it
//!
//! The module is produced by [`build.rs`][build] with the workspace's own compiler, and embedded
//! with `include_bytes!`. The alternative — checking the wasm in — is an artifact no reviewer can
//! read and no test can regenerate on the machine that finds the bug. Here the source is
//! [`java`][java], the build compiles it, and the test that links the result runs the same bytes a
//! user gets.
//!
//! [build]: https://doc.rust-lang.org/cargo/reference/build-scripts.html
//! [java]: https://github.com/topi-banana/jals/tree/main/jals-platform/java
//!
//! # Using it
//!
//! [`Platform::package`] is the whole API a host needs: a binary registers the value — built over
//! the sink its own console writes to — and a project links it by naming [`Platform::NAME`] in
//! `[build] native-packages`.

extern crate alloc;

use alloc::rc::Rc;

use jals_native::console::ConsoleSink;
use jals_native::{Args, NativeError, NativeHost, Results};

mod version;

/// The platform, as the one name a host needs to say it.
///
/// A type rather than a bare pair of constants and a function, because the three say one thing and
/// the association is what keeps them from drifting apart in a caller's code: [`NAME`](Self::NAME)
/// is the link name, [`MODULE`](Self::MODULE) is the artifact, and [`package`](Self::package) is
/// the two of them — and the one binding — in the form the registry takes.
pub struct Platform;

impl Platform {
    /// The link name, which is also the name `[build] native-packages` selects the package by.
    ///
    /// The module name in every import a linked program writes: `java.base` is the Java module the
    /// classes actually live in, and a link name that cannot be confused with a user's `wasm`
    /// dependency.
    pub const NAME: &str = "java.base";

    /// The compiled platform, embedded by the build script that produced it.
    ///
    /// The bytes are the whole package — the code, the ABI it exports, and the Java under `java`
    /// that a consumer indexes — so nothing here is assembled from parts that could disagree. The
    /// version they carry travels beside them into the package below and into the module's ABI
    /// section, so the two cannot be bumped apart.
    pub const MODULE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/java.base.wasm"));

    /// The class the printing binding belongs to, spelled the way the import section spells it.
    const PRINT_OWNER: &'static str = "java/io/PrintStream";

    /// The method the printing binding answers: the name and descriptor the module imports.
    ///
    /// It takes the `char[]` the Java side builds rather than a `String`, because a string's
    /// representation is the backend's own layout and a host that read one would be reading a fact
    /// no declaration states — an array is a first-class object the embedder can read element by
    /// element, and the one shape both halves already agree on.
    const PRINT_SIGNATURE: &'static str = "writeChars([CII)V";

    /// The platform as a binary offers it: the module, under the name its imports are spelled
    /// with, and the one binding its Java cannot supply — where its printed text goes.
    ///
    /// A fresh value per call rather than a `static`: the package carries a sink, and a host that
    /// has one builds the value over it. The module bytes themselves are `'static`, so the value
    /// is cheap.
    #[must_use]
    pub fn package(sink: Rc<dyn ConsoleSink>) -> jals_native::NativePackage {
        let mut package = jals_native::NativePackage::new(Self::NAME, version::VERSION);
        package.library(Self::MODULE);
        package.bind(
            Self::PRINT_OWNER,
            Self::PRINT_SIGNATURE,
            move |host: &mut dyn NativeHost, args: Args<'_>, _: Results<'_>| {
                let chars = args.reference(0)?;
                let offset = args.i32(1)?;
                let count = args.i32(2)?;
                let (Ok(offset), Ok(count)) = (u32::try_from(offset), u32::try_from(count)) else {
                    // A negative offset or count is a bounds error, not a host defect: the module
                    // passed what its own Java computed, and Java would throw here.
                    let len = host.array_len(chars)?;
                    return Err(NativeError::OutOfBounds { index: 0, len });
                };
                let text = host.array_text(chars, offset, count)?;
                if !text.is_empty() {
                    sink.write(&text);
                }
                Ok(())
            },
        );
        package
    }
}
