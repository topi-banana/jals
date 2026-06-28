//! Project manifest, deserialized from `jals.toml`.
//!
//! A `jals.toml` describes a Java project the way `Cargo.toml` describes a Rust crate. Every key is
//! optional; omitted keys fall back to [`Manifest::default`], which encodes the Maven-style
//! `src/main/java` -> `target/classes` layout. Keys are kebab-case and grouped into `[package]`,
//! `[build]`, and `[run]` sections.

use std::collections::BTreeMap;
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
    /// Named entry points (`[[bin]]`). An empty list means the single entry point comes from
    /// `[run] main-class`; otherwise the run target is selected from these. See
    /// [`crate::resolve_run_target`].
    pub bin: Vec<Bin>,
    /// External dependencies (`[dependencies]`), keyed by name (the Java analogue of Cargo's
    /// `[dependencies]`). A `BTreeMap` so iteration order is deterministic — the resolved classpath
    /// and any diagnostics come out in a stable order. Each value is a [`Dependency`] spec; today
    /// only the `jar` form is supported. The host (`jals-cli`/`jals-lsp`) resolves each entry to a
    /// local `.jar` (downloading remote ones) and folds it into the classpath; this crate only
    /// classifies the specs (see [`Manifest::dependency_sources`]), staying pure.
    pub dependencies: BTreeMap<String, Dependency>,
}

/// A single `[dependencies]` entry.
///
/// Modeled as a struct of optional fields rather than an enum so future forms (`version`, a Maven
/// coordinate, a local `path`) can be added without breaking existing manifests; exactly which forms
/// are mutually exclusive is enforced by [`Manifest::validate`]. Today only `jar` is supported.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Dependency {
    /// A `.jar` location: an `https://`/`http://` URL (the host downloads it), a `file://` URL, or a
    /// bare path (relative to the manifest directory). Resolved to a local `.jar` by the host, never
    /// here — see [`Dependency::source`].
    pub jar: Option<String>,
}

/// Where a dependency's jar is obtained, classified purely from its spec (no I/O), so the host knows
/// whether to download it or read it off disk. Produced by [`Dependency::source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    /// An `https://`/`http://` URL the host must download.
    Url(String),
    /// A local `.jar` path — from a `file://` URL or a bare path, the latter resolved against the
    /// manifest directory when relative.
    Path(PathBuf),
}

/// A `[dependencies]` entry whose form could not be classified, found by [`Dependency::source`] /
/// [`Manifest::dependency_sources`]. Carries the dependency name for an actionable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyError {
    /// The `jar` value is empty.
    EmptyJar {
        /// The dependency's name.
        name: String,
    },
    /// The `jar` value uses an unsupported URL scheme (only `https`/`http`/`file` are known).
    UnknownScheme {
        /// The dependency's name.
        name: String,
        /// The offending `jar` value.
        value: String,
    },
    /// No recognised form was specified (no `jar`).
    NoForm {
        /// The dependency's name.
        name: String,
    },
}

impl fmt::Display for DependencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencyError::EmptyJar { name } => {
                write!(f, "dependency `{name}` has an empty `jar`")
            }
            DependencyError::UnknownScheme { name, value } => write!(
                f,
                "dependency `{name}` has an unsupported `jar` URL scheme `{value}` \
                 (expected `https://`, `http://`, `file://`, or a path)"
            ),
            DependencyError::NoForm { name } => {
                write!(
                    f,
                    "dependency `{name}` specifies no source (expected `jar`)"
                )
            }
        }
    }
}

impl Error for DependencyError {}

/// Project metadata (`[package]`).
///
/// Most fields are informational for now — they are not passed to `javac` — and are reserved for
/// future jar packaging. `default-run` is the exception: it selects which `[[bin]]` `jals run`
/// executes by default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Package {
    /// Project name.
    pub name: Option<String>,
    /// Project version.
    pub version: Option<String>,
    /// Which `[[bin]]` `jals run` runs when several exist and `--bin` is not given (Cargo
    /// `[package] default-run`). Must name an existing `[[bin]]`.
    pub default_run: Option<String>,
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
    ///
    /// Used only when no `[[bin]]` is declared; once any `[[bin]]` exists the run target is
    /// selected from the bins instead. See [`crate::resolve_run_target`].
    pub main_class: Option<String>,
}

/// A named entry point (`[[bin]]`), the Java analogue of Cargo's `[[bin]]`.
///
/// Because `javac` compiles all sources together — a `[[bin]]` is not a separate compilation unit
/// as in Rust — `[[bin]]` only selects *which* `main-class` `java` runs; it never affects
/// compilation. `name` is the selector for `--bin <name>` and `[package] default-run`;
/// `main-class` is the fully-qualified class passed to `java`. Both fields are required.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Bin {
    /// The bin's selector name (for `--bin <name>` and `[package] default-run`).
    pub name: String,
    /// Fully-qualified main class this bin runs.
    pub main_class: String,
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

impl Dependency {
    /// Classify this dependency's source without any I/O, so the host knows whether to download it
    /// (`Url`) or read it off disk (`Path`). `name` is only used to label errors; `manifest_dir` is
    /// joined onto bare relative paths (pure path arithmetic, exactly like [`Manifest::classpath_entries`]).
    ///
    /// # Errors
    /// Returns [`DependencyError`] when no form is given, the `jar` value is empty, or it uses an
    /// unsupported URL scheme.
    pub fn source(
        &self,
        name: &str,
        manifest_dir: &Path,
    ) -> Result<DependencySource, DependencyError> {
        let jar = self
            .jar
            .as_deref()
            .ok_or_else(|| DependencyError::NoForm { name: name.into() })?;
        if jar.is_empty() {
            return Err(DependencyError::EmptyJar { name: name.into() });
        }
        if let Some(rest) = jar.strip_prefix("file://") {
            // `file:///abs/path` -> `/abs/path`. A full file-URL decode (percent-encoding, Windows
            // drive letters) can come later; this is enough for the common Unix absolute-path form.
            return Ok(DependencySource::Path(PathBuf::from(rest)));
        }
        if jar.starts_with("https://") || jar.starts_with("http://") {
            return Ok(DependencySource::Url(jar.to_string()));
        }
        if jar.contains("://") {
            return Err(DependencyError::UnknownScheme {
                name: name.into(),
                value: jar.to_string(),
            });
        }
        // No scheme: a path relative to the manifest directory (mirrors `classpath_entries`).
        Ok(DependencySource::Path(manifest_dir.join(jar)))
    }
}

impl Manifest {
    /// Load, parse, and validate a specific `jals.toml` file.
    ///
    /// # Errors
    /// Returns [`ManifestError`] when the file cannot be read, contains invalid TOML, or fails
    /// [`Manifest::validate`] (e.g. duplicate `[[bin]]` names).
    pub fn from_file(path: &Path) -> Result<Manifest, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let manifest: Manifest = toml::from_str(&text).map_err(|source| ManifestError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        manifest
            .validate()
            .map_err(|source| ManifestError::Invalid {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(manifest)
    }

    /// Structurally validate the manifest, independent of any filesystem (pure, so it stays
    /// `wasm32`-compatible and is called by [`Manifest::from_file`] right after parsing).
    ///
    /// Checks the `[[bin]]` table: every bin needs a non-empty `name` and `main-class`, names must
    /// be unique, and `[package] default-run` (when set) must name a declared bin.
    ///
    /// # Errors
    /// Returns [`ValidationError`] describing the first problem found.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let mut seen: Vec<&str> = Vec::with_capacity(self.bin.len());
        for bin in &self.bin {
            if bin.name.is_empty() {
                return Err(ValidationError::EmptyBinField { field: "name" });
            }
            if bin.main_class.is_empty() {
                return Err(ValidationError::EmptyBinField {
                    field: "main-class",
                });
            }
            if seen.contains(&bin.name.as_str()) {
                return Err(ValidationError::DuplicateBin {
                    name: bin.name.clone(),
                });
            }
            seen.push(&bin.name);
        }

        if let Some(default) = &self.package.default_run
            && !self.bin.iter().any(|b| &b.name == default)
        {
            return Err(ValidationError::UnknownDefaultRun {
                name: default.clone(),
                available: self.bin.iter().map(|b| b.name.clone()).collect(),
            });
        }

        // `[dependencies]`: every entry must classify (a non-empty `jar` with a known scheme).
        // Structural problems are hard errors, like Cargo; runtime I/O failures (a download that
        // fails, a missing local jar) are soft warnings handled later by the host's resolver. The
        // `manifest_dir` is irrelevant to the error cases, so a placeholder is fine.
        for (name, dep) in &self.dependencies {
            dep.source(name, Path::new("."))
                .map_err(ValidationError::Dependency)?;
        }

        Ok(())
    }

    /// The absolute `.java` source roots: each `[build] source-dirs` entry resolved against
    /// `manifest_dir` (the manifest's own directory). These feed `javac -sourcepath` and are the
    /// roots scanned for `.java` files.
    pub fn source_roots(&self, manifest_dir: &Path) -> Vec<PathBuf> {
        self.build
            .source_dirs
            .iter()
            .map(|d| manifest_dir.join(d))
            .collect()
    }

    /// The absolute classpath entries: each `[build] classpath` entry (a jar or a directory of
    /// `.class` files) resolved against `manifest_dir`. Symmetric with [`source_roots`]; the host
    /// reads the `.class` files from these (directly or out of a jar) to feed `jals-hir`'s classpath
    /// bridge, keeping this crate pure and `wasm32`-compatible.
    ///
    /// [`source_roots`]: Manifest::source_roots
    pub fn classpath_entries(&self, manifest_dir: &Path) -> Vec<PathBuf> {
        self.build
            .classpath
            .iter()
            .map(|c| manifest_dir.join(c))
            .collect()
    }

    /// Classify every `[dependencies]` entry into a host-resolvable [`DependencySource`], paired with
    /// its name (used for cache filenames and diagnostics), separating the ones that classified from
    /// any [`DependencyError`]s. Pure — no I/O; the host downloads `Url`s and reads `Path`s.
    ///
    /// In practice [`Manifest::validate`] (run by [`Manifest::from_file`]) already rejects the error
    /// cases, so a manifest loaded through `from_file` yields an empty error list; the errors are
    /// surfaced here too for callers that classify a `Manifest` they built or parsed directly.
    pub fn dependency_sources(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>) {
        let mut sources = Vec::new();
        let mut errors = Vec::new();
        for (name, dep) in &self.dependencies {
            match dep.source(name, manifest_dir) {
                Ok(src) => sources.push((name.clone(), src)),
                Err(err) => errors.push(err),
            }
        }
        (sources, errors)
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

/// An error loading, parsing, or validating a manifest file.
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
            ManifestError::Io { path, source } => {
                write!(f, "failed to read manifest {}: {source}", path.display())
            }
            ManifestError::Parse { path, source } => {
                write!(f, "failed to parse manifest {}: {source}", path.display())
            }
            ManifestError::Invalid { path, source } => {
                write!(f, "invalid manifest {}: {source}", path.display())
            }
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ManifestError::Io { source, .. } => Some(source),
            ManifestError::Parse { source, .. } => Some(source),
            ManifestError::Invalid { source, .. } => Some(source),
        }
    }
}

/// A structural problem in a manifest, found by [`Manifest::validate`] (independent of the file it
/// came from — [`ManifestError::Invalid`] adds the path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Two `[[bin]]` entries share a `name`.
    DuplicateBin {
        /// The duplicated bin name.
        name: String,
    },
    /// `[package] default-run` names a bin that does not exist.
    UnknownDefaultRun {
        /// The requested default bin name.
        name: String,
        /// The declared bin names, for an actionable message.
        available: Vec<String>,
    },
    /// A `[[bin]]` has an empty `name` or `main-class`.
    EmptyBinField {
        /// Which field was empty (`"name"` or `"main-class"`).
        field: &'static str,
    },
    /// A `[dependencies]` entry could not be classified — an empty `jar`, an unsupported URL scheme,
    /// or no recognised form. Wraps the classification [`DependencyError`] so the two layers share a
    /// single message and the variant set never drifts apart.
    Dependency(DependencyError),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::DuplicateBin { name } => {
                write!(f, "duplicate `[[bin]]` name `{name}`")
            }
            ValidationError::UnknownDefaultRun { name, available } => write!(
                f,
                "`[package] default-run` is `{name}`, which is not a declared bin (available: {})",
                available.join(", ")
            ),
            ValidationError::EmptyBinField { field } => {
                write!(f, "a `[[bin]]` has an empty `{field}`")
            }
            ValidationError::Dependency(err) => write!(f, "{err}"),
        }
    }
}

impl Error for ValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ValidationError::Dependency(err) => Some(err),
            _ => None,
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
        assert_eq!(m.package.default_run, None);
        assert_eq!(m.run.main_class, None);
        assert!(m.bin.is_empty());
        assert!(m.dependencies.is_empty());
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
        // No `[[bin]]`: the bin list is empty and selection falls back to `[run] main-class`.
        assert!(m.bin.is_empty());
        assert_eq!(m.package.default_run, None);
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
        assert!(m.bin.is_empty());
    }

    #[test]
    fn classpath_entries_resolve_against_manifest_dir() {
        let m: Manifest =
            toml::from_str("[build]\nclasspath = [\"libs/guava.jar\", \"out/classes\"]\n").unwrap();
        assert_eq!(
            m.classpath_entries(Path::new("/proj")),
            vec![
                PathBuf::from("/proj/libs/guava.jar"),
                PathBuf::from("/proj/out/classes"),
            ]
        );
        // The default (empty) classpath resolves to no entries.
        assert!(
            Manifest::default()
                .classpath_entries(Path::new("/proj"))
                .is_empty()
        );
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

    #[test]
    fn parses_bin_array_of_tables() {
        let m: Manifest = toml::from_str(
            r#"
            [package]
            default-run = "server"

            [[bin]]
            name = "server"
            main-class = "com.example.Server"

            [[bin]]
            name = "cli"
            main-class = "com.example.Cli"
            "#,
        )
        .unwrap();
        assert_eq!(m.package.default_run.as_deref(), Some("server"));
        assert_eq!(m.bin.len(), 2);
        assert_eq!(m.bin[0].name, "server");
        assert_eq!(m.bin[0].main_class, "com.example.Server");
        assert_eq!(m.bin[1].name, "cli");
        assert_eq!(m.bin[1].main_class, "com.example.Cli");
        // A well-formed multi-bin manifest with a valid default-run passes validation.
        assert!(m.validate().is_ok());
    }

    #[test]
    fn bin_requires_name_and_main_class() {
        // Both `[[bin]]` fields are mandatory: a missing key is a parse error, not a silent default.
        assert!(toml::from_str::<Manifest>("[[bin]]\nname = \"x\"\n").is_err());
        assert!(toml::from_str::<Manifest>("[[bin]]\nmain-class = \"X\"\n").is_err());
    }

    fn bin(name: &str, main_class: &str) -> Bin {
        Bin {
            name: name.to_string(),
            main_class: main_class.to_string(),
        }
    }

    /// A manifest carrying just the given `[[bin]]` entries (avoids `field_reassign_with_default`).
    fn manifest_with_bins(bin: Vec<Bin>) -> Manifest {
        Manifest {
            bin,
            ..Default::default()
        }
    }

    #[test]
    fn validate_accepts_unique_bins_and_valid_default_run() {
        let mut m = manifest_with_bins(vec![bin("a", "A"), bin("b", "B")]);
        m.package.default_run = Some("b".to_string());
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_duplicate_bin_names() {
        let m = manifest_with_bins(vec![bin("dup", "A"), bin("dup", "B")]);
        assert_eq!(
            m.validate(),
            Err(ValidationError::DuplicateBin {
                name: "dup".to_string()
            })
        );
    }

    #[test]
    fn validate_rejects_unknown_default_run() {
        let mut m = manifest_with_bins(vec![bin("a", "A")]);
        m.package.default_run = Some("ghost".to_string());
        assert_eq!(
            m.validate(),
            Err(ValidationError::UnknownDefaultRun {
                name: "ghost".to_string(),
                available: vec!["a".to_string()],
            })
        );
    }

    #[test]
    fn validate_rejects_empty_bin_fields() {
        let m = manifest_with_bins(vec![bin("", "A")]);
        assert_eq!(
            m.validate(),
            Err(ValidationError::EmptyBinField { field: "name" })
        );

        let m = manifest_with_bins(vec![bin("a", "")]);
        assert_eq!(
            m.validate(),
            Err(ValidationError::EmptyBinField {
                field: "main-class"
            })
        );
    }

    #[test]
    fn parses_dependencies_table() {
        let m: Manifest = toml::from_str(
            r#"
            [dependencies]
            testlib = { jar = "https://example.com/lib.jar" }
            otherlib = { jar = "file:///abs/path/lib.jar" }
            "#,
        )
        .unwrap();
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(
            m.dependencies.get("testlib").and_then(|d| d.jar.as_deref()),
            Some("https://example.com/lib.jar")
        );
        assert_eq!(
            m.dependencies
                .get("otherlib")
                .and_then(|d| d.jar.as_deref()),
            Some("file:///abs/path/lib.jar")
        );
    }

    #[test]
    fn dependency_source_classifies_https_as_url() {
        let dep = Dependency {
            jar: Some("https://example.com/lib.jar".to_string()),
        };
        assert_eq!(
            dep.source("testlib", Path::new("/proj")),
            Ok(DependencySource::Url(
                "https://example.com/lib.jar".to_string()
            ))
        );
    }

    #[test]
    fn dependency_source_classifies_file_url_as_path() {
        let dep = Dependency {
            jar: Some("file:///abs/path/lib.jar".to_string()),
        };
        assert_eq!(
            dep.source("otherlib", Path::new("/proj")),
            Ok(DependencySource::Path(PathBuf::from("/abs/path/lib.jar")))
        );
    }

    #[test]
    fn dependency_source_classifies_bare_path_relative_to_manifest_dir() {
        let dep = Dependency {
            jar: Some("libs/lib.jar".to_string()),
        };
        assert_eq!(
            dep.source("locallib", Path::new("/proj")),
            Ok(DependencySource::Path(PathBuf::from("/proj/libs/lib.jar")))
        );
    }

    #[test]
    fn validate_rejects_empty_jar() {
        let m = Manifest {
            dependencies: BTreeMap::from([(
                "bad".to_string(),
                Dependency {
                    jar: Some(String::new()),
                },
            )]),
            ..Default::default()
        };
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(DependencyError::EmptyJar {
                name: "bad".to_string()
            }))
        );
    }

    #[test]
    fn validate_rejects_unknown_scheme() {
        let m = Manifest {
            dependencies: BTreeMap::from([(
                "bad".to_string(),
                Dependency {
                    jar: Some("ftp://example.com/lib.jar".to_string()),
                },
            )]),
            ..Default::default()
        };
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(
                DependencyError::UnknownScheme {
                    name: "bad".to_string(),
                    value: "ftp://example.com/lib.jar".to_string(),
                }
            ))
        );
    }

    #[test]
    fn dependency_sources_separates_ok_and_errors() {
        let m = Manifest {
            dependencies: BTreeMap::from([
                (
                    "good".to_string(),
                    Dependency {
                        jar: Some("file:///abs/good.jar".to_string()),
                    },
                ),
                (
                    "empty".to_string(),
                    Dependency {
                        jar: Some(String::new()),
                    },
                ),
            ]),
            ..Default::default()
        };
        let (sources, errors) = m.dependency_sources(Path::new("/proj"));
        assert_eq!(
            sources,
            vec![(
                "good".to_string(),
                DependencySource::Path(PathBuf::from("/abs/good.jar"))
            )]
        );
        assert_eq!(
            errors,
            vec![DependencyError::EmptyJar {
                name: "empty".to_string()
            }]
        );
    }
}
