//! Drives a frontend and publishes its output. The only place in the pipeline that owns a cache.

use alloc::vec::Vec;

use jals_storage::{ArtifactCache, CacheBackend, CacheError, CacheNamespace, RelativePath};

use crate::frontend::{Frontend, FrontendError};
use crate::ir::{FrontendDiagnostic, Ir, IrFile, LoweredFile, LoweredTree};
use crate::key::FrontendKey;

/// A completed lowering.
#[derive(Debug)]
pub struct Lowered {
    pub tree: LoweredTree,
    pub diagnostics: Vec<FrontendDiagnostic>,
    /// True when the lowering was restored from cache and the frontend never ran.
    pub cached: bool,
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

/// Lowering namespace: runs a frontend and publishes what it emitted.
pub struct Driver;

impl Driver {
    /// Lower `files` with `frontend`, publishing every emitted file into `cache`.
    ///
    /// The generic `C: CacheBackend` sits on this function rather than on the [`Frontend`] trait:
    /// `ArtifactCache<C>` is not object-safe, so a `&dyn Frontend` could not name it. Keeping the
    /// cache here is also the existing layering — generation logic never knows the cache exists,
    /// and publication happens at exactly one boundary.
    ///
    /// `files` must already be in canonical order ([`FrontendKey::canonical_order`]).
    pub async fn lower<C: CacheBackend>(
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
        let lowering = FrontendKey::lowering(&caps, config, files);
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
            let observed = FrontendKey::observed_input(caps.needs, origin_of(files, path), files);
            let provenance = FrontendKey::emitted(&caps, config, observed, path);
            let key = FrontendKey::artifact(provenance, bytes);
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
        let manifest_key = FrontendKey::artifact(lowering, &manifest);
        cache.publish(&manifest_key, &manifest).await?;
        cache.record_index(&manifest_key).await?;

        Ok(Lowered {
            tree,
            diagnostics: output.diagnostics,
            cached: false,
        })
    }
}
