//! The built-in (in-process) toolchain backend, selected by `[toolchain] compiler = "builtin"` /
//! `runtime = "builtin"`.
//!
//! [`BuiltinToolchain`] implements both halves of the pure toolchain seam — the [`Compiler`] and
//! [`Runtime`] traits — next to the host `SubprocessToolchain`, and is the seam a real embedded
//! compiler will eventually fill. Today it is a **dummy**: `compile` copies every requested source
//! file into the resolved `classes-dir` verbatim (nothing is compiled), and `run` is a no-op that
//! reports success (nothing is executed). What earns it a place now is the shape, not the
//! behavior: it exercises the whole request → backend → outcome path with a non-subprocess
//! backend, and all its I/O goes through a [`jals_fs::FileTree`], so the same implementation
//! drives the host filesystem (`OsFileTree`) and an in-memory tree (`InMemoryFileTree`, a browser
//! host) alike. A future in-process compiler replaces the copy step and inherits everything else.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use jals_fs::{FileTree, FsError};

use crate::manifest_ext::ManifestExt;
use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::{BuildOutcome, Compiler, Runtime, ToolchainError};

/// A [`Compiler`] + [`Runtime`] backend realized in-process over a [`FileTree`] — today a dummy
/// that copies sources instead of compiling them and skips running entirely (see the module docs).
pub struct BuiltinToolchain {
    /// The file tree the backend reads sources from and writes outputs to. `RefCell` bridges the
    /// receivers: the toolchain traits' methods take `&self` while [`FileTree::write`] needs
    /// `&mut`, and a backend is driven single-threaded.
    tree: RefCell<Box<dyn FileTree>>,
}

impl BuiltinToolchain {
    /// A builtin backend over `tree` — the host filesystem (`OsFileTree`) for the CLI, an
    /// `InMemoryFileTree` for tests and browser hosts.
    pub fn new(tree: Box<dyn FileTree>) -> Self {
        Self {
            tree: RefCell::new(tree),
        }
    }

    /// Consume the toolchain and hand back its file tree, so an in-memory host can read the
    /// outputs a compile produced.
    pub fn into_tree(self) -> Box<dyn FileTree> {
        self.tree.into_inner()
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

    /// Render a path for the [`FileTree`] (a UTF-8 virtual path). A non-UTF-8 path cannot be
    /// addressed through the tree, so it fails as an error naming the path rather than panicking.
    fn tree_path(path: &Path) -> Result<&str, ToolchainError> {
        path.to_str().ok_or_else(|| {
            ToolchainError::Fs(FsError::Io(format!(
                "path is not valid UTF-8: {}",
                path.display()
            )))
        })
    }
}

impl Compiler for BuiltinToolchain {
    fn compile(&self, req: &CompileRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        let mut tree = self.tree.borrow_mut();
        for (src, dest) in Self::plan_copies(req) {
            let contents = tree
                .read(Self::tree_path(&src)?)
                .map_err(ToolchainError::Fs)?;
            tree.write(Self::tree_path(&dest)?, &contents)
                .map_err(ToolchainError::Fs)?;
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
    use jals_fs::InMemoryFileTree;

    use super::*;

    fn toolchain(files: &[(&str, &str)]) -> BuiltinToolchain {
        let mut tree = InMemoryFileTree::new();
        for (path, text) in files {
            tree.write(path, text.as_bytes()).unwrap();
        }
        BuiltinToolchain::new(Box::new(tree))
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
            ("/other/lib/Lib.java", "class Lib {}"),
        ]);
        let sources = vec![PathBuf::from("/proj/src/main/java/com/example/A.java")];
        let extra_sources = vec![PathBuf::from("/other/lib/Lib.java")];

        let outcome = toolchain
            .compile(&compile_req(&manifest, &sources, &extra_sources))
            .unwrap();
        assert!(outcome.success());

        let tree = toolchain.into_tree();
        // A source under a source root keeps its root-relative (package) layout; an out-of-tree
        // extra source flattens to its file name.
        assert_eq!(
            tree.read_to_string("/proj/target/classes/com/example/A.java")
                .unwrap(),
            "class A {}"
        );
        assert_eq!(
            tree.read_to_string("/proj/target/classes/Lib.java")
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
                .into_tree()
                .is_file("/proj/target/classes/gen/B.java")
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
