//! The built-in (in-process) toolchain backend, selected by `[toolchain] compiler = "builtin"` /
//! `runtime = "builtin"`.
//!
//! [`BuiltinToolchain`] implements both halves of the pure toolchain seam — the [`Compiler`] and
//! [`Runtime`] traits — next to the host `SubprocessToolchain`, and is the seam a real embedded
//! compiler will eventually fill. Today it is a **dummy**: `compile` copies every requested source
//! file into the resolved `classes-dir` verbatim (nothing is compiled), and `run` is a no-op that
//! reports success (nothing is executed). What earns it a place now is the shape, not the
//! behavior: it exercises the whole request → backend → outcome path with a non-subprocess
//! backend, and all its I/O goes through [`ProjectStorage`], so memory and native adapters share
//! the same transaction/revision contract. A future in-process compiler replaces the copy step and
//! inherits everything else.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jals_storage::{
    CacheBackend, Error, FileKey, MemoryStorage, ProjectStorage, ProjectView, SourceBackend,
};

use crate::manifest_ext::ManifestExt;
use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::{BuildOutcome, Compiler, Runtime, ToolchainError, ToolchainFuture};

/// A [`Compiler`] + [`Runtime`] backend realized in-process over [`ProjectStorage`] — today a dummy
/// that copies sources instead of compiling them and skips running entirely (see the module docs).
pub struct BuiltinToolchain {
    /// The storage aggregate the backend reads and commits. Toolchain methods take `&self` while
    /// transactions require `&mut`, so the memory backend checks its storage *out* for the
    /// duration of a compile (take/put) — never holding a `RefCell` borrow across an await.
    pub(crate) backend: BuiltinBackend,
}

pub(crate) enum BuiltinBackend {
    /// `None` while a compile is in flight; a reentrant compile on the same backend is a
    /// structured [`ToolchainError::Unsupported`] rather than a `BorrowMutError` panic.
    Memory(RefCell<Option<MemoryStorage>>),
    Native(jals_exec::Exec),
}

impl BuiltinToolchain {
    /// A builtin backend over one in-memory project storage aggregate.
    pub const fn new(storage: MemoryStorage) -> Self {
        Self {
            backend: BuiltinBackend::Memory(RefCell::new(Some(storage))),
        }
    }

    /// Consume the toolchain and hand back its storage, so an in-memory host can read the
    /// outputs a compile produced.
    pub fn into_storage(self) -> MemoryStorage {
        match self.backend {
            BuiltinBackend::Memory(storage) => storage
                .into_inner()
                .expect("builtin storage is checked out by an in-flight build"),
            BuiltinBackend::Native(_) => {
                panic!("native builtin storage is owned by the project root")
            }
        }
    }

    /// Plan the copies [`compile`](Compiler::compile) would perform: each source (the project's
    /// own, then the source-dependency extras) mapped to its destination under the resolved
    /// `classes-dir`. Pure path math — total and filesystem-free — shared by `compile` and
    /// `describe_compile`, mirroring the subprocess backend's plan/spawn split.
    fn plan_copies(req: &CompileRequest<'_>) -> Vec<(PathBuf, PathBuf)> {
        let classes_dir = req.project_root.join(&req.manifest.build.classes_dir);
        let roots = req.manifest.source_roots(req.project_root);
        req.sources
            .iter()
            .chain(req.extra_sources.iter())
            .map(|src| {
                let dest = classes_dir.join(Self::relative_dest(src, &roots, req.project_root));
                (src.clone(), dest)
            })
            .collect()
    }

    /// Where `src` lands relative to the `classes-dir`: its path under the deepest source root
    /// containing it (the layout `javac -d` gives the `.class` files), else under the project
    /// root, else its bare file name (an out-of-tree extra source).
    fn relative_dest(src: &Path, roots: &[PathBuf], project_root: &Path) -> PathBuf {
        roots
            .iter()
            .filter_map(|root| src.strip_prefix(root).ok())
            .min_by_key(|rel| rel.components().count())
            .or_else(|| src.strip_prefix(project_root).ok())
            .map_or_else(
                || PathBuf::from(src.file_name().unwrap_or(src.as_os_str())),
                Path::to_path_buf,
            )
    }

    /// Read a planned source's bytes: from the immutable project view when it is a project file,
    /// else straight from its host path. Source-dependency (`extra_sources`) files are materialized
    /// under the project cache, which the project view deliberately excludes, so a project-view
    /// lookup misses them and they are read from disk instead.
    fn read_source(
        view: &ProjectView,
        key: Option<&FileKey>,
        host: &Path,
    ) -> Result<Vec<u8>, ToolchainError> {
        if let Some(key) = key {
            match view.file(key) {
                Ok(file) => return Ok(file.bytes().to_vec()),
                Err(Error::NotFoundFile(_)) => {}
                Err(error) => return Err(ToolchainError::Fs(error)),
            }
        }
        std::fs::read(host)
            .map_err(|error| ToolchainError::Fs(Error::Io(format!("{}: {error}", host.display()))))
    }

    fn key(path: &Path, project_root: &Path) -> Result<FileKey, ToolchainError> {
        jals_storage::RelativePath::from_host_path(project_root, path)
            .and_then(|relative| FileKey::new(relative).ok())
            .ok_or_else(|| {
                ToolchainError::Fs(jals_storage::Error::Io(format!(
                    "source is not addressable in project storage: {}",
                    path.display()
                )))
            })
    }

    async fn compile_in<S: SourceBackend, C: CacheBackend>(
        storage: &mut ProjectStorage<S, C>,
        req: &CompileRequest<'_>,
    ) -> Result<(), ToolchainError> {
        // Stage every copy in one transaction, so the outputs land in a single committed revision
        // and the tree is snapshotted once rather than per file.
        let mut copies = Vec::new();
        let read_revision;
        {
            let view = storage.view();
            read_revision = view.revision();
            for (src, dest) in Self::plan_copies(req) {
                let source = jals_storage::RelativePath::from_host_path(req.project_root, &src)
                    .and_then(|relative| FileKey::new(relative).ok());
                let destination = Self::key(&dest, req.project_root)?;
                let contents = Self::read_source(&view, source.as_ref(), &src)?;
                let exists = view.tree().file(&destination).is_some();
                copies.push((destination, contents, exists));
            }
        }
        let mut staged = std::collections::BTreeSet::new();
        // Commit against the revision the copies were read from, so an interleaved change
        // surfaces as a stale-revision error instead of silently clobbering.
        let mut transaction = storage
            .transaction(read_revision)
            .map_err(ToolchainError::Fs)?;
        for (destination, contents, exists) in copies {
            if exists || !staged.insert(destination.clone()) {
                transaction
                    .replace_file(destination, contents)
                    .map_err(ToolchainError::Fs)?;
            } else {
                transaction
                    .create_file(destination, contents)
                    .map_err(ToolchainError::Fs)?;
            }
        }
        transaction.commit().await.map_err(ToolchainError::Fs)?;
        Ok(())
    }
}

impl Compiler for BuiltinToolchain {
    fn compile<'a>(&'a self, req: &'a CompileRequest<'_>) -> ToolchainFuture<'a> {
        Box::pin(async move {
            match &self.backend {
                BuiltinBackend::Memory(cell) => {
                    // Check the storage out for the duration of the (awaiting) compile; no
                    // borrow is held across an await, and the storage is put back on every
                    // exit path.
                    let mut storage = cell
                        .borrow_mut()
                        .take()
                        .ok_or(ToolchainError::Unsupported("a concurrent builtin build"))?;
                    let result = Self::compile_in(&mut storage, req).await;
                    *cell.borrow_mut() = Some(storage);
                    result?;
                }
                BuiltinBackend::Native(exec) => {
                    let scopes =
                        Self::plan_copies(req)
                            .into_iter()
                            .flat_map(|(source, destination)| {
                                core::iter::once(source)
                                    .chain(core::iter::once(destination))
                                    .filter_map(|path| {
                                        jals_storage::RelativePath::from_host_path(
                                            req.project_root,
                                            &path,
                                        )
                                        .map(jals_storage::NativeScope::all)
                                    })
                            });
                    let mut storage = jals_storage::NativeStorage::for_project_scoped(
                        req.project_root,
                        scopes,
                        exec.clone(),
                    )
                    .await
                    .map_err(ToolchainError::Fs)?;
                    Self::compile_in(&mut storage, req).await?;
                }
            }
            Ok(BuildOutcome { code: Some(0) })
        })
    }

    fn describe_compile(&self, req: &CompileRequest<'_>) -> String {
        let copies = Self::plan_copies(req);
        let mut out = format!(
            "builtin: copy {} source file(s) into {} (dummy compiler; nothing is compiled)",
            copies.len(),
            req.project_root
                .join(&req.manifest.build.classes_dir)
                .display(),
        );
        for (src, dest) in &copies {
            // Writing to a `String` cannot fail.
            let _ = write!(out, "\n  {} -> {}", src.display(), dest.display());
        }
        out
    }
}

impl Runtime for BuiltinToolchain {
    fn run<'a>(&'a self, _req: &'a RunRequest<'_>) -> ToolchainFuture<'a> {
        // The dummy runtime executes nothing and reports success, so a builtin `jals run` drives
        // the compile-then-run pipeline end-to-end.
        Box::pin(async { Ok(BuildOutcome { code: Some(0) }) })
    }

    fn describe_run(&self, req: &RunRequest<'_>) -> String {
        format!(
            "builtin: skip running {} (dummy runtime; nothing is executed)",
            req.main_class
        )
    }
}

#[cfg(test)]
mod tests {
    use jals_config::Manifest;
    use jals_exec::block_on_inline;
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::*;

    fn toolchain(files: &[(&str, &str)]) -> BuiltinToolchain {
        let entries = files.iter().map(|(path, text)| {
            let relative = path.strip_prefix("/proj/").unwrap_or(path);
            Entry::File(FileKey::parse(relative).unwrap(), text.as_bytes().to_vec())
        });
        BuiltinToolchain::new(MemoryStorage::memory(CodeTree::new(entries).unwrap()))
    }

    fn compile_req<'a>(
        manifest: &'a Manifest,
        sources: &'a [PathBuf],
        extra_sources: &'a [PathBuf],
    ) -> CompileRequest<'a> {
        CompileRequest {
            manifest,
            project_root: Path::new("/proj"),
            sources,
            extra_sources,
            extra_classpath: &[],
        }
    }

    #[test]
    fn compile_copies_sources_into_classes_dir() {
        let manifest = Manifest::default(); // source-dirs = ["src/main/java"], classes-dir = "target/classes"
        let toolchain = toolchain(&[
            ("/proj/src/main/java/com/example/A.java", "class A {}"),
            ("/proj/vendor/Lib.java", "class Lib {}"),
        ]);
        let sources = vec![PathBuf::from("/proj/src/main/java/com/example/A.java")];
        let extra_sources = vec![PathBuf::from("/proj/vendor/Lib.java")];

        let outcome =
            block_on_inline(toolchain.compile(&compile_req(&manifest, &sources, &extra_sources)))
                .unwrap();
        assert!(outcome.success());

        let storage = toolchain.into_storage();
        // A source under a source root keeps its root-relative (package) layout; an out-of-tree
        // extra source flattens to its file name.
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("target/classes/com/example/A.java").unwrap())
                .unwrap()
                .text()
                .unwrap(),
            "class A {}"
        );
        assert_eq!(
            storage
                .view()
                .file(&FileKey::parse("target/classes/vendor/Lib.java").unwrap())
                .unwrap()
                .text()
                .unwrap(),
            "class Lib {}"
        );
    }

    #[test]
    fn compile_source_under_project_but_outside_roots_keeps_project_layout() {
        let manifest = Manifest::default();
        let toolchain = toolchain(&[("/proj/gen/B.java", "class B {}")]);
        let sources = vec![PathBuf::from("/proj/gen/B.java")];

        block_on_inline(toolchain.compile(&compile_req(&manifest, &sources, &[]))).unwrap();
        assert!(
            toolchain
                .into_storage()
                .view()
                .tree()
                .file(&FileKey::parse("target/classes/gen/B.java").unwrap())
                .is_some()
        );
    }

    #[test]
    fn compile_missing_source_is_an_fs_error() {
        let manifest = Manifest::default();
        let toolchain = toolchain(&[]);
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];

        let err =
            block_on_inline(toolchain.compile(&compile_req(&manifest, &sources, &[]))).unwrap_err();
        assert!(matches!(err, ToolchainError::Fs(_)), "got {err}");
    }

    #[test]
    fn run_is_a_successful_no_op() {
        let manifest = Manifest::default();
        let toolchain = toolchain(&[]);
        let req = RunRequest {
            manifest: &manifest,
            project_root: Path::new("/proj"),
            main_class: "com.example.Main",
            program_args: &[],
            extra_classpath: &[],
        };
        assert!(block_on_inline(toolchain.run(&req)).unwrap().success());
        assert_eq!(
            toolchain.describe_run(&req),
            "builtin: skip running com.example.Main (dummy runtime; nothing is executed)"
        );
    }

    #[test]
    fn describe_compile_names_the_plan() {
        let manifest = Manifest::default();
        let toolchain = toolchain(&[]);
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];

        let description = toolchain.describe_compile(&compile_req(&manifest, &sources, &[]));
        assert!(description.starts_with(
            "builtin: copy 1 source file(s) into /proj/target/classes (dummy compiler; nothing is compiled)"
        ));
        assert!(description.contains("/proj/src/main/java/A.java -> /proj/target/classes/A.java"));
    }

    #[test]
    fn native_builtin_reads_a_source_materialized_under_the_excluded_cache() {
        let project = tempfile::tempdir().unwrap();
        let source = project
            .path()
            .join("target/jals/cache/source-view/provenance/content/pkg/Lib.java");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"package pkg; class Lib {}").unwrap();
        let manifest = Manifest::default();
        let sources = [source];
        let request = CompileRequest {
            manifest: &manifest,
            project_root: project.path(),
            sources: &[],
            extra_sources: &sources,
            extra_classpath: &[],
        };
        let destination = BuiltinToolchain::plan_copies(&request).pop().unwrap().1;
        let toolchain = BuiltinToolchain {
            backend: BuiltinBackend::Native(jals_exec::Exec::inline()),
        };

        assert!(
            block_on_inline(toolchain.compile(&request))
                .unwrap()
                .success()
        );
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"package pkg; class Lib {}"
        );
    }
}
