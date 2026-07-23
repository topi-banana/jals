//! The frontend seam.

use alloc::boxed::Box;
use alloc::string::String;
use core::future::Future;
use core::pin::Pin;

use jals_storage::ContentDigest;

use crate::ir::{FrontendOutput, Ir};
use crate::level::IrLevel;

/// A frontend's static capability declaration.
///
/// Pure `const`-constructible data, so a pipeline can be validated before a single file is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrontendCaps {
    /// Stable cache identity, folded into every output key.
    ///
    /// A string rather than an enum discriminant on purpose: adding or reordering frontends
    /// must never renumber a shipped frontend's keys and silently invalidate its cache.
    pub id: &'static str,
    /// The lowest IR level this frontend can work from — and therefore the scope of its keys.
    pub needs: IrLevel,
    /// Source file extensions this frontend claims, without the dot.
    pub extensions: &'static [&'static str],
    /// Whether running this frontend can change the project's type graph (add, remove, or
    /// rename a type or member).
    ///
    /// A frontend that only rewrites bodies is stable and runs in a single pass. One that emits
    /// new types invalidates the index it reasoned from, and needs the bounded fixpoint the
    /// driver grows once a cross-file level exists.
    pub type_stable: bool,
    /// Bumped when this frontend's output changes for unchanged input.
    pub version: u32,
}

pub type FrontendFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FrontendOutput, FrontendError>> + 'a>>;

/// Lowers project sources to Java sources.
///
/// Object-safe and `!Send`, matching the `Compiler`/`Runtime` shape already in `jals-build`: a
/// host matches a manifest selector to a frontend and drives it as a `&dyn Frontend`.
pub trait Frontend {
    fn caps(&self) -> FrontendCaps;

    /// Everything about this frontend's configuration that affects its output, folded to one
    /// digest.
    ///
    /// The escape hatch for typed config under object safety: config never crosses the trait,
    /// so the driver keys on this instead. Its completeness is the implementor's obligation —
    /// a config field omitted here is a silent stale-cache bug.
    fn config_digest(&self) -> ContentDigest;

    /// Lower the supplied IR.
    ///
    /// Takes bytes and returns bytes; the driver owns the cache and publishes the result. A
    /// frontend cannot reach the cache even if it wanted to — `ArtifactCache<C>` is generic
    /// over a non-object-safe backend and so cannot appear in this signature.
    fn run<'a>(&'a self, ir: Ir<'a>) -> FrontendFuture<'a>;

    /// What [`run`](Self::run) would do, for `--dry-run`/`-v`. Synchronous: rendering a plan is
    /// display-only and bounded.
    fn describe(&self, ir: &Ir<'_>) -> String;
}

#[derive(Debug)]
pub enum FrontendError {
    /// The driver supplied a level below the frontend's declared `needs`.
    ///
    /// Unreachable when the pipeline was validated, because the driver forces exactly
    /// `caps().needs`. It is not *statically* unreachable only because `dyn Frontend` erases
    /// the level, so it stays here as a defect signal rather than a user-facing failure.
    LevelMismatch { wanted: IrLevel, got: IrLevel },
    /// The frontend rejected its input. Diagnostics carry the detail.
    Rejected,
}

impl core::fmt::Display for FrontendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::LevelMismatch { wanted, got } => {
                write!(f, "frontend needs IR level {wanted:?} but received {got:?}")
            }
            Self::Rejected => f.write_str("frontend rejected its input"),
        }
    }
}
