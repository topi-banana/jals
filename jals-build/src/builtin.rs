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

use jals_storage::{CacheBackend, FileKey, MemoryStorage, ProjectStorage, SourceBackend};

use crate::manifest_ext::ManifestExt;
use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::{BuildOutcome, Compiler, Runtime, ToolchainError};

/// A [`Compiler`] + [`Runtime`] backend realized in-process over [`ProjectStorage`] — today a dummy
/// that copies sources instead of compiling them and skips running entirely (see the module docs).
pub struct BuiltinToolchain {
    /// The storage aggregate the backend reads and commits. `RefCell` bridges the receivers because
    /// toolchain methods take `&self` while transactions require `&mut` access.
    pub(crate) backend: BuiltinBackend,
}

pub(crate) enum BuiltinBackend {
    Memory(RefCell<MemoryStorage>),
    #[cfg(feature = "native")]
    Native,
}

impl BuiltinToolchain {
    /// A builtin backend over one in-memory project storage aggregate.
    pub const fn new(storage: MemoryStorage) -> Self {
        Self {
            backend: BuiltinBackend::Memory(RefCell::new(storage)),
        }
    }

    /// Consume the toolchain and hand back its file tree, so an in-memory host can read the
    /// outputs a compile produced.
    pub fn into_storage(self) -> MemoryStorage {
        match self.backend {
            BuiltinBackend::Memory(storage) => storage.into_inner(),
            #[cfg(feature = "native")]
            BuiltinBackend::Native => panic!("native builtin storage is owned by the project root"),
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

    fn compile_in<S: SourceBackend, C: CacheBackend>(
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
                let source = Self::key(&src, req.project_root)?;
                let destination = Self::key(&dest, req.project_root)?;
                let contents = view
                    .file(&source)
                    .map_err(ToolchainError::Fs)?
                    .bytes()
                    .to_vec();
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
        transaction.commit().map_err(ToolchainError::Fs)?;
        Ok(())
    }
}

impl Compiler for BuiltinToolchain {
    fn compile(&self, req: &CompileRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        match &self.backend {
            BuiltinBackend::Memory(storage) => Self::compile_in(&mut storage.borrow_mut(), req)?,
            #[cfg(feature = "native")]
            BuiltinBackend::Native => {
                let mut storage = jals_storage::NativeStorage::for_project(req.project_root)
                    .map_err(ToolchainError::Fs)?;
                Self::compile_in(&mut storage, req)?;
            }
        }
        Ok(BuildOutcome { code: Some(0) })
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
    fn run(&self, _req: &RunRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        // The dummy runtime executes nothing and reports success, so a builtin `jals run` drives
        // the compile-then-run pipeline end-to-end.
        Ok(BuildOutcome { code: Some(0) })
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

        let outcome = toolchain
            .compile(&compile_req(&manifest, &sources, &extra_sources))
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

        toolchain
            .compile(&compile_req(&manifest, &sources, &[]))
            .unwrap();
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

        let err = toolchain
            .compile(&compile_req(&manifest, &sources, &[]))
            .unwrap_err();
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
        assert!(toolchain.run(&req).unwrap().success());
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
}
