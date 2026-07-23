//! The compile backend seam: frontend output in, compiled classes out.
//!
//! Deliberately **not** behind the `native` feature. `javac` needs a host process, but the
//! *contract* does not, and gating the trait would mean a future in-process or wasm compiler had
//! nothing to implement. Only the `javac` adapter is host-gated.
//!
//! A backend's sole source input is a lowered tree — a manifest of `(path, CacheKey)` pairs
//! produced by a frontend. It never receives, and cannot reach, the project's authored source
//! roots, which is what makes "the backend only ever sees frontend output" a structural property
//! rather than a convention.
//!
//! TODO(backend-tier): deferred — no `impl Backend` exists yet, and compilation still runs through
//! the pre-existing `<dyn Compiler>::select` path (`native.rs`). The [`Backend`] trait and its
//! [`BackendRequest`] / [`BackendOptions`] / [`BackendOutcome`] / [`BackendSelection`] types —
//! together with `CacheKey::derive` and `CacheNamespace::BackendOutput` in `jals-storage` — are the
//! stable surface the `javac` backend adapter will implement and wire up in a later PR.
//! ([`BackendError`] is already in use by `staging.rs` and is not part of the deferral.)

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use jals_storage::{CacheKey, ContentDigest, RelativePath};

/// The compile knobs a backend honours, drawn from `[build]`.
///
/// Carried as data rather than as a `Manifest` reference so the portable half never needs the
/// manifest's host-path resolution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendOptions {
    pub release: Option<u32>,
    pub source: Option<u32>,
    pub target: Option<u32>,
    pub extra_args: Vec<String>,
}

impl BackendOptions {
    /// Everything about these options that affects output, folded to one digest.
    pub fn digest(&self) -> ContentDigest {
        let mut fold = jals_storage::ProvenanceFold::new(b"jals.backend.options\0");
        for value in [self.release, self.source, self.target] {
            // Distinguish "unset" from "0" rather than collapsing both to the same bytes.
            match value {
                Some(value) => fold.bytes(&[1]).version(value),
                None => fold.bytes(&[0]),
            };
        }
        for arg in &self.extra_args {
            // Order matters: `javac` reads its flags in sequence, so two orderings are two
            // different inputs.
            fold.bytes(arg.as_bytes());
        }
        fold.finish()
    }
}

/// What a backend compiles.
#[derive(Debug, Clone, Copy)]
pub struct BackendRequest<'a> {
    /// The only source input: the frontend's published output, in canonical path order.
    pub tree: &'a [(RelativePath, CacheKey)],
    /// Resolved classpath artifacts, in manifest order.
    pub classpath: &'a [CacheKey],
    pub options: &'a BackendOptions,
}

/// The result of a compile: the tool's exit code, or `None` when it was terminated by a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendOutcome {
    pub code: Option<i32>,
}

impl BackendOutcome {
    pub const fn success(self) -> bool {
        matches!(self.code, Some(0))
    }
}

pub type BackendFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BackendOutcome, BackendError>> + 'a>>;

/// Compiles a lowered Java source tree.
///
/// Object-safe and `!Send`, mirroring the existing `Compiler`/`Runtime` pair so a host can match
/// a manifest selector to a backend and drive it as a `&dyn Backend`.
pub trait Backend {
    /// Stable identity, folded into the backend output key. A string rather than a discriminant
    /// so that adding a backend never renumbers a shipped one's cache keys.
    fn id(&self) -> &'static str;

    /// Everything about this backend's configuration that affects its output.
    ///
    /// Must include the identity of the *tool* as well as its flags: the installed `javac` is
    /// host state that no manifest describes, and omitting it means upgrading the JDK silently
    /// reuses class files built by the previous compiler.
    fn config_digest(&self, req: &BackendRequest<'_>) -> ContentDigest;

    fn compile<'a>(&'a self, req: &'a BackendRequest<'a>) -> BackendFuture<'a>;

    /// What [`compile`](Self::compile) would do, for `--dry-run`/`-v`.
    fn describe(&self, req: &BackendRequest<'_>) -> String;
}

/// Whether this host has the requested backend.
///
/// Absence is a value carrying a reason, not an error raised at the end of a doomed pipeline.
/// The distinction matters most on wasm, where running the frontend and stopping is the
/// intended outcome rather than a degraded one.
pub enum BackendSelection {
    Available(Box<dyn Backend>),
    Absent {
        id: &'static str,
        reason: BackendAbsence,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendAbsence {
    /// This host cannot spawn processes at all — the browser.
    NoHostProcess,
    /// Built without the feature that supplies this backend's implementation.
    NotCompiledIn,
    /// The host could spawn, but the tool was not found.
    ToolMissing,
}

impl core::fmt::Display for BackendAbsence {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoHostProcess => f.write_str("this host cannot run external compilers"),
            Self::NotCompiledIn => f.write_str("this build does not include that backend"),
            Self::ToolMissing => f.write_str("the compiler was not found on this host"),
        }
    }
}

#[derive(Debug)]
pub enum BackendError {
    /// The backend could not be launched.
    Launch(String),
    /// A lowered file named a key the cache does not hold.
    MissingArtifact(RelativePath),
    /// Reading or writing a build artifact failed.
    Io(String),
}

impl core::fmt::Display for BackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Launch(message) => write!(f, "failed to launch the compiler: {message}"),
            Self::MissingArtifact(path) => {
                write!(f, "lowered source `{path}` is not in the artifact cache")
            }
            Self::Io(message) => write!(f, "build I/O failed: {message}"),
        }
    }
}
