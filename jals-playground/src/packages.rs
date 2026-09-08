//! The packages this tab offers, and where their output goes.
//!
//! A **package** is Java plus the Rust behind that Java's `native` methods (`jals-native`), and
//! which packages exist is a property of the *binary*: this one ships the same platform library the
//! CLI does, over a different host. There is no console in a browser tab, so the host captures what
//! a module wrote and [`Packages::take_console`] hands it to the Run pane — which is also why the
//! capture is drained rather than read: a second run must not replay the first one's output.
//!
//! # One tab, one host, two consumers
//!
//! The same selection reaches the editor's index and the compile. It used to reach only the
//! compile, so a tab could build and run a program whose every `System.out` was an unresolved name
//! in the editor beside it — the analysis reporting the absence of code the build was compiling.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::{PackageSelection, ResolverChain, StaticResolver};
use jals_platform::{CapturedHost, JavaBase};

/// The packages this tab was built with, plus the host their writes land in.
pub struct Packages {
    platform: Rc<CapturedHost>,
}

impl Packages {
    /// Build the host once per tab.
    #[must_use]
    pub fn new() -> Self {
        Self {
            platform: Rc::new(CapturedHost::new()),
        }
    }

    /// Every package this project resolves, or a message naming what this tab offers.
    ///
    /// # Errors
    /// A name this build does not register.
    /// # The project route is not offered here
    ///
    /// A tab resolves only what it was built with. `[packages]` names Java the *project* ships, and
    /// reading it needs the workspace aggregate — which this tab holds behind an async lock that a
    /// compile deliberately releases before it starts, because a compile is the longest thing it
    /// does. So a declared package is not resolved, and the name comes back as an unknown one
    /// naming what *is* offered.
    ///
    /// That is a refusal rather than a silence, which is the whole reason it is acceptable: the
    /// alternative is a tab that reports every reference into a declared package as an unresolved
    /// name and never says why.
    pub fn select(&self, manifest: &Manifest) -> Result<PackageSelection, String> {
        let names = manifest.package_names();
        if names.is_empty() {
            return Ok(PackageSelection::empty());
        }
        let mut builtin = StaticResolver::new("built into this playground");
        builtin.add(JavaBase::package(Rc::clone(&self.platform) as Rc<_>));
        ResolverChain::new()
            .push(Box::new(builtin))
            .select(&names)
            .map_err(|error| error.to_string())
    }

    /// Everything written to either stream since the last call, and empties both.
    ///
    /// Joined, and the joining is a decision this tab makes rather than one the platform makes for
    /// it: a tab has one place to show text, so keeping the streams apart here would mean choosing
    /// which of the two to drop. `jals run` has two streams and keeps them apart.
    #[must_use]
    pub fn take_console(&self) -> String {
        let mut text = self.platform.take_out();
        text.push_str(&self.platform.take_err());
        text
    }
}

impl Default for Packages {
    fn default() -> Self {
        Self::new()
    }
}
