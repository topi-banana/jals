//! Drives a frontend and publishes its output. The only place in the pipeline that owns a cache.

use alloc::vec::Vec;

use jals_storage::{ArtifactCache, CacheBackend, CacheError, CacheNamespace, RelativePath};

use crate::frontend::{Frontend, FrontendError};
use crate::ir::{FrontendDiagnostic, Ir, IrFile, LoweredFile, LoweredTree, Severity};
use crate::key;

/// A completed lowering.
#[derive(Debug)]
pub struct Lowered {
    pub tree: LoweredTree,
    // Carried for consumers of a completed lowering; currently read only by tests.
    #[allow(dead_code)]
    diagnostics: Vec<FrontendDiagnostic>,
    /// True when the lowering was restored from cache and the frontend never ran.
    // Read only by tests, so the field is dead only in non-test builds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) cached: bool,
}

#[derive(Debug)]
pub enum LowerError {
    Frontend(FrontendError),
    Cache(CacheError),
    DuplicatePath(RelativePath),
    /// The frontend reported an error diagnostic; nothing was published.
    Rejected(Vec<FrontendDiagnostic>),
}

impl From<CacheError> for LowerError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl core::fmt::Display for LowerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Frontend(error) => write!(f, "{error}"),
            Self::Cache(error) => write!(f, "{error}"),
            Self::DuplicatePath(path) => write!(f, "frontend emitted `{path}` more than once"),
            Self::Rejected(diagnostics) => {
                f.write_str("frontend rejected its input")?;
                for diagnostic in diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == Severity::Error)
                {
                    match &diagnostic.file {
                        Some(file) => write!(f, "\n  {file}: {}", diagnostic.message)?,
                        None => write!(f, "\n  {}", diagnostic.message)?,
                    }
                }
                Ok(())
            }
        }
    }
}

pub(crate) use api::lower;

/// Lowering namespace: runs a frontend and publishes what it emitted.
mod api {
    use super::{
        ArtifactCache, CacheBackend, CacheNamespace, Frontend, Ir, IrFile, LowerError, Lowered,
        LoweredFile, LoweredTree, RelativePath, Vec, key,
    };

    /// Lower `files` with `frontend`, publishing every emitted file into `cache`.
    ///
    /// The generic `C: CacheBackend` sits on this function rather than on the [`Frontend`] trait:
    /// `ArtifactCache<C>` is not object-safe, so a `&dyn Frontend` could not name it. Keeping the
    /// cache here is also the existing layering — generation logic never knows the cache exists,
    /// and publication happens at exactly one boundary.
    ///
    /// `files` must already be in canonical order ([`key::canonical_order`]) — which is
    /// why this is crate-internal and
    /// [`FrontendSelection::lower`](crate::FrontendSelection::lower) is what a caller reaches:
    /// a precondition no signature enforces is one every call site has to remember, and there is
    /// exactly one production call site here. (This crate's own tests call it directly, against
    /// input they sort themselves — the internal seam that lets them exercise ordering.)
    pub(crate) async fn lower<C: CacheBackend>(
        frontend: &dyn Frontend,
        cache: &mut ArtifactCache<C>,
        files: &[IrFile],
    ) -> Result<Lowered, LowerError> {
        /// The input file an emitted path came from, or `None` for a synthesized path with no
        /// single origin — which widens that file's key to project scope.
        ///
        /// `files` is in canonical (sorted) order per this function's contract, so this is a
        /// binary search: a linear scan per emitted file is quadratic in project size.
        fn origin_of<'a>(files: &'a [IrFile], emitted: &RelativePath) -> Option<&'a IrFile> {
            files
                .binary_search_by(|file| file.path.cmp(emitted))
                .ok()
                .map(|index| &files[index])
        }

        let caps = frontend.caps();
        let config = frontend.config_digest();

        let ir = match caps.needs {
            crate::level::IrLevel::Bytes => Ir::Bytes { files },
        };

        // Ask whether this exact lowering already exists before running anything. A frontend's
        // output digest is unknowable in advance — that is what makes it a frontend — so the
        // advisory locator index recovers the content half from the provenance we can compute.
        // `indexed_key` is a hint and `record_index` is last-writer-wins, which is safe because
        // the manifest is still read back through a verified lookup: a stale index causes a
        // miss, never a wrong tree.
        let lowering = key::lowering(&caps, config, files);
        if let Some(manifest_key) = cache
            .indexed_key(CacheNamespace::FrontendOutput, lowering)
            .await?
            && let Some(bytes) = cache.lookup(&manifest_key).await?
            && let Ok(tree) = LoweredTree::decode(&bytes)
        {
            return Ok(Lowered {
                tree,
                diagnostics: Vec::new(),
                cached: true,
            });
        }

        let output = frontend.run(ir).await.map_err(LowerError::Frontend)?;
        if output.has_errors() {
            // A failed lowering publishes nothing — not the files, and not the manifest that
            // would certify them. Half a source tree in the cache is indistinguishable from a
            // complete one on the next build.
            return Err(LowerError::Rejected(output.diagnostics));
        }

        let mut lowered = Vec::with_capacity(output.files.len());
        for (path, bytes) in &output.files {
            let observed = key::observed_input(caps.needs, origin_of(files, path), files);
            let provenance = key::emitted(&caps, config, observed, path);
            let key = key::artifact(provenance, bytes);
            // Write-once and idempotent: republishing identical bytes under the same key is a
            // no-op, so re-emitting an unchanged file costs a digest comparison, not a rewrite.
            cache.publish(&key, bytes).await?;
            lowered.push(LoweredFile {
                path: path.clone(),
                key,
            });
        }

        let tree = LoweredTree::new(lowered).map_err(|error| LowerError::DuplicatePath(error.0))?;

        // Publish the manifest last, and only on success: it is the certificate that every
        // member above is present, so it must never be reachable before they are.
        let manifest = tree.encode();
        let manifest_key = key::artifact(lowering, &manifest);
        cache.publish(&manifest_key, &manifest).await?;
        cache.record_index(&manifest_key).await?;

        Ok(Lowered {
            tree,
            diagnostics: output.diagnostics,
            cached: false,
        })
    }
}
