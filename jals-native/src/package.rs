//! What a native package *is*: the Java it publishes, and the Rust behind that Java's `native`
//! methods.
//!
//! One value holds both halves, and one crate owns that value. That is the whole reason this is
//! not a `[dependencies]` entry pointing at Java somewhere and a host table registered somewhere
//! else: two artifacts can disagree about a signature, and one cannot.
//!
//! # The one place the halves are checked against each other
//!
//! A binding is keyed by the class's **internal name** and by the method's **name with its
//! descriptor** — `("jals/io/Out", "writeChars([CII)V")`. Those are exactly the two strings
//! `jals-javac`'s wasm backend writes into the module's import section for the same declaration.
//! So a Rust half that spells the signature differently does not produce a *type* mismatch that
//! has to be diagnosed: it produces an import nothing satisfies, refused when the module is
//! instantiated with both spellings in hand.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::host::NativeHost;
use crate::value::{Args, NativeError, Provenance, Results};

/// One Java compilation unit a package publishes.
///
/// `'static` text rather than a `String`: a package's Java is compiled into the binary that ships
/// it, exactly as `jals-hir`'s standard-library stubs are, so there is no I/O and nothing to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSource {
    /// The logical path, used in diagnostics and to order the unit deterministically.
    pub path: &'static str,
    /// The Java itself.
    pub text: &'static str,
}

/// The Rust half of one `native` method.
///
/// `Rc<dyn Fn>` rather than a function pointer, so a binding can capture the host state it writes
/// through — every runtime in this workspace is current-thread, so an `Rc<RefCell<_>>` in a
/// closure is the ordinary way to hold one and nothing here needs `Send`.
pub type NativeFn =
    Rc<dyn Fn(&mut dyn NativeHost, Args<'_>, Results<'_>) -> Result<(), NativeError>>;

/// A Java package whose `native` methods are implemented in Rust.
#[derive(Clone)]
pub struct NativePackage {
    name: String,
    version: u32,
    sources: Vec<NativeSource>,
    /// The precompiled module this package's Java runs from, when it ships one instead of the
    /// Java being lowered into the consumer.
    ///
    /// The two routes are alternatives on one package, not two definitions of one type: a package
    /// whose Java is compiled by its consumer (`sources`) and a package whose Java was compiled
    /// once into a module (`library`) are two answers to "where does the implementation run", and
    /// the package's *bindings* stay regardless — a module's `native` methods still reach the
    /// host through the same table.
    library: Option<&'static [u8]>,
    bindings: BTreeMap<(String, String), NativeFn>,
}

impl NativePackage {
    /// A package named `name`, at `version`.
    ///
    /// `version` is the package author's, and it exists for the reason
    /// `jals_frontend::FrontendCaps::version` does: a consumer memoizes a compile against
    /// everything it observed, and the bodies of Rust closures are the one input it cannot
    /// observe. Bump it whenever a binding starts answering differently for input that did not
    /// change — otherwise a warm cache serves the previous answer and the fix is invisible.
    pub fn new(name: &str, version: u32) -> Self {
        Self {
            name: name.to_owned(),
            version,
            sources: Vec::new(),
            library: None,
            bindings: BTreeMap::new(),
        }
    }

    /// Ship the package's Java as a **precompiled module** instead of sources.
    ///
    /// The bytes are the whole artifact: a module the wasm backend emitted, carrying the
    /// `jals.library` section that states its API and its types. A consumer indexes the Java the
    /// section publishes, replays the types, and links the module at instantiation — so the
    /// package's `sources` are not lowered, and a package that ships a library should declare
    /// none (the section's text is the API).
    ///
    /// `'static` for the same reason [`NativeSource`] is: the module is compiled into the binary
    /// that ships it, so there is no I/O and nothing to fail.
    pub const fn library(&mut self, bytes: &'static [u8]) -> &mut Self {
        self.library = Some(bytes);
        self
    }

    /// The precompiled module, when this package ships one.
    pub const fn wasm_library(&self) -> Option<&'static [u8]> {
        self.library
    }

    /// Publish one Java compilation unit.
    pub fn source(&mut self, path: &'static str, text: &'static str) -> &mut Self {
        self.sources.push(NativeSource { path, text });
        self
    }

    /// Bind one `native` method.
    ///
    /// `owner` is the declaring class's internal name (`jals/io/Out`) and `signature` is the
    /// method's name followed by its JVM descriptor (`writeChars([CII)V`) — the two strings the
    /// wasm backend writes into the import section for that declaration, which is what makes a
    /// disagreement an unresolved import rather than a silent mismatch.
    pub fn bind<F>(&mut self, owner: &str, signature: &str, binding: F) -> &mut Self
    where
        F: Fn(&mut dyn NativeHost, Args<'_>, Results<'_>) -> Result<(), NativeError> + 'static,
    {
        self.bindings
            .insert((owner.to_owned(), signature.to_owned()), Rc::new(binding));
        self
    }

    /// The package's name, which is what a manifest's `[build] native-packages` lists.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The package author's version.
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The Java this package publishes, in declaration order.
    pub fn sources(&self) -> &[NativeSource] {
        &self.sources
    }

    /// Every binding, as `(owner, signature, implementation)`, in key order.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &str, &NativeFn)> {
        self.bindings
            .iter()
            .map(|((owner, signature), binding)| (owner.as_str(), signature.as_str(), binding))
    }

    /// Everything a consumer's cache key has to observe about this package.
    ///
    /// The name, the version, every source path and its text, and every binding key. Not the
    /// binding *bodies* — see [`new`](Self::new) for why that is the version's job.
    pub fn describe(&self, provenance: &mut Provenance) {
        provenance.field(self.name.as_bytes());
        provenance.number(self.version);
        provenance.number(u32::try_from(self.sources.len()).unwrap_or(u32::MAX));
        for source in &self.sources {
            provenance.field(source.path.as_bytes());
            provenance.field(source.text.as_bytes());
        }
        provenance.number(u32::try_from(self.bindings.len()).unwrap_or(u32::MAX));
        for (owner, signature) in self.bindings.keys() {
            provenance.field(owner.as_bytes());
            provenance.field(signature.as_bytes());
        }
        // A module's bytes are an input like a source's text: a different module is a different
        // artifact, down to the link.
        match self.library {
            Some(bytes) => {
                provenance.number(1);
                provenance.field(bytes);
            }
            None => {
                provenance.number(0);
            }
        }
    }
}

impl core::fmt::Debug for NativePackage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NativePackage")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("sources", &self.sources)
            .field("bindings", &self.bindings.keys())
            .field(
                "library",
                &self
                    .library
                    .map(|bytes| alloc::format!("{} bytes", bytes.len())),
            )
            .finish()
    }
}
