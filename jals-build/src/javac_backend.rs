//! The `javac` adapter: the one [`Backend`] that needs a host process.
//!
//! [`JalsBackend`](crate::JalsBackend) compiles a lowered tree in this process and hands the class
//! files back. `javac` cannot: it reads files from disk, writes its output itself through `-d`, and
//! needs a JDK to run in. This module is what makes that difference an *adapter* rather than a
//! second interface — [`JavacBackend`] implements the same [`Backend`] contract and drives the
//! existing [`Compiler`] invocation layer beneath it.
//!
//! [`BackendRequest`] is portable — bytes, cache keys and `[build]` knobs — so every input with a
//! host path in it arrives at construction time instead, in [`HostCompileInputs`], and the adapter
//! owns it. That is the whole reason the seam can stay ungated while this module is `native`-gated:
//! the *contract* needs no filesystem, only this implementation does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jals_config::{BackendKind, Manifest};
use jals_exec::Exec;
use jals_storage::{ContentDigest, ProvenanceFold};

use jals_progress::{Activity, Outcome};

use crate::backend::{
    Backend, BackendError, BackendFuture, BackendOutcome, BackendRequest, BackendSelection,
};
use crate::request::CompileRequest;
use crate::staging::StagedTree;
use crate::toolchain::{Compiler, ToolIdentity};

/// The compile inputs a [`BackendRequest`] deliberately cannot carry.
///
/// Everything here is already resolved by the host and shaped like a host path, which is exactly
/// what keeps it out of the portable request. Borrowed for the call that builds the backend; the
/// backend owns a copy afterwards, because [`BackendSelection::Available`] is a
/// `Box<dyn Backend>` and therefore `'static`.
pub struct HostCompileInputs<'a> {
    /// Already-resolved `.java` sources compiled alongside the lowered tree — the `git`/`path`
    /// source dependencies, which are lowered under their own manifest's frontend and so never
    /// appear in this project's tree.
    pub extra_sources: &'a [PathBuf],
    /// Already-resolved dependency jars, appended after the manifest's `[build] classpath`.
    pub extra_classpath: &'a [PathBuf],
    /// Extra `javac` arguments a build script contributed, appended after `[build] javac-flags`.
    pub extra_javac_args: &'a [String],
    /// Explicit environment entries for the compiler subprocess.
    pub compile_env: &'a BTreeMap<String, String>,
}

/// The [`Backend`] that compiles a lowered tree with the host's `javac`.
///
/// Built by [`BackendSelection::for_host`]. Wraps a `&dyn Compiler` rather than reaching for
/// `Command` itself, so `[toolchain] compiler` keeps selecting *which* tool runs — including the
/// builtin dummy — while `[build] backend` selects that a host tool runs at all. The two selectors
/// stay independent, as they were before this seam existed.
pub struct JavacBackend {
    /// The invocation layer: the host `javac`, or the builtin dummy.
    compiler: Box<dyn Compiler>,
    /// The manifest, with `[build] source-dirs` already replaced by the staging root — the host does
    /// that before staging so `-sourcepath` excludes the authored roots entirely.
    manifest: Manifest,
    project_root: PathBuf,
    /// Where the lowered tree was materialized. Each request's sources are resolved beneath it.
    staging_root: PathBuf,
    extra_sources: Vec<PathBuf>,
    extra_classpath: Vec<PathBuf>,
    extra_javac_args: Vec<String>,
    compile_env: BTreeMap<String, String>,
}

impl JavacBackend {
    /// Build the adapter for a project whose lowered tree is already staged.
    ///
    /// Taking a [`StagedTree`] is the precondition expressed as a type: it has no constructor other
    /// than [`StagedTree::write`], so a `JavacBackend` cannot exist for a tree that was never
    /// written out.
    async fn new(
        manifest: &Manifest,
        project_root: &Path,
        staged: &StagedTree,
        inputs: &HostCompileInputs<'_>,
        exec: &Exec,
    ) -> Self {
        Self {
            compiler: <dyn Compiler>::select(manifest, exec).await,
            manifest: manifest.clone(),
            project_root: project_root.to_path_buf(),
            staging_root: staged.root().to_path_buf(),
            extra_sources: inputs.extra_sources.to_vec(),
            extra_classpath: inputs.extra_classpath.to_vec(),
            extra_javac_args: inputs.extra_javac_args.to_vec(),
            compile_env: inputs.compile_env.clone(),
        }
    }

    /// Where the staged copy of each requested file lives, in request order.
    ///
    /// Derived from `req.tree` rather than read off [`StagedTree::sources`], so the *request* stays
    /// the definition of what compiles. A host that staged one tree and requested another then gets
    /// a missing file from `javac`, instead of quietly compiling a different source set.
    fn staged_sources(&self, req: &BackendRequest<'_>) -> Vec<PathBuf> {
        req.tree
            .iter()
            .map(|source| source.path.to_host_path(&self.staging_root))
            .collect()
    }

    /// The invocation-layer request for `sources`, which must outlive it.
    fn compile_request<'a>(&'a self, sources: &'a [PathBuf]) -> CompileRequest<'a> {
        CompileRequest {
            manifest: &self.manifest,
            project_root: &self.project_root,
            sources,
            extra_sources: &self.extra_sources,
            extra_classpath: &self.extra_classpath,
            extra_javac_args: &self.extra_javac_args,
            compile_env: &self.compile_env,
        }
    }
}

impl Backend for JavacBackend {
    fn id(&self) -> &'static str {
        // The manifest tag, not a literal: that identity has to be one string and not two.
        BackendKind::Javac {}.tag_name()
    }

    fn config_digest(&self, req: &BackendRequest<'_>) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"jals.backend.javac\0");
        // Resolved here rather than cached on the struct: this is the only reader, and the
        // resolution probes the filesystem, which a build that never asks for a digest should not
        // pay for.
        //
        // TODO(backend-tier): this names the tool but not its *version*. Reading that means running
        // `javac -version`, which is a spawn this method cannot do, and it has to be folded in
        // before `CacheNamespace::BackendOutput` memoization is switched on — otherwise upgrading a
        // JDK in place silently reuses class files the previous compiler emitted.
        match &self.compiler.tool_identity(&self.project_root) {
            ToolIdentity::Builtin => fold.bytes(b"builtin"),
            ToolIdentity::Program(path) => fold
                .bytes(b"program")
                .bytes(path.as_os_str().as_encoded_bytes()),
        };
        // The `[build]` knobs the request carries: release/source/target and the flag list.
        fold.digest(req.options.digest());
        // Everything else `Invocation::build` reads that affects the output. The manifest's own
        // `javac-flags` are normally also in `options.extra_args`; folding a value twice cannot
        // cause a collision, and not folding it here would assume a host wiring this adapter must
        // not depend on.
        //
        // `source-dirs` is deliberately absent. It becomes `-sourcepath`, which only names where to
        // look for sources that were not passed explicitly — and every source *is* passed
        // explicitly, from `req.tree`, whose keys already identify it. A type javac could only find
        // by searching there would be one the frontend never lowered, which staging exists to make
        // impossible. Folding it would also drag a host-derived value into a content identity: the
        // host replaces `source-dirs` with the staging root, absolute when it cannot be made
        // relative, so the same project would digest differently from a different directory.
        let build = &self.manifest.build;
        for entry in build.classpath.iter().chain(&build.javac_flags) {
            fold.bytes(entry.as_bytes());
        }
        // TODO(backend-tier): `classes-dir` is folded because memoizing this backend means *skipping*
        // a compile, which is only valid if the output is already where this compile would put it —
        // but `--out-dir` can make it an absolute host path, so it has to be normalized against the
        // project root before `CacheNamespace::BackendOutput` is switched on.
        fold.bytes(build.classes_dir.as_bytes());
        for path in self.extra_classpath.iter().chain(&self.extra_sources) {
            fold.bytes(path.as_os_str().as_encoded_bytes());
        }
        for arg in &self.extra_javac_args {
            fold.bytes(arg.as_bytes());
        }
        // The compiler's environment: `JAVA_TOOL_OPTIONS` and friends change what a compile does.
        for (name, value) in &self.compile_env {
            fold.bytes(name.as_bytes()).bytes(value.as_bytes());
        }
        fold.finish()
    }

    fn compile<'a>(&'a self, req: &'a BackendRequest<'a>) -> BackendFuture<'a> {
        Box::pin(async move {
            let sources = self.staged_sources(req);
            let request = self.compile_request(&sources);
            // One unit with no count: `javac` is a single process that says nothing until it is
            // finished, so a spinner is the honest picture. The in-process backend, which can see
            // each file go past, opens a bounded one instead.
            let report = req.progress.begin(Activity::Compile, "");
            let outcome = match self.compiler.compile(&request).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    report.finish(Outcome::Failed);
                    return Err(BackendError::Launch(error.to_string()));
                }
            };
            // The exit code is the whole result; see `BackendOutcome::from_code` for why.
            let outcome = BackendOutcome::from_code(outcome.code);
            report.finish(if outcome.success() {
                Outcome::Completed
            } else {
                Outcome::Failed
            });
            Ok(outcome)
        })
    }

    fn describe(&self, req: &BackendRequest<'_>) -> String {
        let sources = self.staged_sources(req);
        self.compiler
            .describe_compile(&self.compile_request(&sources))
    }
}

impl BackendSelection {
    /// The backend `[build] backend` names, on a host that can spawn a process.
    ///
    /// Adds the one arm [`in_process`](BackendSelection::in_process) cannot answer — `javac`, which
    /// needs host paths and a JDK — and delegates the other two straight back to it, so every
    /// [`BackendKind`] is still answered in exactly one place.
    ///
    /// Never reports [`ToolMissing`](crate::BackendAbsence::ToolMissing), deliberately. Tool
    /// resolution probes the candidates the `[toolchain]` selection and `$JAVA_HOME` imply and, when
    /// none exists, falls back to the bare `javac` for the OS to resolve on `PATH` — which is how a
    /// plain `apt install default-jdk` host works. Treating "no probed candidate" as an absent tool
    /// would refuse to build on exactly those hosts. Detecting it for real needs a `PATH` search,
    /// not a different verdict here.
    pub async fn for_host(
        manifest: &Manifest,
        project_root: &Path,
        staged: &StagedTree,
        inputs: &HostCompileInputs<'_>,
        exec: &Exec,
    ) -> Self {
        match manifest.build.backend {
            BackendKind::Javac {} => Self::Available(Box::new(
                JavacBackend::new(manifest, project_root, staged, inputs, exec).await,
            )),
            other => Self::in_process(other, manifest.build.release),
        }
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;
    use jals_storage::{CacheKey, CacheNamespace, RelativePath};

    use super::*;
    use crate::backend::{BackendOptions, BackendSource};

    /// A lowered tree staged under `root`, with the matching request sources.
    ///
    /// Goes through [`StagedTree::write`] because that is its only constructor — the same property
    /// that makes a `StagedTree` a usable precondition for [`JavacBackend`].
    fn stage_tree(root: PathBuf, paths: &[&str]) -> (StagedTree, Vec<BackendSource>) {
        let sources: Vec<_> = paths
            .iter()
            .map(|path| {
                let path = RelativePath::parse(path).expect("a valid path");
                let bytes = b"class Staged {}".to_vec();
                let key = CacheKey::new(
                    CacheNamespace::FrontendOutput,
                    ContentDigest::of(path.to_string().as_bytes()),
                    ContentDigest::of(&bytes),
                );
                BackendSource { path, key, bytes }
            })
            .collect();
        let staged = block_on_inline(StagedTree::write(&sources, root)).expect("staging the tree");
        (staged, sources)
    }

    fn javac_backend(manifest: &Manifest, root: &Path, staged: &StagedTree) -> JavacBackend {
        block_on_inline(JavacBackend::new(
            manifest,
            root,
            staged,
            &HostCompileInputs {
                extra_sources: &[],
                extra_classpath: &[],
                extra_javac_args: &[],
                compile_env: &BTreeMap::new(),
            },
            &Exec::inline(),
        ))
    }

    fn request<'a>(
        sources: &'a [BackendSource],
        options: &'a BackendOptions,
    ) -> BackendRequest<'a> {
        BackendRequest {
            progress: &jals_progress::Progress::SILENT,
            tree: sources,
            classpath: &[],
            options,
        }
    }

    #[test]
    fn the_adapter_is_named_by_its_manifest_tag() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let (staged, _) = stage_tree(root.join("staged"), &["src/main/java/A.java"]);
        let manifest = Manifest::default();

        assert_eq!(
            javac_backend(&manifest, &root, &staged).id(),
            BackendKind::Javac {}.tag_name()
        );
    }

    #[test]
    fn the_toolchain_selection_still_decides_which_tool_runs() {
        // `[build] backend = "javac"` says *a host tool* compiles; `[toolchain] compiler` says which
        // one. Routing the backend through this adapter must not collapse that distinction.
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let (staged, sources) = stage_tree(root.join("staged"), &["src/main/java/A.java"]);
        let options = BackendOptions::default();
        let request = request(&sources, &options);

        let manifest = Manifest::default();
        let description = javac_backend(&manifest, &root, &staged).describe(&request);
        assert!(
            description.contains("javac"),
            "the default manifest should plan a `javac` command, got {description}"
        );

        let builtin: Manifest = "[toolchain]\ncompiler = \"builtin\"\n".parse().unwrap();
        assert!(
            javac_backend(&builtin, &root, &staged)
                .describe(&request)
                .starts_with("builtin:")
        );
    }

    #[test]
    fn the_config_digest_separates_the_builtin_dummy_from_a_real_compiler() {
        // The two produce completely unrelated output — one compiles, the other copies sources
        // verbatim — so they must never share a cache identity.
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let (staged, sources) = stage_tree(root.join("staged"), &["src/main/java/A.java"]);
        let options = BackendOptions::default();
        let request = request(&sources, &options);

        let subprocess = Manifest::default();
        let builtin: Manifest = "[toolchain]\ncompiler = \"builtin\"\n".parse().unwrap();
        assert_ne!(
            javac_backend(&subprocess, &root, &staged).config_digest(&request),
            javac_backend(&builtin, &root, &staged).config_digest(&request)
        );
    }

    #[test]
    fn the_config_digest_folds_the_compile_options_and_the_manifest_flags() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let (staged, sources) = stage_tree(root.join("staged"), &["src/main/java/A.java"]);
        let manifest = Manifest::default();
        let backend = javac_backend(&manifest, &root, &staged);

        let digest = |release| {
            let options = BackendOptions {
                release,
                ..BackendOptions::default()
            };
            backend.config_digest(&request(&sources, &options))
        };
        assert_ne!(digest(Some(17)), digest(Some(21)));

        // A manifest flag the request does not carry still changes the compile.
        let flagged: Manifest = "[build]\njavac-flags = [\"-Xlint:all\"]\n".parse().unwrap();
        let options = BackendOptions::default();
        assert_ne!(
            backend.config_digest(&request(&sources, &options)),
            javac_backend(&flagged, &root, &staged).config_digest(&request(&sources, &options))
        );
    }

    /// `source-dirs` is not part of the identity, deliberately.
    ///
    /// It becomes `-sourcepath`, which only says where to look for sources not passed explicitly —
    /// and all of them are passed explicitly. The host also *rewrites* this field to the staging
    /// root, absolute when it cannot be made relative, so folding it would make the same project
    /// digest differently depending on which directory it was built from. Pinned because the
    /// tempting "fold everything the invocation reads" fix reintroduces exactly that.
    #[test]
    fn a_rewritten_source_path_is_not_part_of_the_identity() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let (staged, sources) = stage_tree(root.join("staged"), &["src/main/java/A.java"]);
        let options = BackendOptions::default();
        let request = request(&sources, &options);

        let here: Manifest = "[build]\nsource-dirs = [\"src/main/java\"]\n"
            .parse()
            .unwrap();
        let elsewhere: Manifest = "[build]\nsource-dirs = [\"/somewhere/else/target/staged\"]\n"
            .parse()
            .unwrap();
        assert_eq!(
            javac_backend(&here, &root, &staged).config_digest(&request),
            javac_backend(&elsewhere, &root, &staged).config_digest(&request)
        );
    }

    #[test]
    fn the_request_decides_which_staged_files_compile() {
        // The source list is derived from `req.tree` beneath the staging root, not read off the
        // staged tree, so the request stays the definition of what compiles.
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let staging_root = root.join("staged");
        let (staged, sources) = stage_tree(
            staging_root.clone(),
            &["src/main/java/A.java", "src/main/java/pkg/B.java"],
        );
        let manifest = Manifest::default();
        let options = BackendOptions::default();

        let planned =
            javac_backend(&manifest, &root, &staged).staged_sources(&request(&sources, &options));
        assert_eq!(
            planned,
            vec![
                staging_root.join("src/main/java/A.java"),
                staging_root.join("src/main/java/pkg/B.java"),
            ]
        );
        // Which is exactly what staging wrote — the two agree when the host requests what it staged.
        assert_eq!(planned, staged.sources());
    }

    #[test]
    fn the_selection_answers_every_backend_kind() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().to_path_buf();
        let (staged, _) = stage_tree(root.join("staged"), &["src/main/java/A.java"]);
        let inputs = HostCompileInputs {
            extra_sources: &[],
            extra_classpath: &[],
            extra_javac_args: &[],
            compile_env: &BTreeMap::new(),
        };

        // On a host that can spawn, every kind is available — including the `javac` arm that
        // `in_process` has to report absent.
        for (source, expected) in [
            ("", BackendKind::Javac {}.tag_name()),
            (
                "[build]\nbackend = { type = \"jals\" }\n",
                BackendKind::Jals {}.tag_name(),
            ),
            (
                "[build]\nbackend = { type = \"jals-wasm\" }\n",
                BackendKind::JalsWasm {}.tag_name(),
            ),
        ] {
            let manifest: Manifest = source.parse().unwrap();
            let selection = block_on_inline(BackendSelection::for_host(
                &manifest,
                &root,
                &staged,
                &inputs,
                &Exec::inline(),
            ));
            match selection {
                BackendSelection::Available(backend) => assert_eq!(backend.id(), expected),
                BackendSelection::Absent { id, reason } => {
                    panic!("`{id}` should be available on a native host, got {reason}")
                }
            }
        }
    }
}
