//! Writing a frontend's lowered tree to disk so a path-based compiler can read it.
//!
//! `javac` takes filesystem paths; a lowered tree is a [`BackendSource`] list. This module is the
//! one place that converts between them, and it is also where "the backend only ever sees
//! frontend output" stops being a comment: a [`StagedTree`] has no constructor other than
//! [`write`](StagedTree::write), so one cannot exist for anything but a frontend's output.
//!
//! Staging lives here rather than inside [`Backend::compile`](crate::Backend) for the same reason
//! the frontend driver owns the cache: `ArtifactCache<C>` is generic over a non-object-safe
//! backend and cannot appear in a `&dyn` signature. The orchestrator resolves the cache keys once
//! and passes the same [`BackendSource`] list here and to the backend; this module only writes
//! bytes out.
//!
//! TODO(backend-tier): with the bytes already resolved, `JavacBackend` could write them itself and
//! stop the host from staging a tree the in-process backends never read. Two things make it a
//! separate change rather than a detail: the `-sourcepath` override at the host would move into the
//! adapter's manifest clone, and `--dry-run` would stop writing this directory at all — a
//! behavioural change worth making deliberately instead of as a side effect.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use jals_exec::tokio_rt::on_blocking_pool;

use crate::backend::{BackendError, BackendSource};

/// Where a lowered tree is written, relative to the project root.
///
/// Below the managed build root, which buys two things at no cost: `jals clean` already removes
/// this tree, and the build-script fingerprint rules already refuse to treat managed build output
/// as a rerun input — so generated sources structurally cannot certify their own build.
pub const FRONTEND_OUT_DIR: &str = "target/jals/build/frontend";

/// A lowered tree materialized on disk.
pub struct StagedTree {
    root: PathBuf,
    /// What reached disk, in tree order. Read only by [`sources`](StagedTree::sources), and so only
    /// by tests — see that method for why nothing in a build needs it.
    #[cfg_attr(not(test), allow(dead_code))]
    sources: Vec<PathBuf>,
}

impl StagedTree {
    /// Write `tree` under `root`, returning the staged paths in tree order.
    ///
    /// Takes the resolved sources rather than the cache they came from: the same list feeds the
    /// backend, so looking the keys up here as well would read and verify every lowered file a
    /// second time. The trade is residency — the whole tree is in memory rather than one file at a
    /// time — which the host pays regardless, for as long as its
    /// [`BackendRequest`](crate::BackendRequest) lives.
    ///
    /// Stale entries are removed afterwards: the destination is entirely jals-owned managed build
    /// output, so a file the current tree does not name is by definition a leftover. That is what
    /// makes this safe without the ownership journal that publishing into a directory jals does
    /// *not* own requires — there, an unknown file might be something a person wrote, and deleting
    /// it would destroy work. Here there is no such file.
    ///
    /// Which is why this takes no destination policy and [`EmittedTree`](crate::EmittedTree) is a
    /// separate type rather than a flag on this one: the rule is a property of where the bytes go,
    /// and a caller that could pick "no pruning" here would also be a caller that could pick
    /// "prune" for a directory someone else owns.
    pub async fn write(tree: &[BackendSource], root: PathBuf) -> Result<Self, BackendError> {
        let mut sources = Vec::with_capacity(tree.len());

        for source in tree {
            let destination = source.path.to_host_path(&root);
            // The blocking task owns what it touches, so the bytes are copied rather than borrowed
            // — a memcpy in place of the cache read and digest pass this loop used to run.
            crate::emit::EmitFile::write(destination.clone(), source.bytes.clone()).await?;
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

    /// The staging root. Used as `-sourcepath` only to exclude the authored source dirs: it is not
    /// a package root (staged files keep their full project-relative path beneath it), so it
    /// resolves nothing implicitly, and every source is passed to the compiler explicitly.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The staged files, in tree order.
    ///
    /// Not what drives a compile, and crate-internal because of it: `JavacBackend` derives the paths
    /// it compiles from its `BackendRequest`'s tree instead, so the *request* stays the definition
    /// of what compiles and a host that staged one tree and requested another gets a missing file
    /// rather than a quietly different source set. What remains is a way to assert what reached
    /// disk, which is a test's question — hence dead in a build, and allowed to be.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
}
