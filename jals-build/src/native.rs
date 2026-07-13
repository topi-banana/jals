//! The host toolchain: discover installed JDKs and spawn `javac`/`java`.
//!
//! [`SubprocessToolchain`] is the default (`native` feature) implementation of the pure
//! [`Toolchain`] trait. It is the one place in `jals-build` that touches the filesystem or spawns a
//! process, so the rest of the crate stays pure/`wasm32`-buildable. Selection is entirely the pure
//! [`ToolResolver`] policy — an `$JAVAC`/`$JAVA` env override wins, then the manifest's
//! `[toolchain]` [`ToolSpec`](jals_config::ToolSpec), then `$JAVA_HOME`, then the bare name on
//! `PATH` — this module only reads the env vars, scans the common install locations (SDKMAN,
//! `~/.jdks`, `~/.jdk`, `/usr/lib/jvm`, the macOS JVM bundle directory) when a distribution
//! selector needs them, probes which candidate exists, and spawns. Downloading a missing JDK is
//! future work.

use std::path::{Path, PathBuf};
use std::process::Command;

use jals_config::{Manifest, ToolSpec};

use crate::invocation::Invocation;
use crate::request::{CompileRequest, RunRequest};
use crate::toolchain::{BuildOutcome, JdkInstall, Tool, ToolResolver, Toolchain, ToolchainError};

/// A [`Toolchain`] that spawns the host's `javac`/`java`, selected per the manifest's `[toolchain]`.
///
/// Built with [`from_manifest`](SubprocessToolchain::from_manifest); the discovered JDK installs are
/// cached (and skipped entirely unless a distribution selector needs them).
pub struct SubprocessToolchain {
    /// The `javac` selection (`[toolchain] compiler`), or `None` for the system compiler.
    compiler: Option<ToolSpec>,
    /// The `java` selection (`[toolchain] runtime`), or `None` for the system runtime.
    runtime: Option<ToolSpec>,
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
        let needs_discovery = matches!(tc.compiler, Some(ToolSpec::Distribution { .. }))
            || matches!(tc.runtime, Some(ToolSpec::Distribution { .. }));
        Self {
            compiler: tc.compiler.clone(),
            runtime: tc.runtime.clone(),
            installs: if needs_discovery {
                Self::discover_installs()
            } else {
                Vec::new()
            },
            path_sep: if cfg!(windows) { ';' } else { ':' },
        }
    }

    /// The [`ToolSpec`] governing `tool`.
    const fn spec(&self, tool: Tool) -> Option<&ToolSpec> {
        match tool {
            Tool::Javac => self.compiler.as_ref(),
            Tool::Java => self.runtime.as_ref(),
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

impl Toolchain for SubprocessToolchain {
    fn compile(&self, req: &CompileRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        Self::spawn(&self.plan_compile(req))
    }

    fn run(&self, req: &RunRequest<'_>) -> Result<BuildOutcome, ToolchainError> {
        Self::spawn(&self.plan_run(req))
    }

    fn describe_compile(&self, req: &CompileRequest<'_>) -> String {
        self.plan_compile(req).display_command()
    }

    fn describe_run(&self, req: &RunRequest<'_>) -> String {
        self.plan_run(req).display_command()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
