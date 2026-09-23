//! The packages this binary offers, and where their output goes.
//!
//! A **package** is Java plus the Rust behind that Java's `native` methods (`jals-native`). Which
//! packages exist is a property of the *binary* rather than of a manifest — `jals` ships the set
//! `jals-platform`'s [`Builtin::packages`] names, the browser playground ships the same set over a
//! different host, and a program embedding this toolchain registers whatever it likes — so a
//! manifest name that is not here is reported here, with the names that are.
//!
//! # Why the registry is built per run rather than once
//!
//! The platform writes text and reads a clock, and this crate has exactly one thing allowed to
//! write to a stream. So the packages are constructed *over* the run's [`Shell`]: the host state
//! is a value the host supplies, which is the shape every stateful package has and the reason a
//! package is built rather than declared as a `const`.
//!
//! # Every run resolves the platform, whatever the backend is
//!
//! [`resolve`](Packages::resolve) is called for `jals lint` and a `javac` build exactly as it is
//! for a wasm one. What differs is not *whether* `java.lang` is indexed but at what fidelity,
//! which `jals_config::Manifest::links_packages` answers and
//! `jals_editor::ProjectLayout::with_packages` applies. A host that skipped the resolution for a
//! `javac` project would be a host whose every `String` stopped resolving.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use jals_config::Manifest;
use jals_native::{PackageRegistry, PackageSelection};
use jals_platform::{Builtin, PlatformHost, Stream};

use crate::shell::Shell;

/// The platform's streams and clock, over the one thing in this crate that touches a stream.
struct ShellHost {
    shell: Arc<Shell>,
    /// When this run started, so `System.nanoTime` is monotonic within it.
    ///
    /// An `Instant` rather than a wall reading: the JDK's contract is that the origin is arbitrary
    /// and only differences are meaningful, and a wall clock can go backwards.
    started: Instant,
}

impl PlatformHost for ShellHost {
    fn write(&self, stream: Stream, text: &str) {
        match stream {
            // Stdout, and machine output rather than a status line: this is the *program's*
            // output, the wasm counterpart of what a `java` child writes when it inherits the
            // stream. Through `machine_bytes` rather than `machine` because a module decides where
            // its own line breaks are — `println` writes one, and a second added here would double
            // every line.
            Stream::Out => {
                let _ = self.shell.machine_bytes(text.as_bytes());
            }
            // Stderr, for the same reason and with the same byte discipline. A program's
            // diagnostics failing to reach a closed stderr is not a reason to fail its build.
            Stream::Err => self.shell.plain_bytes(text.as_bytes()),
        }
    }

    fn current_time_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .unwrap_or(0)
    }

    fn nano_time(&self) -> i64 {
        i64::try_from(self.started.elapsed().as_nanos()).unwrap_or(i64::MAX)
    }
}

/// The packages this binary offers.
pub(crate) struct Packages;

impl Packages {
    /// The registry for one run: the set `jals-platform` ships, over this run's shell.
    ///
    /// One call, not a package named here. A package added to [`Builtin::packages`] is offered by
    /// `jals build`, `jals run`, `jals test` and `jals lint` at once, and — because the same
    /// `JavaPackage` values are what an index reads — by the language server beside it.
    fn registry(shell: &Arc<Shell>) -> PackageRegistry {
        let mut registry = PackageRegistry::new();
        let host = Rc::new(ShellHost {
            shell: Arc::clone(shell),
            started: Instant::now(),
        });
        for package in Builtin::packages(host) {
            registry.add(package);
        }
        registry
    }

    /// Every package this project resolves, or a failure naming what this binary offers.
    ///
    /// A hard error, which is this host's policy and not the seam's: `jals build` compiles against
    /// what it resolves, so a name it cannot resolve is a build that would produce the wrong thing.
    /// The language server answers the same question and *degrades*, because a typo that stopped
    /// every diagnostic would be worse than one that loses a package.
    pub(crate) fn resolve(shell: &Arc<Shell>, manifest: &Manifest) -> Result<PackageSelection> {
        let names = manifest.package_names();
        if names.is_empty() {
            return Ok(PackageSelection::empty());
        }
        match Self::registry(shell).select(&names) {
            Ok(selection) => Ok(selection),
            Err(error) => bail!("{error}"),
        }
    }

    /// Every package this project resolves, warning about the ones it does not and keeping the
    /// rest.
    ///
    /// `jals lint`'s answer to the same question, and the difference from [`resolve`](Self::resolve)
    /// is the whole reason both exist. Lint is best-effort about every input it cannot resolve, and
    /// an all-or-nothing selection made "best-effort" mean *nothing*: one misspelled name dropped
    /// the platform selected beside it, so a typo in a third-party name turned into a project with
    /// no `java.lang` and an unresolved-name diagnostic on every `String`. The name that failed is
    /// the one that goes missing.
    pub(crate) fn resolve_reporting(shell: &Arc<Shell>, manifest: &Manifest) -> PackageSelection {
        let names = manifest.package_names();
        if names.is_empty() {
            return PackageSelection::empty();
        }
        let (selection, failures) = Self::registry(shell).select_reporting(&names);
        for failure in failures {
            shell.warn(format_args!("{failure}"));
        }
        selection
    }
}
