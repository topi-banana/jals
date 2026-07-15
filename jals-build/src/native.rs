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
use jals_fs::OsFileTree;

use crate::builtin::BuiltinToolchain;
use crate::invocation::Invocation;
use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::{
    BuildOutcome, Compiler, JdkInstall, Runtime, Tool, ToolResolver, ToolchainError,
};

impl dyn Compiler {
    /// Select the backend the manifest's `[toolchain] compiler` names, as one boxed [`Compiler`]:
    /// matching the [`jals_config::Compiler`] variant routes `"builtin"` to the in-process
    /// [`BuiltinToolchain`] (over the host filesystem) and every `javac` selector to the host
    /// [`SubprocessToolchain`] — so a host drives one `&dyn Compiler`, whatever the manifest
    /// selects.
    pub fn select(manifest: &Manifest) -> Box<dyn Compiler> {
        match &manifest.toolchain.compiler {
            CompilerSpec::Builtin => Box::new(BuiltinToolchain::host()),
            CompilerSpec::System | CompilerSpec::Path(_) | CompilerSpec::Distribution(_) => {
                Box::new(SubprocessToolchain::from_manifest(manifest))
            }
        }
    }
}

impl dyn Runtime {
    /// Select the backend the manifest's `[toolchain] runtime` names, as one boxed [`Runtime`] —
    /// the run-step mirror of `<dyn Compiler>::select`, matching [`jals_config::Runtime`]. The two
    /// selections are independent, so a builtin compile can pair with a real `java` run (and vice
    /// versa) with no routing composite in between.
    pub fn select(manifest: &Manifest) -> Box<dyn Runtime> {
        match &manifest.toolchain.runtime {
            RuntimeSpec::Builtin => Box::new(BuiltinToolchain::host()),
            RuntimeSpec::System | RuntimeSpec::Path(_) | RuntimeSpec::Distribution(_) => {
                Box::new(SubprocessToolchain::from_manifest(manifest))
            }
        }
    }
}

impl BuiltinToolchain {
    /// The builtin backend over the host filesystem.
    fn host() -> Self {
        Self::new(Box::new(OsFileTree::new()))
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
    pub fn from_manifest(manifest: &Manifest) -> Self {
        let tc = &manifest.toolchain;
        Self {
            compiler: tc.compiler.clone(),
            runtime: tc.runtime.clone(),
            installs: if Self::needs_discovery(manifest) {
                Self::discover_installs_in(Self::install_roots())
            } else {
                Vec::new()
            },
            path_sep: if cfg!(windows) { ';' } else { ':' },
        }
    }

    /// Whether either half of the manifest needs installed-JDK discovery.
    fn needs_discovery(manifest: &Manifest) -> bool {
        let tc = &manifest.toolchain;
        matches!(tc.compiler.spec(), Some(ToolSpec::Distribution { .. }))
            || matches!(tc.runtime.spec(), Some(ToolSpec::Distribution { .. }))
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

    /// Resolve `tool` to a concrete program path: read the environment (`$JAVAC`/`$JAVA`,
    /// `$JAVA_HOME`, `$HOME`) into the pure [`ToolResolver`] policy and pick the first candidate
    /// that exists on disk (else the policy's own fallback).
    fn resolve_program(&self, tool: Tool, project_root: &Path) -> PathBuf {
        let env_override = std::env::var_os(tool.env_var()).map(PathBuf::from);
        let java_home = std::env::var_os("JAVA_HOME").map(PathBuf::from);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let resolver = ToolResolver {
            installs: &self.installs,
            java_home: java_home.as_deref(),
            home: home.as_deref(),
            project_root,
        };
        resolver
            .resolve(tool, self.spec(tool), env_override)
            .pick(Path::is_file)
    }

    /// The `javac` [`Invocation`] with its program resolved, ready to spawn or display.
    fn plan_compile(&self, req: &CompileRequest<'_>) -> Invocation {
        Invocation::build(req, self.path_sep)
            .with_program(self.resolve_program(Tool::Javac, req.project_root))
    }

    /// The `java` [`Invocation`] with its program resolved, ready to spawn or display.
    fn plan_run(&self, req: &RunRequest<'_>) -> Invocation {
        Invocation::run(req, self.path_sep)
            .with_program(self.resolve_program(Tool::Java, req.project_root))
    }

    /// Spawn an invocation, inheriting stdio, and map the exit status to a [`BuildOutcome`].
    fn spawn(invocation: &Invocation) -> Result<BuildOutcome, ToolchainError> {
        let status = Command::new(&invocation.program)
            .args(&invocation.args)
            .status()
            .map_err(|source| ToolchainError::Spawn {
                program: invocation.program.clone(),
                source,
            })?;
        Ok(BuildOutcome {
            code: status.code(),
        })
    }

    /// Scan the common JDK install locations and describe each install for [`ToolResolver`].
    fn discover_installs_in(roots: impl IntoIterator<Item = PathBuf>) -> Vec<JdkInstall> {
        let mut installs = Vec::new();
        for root in roots {
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
    fn compile(&self, req: &CompileRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        Self::spawn(&self.plan_compile(req))
    }

    fn describe_compile(&self, req: &CompileRequest<'_>) -> String {
        self.plan_compile(req).display_command()
    }
}

impl Runtime for SubprocessToolchain {
    fn run(&self, req: &RunRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        Self::spawn(&self.plan_run(req))
    }

    fn describe_run(&self, req: &RunRequest<'_>) -> String {
        self.plan_run(req).display_command()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(test_name: &str) -> Self {
            let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "jals-build-native-{test_name}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovers_when_either_tool_uses_a_distribution() {
        let compiler_only: Manifest =
            "[toolchain]\ncompiler = { distribution = { version = 21 } }\n"
                .parse()
                .unwrap();
        let runtime_only: Manifest =
            "[toolchain]\nruntime = { distribution = { name = \"temurin\" } }\n"
                .parse()
                .unwrap();

        assert!(SubprocessToolchain::needs_discovery(&compiler_only));
        assert!(SubprocessToolchain::needs_discovery(&runtime_only));
        assert!(!SubprocessToolchain::needs_discovery(&Manifest::default()));
    }

    #[test]
    fn spec_uses_the_selector_for_the_requested_tool() {
        let toolchain = SubprocessToolchain {
            compiler: CompilerSpec::Path("/opt/compiler".to_owned()),
            runtime: RuntimeSpec::Builtin,
            installs: Vec::new(),
            path_sep: ':',
        };

        assert_eq!(
            toolchain.spec(Tool::Javac),
            Some(ToolSpec::Path("/opt/compiler"))
        );
        assert_eq!(toolchain.spec(Tool::Java), None);
    }

    #[test]
    fn discovers_direct_and_macos_bundle_jdk_layouts() {
        let root = TempDir::new("discover-installs");
        let direct = root.path().join("temurin-21");
        std::fs::create_dir_all(direct.join("bin")).unwrap();
        let bundle = root.path().join("zulu-17.jdk");
        std::fs::create_dir_all(bundle.join("Contents/Home/bin")).unwrap();
        std::fs::create_dir_all(root.path().join("not-a-jdk")).unwrap();
        std::fs::write(root.path().join("README"), "not a directory").unwrap();

        let mut installs = SubprocessToolchain::discover_installs_in(vec![
            root.path().to_path_buf(),
            root.path().join("missing"),
        ]);
        installs.sort_by(|left, right| left.home.cmp(&right.home));

        let mut expected = vec![
            JdkInstall::from_install_name(direct, "temurin-21"),
            JdkInstall::from_install_name(bundle.join("Contents/Home"), "zulu-17.jdk"),
        ];
        expected.sort_by(|left, right| left.home.cmp(&right.home));
        assert_eq!(installs, expected);
    }

    #[test]
    fn install_roots_include_configured_and_platform_locations() {
        let mut expected = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            expected.push(home.join(".sdkman/candidates/java"));
            expected.push(home.join(".jdks"));
            expected.push(home.join(".jdk"));
        }
        if let Some(sdkman) = std::env::var_os("SDKMAN_CANDIDATES_DIR") {
            expected.push(PathBuf::from(sdkman).join("java"));
        }
        expected.push(PathBuf::from("/usr/lib/jvm"));
        expected.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));

        assert_eq!(SubprocessToolchain::install_roots(), expected);
    }

    #[test]
    fn select_routes_each_tool_to_its_backend() {
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
        };
        let run_req = RunRequest {
            manifest: &manifest,
            project_root: root,
            main_class: "Main",
            program_args: &[],
            extra_classpath: &[],
        };
        let compiler = <dyn Compiler>::select(&manifest);
        assert!(
            compiler
                .describe_compile(&compile_req)
                .starts_with("builtin:")
        );
        let run_description = <dyn Runtime>::select(&manifest).describe_run(&run_req);
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
        };
        let compiler = <dyn Compiler>::select(&manifest);
        assert!(compiler.describe_compile(&compile_req).contains("javac"));

        // runtime = "builtin" alone: the dummy run next to a real `javac` compile.
        let manifest: Manifest = "[toolchain]\nruntime = \"builtin\"\n".parse().unwrap();
        let run_req = RunRequest {
            manifest: &manifest,
            project_root: root,
            main_class: "Main",
            program_args: &[],
            extra_classpath: &[],
        };
        let runtime = <dyn Runtime>::select(&manifest);
        assert!(runtime.describe_run(&run_req).starts_with("builtin:"));
        let compile_req = CompileRequest {
            manifest: &manifest,
            project_root: root,
            sources: &sources,
            extra_sources: &[],
            extra_classpath: &[],
        };
        let compiler = <dyn Compiler>::select(&manifest);
        assert!(compiler.describe_compile(&compile_req).contains("javac"));
    }

    #[test]
    fn default_manifest_plans_bare_tools() {
        // A manifest with no `[toolchain]` and no env override resolves to the system tools; whatever
        // path is chosen, it ends in the tool's binary name.
        let manifest = Manifest::default();
        let toolchain = SubprocessToolchain::from_manifest(&manifest);
        let root = Path::new("/proj");

        let compile_req = CompileRequest {
            manifest: &manifest,
            project_root: root,
            sources: &[],
            extra_sources: &[],
            extra_classpath: &[],
        };
        let program = toolchain.plan_compile(&compile_req).program;
        assert!(
            program.ends_with("javac"),
            "compiler program should end in `javac`, got {program}"
        );

        let run_req = RunRequest {
            manifest: &manifest,
            project_root: root,
            main_class: "Main",
            program_args: &[],
            extra_classpath: &[],
        };
        let program = toolchain.plan_run(&run_req).program;
        assert!(
            program.ends_with("java"),
            "runtime program should end in `java`, got {program}"
        );
    }
}
