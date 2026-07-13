//! Toolchain selection, deserialized from `[toolchain]` in `jals.toml`.
//!
//! `[toolchain]` chooses *which* `javac` compiles the project and *which* `java` runs it — the two
//! are selected independently ([`Toolchain::compiler`] / [`Toolchain::runtime`]), so a project can
//! compile with one JDK and run on another. It is the rough analogue of `rust-toolchain.toml`'s
//! `[toolchain]` table.
//!
//! Each selection is a [`ToolSpec`], written as a single TOML string (the rust-toolchain feel):
//!
//! ```toml
//! [toolchain]
//! compiler = "temurin-21"   # a distribution + version to discover
//! runtime  = "/opt/jdk-17"  # an explicit JDK home or binary path
//! ```
//!
//! This crate only *models* the selection (pure, `no_std`). Turning a [`ToolSpec`] into an actual
//! program path — discovering an installed JDK, honoring `$JAVA_HOME`/`$PATH` — is the host's job and
//! lives in `jals-build`'s `native` feature (`SubprocessToolchain`), which keeps the filesystem and
//! process I/O out of the pure model. Automatic download of a missing JDK is future work; today an
//! unresolvable [`ToolSpec::Distribution`] falls back to the system tools.

use alloc::borrow::ToOwned;
use alloc::string::String;

use serde::Deserialize;
use serde::de::{self, Deserializer};

/// Toolchain selection (`[toolchain]`).
///
/// Both fields default to `None`, which means "use the system tools" — the historic behavior
/// (`$JAVAC`/`$JAVA`, then `$JAVA_HOME/bin`, then `PATH`), so a manifest without a `[toolchain]`
/// table is unaffected. See [`ToolSpec`] for the per-tool forms.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Toolchain {
    /// Which `javac` compiles the project (`compiler = "…"`). `None` uses the system compiler.
    pub compiler: Option<ToolSpec>,
    /// Which `java` runs the project (`runtime = "…"`). `None` uses the system runtime.
    pub runtime: Option<ToolSpec>,
}

/// How to select one JDK tool (`javac` or `java`), parsed from a single TOML string.
///
/// The string is classified into one of three forms; the classification is unambiguous and total
/// (any non-empty string maps to exactly one form):
///
/// - `"system"` — [`System`](ToolSpec::System): use the system tools, exactly as with no
///   `[toolchain]` at all.
/// - a string containing a path separator (`/` or `\`) or starting with `.`/`~` —
///   [`Path`](ToolSpec::Path): an explicit JDK home directory or the tool binary itself.
/// - anything else — [`Distribution`](ToolSpec::Distribution): a `distribution-version` selector
///   (`"temurin-21"`), a bare version (`"21"`), or a bare distribution (`"temurin"`), to be
///   discovered among the installed JDKs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSpec {
    /// Use the system tools (`"system"`): the host's `$JAVAC`/`$JAVA` → `$JAVA_HOME/bin` → `PATH`
    /// resolution.
    System,
    /// An explicit filesystem location (`"/opt/jdk-21"`, `"./jdk/bin/javac"`): either a JDK home
    /// directory (the host appends `bin/<tool>`) or the tool binary itself. Carried verbatim; the
    /// host resolves it against the manifest directory.
    Path(String),
    /// A JDK to discover by distribution and/or version (`"temurin-21"` / `"21"` / `"temurin"`).
    ///
    /// At least one of the two is `Some` (a bare version, a bare distribution, or both). The host
    /// matches it against the installed JDKs it can find; today an unresolved selector falls back to
    /// the system tools (auto-download is future work).
    Distribution {
        /// The JDK distribution / vendor (`temurin`, `openjdk`, `graalvm`, …), if named.
        distribution: Option<String>,
        /// The major Java version (`21`), if named.
        version: Option<u32>,
    },
}

impl ToolSpec {
    /// Classify a `[toolchain]` selector string into a [`ToolSpec`].
    ///
    /// Total over non-empty input (see the type docs for the rules); returns `None` only for an empty
    /// string, which the [`Deserialize`] impl rejects as an error.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if raw == "system" {
            return Some(Self::System);
        }
        // An explicit path: any separator, or a relative/home-anchored prefix.
        if raw.contains('/') || raw.contains('\\') || raw.starts_with('.') || raw.starts_with('~') {
            return Some(Self::Path(raw.to_owned()));
        }
        // Otherwise a distribution/version selector. Split on the last `-`: if the suffix is a bare
        // integer it is the version, and the (possibly empty) prefix is the distribution.
        if let Some((prefix, suffix)) = raw.rsplit_once('-')
            && let Ok(version) = suffix.parse::<u32>()
        {
            let distribution = (!prefix.is_empty()).then(|| prefix.to_owned());
            return Some(Self::Distribution {
                distribution,
                version: Some(version),
            });
        }
        // A bare integer is a version; anything else is a bare distribution name.
        if let Ok(version) = raw.parse::<u32>() {
            return Some(Self::Distribution {
                distribution: None,
                version: Some(version),
            });
        }
        Some(Self::Distribution {
            distribution: Some(raw.to_owned()),
            version: None,
        })
    }
}

// Deserialized as a string then classified by [`ToolSpec::parse`] (the same bridge `Feature`
// uses), so the TOML forms and the parse rules live in one place.
impl<'de> Deserialize<'de> for ToolSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| de::Error::custom("toolchain selector must not be empty"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_system() {
        assert_eq!(ToolSpec::parse("system"), Some(ToolSpec::System));
    }

    #[test]
    fn parses_paths() {
        assert_eq!(
            ToolSpec::parse("/opt/jdk-21"),
            Some(ToolSpec::Path("/opt/jdk-21".into()))
        );
        assert_eq!(
            ToolSpec::parse("./jdk/bin/javac"),
            Some(ToolSpec::Path("./jdk/bin/javac".into()))
        );
        assert_eq!(
            ToolSpec::parse("~/jdks/21"),
            Some(ToolSpec::Path("~/jdks/21".into()))
        );
        assert_eq!(
            ToolSpec::parse("C:\\jdk"),
            Some(ToolSpec::Path("C:\\jdk".into()))
        );
    }

    #[test]
    fn parses_distribution_and_version() {
        assert_eq!(
            ToolSpec::parse("temurin-21"),
            Some(ToolSpec::Distribution {
                distribution: Some("temurin".into()),
                version: Some(21),
            })
        );
        assert_eq!(
            ToolSpec::parse("21"),
            Some(ToolSpec::Distribution {
                distribution: None,
                version: Some(21),
            })
        );
        assert_eq!(
            ToolSpec::parse("temurin"),
            Some(ToolSpec::Distribution {
                distribution: Some("temurin".into()),
                version: None,
            })
        );
    }

    #[test]
    fn empty_is_rejected() {
        assert_eq!(ToolSpec::parse(""), None);
        assert_eq!(ToolSpec::parse("   "), None);
    }

    #[test]
    fn deserializes_from_toml() {
        let tc: Toolchain =
            toml::from_str("compiler = \"temurin-21\"\nruntime = \"system\"\n").unwrap();
        assert_eq!(
            tc.compiler,
            Some(ToolSpec::Distribution {
                distribution: Some("temurin".into()),
                version: Some(21),
            })
        );
        assert_eq!(tc.runtime, Some(ToolSpec::System));
    }

    #[test]
    fn empty_string_is_a_parse_error() {
        assert!(toml::from_str::<Toolchain>("compiler = \"\"\n").is_err());
    }

    #[test]
    fn rejects_unknown_field() {
        assert!(toml::from_str::<Toolchain>("linker = \"ld\"\n").is_err());
    }
}
