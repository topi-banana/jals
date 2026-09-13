//! The packages this server reads, and why it never runs one.
//!
//! A language server resolves a package's Java so the project's own source resolves against it, and
//! instantiates nothing. So the host it constructs discards every write and has no clock — which is
//! a real host rather than a broken one: there is no stream for a write to reach and no run for a
//! clock to time.
//!
//! # A failed resolution degrades rather than fails
//!
//! An unknown name yields no packages instead of an error, which is this host's policy and the
//! opposite of `jals build`'s. Every other analysis input this server cannot resolve degrades the
//! same way — an unbuilt dependency, a missing classpath entry — and a server that stopped indexing
//! a project over one misspelled name would turn a typo into no diagnostics at all.
//!
//! The cost is worth naming: with the platform unresolved, the index has no `java.lang` and every
//! `String` reads as an unresolved name. That is a worse failure than a missing third-party
//! package, and it is still better than none of the file being analysed.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::{PackageSelection, ResolverChain, StaticResolver};
use jals_platform::{JavaBase, SilentHost};

/// The packages this server offers.
pub(crate) struct Packages;

impl Packages {
    /// This project.s packages as index inputs, degrading to none on a failure.
    pub(crate) fn layout_sources<S: jals_storage::SourceBackend, C: jals_storage::CacheBackend>(
        manifest: &Manifest,
        storage: &jals_storage::ProjectStorage<S, C>,
    ) -> Vec<jals_editor::PackageSource> {
        let selection = Self::resolve(manifest, storage).unwrap_or_default();
        jals_editor::ProjectLayout::package_sources_of(&selection, manifest.links_packages())
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
    ) -> Option<PackageSelection> {
        let names = manifest.package_names();
        if names.is_empty() {
            return Some(PackageSelection::empty());
        }
        let mut builtin = StaticResolver::new("built into `jals`");
        builtin.add(JavaBase::package(Rc::new(SilentHost)));
        let (declared, _) =
            jals_editor::packages::ProjectPackages::resolver(storage, &manifest.packages);
        let chain = ResolverChain::new().push(Box::new(builtin));
        let chain = if declared.is_empty() {
            chain
        } else {
            chain.push(Box::new(declared))
        };
        chain.select(&names).ok()
    }
}
