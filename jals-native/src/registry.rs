//! Which packages a binary offers, and which of them one project selected.
//!
//! The two are deliberately different values. A [`NativeRegistry`] is what the *binary* was built
//! with — `jals` ships one, the browser playground ships another, and a program embedding this
//! toolchain builds its own, which is the whole "register a package from Rust" story. A
//! [`NativePackageSet`] is what one project's `[build] native-packages` selected out of it, and
//! it is the value that travels: into the compile as extra Java, into the cache key as
//! provenance, and into the runner as a binding table.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::package::{NativeFn, NativePackage, NativeSource};
use crate::value::Provenance;

/// A name `[build] native-packages` asked for that this binary does not offer.
///
/// Carries what *is* offered, for the same reason
/// `jals_build::WasmRunError::NoSuchExport` carries the export names: the set is fixed when the
/// binary is built, so listing it is the only way a reader can tell a typo from a package that
/// is simply not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownNativePackage {
    /// The name that was asked for.
    pub name: String,
    /// Every name this registry does offer, in order.
    pub available: Vec<String>,
}

impl fmt::Display for UnknownNativePackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "there is no native package named `{}`", self.name)?;
        if self.available.is_empty() {
            return f.write_str("; this build offers none");
        }
        f.write_str("; this build offers ")?;
        for (position, name) in self.available.iter().enumerate() {
            if position > 0 {
                f.write_str(", ")?;
            }
            f.write_str(name)?;
        }
        Ok(())
    }
}

impl core::error::Error for UnknownNativePackage {}

/// The packages one binary offers, by name.
#[derive(Debug, Default, Clone)]
pub struct NativeRegistry {
    packages: BTreeMap<String, Rc<NativePackage>>,
}

impl NativeRegistry {
    /// An empty registry, for a host that ships no package of its own.
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer `package` under its own name, replacing one already offered under it.
    pub fn add(&mut self, package: NativePackage) -> &mut Self {
        self.packages
            .insert(package.name().to_owned(), Rc::new(package));
        self
    }

    /// Every name this registry offers, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.packages.keys().map(String::as_str)
    }

    /// Resolve what a manifest asked for.
    ///
    /// Order follows `names`, not the manifest: a selection is a *set*, and letting the manifest's
    /// order through would make two manifests that select the same packages produce two cache
    /// keys and two module layouts.
    pub fn select(&self, names: &[String]) -> Result<NativePackageSet, UnknownNativePackage> {
        let mut wanted = BTreeMap::new();
        for name in names {
            let package = self
                .packages
                .get(name.as_str())
                .ok_or_else(|| UnknownNativePackage {
                    name: name.clone(),
                    available: self.names().map(str::to_owned).collect(),
                })?;
            wanted.insert(name.clone(), Rc::clone(package));
        }
        Ok(NativePackageSet {
            packages: wanted.into_values().collect(),
        })
    }
}

/// The packages one project selected.
#[derive(Debug, Default, Clone)]
pub struct NativePackageSet {
    packages: Vec<Rc<NativePackage>>,
}

impl NativePackageSet {
    /// The selection of a project that asked for none.
    pub const fn empty() -> Self {
        Self {
            packages: Vec::new(),
        }
    }

    /// Whether nothing was selected — the ordinary case, and the one where every step below is a
    /// no-op.
    pub const fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// The selected package names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.packages.iter().map(|package| package.name())
    }

    /// Every Java compilation unit the selection publishes, as `(package name, source)`.
    ///
    /// This is what joins the compile: `jals-javac`'s wasm backend lays these classes out and
    /// lowers their bodies exactly as it does the project's own, and `jals-hir` indexes them so
    /// the project's source resolves against them.
    pub fn sources(&self) -> impl Iterator<Item = (&str, &NativeSource)> {
        self.packages
            .iter()
            .flat_map(|package| package.sources().iter().map(|src| (package.name(), src)))
    }

    /// The binding table the runner links a module's imports against.
    pub fn bindings(&self) -> NativeBindings {
        let mut table: BTreeMap<String, BTreeMap<String, NativeFn>> = BTreeMap::new();
        for package in &self.packages {
            for (owner, signature, binding) in package.bindings() {
                table
                    .entry(owner.to_owned())
                    .or_default()
                    .insert(signature.to_owned(), Rc::clone(binding));
            }
        }
        NativeBindings { table }
    }

    /// Everything a consumer's cache key has to observe about this selection.
    pub fn provenance(&self) -> Vec<u8> {
        let mut provenance = Provenance::new();
        provenance.number(u32::try_from(self.packages.len()).unwrap_or(u32::MAX));
        for package in &self.packages {
            package.describe(&mut provenance);
        }
        provenance.into_bytes()
    }
}

/// Every `native` method implementation the selection supplies, by import key.
///
/// Nested by owner rather than keyed on a joined string, because that is the shape a lookup is:
/// the runner walks the module's import section, which already hands it the two halves apart, and
/// a joined key would make every one of those lookups allocate the joined form first.
#[derive(Default, Clone)]
pub struct NativeBindings {
    table: BTreeMap<String, BTreeMap<String, NativeFn>>,
}

impl fmt::Debug for NativeBindings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.keys()).finish()
    }
}

impl NativeBindings {
    /// An empty table, for a run that selected no package.
    pub const fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    /// Whether nothing is bound.
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// The implementation of one import, if this selection supplies it.
    pub fn get(&self, owner: &str, signature: &str) -> Option<&NativeFn> {
        self.table.get(owner)?.get(signature)
    }

    /// Every key, as `owner` and `signature`, in order — what an unresolved import lists.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> {
        self.table.iter().flat_map(|(owner, methods)| {
            methods
                .keys()
                .map(move |signature| (owner.as_str(), signature.as_str()))
        })
    }
}
