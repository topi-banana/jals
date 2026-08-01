//! The compile frontend seam: project sources lowered to Java sources.
//!
//! `jals build` used to hand authored `.java` files straight to `javac`. This crate inserts the
//! stage a language with its own constructs needs — the frontend — so the backend only ever
//! sees what a frontend emitted. Today the only frontend is [`VanillaFrontend`], the identity
//! lowering, which makes the seam real without yet making it do anything.
//!
//! Three properties are load-bearing and easy to lose:
//!
//! - **This crate is portable in every configuration.** It has no features at all, so there is
//!   no build in which it stops being `no_std + alloc`. A frontend never needs a host
//!   capability; the backend does, and lives elsewhere.
//! - **Frontends never touch the cache.** A [`Frontend`] takes bytes and returns bytes; the
//!   internal driver publishes. This mirrors how the decompiler leaves publication to its
//!   caller, and it is also forced: `ArtifactCache<C>` is generic over a non-object-safe backend
//!   and so cannot appear in a `&dyn Frontend` signature.
//! - **Hosts never answer `[build.frontend]` themselves.** [`FrontendSelection`] is the only way
//!   in: it holds the decision table, projects `[package] features` onto the dialect flags, and
//!   drives the frontend, so the rule exists once instead of once per host. The driver is
//!   crate-internal for that reason — the `Frontend` trait is the seam for *implementors*, and
//!   `FrontendSelection` is the seam for *callers*.
//!
//! A frontend declares the IR level it observes ([`IrLevel`]), and that declaration *is* the
//! scope of its cache key — so a per-file frontend stays per-file invalidated while a
//! project-wide one is honestly keyed on the whole project.

#![no_std]

extern crate alloc;

mod attr;
// Crate-internal, both: `[build.frontend]` is answered by [`FrontendSelection`], so the dialect's
// flag projection and the driver that publishes into the cache are things it does, not things a
// caller assembles. (`key::FrontendKey` is crate-internal for the same reason, inside a module that
// still carries the public `BackendKey`.) Widening any of them back out re-opens the decision table
// this seam closed.
mod dialect;
pub(crate) mod driver;
pub mod frontend;
pub mod ir;
pub mod key;
pub mod level;
pub mod selection;
pub mod vanilla;

pub use driver::{LowerError, Lowered};
pub use frontend::{Frontend, FrontendCaps, FrontendError, FrontendFuture};
pub use ir::{
    FrontendDiagnostic, FrontendOutput, Ir, IrFile, LoweredFile, LoweredTree, OriginSpan, Severity,
};
pub use key::BackendKey;
pub use level::IrLevel;
pub use selection::FrontendSelection;
pub use vanilla::VanillaFrontend;

#[cfg(test)]
mod tests;
