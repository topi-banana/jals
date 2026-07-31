//! The one place `[build.frontend]` is answered, and the only way to drive what it names.
//!
//! Mirrors `jals_build::BackendSelection` on the other side of the pipeline (that crate is not a
//! dependency — `jals-frontend` must never depend on `jals-build` — so the correspondence is in
//! shape only): a host calls the constructor once and never matches on the manifest selector
//! itself. Without this, the decision table is copied per host, and it *was* — three times, one of
//! them inside a portable crate, differing only in where the build features came from.
//!
//! This module is the single place in the crate that reads `jals-config`. `DialectFlags` and every
//! [`Frontend`] implementation stay config-free: the projection from a manifest onto flags happens
//! here, so a frontend still knows nothing about `jals.toml`.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::{Feature, FrontendKind, Manifest};
use jals_storage::{ArtifactCache, CacheBackend};

use crate::dialect::{DialectFlags, DialectFrontend};
use crate::driver::{Driver, LowerError, Lowered};
use crate::frontend::Frontend;
use crate::ir::IrFile;
use crate::key::FrontendKey;
use crate::vanilla::VanillaFrontend;

/// The frontend a project's manifest selects, ready to run.
///
/// A struct and not an enum, which is where the correspondence with
/// `jals_build::BackendSelection` stops: a backend can be absent because a host cannot spawn
/// `javac`, but every frontend here is portable computation, so selecting one always succeeds.
/// An `Absent` arm would be a fiction — and it would be the one thing hosts still had to match on.
pub struct FrontendSelection {
    frontend: Box<dyn Frontend>,
}

impl FrontendSelection {
    /// The frontend `[build.frontend]` names for `manifest`, with the dialect desugarings its
    /// `[package] features` turn on.
    ///
    /// Enabling a jals dialect feature drives the build to desugar it, so a dialect compiles
    /// without a separate `[build.frontend]` selection. When no dialect feature is on, the result
    /// is [`vanilla`](Self::vanilla) rather than a dialect frontend with every flag off: the two
    /// behave identically but carry different `caps().id`s, and the id is folded into every cache
    /// key — so picking the wrong one is not a style choice, it invalidates every cached lowering
    /// an ordinary project has.
    ///
    /// `build_features` is what `#[cfg(feature = "…")]` tests. It is read **only** when the
    /// attributes dialect is on, which is why callers pass it unconditionally: the guard is part
    /// of the rule, and keeping it here is what makes an attribute-free project's cache identity
    /// independent of the feature selection on every host at once.
    pub fn for_manifest(manifest: &Manifest, build_features: &BTreeSet<String>) -> Self {
        let feature_set = manifest.feature_set();
        let attributes = feature_set.contains(Feature::Attributes);
        let flags = DialectFlags {
            grouped_imports: feature_set.contains(Feature::GroupedImports),
            attributes,
            build_features: if attributes {
                build_features.clone()
            } else {
                BTreeSet::new()
            },
        };
        // Exhaustive with no `_` arm, deliberately: adding a `[build.frontend]` variant must be a
        // compile error *here*, which is the whole reason the table moved into one place.
        match manifest.build.frontend {
            FrontendKind::Vanilla {} if flags.any() => Self {
                frontend: Box::new(DialectFrontend::new(flags)),
            },
            FrontendKind::Vanilla {} => Self::vanilla(),
        }
    }

    /// The identity lowering, for a source tree with no manifest to select from.
    ///
    /// A legacy source or binary dependency node has no `jals.toml`, so it has no
    /// `[build.frontend]` and no `[package] features` — there is nothing to desugar and nothing to
    /// decide.
    pub fn vanilla() -> Self {
        Self {
            frontend: Box::new(VanillaFrontend),
        }
    }

    /// The selected frontend's stable identity, for diagnostics. Also its cache identity.
    pub fn id(&self) -> &'static str {
        self.frontend.caps().id
    }

    /// Lower `files` and publish every emitted file into `cache`.
    ///
    /// Takes the input by value and imposes `FrontendKey::canonical_order` itself. Source
    /// discovery walks a filesystem — or a build script's registration order — neither of which is
    /// sorted, and every digest below this depends on that order, so leaving it to the caller made
    /// a cache entry's portability a documented precondition that three call sites each had to
    /// remember. Here it is not a precondition at all.
    pub async fn lower<C: CacheBackend>(
        &self,
        cache: &mut ArtifactCache<C>,
        mut files: Vec<IrFile>,
    ) -> Result<Lowered, LowerError> {
        FrontendKey::canonical_order(&mut files);
        Driver::lower(self.frontend.as_ref(), cache, &files).await
    }
}
