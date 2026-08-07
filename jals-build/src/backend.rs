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
//! Two adapters implement it: the portable [`JalsBackend`](crate::JalsBackend), which emits class
//! files or one WebAssembly module in this process, and the `native`-gated
//! `JavacBackend`, which runs the host's `javac` over the same tree materialized on disk. Hosts
//! reach both through [`BackendSelection`] — [`in_process`](BackendSelection::in_process) where
//! there is no process to spawn, `BackendSelection::for_host` where there is — so the
//! `[build] backend` decision table exists in exactly one place.
//!
//! TODO(backend-tier): output memoization under `CacheNamespace::BackendOutput` is still unbuilt,
//! and without it every build recompiles every source — `jals_frontend::BackendKey`, which folds
//! [`config_digest`](Backend::config_digest), still has no caller. Each adapter names what it must
//! fold before that can be switched on.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use jals_config::{BackendKind, Manifest};
use jals_storage::{CacheKey, ContentDigest, RelativePath};

/// The compile knobs a backend honours, drawn from `[build]`.
///
/// Carried as data rather than as a `Manifest` reference so the portable half never needs the
/// manifest's host-path resolution.
///
/// The fields are crate-internal because [`from_manifest`](Self::from_manifest) is the only way a
/// host builds one: with the knob list written once there, a host that set the fields itself would
/// be the second place to update when `[build]` grows one. Only the backends in this crate read
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendOptions {
    pub(crate) release: Option<u32>,
    pub(crate) source: Option<u32>,
    pub(crate) target: Option<u32>,
    pub(crate) extra_args: Vec<String>,
}

impl BackendOptions {
    /// The `[build]` compile knobs, lifted out of a manifest.
    ///
    /// Lives here rather than in each host so the knob list is written once: a host that assembled
    /// it itself would be a second place to update when `[build]` grows one. Lifting only — the
    /// manifest is read here and not kept, so the struct still carries no reference to one and the
    /// portable half still never sees the manifest's host-path resolution.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self {
            release: manifest.build.release,
            source: manifest.build.source,
            target: manifest.build.target,
            extra_args: manifest.build.javac_flags.clone(),
        }
    }

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
    /// Crate-internal, along with the constructors that set it: an outcome is *built* here and only
    /// *read* outside. Most callers want [`success`](Self::success); [`code`](Self::code) exists for
    /// the one driver that has to turn this into its own process's exit status, because deciding
    /// what a signal — or a `javac` exit 2 — means is that driver's job and not the same question on
    /// every host.
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
    ///
    /// Errors-only by construction: [`failed`](Self::failed) is the only constructor that
    /// populates this, and it sets a failing [`code`](Self::code) — which is private, so nothing
    /// outside this crate can build an outcome that both [`success`](Self::success)es and carries
    /// messages. A host promoting these into its own error channel — the CLI's `error:` lead, a
    /// browser's compile-failure text — therefore needs no severity test, exactly as
    /// `BuildScriptOutput::diagnostics` needs none to promote a warning. Writing a severity beside
    /// each message would create a second answer that could contradict the code.
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

    /// A compile a host tool ran to completion, carrying its exit status verbatim.
    ///
    /// Neither artifacts nor messages: a process-based backend wrote its own output through
    /// `javac -d` and reported its own diagnostics on the stderr it inherited. The code is kept as
    /// the tool gave it because `javac` distinguishes a compile error (1) from bad arguments (2), a
    /// system error (3) and abnormal termination (4) — collapsing those to one "failed" throws away
    /// the only signal that separates a broken invocation from broken source.
    // Only a process-based backend builds one of these, so a `--no-default-features` core has no
    // caller — the seam is portable, the adapter that needs this constructor is not.
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    pub(crate) const fn from_code(code: Option<i32>) -> Self {
        Self {
            code,
            artifacts: Vec::new(),
            messages: Vec::new(),
        }
    }

    pub const fn success(&self) -> bool {
        matches!(self.code, Some(0))
    }

    /// The tool's exit code, or `None` when it was terminated by a signal.
    ///
    /// For the driver that maps a compile onto its own process's exit status, and nothing else —
    /// every other question about an outcome is [`success`](Self::success).
    pub const fn code(&self) -> Option<i32> {
        self.code
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

impl BackendSelection {
    /// The backend `[build] backend` names, on a host with no process to spawn.
    ///
    /// Choosing this entry point *is* the declaration that this host cannot spawn one, which is why
    /// `javac` comes back [`Absent`](Self::Absent) with [`BackendAbsence::NoHostProcess`] rather
    /// than being probed for. A native host calls `BackendSelection::for_host` instead, which adds
    /// the `javac` arm and delegates the other two straight back here — so every [`BackendKind`] is
    /// answered in exactly one place.
    pub fn in_process(backend: BackendKind, release: Option<u32>) -> Self {
        match backend {
            BackendKind::Jals {} => Self::Available(Box::new(crate::JalsBackend::new(release))),
            // wasm is a different *target*, not just a different tool: one module for the whole
            // project, and the host's collector rather than a JVM's.
            BackendKind::JalsWasm {} => Self::Available(Box::new(crate::JalsBackend::wasm())),
            BackendKind::Javac {} => Self::Absent {
                id: backend.tag_name(),
                reason: BackendAbsence::NoHostProcess,
            },
        }
    }
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

#[cfg(test)]
mod tests {
    use alloc::borrow::ToOwned;
    use alloc::vec;

    use super::*;

    /// The [`Available`](BackendSelection::Available) backend's id, or `None` when absent. Written as
    /// a helper because [`BackendSelection`] holds a `Box<dyn Backend>` and so cannot derive
    /// `Debug`: every assertion here matches rather than compares.
    fn available_id(selection: &BackendSelection) -> Option<&'static str> {
        match selection {
            BackendSelection::Available(backend) => Some(backend.id()),
            BackendSelection::Absent { .. } => None,
        }
    }

    #[test]
    fn an_unset_option_is_not_the_same_input_as_zero() {
        // `--release 0` is nonsense, but a digest that cannot tell it from "no `--release` at all"
        // would let one manifest's cached output answer for the other.
        let unset = BackendOptions::default();
        let zero = BackendOptions {
            release: Some(0),
            ..BackendOptions::default()
        };
        assert_ne!(unset.digest(), zero.digest());

        // And the three version knobs are distinguishable from each other, not just from unset.
        let source_zero = BackendOptions {
            source: Some(0),
            ..BackendOptions::default()
        };
        assert_ne!(zero.digest(), source_zero.digest());
    }

    #[test]
    fn extra_argument_order_is_part_of_the_configuration() {
        // `javac` reads its flags in sequence — `-Xlint:all -Xlint:none` and its reverse are two
        // different compiles, so they must not share a cache identity.
        let forward = BackendOptions {
            extra_args: vec!["-Xlint:all".to_owned(), "-Xlint:none".to_owned()],
            ..BackendOptions::default()
        };
        let reversed = BackendOptions {
            extra_args: vec!["-Xlint:none".to_owned(), "-Xlint:all".to_owned()],
            ..BackendOptions::default()
        };
        assert_ne!(forward.digest(), reversed.digest());
    }

    #[test]
    fn only_a_zero_exit_code_is_a_success() {
        assert!(BackendOutcome::from_code(Some(0)).success());
        assert!(!BackendOutcome::from_code(Some(1)).success());
        // Terminated by a signal: no code, and not a success.
        assert!(!BackendOutcome::from_code(None).success());

        assert!(BackendOutcome::compiled(Vec::new()).success());
        assert!(!BackendOutcome::failed(vec!["not lowered yet".to_owned()]).success());
    }

    #[test]
    fn a_host_tools_exit_code_survives_the_outcome() {
        // `javac` uses 2 for bad arguments and 3 for a system error. Both are failures, and both
        // have to stay distinguishable from 1 (a compile error) all the way to the driver.
        for code in [1, 2, 3, 4, 130] {
            assert_eq!(BackendOutcome::from_code(Some(code)).code(), Some(code));
        }
        assert_eq!(BackendOutcome::from_code(None).code(), None);
    }

    #[test]
    fn a_process_free_host_gets_the_in_process_backends_and_no_javac() {
        // Each arm answers with the backend whose `id` is the manifest tag that selected it, so the
        // selection cannot silently route one backend's key to another's output.
        assert_eq!(
            available_id(&BackendSelection::in_process(BackendKind::Jals {}, None)),
            Some(BackendKind::Jals {}.tag_name())
        );
        assert_eq!(
            available_id(&BackendSelection::in_process(
                BackendKind::JalsWasm {},
                None
            )),
            Some(BackendKind::JalsWasm {}.tag_name())
        );

        // javac is absent as a *value* carrying its reason, not an error raised later.
        match BackendSelection::in_process(BackendKind::Javac {}, None) {
            BackendSelection::Absent { id, reason } => {
                assert_eq!(id, BackendKind::Javac {}.tag_name());
                assert_eq!(reason, BackendAbsence::NoHostProcess);
            }
            BackendSelection::Available(_) => {
                panic!("a host with no process to spawn cannot supply javac")
            }
        }
    }

    #[test]
    fn the_release_level_reaches_the_selected_backend() {
        // `in_process` passes `release` through to the class-file backend, so two releases are two
        // configurations rather than one shared cache identity.
        let options = BackendOptions::default();
        let request = BackendRequest {
            tree: &[],
            classpath: &[],
            options: &options,
        };
        let digest = |release| match BackendSelection::in_process(BackendKind::Jals {}, release) {
            BackendSelection::Available(backend) => backend.config_digest(&request),
            BackendSelection::Absent { .. } => panic!("the jals backend is always available"),
        };
        assert_ne!(digest(Some(17)), digest(Some(21)));
    }
}
