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
//! Both packages write text, and this crate has exactly one thing allowed to write to a stream. So
//! each is constructed *over* the run's [`Shell`]: the sink is a value the host supplies, which is
//! the shape every stateful native package has and the reason a package is a `NativePackage`
//! rather than a `const`. `java.base` also wants a clock, which is the other thing a WebAssembly
//! module cannot reach on its own, and this is where one exists.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use jals_config::Manifest;
use jals_native::packages::jals_io::{ConsoleSink, JalsIo};
use jals_native::packages::java_base::{JavaBase, Stream, SystemHost};
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

/// `java.base`'s two streams and its clock.
///
/// The split is the whole reason this is not `ShellConsole` with a second method: `System.out` is
/// the program's output and belongs on stdout beside a `java` child's, while `System.err` is its
/// diagnostics and belongs on stderr beside this crate's own — and a host that folded them would
/// put a program's output where a script reading stdout cannot tell the two apart.
struct ShellSystem {
    shell: Arc<Shell>,
    /// When this run started, so that `System.nanoTime` is monotonic and means something as a
    /// difference. A wall clock read twice can go backwards; this cannot.
    started: Instant,
}

impl SystemHost for ShellSystem {
    fn write(&self, stream: Stream, text: &str) {
        match stream {
            Stream::Out => {
                let _ = self.shell.machine_bytes(text.as_bytes());
            }
            Stream::Err => self.shell.plain_bytes(text.as_bytes()),
        }
    }

    fn current_time_millis(&self) -> i64 {
        // A clock reading before the epoch is answered as the epoch rather than as a negative
        // number, which is what a JVM does with one and what `Duration` refuses to represent.
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

/// The native packages this binary offers.
pub(crate) struct Natives;

impl Natives {
    /// Build the registry for one run.
    fn registry(shell: &Arc<Shell>) -> NativeRegistry {
        let mut registry = NativeRegistry::new();
        registry.add(JalsIo::package(Rc::new(ShellConsole {
            shell: Arc::clone(shell),
        })));
        registry.add(JavaBase::package(Rc::new(ShellSystem {
            shell: Arc::clone(shell),
            started: Instant::now(),
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
