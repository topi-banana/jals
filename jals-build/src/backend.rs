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
//! TODO(backend-tier): [`JalsBackend`](crate::JalsBackend) implements this contract and `jals
//! build` reaches it — but by matching on `[build] backend` in `jals-cli` and constructing the
//! backend directly, not through [`BackendSelection`]. Two things are therefore still unbuilt:
//! the selection factory (which is what would let a host report [`BackendAbsence`] instead of
//! failing later, and what `wasm32` needs to say "no host process" as a *result* rather than an
//! error), and output memoization under `CacheNamespace::BackendOutput` — without which every
//! build recompiles every source.

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
    ///
    /// Crate-internal until backend output is memoized: the only caller that needs it outside is
    /// the one that would fold it into a `BackendKey`, and nothing builds those yet.
    pub(crate) fn digest(&self) -> ContentDigest {
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

/// One lowered source file: where it lives, what the frontend published it as, and its content.
///
/// All three travel together because each answers a different question and no two backends ask the
/// same ones. The `key` is provenance — it is what [`BackendKey`](jals_frontend::BackendKey) folds
/// to decide whether this compile is already cached. The `bytes` are the content, resolved from the
/// cache by the driver rather than by the backend, because [`Backend`] is object-safe and
/// `ArtifactCache` is not. The `path` is what a compiler reports errors against and what a
/// process-based backend writes to disk.
#[derive(Debug, Clone)]
pub struct BackendSource {
    /// The file's project-relative path.
    pub path: RelativePath,
    /// The cache key the frontend published it under.
    pub key: CacheKey,
    /// The file's contents.
    pub bytes: Vec<u8>,
}

/// What a backend compiles.
#[derive(Debug, Clone, Copy)]
pub struct BackendRequest<'a> {
    /// The only source input: the frontend's published output, in canonical path order.
    pub tree: &'a [BackendSource],
    /// Resolved classpath artifacts, in manifest order.
    pub classpath: &'a [CacheKey],
    pub options: &'a BackendOptions,
}

/// The result of a compile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendOutcome {
    /// The tool's exit code, or `None` when it was terminated by a signal. An in-process backend
    /// reports `Some(0)` or `Some(1)`; the field exists because a host process is the case that
    /// can end without one.
    ///
    /// Crate-internal, along with the two constructors that set it: an outcome is *built* here and
    /// only *read* outside, through [`success`](Self::success). A caller that matched on the code
    /// would be deciding what a signal means, which is the driver's job and not the same question
    /// on every host.
    code: Option<i32>,
    /// What the compile produced, by project-relative path: one class file per type, or one
    /// WebAssembly module for the whole project.
    ///
    /// Empty for a process-based backend, which writes its own output through `javac -d` and has
    /// nothing to hand back. An in-process backend cannot write anything — it has no filesystem —
    /// so its output *is* its return value, and the driver decides where it lands.
    pub artifacts: Vec<(RelativePath, Vec<u8>)>,
    /// What the backend has to say about the compile. A backend does not type-check, so these are
    /// reports of source it could not compile, not of source that is wrong.
    pub messages: Vec<String>,
}

impl BackendOutcome {
    /// A compile that produced `artifacts` and nothing to report.
    pub(crate) const fn compiled(artifacts: Vec<(RelativePath, Vec<u8>)>) -> Self {
        Self {
            code: Some(0),
            artifacts,
            messages: Vec::new(),
        }
    }

    /// A compile that failed, with the reasons.
    pub(crate) const fn failed(messages: Vec<String>) -> Self {
        Self {
            code: Some(1),
            artifacts: Vec::new(),
            messages,
        }
    }

    pub const fn success(&self) -> bool {
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
