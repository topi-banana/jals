//! Cache-key derivation for the two compile tiers.

use alloc::string::ToString;

use jals_storage::{CacheKey, CacheNamespace, ContentDigest, ProvenanceFold, RelativePath};

use crate::frontend::FrontendCaps;
use crate::ir::IrFile;
use crate::level::{IrLevel, PIPELINE_API_VERSION};

/// Key derivation namespace for the frontend tier.
pub struct FrontendKey;

impl FrontendKey {
    const KIND: &'static [u8] = b"jals.frontend\0";

    /// The digest of everything a frontend at `level` is permitted to observe.
    ///
    /// This is where the level lattice earns its keep. A `Bytes`-level frontend is keyed on the
    /// one file it read, so editing a sibling leaves its entry valid; a project-level frontend is
    /// keyed on every file, because it genuinely looked at every file. A design without levels
    /// must key every frontend on the whole tree conservatively, losing per-file reuse for the
    /// cheap ones.
    ///
    /// `origin` is the input file an emitted path came from, when there is exactly one. A
    /// frontend that synthesizes a file with no single originating input passes `None`, widening
    /// the scope to the whole project — the conservative direction, and the only sound one: a key
    /// must never claim a narrower dependency than the output actually has.
    ///
    /// The match is deliberately exhaustive with no `_` arm, so adding a level is a compile error
    /// here and its observation scope gets decided rather than defaulted.
    pub fn observed_input(
        level: IrLevel,
        origin: Option<&IrFile>,
        all: &[IrFile],
    ) -> ContentDigest {
        match (level, origin) {
            (IrLevel::Bytes, Some(file)) => file.digest,
            (IrLevel::Bytes, None) => Self::project(all),
        }
    }

    /// Fold every input file in canonical order. `all` must already be sorted by path.
    pub fn project(all: &[IrFile]) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"jals.frontend.observed.project\0");
        for entry in all {
            fold.bytes(entry.path.to_string().as_bytes())
                .digest(entry.digest);
        }
        fold.finish()
    }

    /// Provenance for one file a frontend emitted.
    pub fn emitted(
        caps: &FrontendCaps,
        config: ContentDigest,
        observed: ContentDigest,
        path: &RelativePath,
    ) -> ContentDigest {
        let mut fold = ProvenanceFold::new(Self::KIND);
        fold.version(PIPELINE_API_VERSION)
            .bytes(caps.id.as_bytes())
            .version(caps.version)
            .bytes(&[caps.needs.tag()])
            .digest(config)
            .digest(observed)
            .bytes(path.to_string().as_bytes());
        fold.finish()
    }

    /// Provenance of a whole lowering, used to answer "is this already cached?" before the
    /// frontend runs.
    ///
    /// Necessarily project-scoped even for a per-file frontend: which files get *emitted* depends
    /// on which files exist, so skipping the frontend entirely requires knowing the whole input
    /// set is unchanged. Per-file scoping still pays off below this, where identical emitted
    /// bytes dedupe across builds regardless of what else moved.
    pub fn lowering(caps: &FrontendCaps, config: ContentDigest, all: &[IrFile]) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"jals.frontend.lowering\0");
        fold.version(PIPELINE_API_VERSION)
            .bytes(caps.id.as_bytes())
            .version(caps.version)
            .bytes(&[caps.needs.tag()])
            .digest(config)
            .digest(Self::project(all));
        fold.finish()
    }

    pub fn artifact(provenance: ContentDigest, bytes: &[u8]) -> CacheKey {
        CacheKey::new(
            CacheNamespace::FrontendOutput,
            provenance,
            ContentDigest::of(bytes),
        )
    }

    /// Impose the canonical order every digest depends on.
    ///
    /// Source discovery walks the filesystem, whose order is neither sorted nor stable across
    /// platforms, so a digest folded over "all files" would otherwise be machine-dependent.
    /// Sorting by logical path — here, once, before anything is hashed — is what makes a cache
    /// entry produced on one machine valid on another.
    pub fn canonical_order(files: &mut [IrFile]) {
        files.sort_by(|left, right| left.path.cmp(&right.path));
    }
}

/// Key derivation namespace for the backend tier.
pub struct BackendKey;

impl BackendKey {
    const KIND: &'static [u8] = b"jals.backend\0";

    /// Provenance for a backend's output.
    ///
    /// Folds the frontend output key as a parent, plus everything about the backend and the host
    /// toolchain. `tool_identity` is *host state* no manifest describes: omit it and upgrading
    /// the JDK silently reuses class files built by the previous compiler.
    ///
    /// Note what is absent — nothing here flows back into a frontend key. Because a frontend's
    /// provenance folds only its own inputs, changing a compiler flag, the classpath, or the JDK
    /// provably cannot invalidate a cached lowering. The two tiers are separated by the shape of
    /// the fold, not by discipline.
    pub fn output(
        backend_id: &str,
        config: ContentDigest,
        classpath: ContentDigest,
        tool_identity: ContentDigest,
        frontend_out: &CacheKey,
    ) -> ContentDigest {
        let mut fold = ProvenanceFold::new(Self::KIND);
        fold.version(PIPELINE_API_VERSION)
            .bytes(backend_id.as_bytes())
            .digest(config)
            .digest(classpath)
            .digest(tool_identity)
            .parent(frontend_out);
        fold.finish()
    }

    /// The digest of a resolved classpath.
    ///
    /// Order-preserving, unlike the tree fold: `javac` observes classpath order, so two orderings
    /// are two different inputs and must not collide.
    pub fn classpath(entries: &[CacheKey]) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"jals.backend.classpath\0");
        for entry in entries {
            fold.parent(entry);
        }
        fold.finish()
    }
}
