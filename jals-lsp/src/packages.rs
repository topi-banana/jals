//! The packages this server reads, and why it never runs one.
//!
//! A language server resolves a package's Java so the project's own source resolves against it,
//! and instantiates nothing. So the host it constructs discards every write and has no clock —
//! which is a real host rather than a broken one: there is no stream for a write to reach and no
//! run for a clock to time.
//!
//! # A failed resolution degrades, one name at a time
//!
//! An unknown name yields no *package of that name* instead of an error, which is this host's
//! policy and the opposite of `jals build`'s. Every other analysis input this server cannot
//! resolve degrades the same way — an unbuilt dependency, a missing classpath entry — and a server
//! that stopped indexing a project over one misspelled name would turn a typo into no diagnostics
//! at all.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::{PackageRegistry, PackageSelection};
use jals_platform::{Builtin, SilentHost};

/// The packages this server offers.
pub(crate) struct Packages;

impl Packages {
    /// This project's packages as index inputs, dropping only the names that did not resolve.
    pub(crate) fn layout_sources(manifest: &Manifest) -> Vec<jals_editor::PackageSource> {
        let selection = Self::resolve(manifest);
        jals_editor::ProjectLayout::package_sources_of(&selection, manifest.links_packages())
    }

    /// The same, for a workspace with **no manifest to resolve from**.
    ///
    /// A document under no project, and a project whose manifest is missing or unparsable, are
    /// still Java: they say `String`, and an index built with no library has no `java.lang` at all
    /// — not the type, not the implicit `Object` supertype edge — so every reference into the
    /// standard library would report as an unresolved name. There is no fallback behind the
    /// packages any more, so the defaults answer: the default platform, and no linking.
    ///
    /// `jals lint`'s detached fallback is the same answer one crate over, for the same reason.
    pub(crate) fn default_sources() -> Vec<jals_editor::PackageSource> {
        let manifest = Manifest::default();
        Self::layout_sources(&manifest)
    }

    /// Every package this project resolves: what this server was built with, one name at a time.
    ///
    /// A *selection* failure is discarded and the successes kept: that one is a name this server
    /// does not offer, which is already what a reference into it reports.
    fn resolve(manifest: &Manifest) -> PackageSelection {
        let names = manifest.package_names();
        if names.is_empty() {
            return PackageSelection::empty();
        }
        Self::chain().select_reporting(&names).0
    }

    /// The one route this server consults: what it was built with.
    ///
    /// One call, not a package named here, so a package added to `jals_platform::Builtin::packages`
    /// is offered by `jals build`, `jals lint` and this server at once — the three readers of one
    /// definition, which is what keeps an editor from reporting the absence of code the build
    /// compiles.
    fn chain() -> PackageRegistry {
        let mut registry = PackageRegistry::new();
        for package in Builtin::packages(Rc::new(SilentHost)) {
            registry.add(package);
        }
        registry
    }
}
