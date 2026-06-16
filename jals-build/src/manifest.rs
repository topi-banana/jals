//! Project manifest, deserialized from `jals.toml`.
//!
//! A `jals.toml` describes a Java project the way `Cargo.toml` describes a Rust crate. Every key is
//! optional; omitted keys fall back to [`Manifest::default`], which encodes the Maven-style
//! `src/main/java` -> `target/classes` layout. Keys are kebab-case and grouped into `[package]`,
//! `[build]`, and `[run]` sections.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A parsed `jals.toml` project manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Manifest {
    /// Project metadata (`[package]`). Informational for now.
    pub package: Package,
    /// Compilation settings (`[build]`).
    pub build: Build,
    /// Run settings (`[run]`).
    pub run: Run,
}

/// Project metadata (`[package]`).
///
/// These fields are informational for now — they are not passed to `javac` — and are reserved for
/// future jar packaging.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Package {
    /// Project name.
    pub name: Option<String>,
    /// Project version.
    pub version: Option<String>,
}

/// Compilation settings (`[build]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Build {
    /// Source roots, relative to the manifest directory. These feed `javac`'s `-sourcepath` and
    /// are the roots scanned for `.java` files. Defaults to `["src/main/java"]`.
    pub source_dirs: Vec<String>,
    /// Output directory for `.class` files (`javac -d`), relative to the manifest directory.
    /// Defaults to `"target/classes"`.
    pub classes_dir: String,
    /// `javac --release N`. When set it determines source level, target level, and bootclasspath
    /// together, and `source`/`target` are ignored.
    pub release: Option<u32>,
    /// `javac --source N`, used only when `release` is unset.
    pub source: Option<u32>,
    /// `javac --target N`, used only when `release` is unset.
    pub target: Option<u32>,
    /// Classpath entries (jars or directories), relative to the manifest directory.
    pub classpath: Vec<String>,
    /// Extra raw flags appended verbatim after the generated `javac` arguments (before the source
    /// files). An escape hatch for anything the manifest does not model yet.
    pub javac_flags: Vec<String>,
}

/// Run settings (`[run]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Run {
    /// Fully-qualified main class used as the entry point for `jals run`.
    pub main_class: Option<String>,
}

impl Default for Build {
    fn default() -> Self {
        Build {
            source_dirs: vec!["src/main/java".to_string()],
            classes_dir: "target/classes".to_string(),
            release: None,
            source: None,
            target: None,
            classpath: Vec::new(),
            javac_flags: Vec::new(),
        }
    }
}

impl Manifest {
    /// Load and parse a specific `jals.toml` file.
    ///
    /// # Errors
    /// Returns [`ManifestError`] when the file cannot be read or contains invalid TOML.
    pub fn from_file(path: &Path) -> Result<Manifest, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ManifestError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Search upward from `start_dir` for a `jals.toml`, returning its path.
    ///
    /// The project root is the returned path's parent directory; all manifest paths are resolved
    /// relative to it. Returns `None` when no manifest is found in `start_dir` or any ancestor.
    /// Unlike the formatter/linter configs (where a missing file means "use defaults"), a missing
    /// manifest is left for the caller to treat as an error — there is nothing to build without one.
    pub fn discover_path(start_dir: &Path) -> Option<PathBuf> {
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
}

/// An error loading or parsing a manifest file.
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
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io { path, source } => {
                write!(f, "failed to read manifest {}: {source}", path.display())
            }
            ManifestError::Parse { path, source } => {
                write!(f, "failed to parse manifest {}: {source}", path.display())
            }
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ManifestError::Io { source, .. } => Some(source),
            ManifestError::Parse { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_maven_layout() {
        let m = Manifest::default();
        assert_eq!(m.build.source_dirs, vec!["src/main/java".to_string()]);
        assert_eq!(m.build.classes_dir, "target/classes");
        assert_eq!(m.build.release, None);
        assert!(m.build.classpath.is_empty());
        assert!(m.build.javac_flags.is_empty());
        assert_eq!(m.package.name, None);
        assert_eq!(m.run.main_class, None);
    }

    #[test]
    fn parses_full_manifest() {
        let m: Manifest = toml::from_str(
            r#"
            [package]
            name = "hello"
            version = "0.1.0"

            [build]
            source-dirs = ["src/main/java", "generated"]
            classes-dir = "out"
            release = 21
            classpath = ["libs/guava.jar"]
            javac-flags = ["-Xlint:all"]

            [run]
            main-class = "com.example.Main"
            "#,
        )
        .unwrap();
        assert_eq!(m.package.name.as_deref(), Some("hello"));
        assert_eq!(m.package.version.as_deref(), Some("0.1.0"));
        assert_eq!(
            m.build.source_dirs,
            vec!["src/main/java".to_string(), "generated".to_string()]
        );
        assert_eq!(m.build.classes_dir, "out");
        assert_eq!(m.build.release, Some(21));
        assert_eq!(m.build.classpath, vec!["libs/guava.jar".to_string()]);
        assert_eq!(m.build.javac_flags, vec!["-Xlint:all".to_string()]);
        assert_eq!(m.run.main_class.as_deref(), Some("com.example.Main"));
    }

    #[test]
    fn partial_manifest_falls_back_to_defaults() {
        // Only [package].name is given; the absent [build]/[run] tables keep their defaults, and a
        // present-but-partial table fills missing keys from the struct's Default (serde container
        // `default`).
        let m: Manifest = toml::from_str("[package]\nname = \"x\"\n").unwrap();
        assert_eq!(m.package.name.as_deref(), Some("x"));
        assert_eq!(m.build.source_dirs, vec!["src/main/java".to_string()]);
        assert_eq!(m.build.classes_dir, "target/classes");
        assert_eq!(m.run.main_class, None);
    }

    #[test]
    fn source_and_target_without_release() {
        let m: Manifest = toml::from_str("[build]\nsource = 17\ntarget = 17\n").unwrap();
        assert_eq!(m.build.release, None);
        assert_eq!(m.build.source, Some(17));
        assert_eq!(m.build.target, Some(17));
        // The omitted keys still come from the Maven default.
        assert_eq!(m.build.source_dirs, vec!["src/main/java".to_string()]);
    }

    #[test]
    fn discover_path_returns_none_when_absent() {
        // A path with no `jals.toml` anywhere above it yields None. Use the root, which has none.
        assert_eq!(Manifest::discover_path(Path::new("/")), None);
    }
}
