//! Pure resolution of which entry point (`main-class`) `jals run` should execute.
//!
//! [`RunTarget::resolve`] maps a [`Manifest`] plus an optional `--bin <name>` selector to the
//! fully-qualified main class to hand to `java`. Like the rest of the crate it touches neither the
//! filesystem nor a process, so it stays deterministic, unit-testable, and `wasm32`-compatible;
//! `jals-cli` calls it and feeds the result into [`crate::run_invocation`].
//!
//! Because `javac` compiles every source together, `[[bin]]` is purely a *run-time* selector — it
//! never changes what is compiled. The `--main-class <FQCN>` flag is handled by `jals-cli` *before*
//! this function (an explicit class bypasses all manifest-based selection), so it is not an input
//! here.

use std::error::Error;
use std::fmt;

use jals_config::Manifest;

/// Namespace for resolving which entry point (`main-class`) `jals run` should execute.
pub struct RunTarget;

impl RunTarget {
    /// Resolve the fully-qualified main class `jals run` should execute, given an optional `--bin
    /// <name>` selector.
    ///
    /// Precedence (highest first):
    /// 1. `bin = Some(name)` — the `[[bin]]` with that name, or [`ResolveTargetError::UnknownBin`].
    /// 2. exactly one `[[bin]]` — that bin.
    /// 3. several `[[bin]]` with `[package] default-run` set — the named default (an unknown name is
    ///    [`ResolveTargetError::UnknownBin`], though [`Manifest::validate`] normally rejects it first).
    /// 4. several `[[bin]]` without `default-run` — [`ResolveTargetError::Ambiguous`].
    /// 5. no `[[bin]]` — `[run] main-class` if set, else [`ResolveTargetError::NoTarget`].
    ///
    /// The returned `&str` borrows from `manifest`, matching [`Invocation::run`](crate::Invocation::run)'s
    /// `main_class` parameter so the caller can pass it straight through.
    ///
    /// # Errors
    /// Returns [`ResolveTargetError`] when no single target can be chosen.
    pub fn resolve<'m>(
        manifest: &'m Manifest,
        bin: Option<&str>,
    ) -> Result<&'m str, ResolveTargetError> {
        /// The declared `[[bin]]` names, for actionable error messages.
        fn bin_names(manifest: &Manifest) -> Vec<String> {
            manifest.bin.iter().map(|b| b.name.clone()).collect()
        }

        if let Some(name) = bin {
            return manifest
                .bin
                .iter()
                .find(|b| b.name == name)
                .map(|b| b.main_class.as_str())
                .ok_or_else(|| ResolveTargetError::UnknownBin {
                    name: name.to_owned(),
                    available: bin_names(manifest),
                });
        }

        match manifest.bin.as_slice() {
            [] => manifest
                .run
                .main_class
                .as_deref()
                .ok_or(ResolveTargetError::NoTarget),
            [only] => Ok(only.main_class.as_str()),
            many => manifest.package.default_run.as_deref().map_or_else(
                || {
                    Err(ResolveTargetError::Ambiguous {
                        available: bin_names(manifest),
                    })
                },
                |name| {
                    many.iter().find(|b| b.name == name).map_or_else(
                        || {
                            Err(ResolveTargetError::UnknownBin {
                                name: name.into(),
                                available: bin_names(manifest),
                            })
                        },
                        |b| Ok(b.main_class.as_str()),
                    )
                },
            ),
        }
    }
}

/// Why [`RunTarget::resolve`] could not choose a single run target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveTargetError {
    /// `--bin <name>` (or `default-run`) named a bin that does not exist.
    UnknownBin {
        /// The requested bin name.
        name: String,
        /// The declared bin names.
        available: Vec<String>,
    },
    /// Several `[[bin]]` exist and neither `--bin` nor `[package] default-run` chose one.
    Ambiguous {
        /// The declared bin names.
        available: Vec<String>,
    },
    /// There is no `[[bin]]` and no `[run] main-class`.
    NoTarget,
}

impl fmt::Display for ResolveTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBin { name, available } => write!(
                f,
                "no bin named `{name}` (available: {})",
                available.join(", ")
            ),
            Self::Ambiguous { available } => write!(
                f,
                "multiple bins ({}); pass --bin <name> or set `[package] default-run`",
                available.join(", ")
            ),
            Self::NoTarget => write!(
                f,
                "no main class: set `[run] main-class`, add a `[[bin]]`, or pass --main-class"
            ),
        }
    }
}

impl Error for ResolveTargetError {}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_config::{Bin, Manifest};

    fn bin(name: &str, main_class: &str) -> Bin {
        Bin {
            name: name.to_owned(),
            main_class: main_class.to_owned(),
        }
    }

    /// A manifest carrying just the given `[[bin]]` entries (avoids `field_reassign_with_default`).
    fn manifest_with_bins(bin: Vec<Bin>) -> Manifest {
        Manifest {
            bin,
            ..Default::default()
        }
    }

    fn two_bins() -> Manifest {
        manifest_with_bins(vec![
            bin("one", "com.example.One"),
            bin("two", "com.example.Two"),
        ])
    }

    #[test]
    fn no_bins_uses_run_main_class() {
        let mut m = Manifest::default();
        m.run.main_class = Some("com.example.Main".to_owned());
        assert_eq!(RunTarget::resolve(&m, None), Ok("com.example.Main"));
    }

    #[test]
    fn no_bins_no_main_class_is_no_target() {
        let m = Manifest::default();
        assert_eq!(
            RunTarget::resolve(&m, None),
            Err(ResolveTargetError::NoTarget)
        );
    }

    #[test]
    fn single_bin_is_unambiguous() {
        let m = manifest_with_bins(vec![bin("only", "com.example.Only")]);
        assert_eq!(RunTarget::resolve(&m, None), Ok("com.example.Only"));
    }

    #[test]
    fn single_bin_wins_over_run_main_class() {
        // Option A: once any `[[bin]]` exists, `[run] main-class` is ignored for selection.
        let mut m = manifest_with_bins(vec![bin("only", "com.example.Only")]);
        m.run.main_class = Some("com.example.Legacy".to_owned());
        assert_eq!(RunTarget::resolve(&m, None), Ok("com.example.Only"));
    }

    #[test]
    fn explicit_bin_flag_selects() {
        let m = two_bins();
        assert_eq!(RunTarget::resolve(&m, Some("two")), Ok("com.example.Two"));
    }

    #[test]
    fn unknown_bin_flag_errors() {
        let m = two_bins();
        assert_eq!(
            RunTarget::resolve(&m, Some("nope")),
            Err(ResolveTargetError::UnknownBin {
                name: "nope".to_owned(),
                available: vec!["one".to_owned(), "two".to_owned()],
            })
        );
    }

    #[test]
    fn multiple_bins_without_default_is_ambiguous() {
        let m = two_bins();
        assert_eq!(
            RunTarget::resolve(&m, None),
            Err(ResolveTargetError::Ambiguous {
                available: vec!["one".to_owned(), "two".to_owned()],
            })
        );
    }

    #[test]
    fn default_run_selects_among_many() {
        let mut m = two_bins();
        m.package.default_run = Some("two".to_owned());
        assert_eq!(RunTarget::resolve(&m, None), Ok("com.example.Two"));
    }

    #[test]
    fn explicit_bin_overrides_default_run() {
        let mut m = two_bins();
        m.package.default_run = Some("one".to_owned());
        assert_eq!(RunTarget::resolve(&m, Some("two")), Ok("com.example.Two"));
    }

    #[test]
    fn display_strings_carry_actionable_text() {
        // The CLI surfaces these messages verbatim, so the key phrases are part of the contract.
        assert!(
            ResolveTargetError::NoTarget
                .to_string()
                .contains("main class")
        );
        assert!(
            ResolveTargetError::Ambiguous {
                available: vec!["a".to_owned(), "b".to_owned()],
            }
            .to_string()
            .contains("multiple bins")
        );
        assert!(
            ResolveTargetError::UnknownBin {
                name: "x".to_owned(),
                available: vec!["a".to_owned()],
            }
            .to_string()
            .contains("no bin named")
        );
    }
}
