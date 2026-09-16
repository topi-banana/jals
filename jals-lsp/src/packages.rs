//! The packages this server reads, and why it never runs one.
//!
//! A language server resolves a package's Java so the project's own source resolves against it, and
//! instantiates nothing. So the host it constructs discards every write and has no clock — which is
//! a real host rather than a broken one: there is no stream for a write to reach and no run for a
//! clock to time.
//!
//! # A failed resolution degrades, one name at a time
//!
//! An unknown name yields no *package of that name* instead of an error, which is this host's
//! policy and the opposite of `jals build`'s. Every other analysis input this server cannot resolve
//! degrades the same way — an unbuilt dependency, a missing classpath entry — and a server that
//! stopped indexing a project over one misspelled name would turn a typo into no diagnostics at
//! all.
//!
//! Per name, and that qualifier is the whole property. Degrading through an all-or-nothing
//! selection meant a misspelled `[packages]` key took the platform down with it, so the index had
//! no `java.lang` and every `String` in the project read as an unresolved name — a typo answered
//! with a diagnostic on every line, which is the outcome this policy exists to avoid.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::{PackageSelection, ResolverChain, StaticResolver};
use jals_platform::{JavaBase, SilentHost};

/// The packages this server offers.
pub(crate) struct Packages;

impl Packages {
    /// This project's packages as index inputs, dropping only the names that did not resolve.
    pub(crate) fn layout_sources<S: jals_storage::SourceBackend, C: jals_storage::CacheBackend>(
        manifest: &Manifest,
        storage: &jals_storage::ProjectStorage<S, C>,
    ) -> Vec<jals_editor::PackageSource> {
        let selection = Self::resolve(manifest, storage);
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
    /// `jals lint`'s `App::detached` is the same answer one crate over, for the same reason.
    pub(crate) fn default_sources() -> Vec<jals_editor::PackageSource> {
        let manifest = Manifest::default();
        // The failures are discarded for the reason [`resolve`] discards them: this server has no
        // channel to report a manifest problem on, and there is no manifest here to report about.
        let selection = Self::chain(None)
            .select_reporting(&manifest.package_names())
            .0;
        jals_editor::ProjectLayout::package_sources_of(&selection, manifest.links_packages())
    }

    /// The routes this server consults, in order: what it was built with, then what the project
    /// declared.
    ///
    /// One place, because the route order and the "offer nothing, push nothing" guard are one rule
    /// and a second spelling of either is a route this server consults and `jals build` does not.
    fn chain(declared: Option<jals_native::SourceResolver>) -> ResolverChain {
        let mut builtin = StaticResolver::new("built into `jals`");
        builtin.add(JavaBase::package(Rc::new(SilentHost)));
        let chain = ResolverChain::new().push(Box::new(builtin));
        match declared {
            Some(project) if !project.is_empty() => chain.push(Box::new(project)),
            _ => chain,
        }
    }

    /// Every package this project resolves: what this server was built with, then what the project
    /// declared in `[packages]`.
    ///
    /// A package the project declared but whose Java cannot be read is dropped along with its
    /// warning, and the name then fails to resolve like any other unknown one — which this host
    /// degrades on rather than reports, for the reason the module docs give.
    fn resolve<S: jals_storage::SourceBackend, C: jals_storage::CacheBackend>(
        manifest: &Manifest,
        storage: &jals_storage::ProjectStorage<S, C>,
    ) -> PackageSelection {
        let names = manifest.package_names();
        if names.is_empty() {
            return PackageSelection::empty();
        }
        let (declared, _) =
            jals_editor::packages::ProjectPackages::resolver(storage, &manifest.packages);
        // The failures are discarded and the successes kept: this server has no channel to report
        // a manifest problem on, and the name that failed is already absent from the index, which
        // is what a reference into it reports.
        Self::chain(Some(declared)).select_reporting(&names).0
    }
}
