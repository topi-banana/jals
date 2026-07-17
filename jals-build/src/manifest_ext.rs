//! Host-only, `std::path`-based resolution over a [`Manifest`].
//!
//! The pure serde model and validation of a `jals.toml` live in [`jals_config`]; that crate is
//! `no_std` and cannot build a [`PathBuf`] or read a file. This module adds the host half — the
//! [`ManifestExt`] extension trait (`from_file` / `discover_path` / `source_roots`) — on top of
//! `jals_config::Manifest`. Everything here is pure apart from `from_file`/`discover_path`
//! (`std::fs` reads); `jals-cli`/`jals-lsp` consume it. Dependency classification is portable and
//! lives in `jals-classpath`'s input plan.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use jals_config::{Manifest, ManifestParseError, ValidationError};

/// An error loading, parsing, or validating a manifest file from disk.
///
/// The host-side counterpart of [`ManifestParseError`], adding the `std::io` read failure and
/// re-stamping parse / validation errors with the real [`PathBuf`].
#[derive(Debug)]
pub enum ManifestError {
    /// The file could not be read.
    Io {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The file contained invalid TOML.
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The underlying parse error.
        source: toml::de::Error,
    },
    /// The file parsed but is structurally invalid (see [`ValidationError`]).
    Invalid {
        /// The path that failed validation.
        path: PathBuf,
        /// The validation failure.
        source: ValidationError,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "failed to read manifest {}: {source}", path.display())
            }
            Self::Parse { path, source } => {
                write!(f, "failed to parse manifest {}: {source}", path.display())
            }
            Self::Invalid { path, source } => {
                write!(f, "invalid manifest {}: {source}", path.display())
            }
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Invalid { source, .. } => Some(source),
        }
    }
}

/// The host-side, `std::path`-based resolution over a [`Manifest`].
///
/// This is the counterpart of the pure model in [`jals_config`]. Brought into scope alongside
/// `jals_config::Manifest`, its methods are callable with the historic `manifest.method(dir)` /
/// `Manifest::from_file(path)` syntax.
pub trait ManifestExt {
    /// Load, parse, and validate a specific `jals.toml` file. Delegates parse+validation to
    /// `jals_config`'s [`FromStr`](core::str::FromStr), re-stamping errors with the real path.
    ///
    /// # Errors
    /// Returns [`ManifestError`] when the file cannot be read, contains invalid TOML, or fails
    /// validation (e.g. duplicate `[[bin]]` names).
    fn from_file(path: &Path) -> Result<Manifest, ManifestError>;

    /// Search upward from `start_dir` for a `jals.toml`, returning its path.
    ///
    /// The project root is the returned path's parent directory; all manifest paths are resolved
    /// relative to it. Returns `None` when no manifest is found in `start_dir` or any ancestor.
    /// Unlike the formatter/linter configs (where a missing file means "use defaults"), a missing
    /// manifest is left for the caller to treat as an error — there is nothing to build without one.
    fn discover_path(start_dir: &Path) -> Option<PathBuf>;

    /// The absolute `.java` source roots: each `[build] source-dirs` entry resolved against
    /// `manifest_dir` (the manifest's own directory). These feed `javac -sourcepath` and are the
    /// roots scanned for `.java` files.
    fn source_roots(&self, manifest_dir: &Path) -> Vec<PathBuf>;
}

impl ManifestExt for Manifest {
    fn from_file(path: &Path) -> Result<Manifest, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        text.parse::<Self>().map_err(|err| match err {
            ManifestParseError::Parse { source, .. } => ManifestError::Parse {
                path: path.to_path_buf(),
                source,
            },
            ManifestParseError::Invalid { source, .. } => ManifestError::Invalid {
                path: path.to_path_buf(),
                source,
            },
        })
    }

    fn discover_path(start_dir: &Path) -> Option<PathBuf> {
        let mut dir = Some(start_dir);
        while let Some(d) = dir {
            let candidate = d.join("jals.toml");
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = d.parent();
        }
        None
    }

    fn source_roots(&self, manifest_dir: &Path) -> Vec<PathBuf> {
        self.build
            .source_dirs
            .iter()
            .map(|d| manifest_dir.join(d))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_path_returns_none_when_absent() {
        // A path with no `jals.toml` anywhere above it yields None. Use the root, which has none.
        assert_eq!(Manifest::discover_path(Path::new("/")), None);
    }

    #[test]
    fn source_roots_resolve_against_manifest_dir() {
        let m: Manifest = toml::from_str("[build]\nsource-dirs = [\"src\", \"gen\"]\n").unwrap();
        assert_eq!(
            m.source_roots(Path::new("/proj")),
            vec![PathBuf::from("/proj/src"), PathBuf::from("/proj/gen")]
        );
    }
}
