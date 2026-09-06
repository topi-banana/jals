//! Toolchain selection, (de)serialized from `[toolchain]` in `jals.toml`.
//!
//! `[toolchain]` chooses *which* `javac` compiles the project and *which* `java` runs it — the two
//! are selected independently ([`Toolchain::compiler`] / [`Toolchain::runtime`]), so a project can
//! compile with one JDK and run on another. It is the rough analogue of `rust-toolchain.toml`'s
//! `[toolchain]` table.
//!
//! Each selection is its own defaulted enum — [`Compiler`] / [`Runtime`] — whose TOML forms are
//! exactly serde's derived (externally tagged) representation, so there is no hand-written
//! (de)serialization or string classification anywhere:
//!
//! ```toml
//! [toolchain]
//! compiler = { distribution = { name = "temurin", version = 21 } }  # discover an installed JDK
//! runtime  = { path = "/opt/jdk-17" }                               # an explicit JDK home or binary
//! # compiler = "system"    # the system tools (the default)
//! # compiler = "builtin"   # the in-process backend
//! ```
//!
//! Matching the enum is how a host picks its backend: [`Builtin`](Compiler::Builtin) is the
//! in-process backend, and every other variant selects a program, exposed to the resolver as the
//! borrowed [`ToolSpec`] view (see [`Compiler::spec`]). This crate only *models* the selection
//! (pure, `no_std`). Turning a [`ToolSpec`] into an actual program path — discovering an installed
//! JDK, honoring `$JAVA_HOME`/`$PATH` — is the host's job and lives in `jals-build`'s `native`
//! feature (`SubprocessToolchain`), which keeps the filesystem and process I/O out of the pure
//! model. Automatic download of a missing JDK is future work; today an unresolvable
//! [`Distribution`] falls back to the system tools.

use alloc::string::String;

use serde::{Deserialize, Serialize};

/// Toolchain selection (`[toolchain]`).
///
/// Both fields default to the system tools — the historic behavior (`$JAVAC`/`$JAVA`, then
/// `$JAVA_HOME/bin`, then `PATH`) — so a manifest without a `[toolchain]` table is unaffected.
/// Each field is its own enum ([`Compiler`] / [`Runtime`]); matching its variant is how a host
/// routes to a backend.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Toolchain {
    /// Which `javac` compiles the project (`compiler = …`). Defaults to the system compiler.
    pub compiler: Compiler,
    /// Which `java` runs the project (`runtime = …`). Defaults to the system runtime.
    pub runtime: Runtime,
}

/// Which `javac` compiles the project (`[toolchain] compiler`).
///
/// The TOML forms are serde's derived representation of the variants: the keywords `"system"` /
/// `"builtin"`, or a tagged table `{ path = "…" }` / `{ distribution = { … } }`. Defaults to the
/// system `javac`. Matching the variant is how a host picks the compile backend (in `jals-build`,
/// a `&dyn Compiler` trait object per variant).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compiler {
    /// The system `javac` (`"system"`): the host's `$JAVAC` → `$JAVA_HOME/bin` → `PATH`
    /// resolution — exactly as with no `[toolchain]` at all (the default).
    #[default]
    System,
    /// The host's in-process backend (`"builtin"`) instead of a spawned `javac`. Today that
    /// backend is a placeholder (the dummy compiler in `jals-build`); the selector is the seam a
    /// real embedded compiler will fill without a manifest change.
    Builtin,
    /// An explicit filesystem location (`{ path = "/opt/jdk-21" }`): either a JDK home directory
    /// (the host appends `bin/javac`) or the binary itself. Carried verbatim; the host resolves it
    /// against the manifest directory.
    Path(String),
    /// A JDK to discover among the installed ones (`{ distribution = { name = "temurin",
    /// version = 21 } }`), see [`Distribution`].
    Distribution(Distribution),
}

impl Compiler {
    /// The `javac` selector as the borrowed [`ToolSpec`] view, or `None` for the in-process
    /// backend (which resolves no program).
    pub fn spec(&self) -> Option<ToolSpec<'_>> {
        match self {
            Self::Builtin => None,
            Self::System => Some(ToolSpec::System),
            Self::Path(path) => Some(ToolSpec::Path(path)),
            Self::Distribution(distribution) => Some(distribution.spec()),
        }
    }
}

/// What runs the project (`[toolchain] runtime`).
///
/// Almost the mirror of [`Compiler`] for the run half, with the same TOML forms: the keywords
/// `"system"` / `"builtin"`, or a tagged table `{ path = "…" }` / `{ distribution = { … } }`.
/// Defaults to the system `java`.
///
/// The one selector with no counterpart on the compile side is [`Wasm`](Self::Wasm), and the
/// asymmetry is the point: every other value here answers *which `java`*, while that one answers
/// *not a `java` at all*. It is therefore the only value constrained by `[build] backend` —
/// [`Manifest::validate`](crate::Manifest::validate) rejects it without the backend that emits a
/// module — because a runtime that runs modules has nothing to run when the compile produced
/// class files.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// The system `java` (`"system"`): the host's `$JAVA` → `$JAVA_HOME/bin` → `PATH` resolution —
    /// exactly as with no `[toolchain]` at all (the default).
    #[default]
    System,
    /// The host's in-process backend (`"builtin"`) instead of a spawned `java` — today a dummy
    /// no-op runtime in `jals-build`, the same seam as [`Compiler::Builtin`].
    Builtin,
    /// An explicit filesystem location (`{ path = "/opt/jdk-21" }`): either a JDK home directory
    /// (the host appends `bin/java`) or the binary itself. Carried verbatim; the host resolves it
    /// against the manifest directory.
    Path(String),
    /// A JDK to discover among the installed ones (`{ distribution = { name = "temurin",
    /// version = 21 } }`), see [`Distribution`].
    Distribution(Distribution),
    /// The WebAssembly engine compiled into the host (`"wasm"`), running the module
    /// `[build] backend = { type = "jals-wasm" }` emitted.
    ///
    /// Not a JDK, and so not a [`ToolSpec`]: there is no program to find, because the engine is
    /// already linked in. That is also why it is the one runtime a manifest cannot pair freely —
    /// it runs a module, and only the `jals-wasm` backend produces one.
    Wasm,
}

impl Runtime {
    /// The `java` selector as the borrowed [`ToolSpec`] view, or `None` for a runtime that
    /// resolves no program at all — the in-process dummy and the wasm engine alike.
    pub fn spec(&self) -> Option<ToolSpec<'_>> {
        match self {
            Self::Builtin | Self::Wasm => None,
            Self::System => Some(ToolSpec::System),
            Self::Path(path) => Some(ToolSpec::Path(path)),
            Self::Distribution(distribution) => Some(distribution.spec()),
        }
    }

    /// Whether this selector runs a WebAssembly module rather than a `java` process.
    ///
    /// A predicate rather than a `matches!` at each site, because three crates ask it and the
    /// question — "is this the module runtime" — is the manifest's to answer, not each caller's
    /// to re-spell.
    #[must_use]
    pub const fn is_wasm(&self) -> bool {
        matches!(self, Self::Wasm)
    }
}

/// A JDK to discover by distribution and/or version
/// (`{ distribution = { name = "temurin", version = 21 } }`).
///
/// Both keys are optional: a bare version matches any distribution, a bare name any version, and
/// an empty table any installed JDK. The host matches the selector against the installed JDKs it
/// can find; today an unresolved selector falls back to the system tools (auto-download is future
/// work).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Distribution {
    /// The JDK distribution / vendor (`temurin`, `openjdk`, `graalvm`, …), if named.
    pub(crate) name: Option<String>,
    /// The major Java version (`21`), if named.
    pub(crate) version: Option<u32>,
}

impl Distribution {
    /// This selector as the borrowed [`ToolSpec`] view.
    fn spec(&self) -> ToolSpec<'_> {
        ToolSpec::Distribution {
            name: self.name.as_deref(),
            version: self.version,
        }
    }
}

/// A borrowed view of the program-selecting half of a [`Compiler`] / [`Runtime`] — every variant
/// except the in-process `builtin` backend, which selects no program.
///
/// This is the one vocabulary `jals-build`'s resolver consumes for both tools (obtained via
/// [`Compiler::spec`] / [`Runtime::spec`]); it is not a serde type and never appears in a
/// manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSpec<'a> {
    /// Use the system tools: the host's `$JAVAC`/`$JAVA` → `$JAVA_HOME/bin` → `PATH` resolution.
    System,
    /// An explicit JDK home directory or tool binary path.
    Path(&'a str),
    /// A JDK to discover by distribution and/or version (see [`Distribution`]).
    Distribution {
        /// The JDK distribution / vendor to match, if named.
        name: Option<&'a str>,
        /// The major Java version to match, if named.
        version: Option<u32>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_system_tools() {
        let tc = Toolchain::default();
        assert_eq!(tc.compiler, Compiler::System);
        assert_eq!(tc.runtime, Runtime::System);
        // An empty table deserializes to the same defaults.
        assert_eq!(toml::from_str::<Toolchain>("").unwrap(), tc);
    }

    #[test]
    fn deserializes_the_keyword_forms() {
        let tc: Toolchain =
            toml::from_str("compiler = \"builtin\"\nruntime = \"system\"\n").unwrap();
        assert_eq!(tc.compiler, Compiler::Builtin);
        assert_eq!(tc.runtime, Runtime::System);
    }

    #[test]
    fn deserializes_the_table_forms() {
        let tc: Toolchain = toml::from_str(
            "compiler = { distribution = { name = \"temurin\", version = 21 } }\n\
             runtime = { path = \"/opt/jdk-17/bin/java\" }\n",
        )
        .unwrap();
        assert_eq!(
            tc.compiler,
            Compiler::Distribution(Distribution {
                name: Some("temurin".into()),
                version: Some(21),
            })
        );
        assert_eq!(tc.runtime, Runtime::Path("/opt/jdk-17/bin/java".into()));
    }

    #[test]
    fn distribution_keys_are_optional() {
        let tc: Toolchain =
            toml::from_str("compiler = { distribution = { version = 21 } }\n").unwrap();
        assert_eq!(
            tc.compiler,
            Compiler::Distribution(Distribution {
                name: None,
                version: Some(21),
            })
        );
        let tc: Toolchain =
            toml::from_str("compiler = { distribution = { name = \"temurin\" } }\n").unwrap();
        assert_eq!(
            tc.compiler,
            Compiler::Distribution(Distribution {
                name: Some("temurin".into()),
                version: None,
            })
        );
    }

    #[test]
    fn spec_lowers_every_program_selecting_variant() {
        assert_eq!(Compiler::System.spec(), Some(ToolSpec::System));
        assert_eq!(Compiler::Builtin.spec(), None);
        assert_eq!(
            Compiler::Path("/opt/jdk-21".into()).spec(),
            Some(ToolSpec::Path("/opt/jdk-21"))
        );
        assert_eq!(
            Runtime::Distribution(Distribution {
                name: Some("temurin".into()),
                version: Some(21),
            })
            .spec(),
            Some(ToolSpec::Distribution {
                name: Some("temurin"),
                version: Some(21),
            })
        );
        assert_eq!(Runtime::Builtin.spec(), None);
        // The wasm engine is linked in, so there is no program to resolve — the same answer the
        // in-process dummy gives, for a different reason.
        assert_eq!(Runtime::Wasm.spec(), None);
    }

    /// `"wasm"` is a keyword like `"system"` and `"builtin"`, and it exists on the run half only:
    /// there is no wasm *compiler* selector, because what emits a module is `[build] backend`.
    #[test]
    fn the_wasm_runtime_is_a_keyword_on_the_run_half_alone() {
        let tc: Toolchain = toml::from_str("runtime = \"wasm\"\n").unwrap();
        assert_eq!(tc.runtime, Runtime::Wasm);
        assert!(tc.runtime.is_wasm());
        assert!(!Runtime::System.is_wasm());

        assert!(toml::from_str::<Toolchain>("compiler = \"wasm\"\n").is_err());
    }

    #[test]
    fn rejects_a_free_form_selector_string() {
        // The historic classified-string forms are not part of the derived schema: anything but
        // the two keywords must use its tagged table form.
        assert!(toml::from_str::<Toolchain>("compiler = \"temurin-21\"\n").is_err());
        assert!(toml::from_str::<Toolchain>("compiler = \"/opt/jdk-21\"\n").is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(toml::from_str::<Toolchain>("linker = \"ld\"\n").is_err());
        // `deny_unknown_fields` reaches inside the distribution table, so a typo errors instead
        // of silently matching any JDK.
        assert!(
            toml::from_str::<Toolchain>("compiler = { distribution = { nam = \"temurin\" } }\n")
                .is_err()
        );
    }
}
