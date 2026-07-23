//! Writing a frontend's lowered tree to disk so a path-based compiler can read it.
//!
//! `javac` takes filesystem paths; a lowered tree is a manifest of cache keys. This module is the
//! one place that converts between them, and it is also where "the backend only ever sees
//! frontend output" stops being a comment: a [`StagedTree`] can only be produced by writing out a
//! lowered tree, so a compile driven from [`StagedTree::sources`] cannot name an authored file.
//!
//! Staging lives here rather than inside [`Backend::compile`](crate::Backend) for the same reason
//! the frontend driver owns the cache: `ArtifactCache<C>` is generic over a non-object-safe
//! backend and cannot appear in a `&dyn` signature. The orchestrator reads the cache; the
//! compiler receives bytes already on disk.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jals_exec::tokio_rt::on_blocking_pool;
use jals_storage::{ArtifactCache, CacheBackend, CacheKey, RelativePath};

use crate::backend::BackendError;

/// Where a lowered tree is written, relative to the project root.
///
/// Below the managed build root, which buys two things at no cost: `jals clean` already removes
/// this tree, and the build-script fingerprint rules already refuse to treat managed build output
/// as a rerun input — so generated sources structurally cannot certify their own build.
pub const FRONTEND_OUT_DIR: &str = "target/jals/build/frontend";

/// A lowered tree materialized on disk.
pub struct StagedTree {
    root: PathBuf,
    sources: Vec<PathBuf>,
}

impl StagedTree {
    /// Write `tree` under `root`, returning the staged paths in tree order.
    ///
    /// Stale entries are removed afterwards: the destination is entirely jals-owned managed build
    /// output, so a file the current tree does not name is by definition a leftover. That is what
    /// makes this safe without the ownership journal that publishing into a *user* source root
    /// requires — there, an unknown file might be something a person wrote, and deleting it would
    /// destroy work. Here there is no such file.
    pub async fn write<C: CacheBackend>(
        cache: &ArtifactCache<C>,
        tree: &[(RelativePath, CacheKey)],
        root: PathBuf,
    ) -> Result<Self, BackendError> {
        let mut sources = Vec::with_capacity(tree.len());

        for (path, key) in tree {
            let bytes = cache
                .lookup(key)
                .await
                .map_err(|error| BackendError::Io(format!("{error:?}")))?
                .ok_or_else(|| BackendError::MissingArtifact(path.clone()))?;

            let mut destination = root.clone();
            for segment in path.segments() {
                destination.push(segment.as_str());
            }

            let target = destination.clone();
            on_blocking_pool(move || -> std::io::Result<()> {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                // Skip the write when the bytes already match, so a warm rebuild leaves mtimes
                // alone and `javac`'s own staleness checks keep working.
                if std::fs::read(&target).is_ok_and(|existing| existing == bytes) {
                    return Ok(());
                }
                std::fs::write(&target, &bytes)
            })
            .await
            .map_err(|error| BackendError::Io(error.to_string()))?;

            sources.push(destination);
        }

        Self::prune(&root, &sources).await?;
        Ok(Self { root, sources })
    }

    /// Delete anything under `root` that the current tree does not name.
    async fn prune(root: &Path, keep: &[PathBuf]) -> Result<(), BackendError> {
        let root = root.to_path_buf();
        let keep: BTreeSet<PathBuf> = keep.iter().cloned().collect();
        on_blocking_pool(move || -> std::io::Result<()> {
            fn walk(dir: &Path, keep: &BTreeSet<PathBuf>) -> std::io::Result<()> {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    return Ok(());
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    // `read_dir` file types do not follow symlinks, unlike `Path::is_dir`. A
                    // symlinked directory under this root must be unlinked as an unwanted entry,
                    // never walked — recursing would delete files outside the staging tree, which
                    // is exactly the "someone wrote that" case this pruning is allowed to skip.
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        walk(&path, keep)?;
                        // Prune directories this pass emptied. A still-populated directory fails,
                        // which is the intended no-op.
                        drop(std::fs::remove_dir(&path));
                    } else if !keep.contains(&path) {
                        std::fs::remove_file(&path)?;
                    }
                }
                Ok(())
            }
            walk(&root, &keep)
        })
        .await
        .map_err(|error| BackendError::Io(error.to_string()))
    }

    /// The staging root, for `-sourcepath`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The staged files, in tree order — the only sources a compile should be given.
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
}
