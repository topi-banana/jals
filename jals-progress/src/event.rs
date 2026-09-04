//! What a run reports: the identity of one unit of work, what kind of work it is, and how it ended.
//!
//! Everything here is a *fact about work*. None of it is presentation — a terminal verb, a colour,
//! a bar template, and an HTML palette are the consumer's, exactly as `jals-hir` states a fact and
//! the `jals-lint` rule that reports it owns the wording.

use alloc::{
    borrow::ToOwned,
    string::{String, ToString},
};

use serde::Serialize;

/// A unit of work's identity within one run.
///
/// Allocated by [`Progress`](crate::Progress), dense and monotonic, so a consumer can index by it.
/// It is meaningless across runs, which is why nothing persists one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct UnitId(u64);

impl UnitId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw number, for a consumer keying a map on it.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl core::fmt::Display for UnitId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What kind of work a unit is.
///
/// A *name*, not a status verb: `Fetch`, never "Downloading". The terminal's conjugation lives in
/// `jals-cli`, so the same fact can read as `Downloading foo` while it runs and `Downloaded foo`
/// when it ends. [`label`](Self::label) exists only because a written report has to put some word
/// on a row, and one word chosen here beats each consumer inventing its own.
///
/// Every variant has a real emitter. A kind of work nothing performs is a kind of work this enum
/// must not name — `cargo hawk check` reports a published vocabulary no test drives, and a variant
/// nothing constructs is the same defect one level down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activity {
    /// A project's `build.rhai` phase, before its task plan runs.
    Script,
    /// Discovering or preprocessing one node of the dependency graph.
    Resolve,
    /// Acquiring bytes from outside the project — HTTP, `file://`, or a host path.
    Fetch,
    /// Reading a source or nested archive out of a jar.
    Extract,
    /// Rewriting a jar's names through a mapping set.
    Remap,
    /// Folding two jars into one.
    Merge,
    /// Reconstructing Java from class files.
    Decompile,
    /// Writing a produced source tree into a project or an artifact cache.
    Publish,
    /// Reading a classpath into the analysis index.
    Index,
    /// Turning Java into class files or a wasm module.
    Compile,
    /// Packaging compiled output into a jar.
    Package,
    /// Running a program the project produced.
    Run,
    /// Running the project's tests.
    Test,
    /// Formatting source files.
    Format,
    /// Linting source files.
    Lint,
}

impl Activity {
    /// The activity's name, for a row in a written report.
    ///
    /// Not a status verb — see the type's own documentation.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Resolve => "resolve",
            Self::Fetch => "fetch",
            Self::Extract => "extract",
            Self::Remap => "remap",
            Self::Merge => "merge",
            Self::Decompile => "decompile",
            Self::Publish => "publish",
            Self::Index => "index",
            Self::Compile => "compile",
            Self::Package => "package",
            Self::Run => "run",
            Self::Test => "test",
            Self::Format => "format",
            Self::Lint => "lint",
        }
    }
}

/// How a unit of work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The work ran and produced its result.
    Completed,
    /// A memo answered instead, so nothing ran. Cargo's `Fresh`.
    Fresh,
    /// The work was not needed at all — a filter excluded it, a policy declined it.
    Skipped,
    /// The work ran and failed. Emitted explicitly by the failing path.
    Failed,
    /// Nothing said how it ended.
    ///
    /// A [`Task`](crate::Task) dropped without [`finish`](crate::Task::finish) reports this, so a
    /// display never keeps a bar for work that stopped. It means the emitter has a hole in it: an
    /// error path that returns without saying it failed. Reaching it is a bug in the *emitter*,
    /// not in the run.
    Abandoned,
}

impl Outcome {
    /// Whether this outcome means the work actually ran.
    ///
    /// A display that counts "how much did this build do" asks here, so `Fresh` is not scored as
    /// work and a failure is.
    #[must_use]
    pub const fn ran(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// The package a unit of work belongs to.
///
/// `version` is optional because it is optional in `jals.toml`, and because a node with no
/// manifest at all — a plain-source or binary dependency — still has a name worth showing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageRef {
    pub name: String,
    pub version: Option<String>,
}

impl PackageRef {
    /// A package known only by name.
    pub fn unversioned(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
        }
    }

    /// A package with the version its manifest declared, when it declared one.
    pub fn new(name: impl Into<String>, version: Option<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            version: version.map(Into::into),
        }
    }
}

impl core::fmt::Display for PackageRef {
    /// `name v1.2.3`, cargo's spelling — or just the name when there is no version.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match &self.version {
            Some(version) => write!(f, "{} v{version}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// One unit of work, described at the moment it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unit {
    /// The package this work is for, when the emitter knows one. The root project's own work
    /// carries the root package; a fetch nobody has attributed carries `None`.
    pub package: Option<PackageRef>,
    /// What kind of work it is.
    pub activity: Activity,
    /// What the work is on, already in one human-readable line: a URL, a jar name, a file count.
    pub subject: String,
    /// The total this unit counts up to, when it is known before the work starts.
    ///
    /// Bytes for a download that announced a length, entries for an archive, files for a format
    /// run. `None` means the emitter cannot say — a display shows a spinner rather than a bar, and
    /// [`Task::set_total`](crate::Task::set_total) may still fill it in later.
    pub total: Option<u64>,
}

impl Unit {
    /// The unit's subject, prefixed by its package when it has one.
    ///
    /// The line a consumer puts after its verb: `hello v0.1.0`, or `mappings.txt` for work that
    /// belongs to no package.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.package {
            Some(package) if self.subject.is_empty() => package.to_string(),
            Some(package) => {
                let mut described = package.to_string();
                described.push_str(" (");
                described.push_str(&self.subject);
                described.push(')');
                described
            }
            None if self.subject.is_empty() => self.activity.label().to_owned(),
            None => self.subject.clone(),
        }
    }
}

/// What an emitter reports.
///
/// Three events and no more. Anything a consumer wants to know beyond them — how long a unit took,
/// how many ran at once, which package dominated a build — is derivable from the sequence, which
/// is what keeps one stream serving a progress bar, a JSON stream, and a timing report at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// Work began.
    Started { id: UnitId, unit: Unit },
    /// Work progressed. `done` counts toward `total` in whatever the unit counts.
    Advanced {
        id: UnitId,
        done: u64,
        total: Option<u64>,
    },
    /// Work ended, however it ended.
    Finished { id: UnitId, outcome: Outcome },
}

impl Event {
    /// The unit this event is about.
    #[must_use]
    pub const fn id(&self) -> UnitId {
        match self {
            Self::Started { id, .. } | Self::Advanced { id, .. } | Self::Finished { id, .. } => *id,
        }
    }
}
