//! The host toolchain: discover installed JDKs and spawn `javac`/`java`.
//!
//! [`SubprocessToolchain`] is the default (`native` feature) implementation of the pure
//! [`Compiler`] and [`Runtime`] traits. It is the one place in `jals-build` that touches the
//! filesystem or spawns a process, so the rest of the crate stays pure/`wasm32`-buildable.
//! Selection is entirely the pure [`ToolResolver`] policy — an `$JAVAC`/`$JAVA` env override wins,
//! then the manifest's `[toolchain]` selection (its [`ToolSpec`](jals_config::ToolSpec) view),
//! then `$JAVA_HOME`, then the bare name on `PATH` — this module only reads the env vars, scans
//! the common install
//! locations (SDKMAN, `~/.jdks`, `~/.jdk`, `/usr/lib/jvm`, the macOS JVM bundle directory) when a
//! distribution selector needs them, probes which candidate exists, and spawns. Downloading a
//! missing JDK is future work.

use std::path::{Path, PathBuf};
use std::process::Command;

use jals_config::{Compiler as CompilerSpec, Manifest, Runtime as RuntimeSpec, ToolSpec};
use jals_exec::Exec;
use jals_exec::tokio_rt::on_blocking_pool;

use crate::builtin::BuiltinToolchain;
use crate::invocation::Invocation;
use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::{
    BuildOutcome, Compiler, JdkInstall, Runtime, Tool, ToolResolver, ToolchainError,
    ToolchainFuture,
};

impl dyn Compiler {
    /// Select the backend the manifest's `[toolchain] compiler` names, as one boxed [`Compiler`]:
    /// matching the [`jals_config::Compiler`] variant routes `"builtin"` to the in-process
    /// [`BuiltinToolchain`] (over the host filesystem) and every `javac` selector to the host
    /// [`SubprocessToolchain`] — so a host drives one `&dyn Compiler`, whatever the manifest
    /// selects. The `exec` handle backs the builtin backend's native project storage.
    pub async fn select(manifest: &Manifest, exec: &Exec) -> Box<dyn Compiler> {
        match &manifest.toolchain.compiler {
            CompilerSpec::Builtin => Box::new(BuiltinToolchain::host(exec.clone())),
            CompilerSpec::System | CompilerSpec::Path(_) | CompilerSpec::Distribution(_) => {
                Box::new(SubprocessToolchain::from_manifest(manifest).await)
            }
        }
    }
}

impl dyn Runtime {
    /// Select the backend the manifest's `[toolchain] runtime` names, as one boxed [`Runtime`] —
    /// the run-step mirror of `<dyn Compiler>::select`, matching [`jals_config::Runtime`]. The two
    /// selections are independent, so a builtin compile can pair with a real `java` run (and vice
    /// versa) with no routing composite in between.
    pub async fn select(manifest: &Manifest, exec: &Exec) -> Box<dyn Runtime> {
        match &manifest.toolchain.runtime {
            RuntimeSpec::Builtin => Box::new(BuiltinToolchain::host(exec.clone())),
            RuntimeSpec::System | RuntimeSpec::Path(_) | RuntimeSpec::Distribution(_) => {
                Box::new(SubprocessToolchain::from_manifest(manifest).await)
            }
        }
    }
}

impl BuiltinToolchain {
    /// The builtin backend over the host filesystem.
    const fn host(exec: Exec) -> Self {
        Self {
            backend: super::builtin::BuiltinBackend::Native(exec),
        }
    }
}

/// A [`Compiler`] + [`Runtime`] backend that spawns the host's `javac`/`java`, selected per the
/// manifest's `[toolchain]`.
///
/// Built with [`from_manifest`](SubprocessToolchain::from_manifest); the discovered JDK installs are
/// cached (and skipped entirely unless a distribution selector needs them).
pub struct SubprocessToolchain {
    /// The `javac` selection (`[toolchain] compiler`).
    compiler: CompilerSpec,
    /// The `java` selection (`[toolchain] runtime`).
    runtime: RuntimeSpec,
    /// The installed JDKs discovered on this host (empty when no distribution selector needs them).
    installs: Vec<JdkInstall>,
    /// The platform classpath separator (`:` on Unix, `;` on Windows) — a command-line encoding
    /// detail this backend owns, injected into the pure [`Invocation`] planners.
    path_sep: char,
}

impl SubprocessToolchain {
    /// Build a toolchain from a manifest's `[toolchain]` selection.
    ///
    /// Discovers installed JDKs only when a [`ToolSpec::Distribution`] selector is present (the
    /// common no-`[toolchain]` project pays no discovery cost).
    pub async fn from_manifest(manifest: &Manifest) -> Self {
        let tc = &manifest.toolchain;
        let needs_discovery = matches!(tc.compiler.spec(), Some(ToolSpec::Distribution { .. }))
            || matches!(tc.runtime.spec(), Some(ToolSpec::Distribution { .. }));
        Self {
            compiler: tc.compiler.clone(),
            runtime: tc.runtime.clone(),
            installs: if needs_discovery {
                on_blocking_pool(Self::discover_installs).await
            } else {
                Vec::new()
            },
            path_sep: if cfg!(windows) { ';' } else { ':' },
        }
    }

    /// The [`ToolSpec`] view governing `tool`, or `None` when that half of the manifest selects
    /// the in-process backend (normally routed away by the `select` factories before any resolver
    /// runs; the resolver answers `None` with the system tools).
    fn spec(&self, tool: Tool) -> Option<ToolSpec<'_>> {
        match tool {
            Tool::Javac => self.compiler.spec(),
            Tool::Java => self.runtime.spec(),
        }
    }

    /// The [`Candidates`](crate::Candidates) for `tool`: the environment (`$JAVAC`/`$JAVA`,
    /// `$JAVA_HOME`, `$HOME`) read into the pure [`ToolResolver`] policy.
    fn candidates(&self, tool: Tool, project_root: &Path) -> crate::Candidates {
        let env_override = std::env::var_os(tool.env_var()).map(PathBuf::from);
        let java_home = std::env::var_os("JAVA_HOME").map(PathBuf::from);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let resolver = ToolResolver {
            installs: &self.installs,
            java_home: java_home.as_deref(),
            home: home.as_deref(),
            project_root,
        };
        resolver.resolve(tool, self.spec(tool), env_override)
    }

    /// Resolve `tool` to a concrete program path, probing candidate existence off the executor.
    async fn resolve_program(&self, tool: Tool, project_root: &Path) -> PathBuf {
        self.candidates(tool, project_root)
            .pick_async(async |path: &Path| {
                let path = path.to_path_buf();
                on_blocking_pool(move || path.is_file()).await
            })
            .await
    }

    /// [`resolve_program`](Self::resolve_program) with inline probes, for the display-only
    /// `describe_*` paths (a handful of `stat` calls; not worth suspending over).
    fn resolve_program_blocking(&self, tool: Tool, project_root: &Path) -> PathBuf {
        self.candidates(tool, project_root).pick(Path::is_file)
    }

    /// The `javac` [`Invocation`] with its program resolved, ready to spawn.
    async fn plan_compile(&self, req: &CompileRequest<'_>) -> Invocation {
        Invocation::build(req, self.path_sep)
            .with_program(self.resolve_program(Tool::Javac, req.project_root).await)
    }

    /// The `java` [`Invocation`] with its program resolved, ready to spawn.
    async fn plan_run(&self, req: &RunRequest<'_>) -> Invocation {
        Invocation::run(req, self.path_sep)
            .with_program(self.resolve_program(Tool::Java, req.project_root).await)
    }

    /// Spawn an invocation on the blocking pool, inheriting stdio, and map the exit status to a
    /// [`BuildOutcome`]. The subprocess wait blocks one pool thread; the executor keeps running.
    async fn spawn(invocation: Invocation) -> Result<BuildOutcome, ToolchainError> {
        on_blocking_pool(move || {
            let status = Command::new(&invocation.program)
                .args(&invocation.args)
                .current_dir(&invocation.working_dir)
                .envs(&invocation.environment)
                .status()
                .map_err(|source| ToolchainError::Spawn {
                    program: invocation.program.clone(),
                    source,
                })?;
            Ok(BuildOutcome {
                code: status.code(),
            })
        })
        .await
    }

    /// Scan the common JDK install locations and describe each install for [`ToolResolver`].
    fn discover_installs() -> Vec<JdkInstall> {
        let mut installs = Vec::new();
        for root in Self::install_roots() {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                // The entry is normally the JDK home itself; macOS bundles it under
                // `<entry>/Contents/Home`.
                let home = if dir.join("bin").is_dir() {
                    dir
                } else {
                    let bundled = dir.join("Contents/Home");
                    if !bundled.join("bin").is_dir() {
                        continue;
                    }
                    bundled
                };
                installs.push(JdkInstall::from_install_name(home, &name));
            }
        }
        installs
    }

    /// The directories that contain per-JDK subdirectories, across the common install managers/OSes.
    fn install_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            roots.push(home.join(".sdkman/candidates/java")); // SDKMAN
            roots.push(home.join(".jdks")); // IntelliJ IDEA
            roots.push(home.join(".jdk")); // common manual / per-user install dir
        }
        if let Some(sdkman) = std::env::var_os("SDKMAN_CANDIDATES_DIR") {
            roots.push(PathBuf::from(sdkman).join("java"));
        }
        roots.push(PathBuf::from("/usr/lib/jvm")); // Debian/Ubuntu/Fedora
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines")); // macOS
        roots
    }
}

impl Compiler for SubprocessToolchain {
    fn compile<'a>(&'a self, req: &'a CompileRequest<'_>) -> ToolchainFuture<'a> {
        Box::pin(async move { Self::spawn(self.plan_compile(req).await).await })
    }

    fn describe_compile(&self, req: &CompileRequest<'_>) -> String {
        Invocation::build(req, self.path_sep)
            .with_program(self.resolve_program_blocking(Tool::Javac, req.project_root))
            .display_command()
    }
}

impl Runtime for SubprocessToolchain {
    fn run<'a>(&'a self, req: &'a RunRequest<'_>) -> ToolchainFuture<'a> {
        Box::pin(async move { Self::spawn(self.plan_run(req).await).await })
    }

    fn describe_run(&self, req: &RunRequest<'_>) -> String {
        Invocation::run(req, self.path_sep)
            .with_program(self.resolve_program_blocking(Tool::Java, req.project_root))
            .display_command()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jals_exec::block_on_inline;

    use super::*;

    #[test]
    fn select_routes_each_tool_to_its_backend() {
        let exec = Exec::inline();
        let root = Path::new("/proj");
        let sources = vec![PathBuf::from("/proj/src/main/java/A.java")];

        // compiler = "builtin", runtime unset: the dummy compile plan next to a real `java`
        // command — each factory matches its own enum, so the two selections are independent.
        let manifest: Manifest = "[toolchain]\ncompiler = \"builtin\"\n".parse().unwrap();
        let compile_req = CompileRequest {
            manifest: &manifest,
            project_root: root,
            sources: &sources,
            extra_sources: &[],
            extra_classpath: &[],
            extra_javac_args: &[],
            compile_env: &BTreeMap::new(),
        };
        let run_req = RunRequest {
            manifest: &manifest,
            project_root: root,
            jvm_args: &[],
            main_class: "Main",
            program_args: &[],
            extra_classpath: &[],
            run_env: &BTreeMap::new(),
        };
        let compiler = block_on_inline(<dyn Compiler>::select(&manifest, &exec));
        assert!(
            compiler
                .describe_compile(&compile_req)
                .starts_with("builtin:")
        );
        let run_description =
            block_on_inline(<dyn Runtime>::select(&manifest, &exec)).describe_run(&run_req);
        assert!(run_description.contains("java"));
        assert!(!run_description.starts_with("builtin:"));

        // No [toolchain]: the subprocess backend for both steps.
        let manifest = Manifest::default();
        let compile_req = CompileRequest {
            manifest: &manifest,
            project_root: root,
            sources: &sources,
            extra_sources: &[],
            extra_classpath: &[],
            extra_javac_args: &[],
            compile_env: &BTreeMap::new(),
        };
        let compiler = block_on_inline(<dyn Compiler>::select(&manifest, &exec));
        assert!(compiler.describe_compile(&compile_req).contains("javac"));

        // runtime = "builtin" alone: the dummy run next to a real `javac` compile.
        let manifest: Manifest = "[toolchain]\nruntime = \"builtin\"\n".parse().unwrap();
        let run_req = RunRequest {
            manifest: &manifest,
            project_root: root,
            jvm_args: &[],
            main_class: "Main",
            program_args: &[],
            extra_classpath: &[],
            run_env: &BTreeMap::new(),
        };
        let runtime = block_on_inline(<dyn Runtime>::select(&manifest, &exec));
        assert!(runtime.describe_run(&run_req).starts_with("builtin:"));
        let compile_req = CompileRequest {
            manifest: &manifest,
            project_root: root,
            sources: &sources,
            extra_sources: &[],
            extra_classpath: &[],
            extra_javac_args: &[],
            compile_env: &BTreeMap::new(),
        };
        let compiler = block_on_inline(<dyn Compiler>::select(&manifest, &exec));
        assert!(compiler.describe_compile(&compile_req).contains("javac"));
    }

    #[test]
    fn default_manifest_plans_bare_tools() {
        // A manifest with no `[toolchain]` and no env override resolves to the system tools; whatever
        // path is chosen, it ends in the tool's binary name.
        let manifest = Manifest::default();
        let toolchain = block_on_inline(SubprocessToolchain::from_manifest(&manifest));
        let root = Path::new("/proj");

        let compile_req = CompileRequest {
            manifest: &manifest,
            project_root: root,
            sources: &[],
            extra_sources: &[],
            extra_classpath: &[],
            extra_javac_args: &[],
            compile_env: &BTreeMap::new(),
        };
        let program = block_on_inline(toolchain.plan_compile(&compile_req)).program;
        assert!(
            program.ends_with("javac"),
            "compiler program should end in `javac`, got {program}"
        );

        let run_req = RunRequest {
            manifest: &manifest,
            project_root: root,
            jvm_args: &[],
            main_class: "Main",
            program_args: &[],
            extra_classpath: &[],
            run_env: &BTreeMap::new(),
        };
        let program = block_on_inline(toolchain.plan_run(&run_req)).program;
        assert!(
            program.ends_with("java"),
            "runtime program should end in `java`, got {program}"
        );
    }

    #[test]
    fn spawn_applies_working_directory_and_environment_without_clearing_inherited_variables() {
        let temp_dir = tempfile::tempdir().unwrap();
        let working_dir = std::fs::canonicalize(temp_dir.path()).unwrap();
        let (inherited_name, inherited_value) = std::env::vars()
            .find(|(name, _)| !name.starts_with("JALS_BUILD_SUBPROCESS_TEST_"))
            .expect("the test process should have at least one inherited environment variable");
        let environment = BTreeMap::from([
            (
                "JALS_BUILD_SUBPROCESS_TEST_CWD".to_owned(),
                working_dir.to_string_lossy().into_owned(),
            ),
            (
                "JALS_BUILD_SUBPROCESS_TEST_EXPLICIT".to_owned(),
                "accepted".to_owned(),
            ),
            (
                "JALS_BUILD_SUBPROCESS_TEST_INHERITED_NAME".to_owned(),
                inherited_name,
            ),
            (
                "JALS_BUILD_SUBPROCESS_TEST_INHERITED_VALUE".to_owned(),
                inherited_value,
            ),
        ]);
        let invocation = Invocation {
            program: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec![
                "--exact".to_owned(),
                "native::tests::subprocess_observes_invocation_context".to_owned(),
                "--quiet".to_owned(),
            ],
            working_dir,
            environment,
        };

        assert!(
            block_on_inline(SubprocessToolchain::spawn(invocation))
                .unwrap()
                .success()
        );
    }

    #[test]
    fn subprocess_observes_invocation_context() {
        let Ok(expected_cwd) = std::env::var("JALS_BUILD_SUBPROCESS_TEST_CWD") else {
            return;
        };

        assert_eq!(
            std::env::current_dir().unwrap(),
            PathBuf::from(expected_cwd)
        );
        assert_eq!(
            std::env::var("JALS_BUILD_SUBPROCESS_TEST_EXPLICIT").unwrap(),
            "accepted"
        );
        let inherited_name = std::env::var("JALS_BUILD_SUBPROCESS_TEST_INHERITED_NAME").unwrap();
        let inherited_value = std::env::var("JALS_BUILD_SUBPROCESS_TEST_INHERITED_VALUE").unwrap();
        assert_eq!(std::env::var(inherited_name).unwrap(), inherited_value);
    }
}
