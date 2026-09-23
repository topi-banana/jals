//! The packages this tab offers, and where their output goes.
//!
//! A **package** is Java plus the Rust behind that Java's `native` methods (`jals-native`), and
//! which packages exist is a property of the *binary*: this one ships the same platform the CLI
//! does, over a different host. There is no console in a browser tab, so the host captures what a
//! module wrote and [`Packages::take_console`] hands it to the Run pane — which is also why the
//! capture is drained rather than read: a second run must not replay the first one's output.
//!
//! # One tab, one host, two consumers
//!
//! The same selection reaches the editor's index and the compile. It used to reach only the
//! compile, so a tab could build and run a program whose every `System.out` was an unresolved name
//! in the editor beside it — the analysis reporting the absence of code the build was compiling.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::{PackageRegistry, PackageSelection};
use jals_platform::{Builtin, CapturedHost, PlatformHost};

/// The packages this tab was built with, plus the host their writes land in.
pub struct Packages {
    host: Rc<CapturedHost>,
}

impl Packages {
    /// Build the host once per tab.
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: Rc::new(CapturedHost::new()),
        }
    }

    /// Every package this project resolves, or a message naming what this tab offers.
    ///
    /// # Errors
    /// A name this build does not register.
    pub fn select(&self, manifest: &Manifest) -> Result<PackageSelection, String> {
        let names = manifest.package_names();
        if names.is_empty() {
            return Ok(PackageSelection::empty());
        }
        self.registry()
            .select(&names)
            .map_err(|error| error.to_string())
    }

    /// The same selection for the **index**, keeping whatever resolved.
    ///
    /// A compile is all-or-nothing — it produces the wrong module otherwise, which is what
    /// [`select`](Self::select) answers — but the editor beside it is not: a name this tab does not
    /// offer should cost the project that package and nothing else. Resolving the whole list or
    /// none of it meant one such name dropped the platform too, and the pane then reported every
    /// `String` in the sample as an unresolved name with the Build button still explaining why.
    #[must_use]
    pub fn index_sources(&self, manifest: &Manifest) -> PackageSelection {
        let names = manifest.package_names();
        if names.is_empty() {
            return PackageSelection::empty();
        }
        self.registry().select_reporting(&names).0
    }

    /// The one route this tab offers: what it was built with.
    ///
    /// One call, not a package named here, so a package added to `jals_platform::Builtin::packages`
    /// is offered by `jals build`, `jals lint` and this tab at once.
    fn registry(&self) -> PackageRegistry {
        let mut registry = PackageRegistry::new();
        let host = Rc::clone(&self.host) as Rc<dyn PlatformHost>;
        for package in Builtin::packages(host) {
            registry.add(package);
        }
        registry
    }

    /// Everything written to either stream since the last call, and empties both.
    ///
    /// Joined, and the joining is a decision this tab makes rather than one the platform makes for
    /// it: a tab has one place to show text, so keeping the streams apart here would mean choosing
    /// which of the two to drop. `jals run` has two streams and keeps them apart.
    #[must_use]
    pub fn take_console(&self) -> String {
        let mut text = self.host.take_out();
        text.push_str(&self.host.take_err());
        text
    }
}

impl Default for Packages {
    fn default() -> Self {
        Self::new()
    }
}
