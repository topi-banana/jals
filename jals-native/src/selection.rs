//! What one project resolved: the packages behind it, and the two questions a consumer asks them.
//!
//! # The two questions, and why they are not the same list
//!
//! **What does this project's analysis index?** Everything, at both kinds. An editor resolves
//! `java.lang.Object` for a project whose backend has no `java.base` and never will, because the
//! type is still nameable in the source it is editing.
//!
//! **What does this build compile?** Only [`SourceKind::Implementation`], and only when the build
//! links packages at all. A signature-only unit has nothing to lower, and a project whose backend
//! links a real JDK compiles none of this.
//!
//! Keeping them apart here is what makes one property structural that was otherwise a rule somebody
//! had to remember: `java.lang.Object` is the wasm backend's own `anyref`, and a declared `Object`
//! would be one question with two answers. It is a signature unit, so [`link_sources`] cannot yield
//! it. No prose required.
//!
//! [`link_sources`]: PackageSelection::link_sources

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::package::{JavaPackage, JavaSource, NativeFn, SourceKind};
use crate::value::Provenance;

/// The packages one project resolved, in package-name order.
#[derive(Debug, Default, Clone)]
pub struct PackageSelection {
    pub(crate) packages: Vec<Rc<JavaPackage>>,
}

impl PackageSelection {
    /// A selection of nothing — the shape a project that resolved no package takes.
    ///
    /// Note what this is *not*: a project with no packages has no `java.lang` either. That is a
    /// real configuration (a module that speaks only in `char[]` and primitives) and not a
    /// degraded one, which is why it has a value rather than being something a host falls back to.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            packages: Vec::new(),
        }
    }

    /// Whether nothing was selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Every selected package's name, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.packages.iter().map(|p| p.name())
    }

    /// Every published unit, of either kind, as `(package name, source)`.
    ///
    /// This is what an *index* takes. How each unit should be read is the caller's to decide from
    /// [`JavaSource::kind`] and from whether this build links packages — see the module docs.
    pub fn analysis_sources(&self) -> impl Iterator<Item = (&str, &JavaSource)> {
        self.packages
            .iter()
            .flat_map(|p| p.sources().iter().map(move |s| (p.name(), s)))
    }

    /// Every unit a compile lowers: [`SourceKind::Implementation`] and nothing else.
    ///
    /// What a backend receives, and the reason a signature-only type cannot accidentally become a
    /// declared one.
    pub fn link_sources(&self) -> impl Iterator<Item = (&str, &JavaSource)> {
        self.analysis_sources()
            .filter(|(_, source)| matches!(source.kind, SourceKind::Implementation))
    }

    /// Every `native` method implementation, by import key.
    pub fn bindings(&self) -> NativeBindings {
        let mut table: BTreeMap<String, BTreeMap<String, NativeFn>> = BTreeMap::new();
        for package in &self.packages {
            for (owner, signature, binding) in package.bindings() {
                table
                    .entry(String::from(owner))
                    .or_default()
                    .insert(String::from(signature), Rc::clone(binding));
            }
        }
        NativeBindings { table }
    }

    /// Everything a consumer's cache key has to observe about this selection.
    #[must_use]
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
    #[must_use]
    pub const fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    /// Whether nothing is bound.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// The implementation of one import, if this selection supplies it.
    #[must_use]
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
