//! The compile frontend seam: project sources lowered to Java sources.
//!
//! `jals build` used to hand authored `.java` files straight to `javac`. This crate inserts the
//! stage a language with its own constructs needs — the frontend — so the backend only ever
//! sees what a frontend emitted. Today the only frontend is [`VanillaFrontend`], the identity
//! lowering, which makes the seam real without yet making it do anything.
//!
//! Two properties are load-bearing and easy to lose:
//!
//! - **This crate is portable in every configuration.** It has no features at all, so there is
//!   no build in which it stops being `no_std + alloc`. A frontend never needs a host
//!   capability; the backend does, and lives elsewhere.
//! - **Frontends never touch the cache.** A [`Frontend`] takes bytes and returns bytes; the
//!   [`driver`] publishes. This mirrors how the decompiler leaves publication to its caller,
//!   and it is also forced: `ArtifactCache<C>` is generic over a non-object-safe backend and
//!   so cannot appear in a `&dyn Frontend` signature.
//!
//! A frontend declares the IR level it observes ([`IrLevel`]), and that declaration *is* the
//! scope of its cache key — so a per-file frontend stays per-file invalidated while a
//! project-wide one is honestly keyed on the whole project.

#![no_std]

pub mod driver;
pub mod frontend;
pub mod ir;
pub mod key;
pub mod level;
pub mod vanilla;

pub use driver::{Driver, LowerError, Lowered};
pub use frontend::{Frontend, FrontendCaps, FrontendError, FrontendFuture};
pub use ir::{
    FrontendDiagnostic, FrontendOutput, Ir, IrFile, LoweredFile, LoweredTree, OriginSpan, Severity,
};
pub use key::{BackendKey, FrontendKey};
pub use level::{IrLevel, PIPELINE_API_VERSION};
pub use vanilla::VanillaFrontend;

#[cfg(test)]
mod tests;
