//! The native packages this tab offers, and where their output goes.
//!
//! A **native package** is a Java package whose `native` methods are implemented in Rust
//! (`jals-native`), and which packages exist is a property of the *binary*: this one ships the same
//! `jals.io` the CLI does, over a different sink. There is no console in a browser tab, so the sink
//! captures what a module wrote and [`Natives::take_console`] hands it to the Run pane — which is
//! also why the capture is drained rather than read: a second run must not replay the first one's
//! output.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::packages::jals_io::{CapturedConsole, JalsIo};
use jals_native::{NativePackageSet, NativeRegistry};

/// The registry this tab was built with, plus the console its `jals.io` writes into.
pub struct Natives {
    registry: NativeRegistry,
    console: Rc<CapturedConsole>,
}

impl Natives {
    /// Build the registry once per tab.
    #[must_use]
    pub fn new() -> Self {
        let console = Rc::new(CapturedConsole::new());
        let mut registry = NativeRegistry::new();
        registry.add(JalsIo::package(Rc::clone(&console) as Rc<_>));
        Self { registry, console }
    }

    /// What `[build] native-packages` selected, or a message naming what this tab offers.
    ///
    /// # Errors
    /// A name this build does not register.
    pub fn select(&self, manifest: &Manifest) -> Result<NativePackageSet, String> {
        if manifest.build.native_packages.is_empty() {
            return Ok(NativePackageSet::empty());
        }
        self.registry
            .select(&manifest.build.native_packages)
            .map_err(|unknown| format!("`[build] native-packages`: {unknown}"))
    }

    /// Everything written to the console since the last call, and empties it.
    #[must_use]
    pub fn take_console(&self) -> String {
        self.console.take()
    }
}

impl Default for Natives {
    fn default() -> Self {
        Self::new()
    }
}
