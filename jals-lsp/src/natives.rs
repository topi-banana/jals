//! The native packages this server reads, and why it never runs one.
//!
//! A **native package** is a Java package whose `native` methods are implemented in Rust
//! (`jals-native`). This server wants only the Java half: a project that selected a package writes
//! names into it, and an index that did not hold its declarations would report every one of them
//! as unresolved — an analysis reporting the absence of code the build compiles.
//!
//! A package is one value holding both halves, so getting the Java means constructing the Rust too.
//! What that costs here is a sink per package that discards, and saying so is the point: nothing in
//! a language server instantiates a module, so nothing ever writes through one.

use jals_config::Manifest;
use jals_native::packages::jals_io::{ConsoleSink, JalsIo};
use jals_native::packages::java_base::{JavaBase, Stream, SystemHost};
use jals_native::{NativeRegistry, UnknownNativePackage};

/// `jals.io`'s output, in a host that runs nothing.
struct SilentConsole;

impl ConsoleSink for SilentConsole {
    fn write(&self, _text: &str) {}
}

/// `java.base`'s streams and clock, in a host that runs nothing.
///
/// The clock takes the trait's defaults — zero — for the same reason the sink discards: nothing
/// here instantiates a module, so nothing ever reads either.
struct SilentSystem;

impl SystemHost for SilentSystem {
    fn write(&self, _stream: Stream, _text: &str) {}
}

/// The native packages this server can describe.
pub(crate) struct Natives;

impl Natives {
    /// The Java each package `[build] native-packages` selected publishes.
    ///
    /// An unknown name yields nothing rather than a failure. Every other analysis input this
    /// server cannot resolve degrades the same way — an unbuilt dependency, a classpath entry that
    /// is not there — and a language server that stopped indexing a project over one misspelled
    /// package name would be the one input that turns a typo into no diagnostics at all. `jals
    /// build` is where the same name is an error.
    pub(crate) fn layout_sources(manifest: &Manifest) -> Vec<jals_editor::PackageSource> {
        Self::select(manifest).unwrap_or_default()
    }

    fn select(
        manifest: &Manifest,
    ) -> Result<Vec<jals_editor::PackageSource>, UnknownNativePackage> {
        if manifest.build.native_packages.is_empty() {
            return Ok(Vec::new());
        }
        let mut registry = NativeRegistry::new();
        registry.add(JalsIo::package(std::rc::Rc::new(SilentConsole)));
        registry.add(JavaBase::package(std::rc::Rc::new(SilentSystem)));
        Ok(registry
            .select(&manifest.build.native_packages)?
            .sources()
            .map(|(_, source)| jals_editor::PackageSource {
                path: source.path.to_owned(),
                text: source.text.to_owned(),
            })
            .collect())
    }
}
