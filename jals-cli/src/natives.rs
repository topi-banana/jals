//! The native packages this binary offers, and where their output goes.
//!
//! A **native package** is a Java package whose `native` methods are implemented in Rust
//! (`jals-native`). Which packages exist is a property of the *binary* rather than of a manifest —
//! `jals` ships the set below, the browser playground ships its own, and a program embedding this
//! toolchain registers whatever it likes — so a `[build] native-packages` name that is not here is
//! reported here, with the names that are.
//!
//! # Why the registry is built per run rather than once
//!
//! `jals.io` writes text, and this crate has exactly one thing allowed to write to a stream. So the
//! package is constructed *over* the run's [`Shell`]: the sink is a value the host supplies, which
//! is the shape every stateful native package has and the reason a package is a `NativePackage`
//! rather than a `const`.

use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Result, bail};
use jals_config::Manifest;
use jals_native::packages::jals_io::{ConsoleSink, JalsIo};
use jals_native::{NativePackageSet, NativeRegistry};

use crate::shell::Shell;

/// `jals.io`'s output, written through the one thing in this crate that touches a stream.
struct ShellConsole {
    shell: Arc<Shell>,
}

impl ConsoleSink for ShellConsole {
    fn write(&self, text: &str) {
        // Stdout, and machine output rather than a status line: this is the *program's* output,
        // the wasm counterpart of what a `java` child writes when it inherits the stream. It goes
        // through `machine_bytes` rather than `machine` because a module decides where its own
        // line breaks are — `Out.println` writes one, and a second one added here would double
        // every line.
        let _ = self.shell.machine_bytes(text.as_bytes());
    }
}

/// The native packages this binary offers.
pub(crate) struct Natives;

impl Natives {
    /// Build the registry for one run.
    fn registry(shell: &Arc<Shell>) -> NativeRegistry {
        let mut registry = NativeRegistry::new();
        registry.add(JalsIo::package(Rc::new(ShellConsole {
            shell: Arc::clone(shell),
        })));
        registry
    }

    /// What `[build] native-packages` selected, or a failure naming what this binary offers.
    ///
    /// The empty selection — every project that names none — costs one allocation and reaches
    /// every step below as "this module needs nothing from the host", which is the same thing
    /// those steps do for a project with no import section.
    pub(crate) fn select(shell: &Arc<Shell>, manifest: &Manifest) -> Result<NativePackageSet> {
        if manifest.build.native_packages.is_empty() {
            return Ok(NativePackageSet::empty());
        }
        match Self::registry(shell).select(&manifest.build.native_packages) {
            Ok(selection) => Ok(selection),
            Err(unknown) => bail!("`[build] native-packages`: {unknown}"),
        }
    }
}
