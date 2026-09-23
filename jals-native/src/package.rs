//! What a Java package *is*: the Java it publishes (or the declarations it states), and the Rust
//! behind that Java's `native` methods.
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
//!
//! # Two kinds of Java, and why the distinction is not a fidelity
//!
//! A package publishes some Java that has bodies and, often, some that does not — `java.lang.Object`
//! has no body it *could* have on a target where it is the engine's own reference type, and a
//! container nobody has implemented yet is a declaration and no more. [`SourceKind`] says which a
//! file is. It is a fact about the text and nothing else.
//!
//! How an *index* should read that text is a different question with a different answer, and it
//! belongs to whoever knows what this build links: the same `String.java` is the code that will run
//! for a project compiling it into its own module, and a record of a JDK that will supply the real
//! thing for every other project. That answer is `jals_hir::LibraryFidelity`, and deliberately not
//! a second enum here — a package author states what they wrote, never how somebody else's build
//! should treat it.

use alloc::borrow::{Cow, ToOwned};
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::host::{HostObjects as _, NativeHost};
use crate::value::{Args, HostId, NativeError, NativeValue, Provenance, Results};

/// Whether one published compilation unit carries bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceKind {
    /// Declarations only — no method bodies, and no `native` method either.
    ///
    /// A file like this is never compiled into anything: there is nothing to lower. It exists so a
    /// type the package cannot implement is still *nameable* — the engine's own root reference
    /// type, a container a later version will fill in — which is what keeps an editor resolving
    /// `Object` for a project whose backend has no `java.base` at all.
    Signatures,
    /// Real Java: bodies, and `native` methods bound by this package's Rust half.
    Implementation,
}

/// One Java compilation unit a package publishes.
///
/// [`Cow`] rather than `&'static str` or `String`, because both routes a package arrives by are
/// real and they want opposite things. A package compiled into the binary holds `include_str!`
/// text, which is `'static` and must not be copied — the platform is fifty files, re-cloned every
/// time a language server rebuilds its index. A package a *project* declares holds text somebody
/// read off disk a moment ago, which cannot be `'static` at all. Borrowing where it can and owning
/// where it must is the only shape that serves both without one of them paying for the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaSource {
    /// The logical path, used in diagnostics and to order the unit deterministically.
    pub path: Cow<'static, str>,
    /// The Java itself.
    pub text: Cow<'static, str>,
    /// Whether this unit carries bodies.
    pub kind: SourceKind,
}

/// The Rust half of one `native` method.
///
/// `Rc<dyn Fn>` rather than a function pointer, so a binding can capture the host state it writes
/// through — every runtime in this workspace is current-thread, so an `Rc<RefCell<_>>` in a
/// closure is the ordinary way to hold one and nothing here needs `Send`.
pub type NativeFn =
    Rc<dyn Fn(&mut dyn NativeHost, Args<'_>, Results<'_>) -> Result<(), NativeError>>;

/// A Java package: the Java it publishes, the declarations it states as data, and the Rust
/// implementing that Java's `native` methods.
#[derive(Clone)]
pub struct JavaPackage {
    name: String,
    version: u32,
    sources: Vec<JavaSource>,
    declarations: Vec<crate::declaration::DeclaredType>,
    bindings: BTreeMap<(String, String), NativeFn>,
}

impl JavaPackage {
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
            declarations: Vec::new(),
            bindings: BTreeMap::new(),
        }
    }

    /// Publish one Java compilation unit.
    pub fn source(
        &mut self,
        path: impl Into<Cow<'static, str>>,
        text: impl Into<Cow<'static, str>>,
        kind: SourceKind,
    ) -> &mut Self {
        self.sources.push(JavaSource {
            path: path.into(),
            text: text.into(),
            kind,
        });
        self
    }

    /// Publish one declaration-only unit — `Object`, an interface, a container no one has built.
    pub fn signature(
        &mut self,
        path: impl Into<Cow<'static, str>>,
        text: impl Into<Cow<'static, str>>,
    ) -> &mut Self {
        self.source(path, text, SourceKind::Signatures)
    }

    /// Publish one unit whose bodies are compiled and whose `native` methods this package binds.
    pub fn implementation(
        &mut self,
        path: impl Into<Cow<'static, str>>,
        text: impl Into<Cow<'static, str>>,
    ) -> &mut Self {
        self.source(path, text, SourceKind::Implementation)
    }

    /// State one type's API as data, with no Java text.
    ///
    /// The declaration is what an index reads when this package is resolved for analysis, and it is
    /// the package author's statement of the complete member set — an absence is an absence, not an
    /// implied default. A unit with method bodies cannot be expressed this way; it publishes Java
    /// through [`implementation`](Self::implementation) and the compiler lowers the bodies.
    pub fn declare(&mut self, ty: crate::declaration::DeclaredType) -> &mut Self {
        self.declarations.push(ty);
        self
    }

    /// The declarations this package states, in declaration order.
    pub(crate) fn declarations(&self) -> &[crate::declaration::DeclaredType] {
        &self.declarations
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

    /// Register the Rust type behind a Java class whose instances carry this package's handle.
    ///
    /// The Java side owns a field holding an `int` this registered Rust object is addressed by:
    /// a `static native int allocate()` fills it in a constructor, and every other `native`
    /// method receives it as argument 0. The returned builder binds those methods to `&mut T`
    /// closures, so a package author writes the Java declaration once and never touches a handle
    /// or an `Args` index for the receiver.
    pub fn native_class<T: 'static>(&mut self, owner: &str) -> NativeClass<'_, T> {
        NativeClass {
            package: self,
            owner: owner.to_owned(),
            _marker: PhantomData,
        }
    }

    /// The package's name, which is what a manifest's `[build] platform` or
    /// `[build] native-packages` names.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The package author's version.
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The Java this package publishes, in declaration order, both kinds interleaved as declared.
    pub fn sources(&self) -> &[JavaSource] {
        &self.sources
    }

    /// Every binding, as `(owner, signature, implementation)`, in key order.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &str, &NativeFn)> {
        self.bindings
            .iter()
            .map(|((owner, signature), binding)| (owner.as_str(), signature.as_str(), binding))
    }

    /// How many `native` methods this package binds.
    ///
    /// Published because it is the one number a package's own prose is most likely to state and
    /// least likely to keep true.
    pub fn binding_count(&self) -> usize {
        self.bindings.len()
    }

    /// Everything a consumer's cache key has to observe about this package.
    ///
    /// The name, the version, every source path with its kind and text, and every binding key. Not
    /// the binding *bodies* — see [`new`](Self::new) for why that is the version's job.
    pub(crate) fn describe(&self, provenance: &mut Provenance) {
        provenance.field(self.name.as_bytes());
        provenance.number(self.version);
        provenance.number(u32::try_from(self.sources.len()).unwrap_or(u32::MAX));
        for source in &self.sources {
            provenance.field(source.path.as_bytes());
            // The kind decides whether this unit is compiled at all, so two selections that differ
            // only in it produce two different artifacts and must not share a cache entry.
            provenance.number(match source.kind {
                SourceKind::Signatures => 0,
                SourceKind::Implementation => 1,
            });
            provenance.field(source.text.as_bytes());
        }
        provenance.number(u32::try_from(self.bindings.len()).unwrap_or(u32::MAX));
        for (owner, signature) in self.bindings.keys() {
            provenance.field(owner.as_bytes());
            provenance.field(signature.as_bytes());
        }
        // The declarations are the other half of the API, and a cache key has to see them too: two
        // packages that differ only in a declared parameter type compile to different modules.
        provenance.number(u32::try_from(self.declarations.len()).unwrap_or(u32::MAX));
        for declaration in &self.declarations {
            declaration.describe(provenance);
        }
    }
}

impl core::fmt::Debug for JavaPackage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JavaPackage")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("sources", &self.sources)
            .field("declarations", &self.declarations)
            .field("bindings", &self.bindings.keys())
            .finish()
    }
}

/// The typed registration of one handle-carrying Java class.
///
/// Built by [`JavaPackage::native_class`]. `T` is the Rust object a Java instance of the class
/// stands for — the platform's `ArrayList` is a `Vec<HostValue>` — and each binding takes it by
/// `&mut` while the host has it out of its table.
///
/// The convention the three primitives share: the Java declaration owns an `int` field, a
/// `static native int allocate()` fills it from [`allocate`](Self::allocate), and every other
/// `native` method is a `static` one whose **first parameter is that handle** — Java passes it
/// explicitly, because the host cannot read a field. If the declaration named the handle
/// anything else, the import would not link; if a method's first parameter were not the handle,
/// the binding would read the wrong argument and say so.
pub struct NativeClass<'p, T> {
    package: &'p mut JavaPackage,
    owner: String,
    _marker: PhantomData<fn() -> T>,
}

impl<T: 'static> NativeClass<'_, T> {
    /// Bind the `static native int allocate()` a constructor calls.
    ///
    /// `f` builds the Rust object — an empty `Vec`, a table, a cursor — and the id the host gives
    /// it back is the result. The Java field starts at zero, so an object whose constructor did
    /// not call `allocate` is a handle that names nothing, reported when a method reads it.
    pub fn allocate<F>(&mut self, signature: &str, f: F) -> &mut Self
    where
        F: Fn(Args<'_>) -> Result<T, NativeError> + 'static,
    {
        let allocation = Rc::new(f);
        self.bind(signature, move |host, args, mut results| {
            let object = allocation(args)?;
            let id = host.put(object)?;
            let handle = i32::try_from(id.raw())
                .map_err(|_| NativeError::Message(String::from("the host table is full")))?;
            results.set(0, NativeValue::I32(handle));
            Ok(())
        });
        self
    }

    /// Bind one instance method, over the object its handle names.
    ///
    /// Argument 0 of the Java call is the handle and is consumed here; `f` sees the rest of the
    /// arguments at their declared positions minus one. The object is taken out of the host table
    /// for the call and put back whatever `f` answered, so a refusal mid-mutation keeps the
    /// object rather than dropping it.
    pub fn method<F>(&mut self, signature: &str, f: F) -> &mut Self
    where
        F: Fn(&mut T, &mut dyn NativeHost, Args<'_>, Results<'_>) -> Result<(), NativeError>
            + 'static,
    {
        let implementation = Rc::new(f);
        self.bind(signature, move |host, args, results| {
            let id = Self::receiver(&args)?;
            let mut object = host.take::<T>(id)?;
            let arguments = Args::new(args.values().get(1..).unwrap_or_default());
            let outcome = implementation(&mut object, host, arguments, results);
            host.put_back(id, object)?;
            outcome
        });
        self
    }

    /// Bind the `static native void release(int handle)` a Java method calls to free an instance.
    ///
    /// Nothing forces a class to have one — the host's whole table is dropped when the run ends —
    /// but a program that allocates in a loop wants the object gone before then.
    pub fn release(&mut self, signature: &str) -> &mut Self {
        self.bind(signature, |host, args, _results| {
            host.object_drop(Self::receiver(&args)?)
        });
        self
    }

    /// The one place a binding is installed, so every primitive has the same key shape.
    fn bind<F>(&mut self, signature: &str, f: F)
    where
        F: Fn(&mut dyn NativeHost, Args<'_>, Results<'_>) -> Result<(), NativeError> + 'static,
    {
        self.package.bind(&self.owner, signature, f);
    }

    /// The handle a typed method's argument 0 carries.
    ///
    /// A negative or absent one is [`NativeError::UnknownHandle`] with the value it read, because
    /// "the constructor did not run" and "the field was overwritten" are the same problem at this
    /// point and the same fix: the Java side must pass a handle `allocate` returned.
    fn receiver(args: &Args<'_>) -> Result<HostId, NativeError> {
        let raw = args.i32(0)?;
        let handle = u32::try_from(raw).map_err(|_| NativeError::UnknownHandle {
            id: raw.cast_unsigned(),
        })?;
        Ok(HostId::new(handle))
    }
}
