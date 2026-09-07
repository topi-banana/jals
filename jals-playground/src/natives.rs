//! The native packages this tab offers, and where their output goes.
//!
//! A **native package** is a Java package whose `native` methods are implemented in Rust
//! (`jals-native`), and which packages exist is a property of the *binary*: this one ships the same
//! two the CLI does, over different sinks. There is no console in a browser tab, so each sink
//! captures what a module wrote and [`Natives::take_console`] hands it to the Run pane — which is
//! also why the capture is drained rather than read: a second run must not replay the first one's
//! output.
//!
//! `System.out` and `System.err` are joined into that one pane, and deliberately: a tab has one
//! place to show text, so keeping them apart here would mean choosing which of the two to drop.

use std::rc::Rc;

use jals_config::Manifest;
use jals_native::packages::jals_io::{CapturedConsole, JalsIo};
use jals_native::packages::java_base::{CapturedSystem, JavaBase};
use jals_native::{NativePackageSet, NativeRegistry};

/// The registry this tab was built with, plus the consoles its packages write into.
pub struct Natives {
    registry: NativeRegistry,
    console: Rc<CapturedConsole>,
    system: Rc<CapturedSystem>,
}

impl Natives {
    /// Build the registry once per tab.
    #[must_use]
    pub fn new() -> Self {
        let console = Rc::new(CapturedConsole::new());
        let system = Rc::new(CapturedSystem::new());
        let mut registry = NativeRegistry::new();
        registry.add(JalsIo::package(Rc::clone(&console) as Rc<_>));
        registry.add(JavaBase::package(Rc::clone(&system) as Rc<_>));
        Self {
            registry,
            console,
            system,
        }
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
    ///
    /// Both packages' output, in the order the panes have to show it: `jals.io`'s, then
    /// `java.base`'s `System.out`, then its `System.err`. A run writes through at most one of
    /// them, so the join is what lets the caller stay unaware of which package a project selected.
    #[must_use]
    pub fn take_console(&self) -> String {
        let mut text = self.console.take();
        text.push_str(&self.system.take_out());
        text.push_str(&self.system.take_err());
        text
    }
}

impl Default for Natives {
    fn default() -> Self {
        Self::new()
    }
}
