//! Host-only, `std::path`-based resolution over a [`Manifest`].
//!
//! The pure serde model and validation of a `jals.toml` live in [`jals_config`]; that crate is
//! `no_std` and cannot build a [`PathBuf`] or read a file. This module adds the host half — the
//! [`ManifestExt`] extension trait (`from_file` / `discover_path` / `source_roots` /
//! `classpath_entries` / the dependency classifiers) plus the `PathBuf`-carrying "resolved" types
//! ([`DependencySource`], [`PathSource`], [`GitSource`], [`SourceDependency`]) — on top of
//! `jals_config::Manifest`. Everything here is pure apart from `from_file`/`discover_path`
//! (`std::fs` reads); `jals-cli`/`jals-classpath`/`jals-lsp` consume it.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use jals_config::{
    Dependency, DependencyError, GitDependency, GitRef, Manifest, ManifestParseError,
    PathDependency, ValidationError,
};

/// A `git` dependency's classified spec.
///
/// Contains the clone URL, which commit to check out, and an optional source-root subdirectory. The
/// host clones the URL, checks out the [`reference`](GitSource::reference), then reads the `.java`
/// under [`dir`](GitSource::dir) (or the auto-detected source root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// The repository URL to clone.
    pub url: String,
    /// Which commit to check out.
    pub reference: GitRef,
    /// The source root within the repo (`dir = "..."`); `None` to auto-detect.
    pub dir: Option<String>,
}

/// A `path` dependency's classified spec: the local directory (resolved against the manifest dir) and
/// an optional source-root subdirectory within it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSource {
    /// The dependency's root directory (the `path` value resolved against the manifest dir).
    pub root: PathBuf,
    /// The source root within `root` (`dir = "..."`); `None` to auto-detect.
    pub dir: Option<String>,
}

/// A `git` / `path` dependency whose `.java` source the host indexes for analysis and navigation.
///
/// This is the resolved source-form of a [`Dependency`], collected by
/// [`ManifestExt::dependency_source_dirs`]. A `jar` dependency is never one of these; its classes
/// come from the classpath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDependency {
    /// A git repository to clone and read `.java` from.
    Git(GitSource),
    /// A local directory to read `.java` from.
    Path(PathSource),
}

/// Where a dependency's jar is obtained.
///
/// Classified purely from its spec (no I/O), so the host knows whether to download it or read it off
/// disk. Produced by [`ManifestExt::dependency_sources`] /
/// [`ManifestExt::dependency_source_jars`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    /// An `https://`/`http://` URL the host must download.
    Url(String),
    /// A local `.jar` path — from a `file://` URL or a bare path, the latter resolved against the
    /// manifest directory when relative.
    Path(PathBuf),
}

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

impl DependencySource {
    /// Classify a jar-location string (a `jar` or `sources` value) into a [`DependencySource`]
    /// without any I/O, shared by [`Self::from_jar`] and [`Self::from_sources`]. Reuses
    /// [`Dependency::validate_jar_location`] for the value-level errors, then resolves the happy path to a
    /// `PathBuf` / URL. `field` names the source field for error messages; `manifest_dir` is joined
    /// onto a bare relative path (mirrors [`ManifestExt::classpath_entries`]).
    fn classify(
        value: &str,
        name: &str,
        field: &'static str,
        manifest_dir: &Path,
    ) -> Result<Self, DependencyError> {
        Dependency::validate_jar_location(value, name, field)?;
        if let Some(rest) = value.strip_prefix("file://") {
            // `file:///abs/path` -> `/abs/path`. A full file-URL decode (percent-encoding, Windows
            // drive letters) can come later; this is enough for the common Unix absolute-path form.
            return Ok(Self::Path(PathBuf::from(rest)));
        }
        if value.starts_with("https://") || value.starts_with("http://") {
            return Ok(Self::Url(value.to_string()));
        }
        // Validated bare path: relative to the manifest directory (mirrors `classpath_entries`).
        Ok(Self::Path(manifest_dir.join(value)))
    }

    /// The compiled-jar classpath source of a `jar` dependency, or `None` for a `git`/`path` source
    /// dependency (which contributes no classpath jar).
    fn from_jar(
        dep: &Dependency,
        name: &str,
        manifest_dir: &Path,
    ) -> Option<Result<Self, DependencyError>> {
        match dep {
            Dependency::Jar(jar) => Some(Self::classify(&jar.jar, name, "jar", manifest_dir)),
            Dependency::Git(_) | Dependency::Path(_) => None,
        }
    }

    /// The optional companion **sources** jar of a `jar` dependency, classified the same way as the
    /// `jar` value. `None` when this is not a `jar` dependency or it declares no `sources`.
    fn from_sources(
        dep: &Dependency,
        name: &str,
        manifest_dir: &Path,
    ) -> Option<Result<Self, DependencyError>> {
        match dep {
            Dependency::Jar(jar) => jar
                .sources
                .as_deref()
                .map(|sources| Self::classify(sources, name, "sources", manifest_dir)),
            Dependency::Git(_) | Dependency::Path(_) => None,
        }
    }

    /// Classify every `[dependencies]` entry through `classify`, collecting the `Some(Ok)` values
    /// (each paired with its dependency name) and the `Some(Err)`s into separate vectors; a `None`
    /// (a form this `classify` does not apply to) is skipped. The shared spine of the
    /// `dependency_*` accessors.
    fn collect_dependencies<T>(
        manifest: &Manifest,
        manifest_dir: &Path,
        classify: impl Fn(&Dependency, &str, &Path) -> Option<Result<T, DependencyError>>,
    ) -> (Vec<(String, T)>, Vec<DependencyError>) {
        let mut oks = Vec::new();
        let mut errors = Vec::new();
        for (name, dep) in &manifest.dependencies {
            match classify(dep, name, manifest_dir) {
                Some(Ok(value)) => oks.push((name.clone(), value)),
                Some(Err(err)) => errors.push(err),
                None => {}
            }
        }
        (oks, errors)
    }
}

impl SourceDependency {
    /// The resolved `.java` source tree of a `git`/`path` dependency, or `None` for a `jar`
    /// dependency (whose classes come from the classpath).
    fn from_dependency(
        dep: &Dependency,
        name: &str,
        manifest_dir: &Path,
    ) -> Option<Result<Self, DependencyError>> {
        match dep {
            Dependency::Jar(_) => None,
            Dependency::Git(git) => Some(Self::from_git(git, name)),
            Dependency::Path(path) => Some(Self::from_path(path, name, manifest_dir)),
        }
    }

    /// Resolve a `git` dependency into a [`SourceDependency::Git`]: a non-empty clone URL and a
    /// single pinned [`GitRef`] (from `jals_config`'s pure [`GitDependency::git_ref`]).
    fn from_git(git: &GitDependency, name: &str) -> Result<Self, DependencyError> {
        if git.git.is_empty() {
            return Err(DependencyError::Empty {
                name: name.to_string(),
                field: "git",
            });
        }
        Ok(Self::Git(GitSource {
            url: git.git.clone(),
            reference: git.git_ref(name)?,
            dir: git.dir.clone(),
        }))
    }

    /// Resolve a `path` dependency into a [`SourceDependency::Path`]: a non-empty directory resolved
    /// against `manifest_dir`, plus the optional source-root `dir`.
    fn from_path(
        path: &PathDependency,
        name: &str,
        manifest_dir: &Path,
    ) -> Result<Self, DependencyError> {
        if path.path.is_empty() {
            return Err(DependencyError::Empty {
                name: name.to_string(),
                field: "path",
            });
        }
        Ok(Self::Path(PathSource {
            root: manifest_dir.join(&path.path),
            dir: path.dir.clone(),
        }))
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

    /// The absolute classpath entries: each `[build] classpath` entry (a jar or a directory of
    /// `.class` files) resolved against `manifest_dir`. Symmetric with [`source_roots`](ManifestExt::source_roots);
    /// the host reads the `.class` files from these (directly or out of a jar) to feed `jals-hir`'s
    /// classpath bridge.
    fn classpath_entries(&self, manifest_dir: &Path) -> Vec<PathBuf>;

    /// Classify every **`jar`** `[dependencies]` entry into a host-resolvable [`DependencySource`],
    /// paired with its name, separating the ones that classified from any [`DependencyError`]s. The
    /// binary-classpath half of dependency resolution. Pure — no I/O; the host downloads `Url`s and
    /// reads `Path`s. Source-form (`git`/`path`) dependencies are skipped (see
    /// [`dependency_source_dirs`](ManifestExt::dependency_source_dirs)).
    fn dependency_sources(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>);

    /// Classify the **sources** jar of every `[dependencies]` entry that declares one, paired with its
    /// name — the sources-jar counterpart of [`dependency_sources`](ManifestExt::dependency_sources).
    /// Entries without a `sources` field are simply absent.
    fn dependency_source_jars(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>);

    /// Classify every **source-form** (`git`/`path`) `[dependencies]` entry into a host-resolvable
    /// [`SourceDependency`], paired with its name — the source-tree counterpart of
    /// [`dependency_sources`](ManifestExt::dependency_sources). `jar` dependencies are skipped.
    fn dependency_source_dirs(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, SourceDependency)>, Vec<DependencyError>);
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

    fn classpath_entries(&self, manifest_dir: &Path) -> Vec<PathBuf> {
        self.build
            .classpath
            .iter()
            .map(|c| manifest_dir.join(c))
            .collect()
    }

    fn dependency_sources(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>) {
        DependencySource::collect_dependencies(self, manifest_dir, DependencySource::from_jar)
    }

    fn dependency_source_jars(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>) {
        DependencySource::collect_dependencies(self, manifest_dir, DependencySource::from_sources)
    }

    fn dependency_source_dirs(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, SourceDependency)>, Vec<DependencyError>) {
        DependencySource::collect_dependencies(
            self,
            manifest_dir,
            SourceDependency::from_dependency,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jals_config::{GitDependency, JarDependency, PathDependency};
    use std::collections::BTreeMap;

    /// A `jar`-form dependency with no companion `sources` jar and no bundled-jar recursion.
    fn jar_dep(jar: &str) -> Dependency {
        Dependency::Jar(JarDependency {
            jar: jar.to_string(),
            sources: None,
            recursive: None,
        })
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
    fn discover_path_returns_none_when_absent() {
        // A path with no `jals.toml` anywhere above it yields None. Use the root, which has none.
        assert_eq!(Manifest::discover_path(Path::new("/")), None);
    }

    #[test]
    fn dependency_jar_classifies_https_as_url() {
        let dep = jar_dep("https://example.com/lib.jar");
        assert_eq!(
            DependencySource::from_jar(&dep, "testlib", Path::new("/proj")),
            Some(Ok(DependencySource::Url(
                "https://example.com/lib.jar".to_string()
            )))
        );
    }

    #[test]
    fn dependency_jar_classifies_file_url_as_path() {
        let dep = jar_dep("file:///abs/path/lib.jar");
        assert_eq!(
            DependencySource::from_jar(&dep, "otherlib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/abs/path/lib.jar"
            ))))
        );
    }

    #[test]
    fn dependency_jar_classifies_bare_path_relative_to_manifest_dir() {
        let dep = jar_dep("libs/lib.jar");
        assert_eq!(
            DependencySource::from_jar(&dep, "locallib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/proj/libs/lib.jar"
            ))))
        );
    }

    #[test]
    fn dependency_sources_separates_ok_and_errors() {
        let m = Manifest {
            dependencies: BTreeMap::from([
                ("good".to_string(), jar_dep("file:///abs/good.jar")),
                ("empty".to_string(), jar_dep("")),
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
            vec![DependencyError::Empty {
                name: "empty".to_string(),
                field: "jar",
            }]
        );
    }

    #[test]
    fn sources_source_is_none_without_a_sources_field() {
        let dep = jar_dep("libs/lib.jar");
        assert_eq!(
            DependencySource::from_sources(&dep, "lib", Path::new("/proj")),
            None
        );
    }

    #[test]
    fn sources_source_classifies_like_jar() {
        let dep = Dependency::Jar(JarDependency {
            jar: "libs/lib.jar".to_string(),
            sources: Some("https://example.com/lib-sources.jar".to_string()),
            recursive: None,
        });
        assert_eq!(
            DependencySource::from_sources(&dep, "lib", Path::new("/proj")),
            Some(Ok(DependencySource::Url(
                "https://example.com/lib-sources.jar".to_string()
            )))
        );

        let local = Dependency::Jar(JarDependency {
            jar: "libs/lib.jar".to_string(),
            sources: Some("libs/lib-sources.jar".to_string()),
            recursive: None,
        });
        assert_eq!(
            DependencySource::from_sources(&local, "lib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/proj/libs/lib-sources.jar"
            ))))
        );
    }

    #[test]
    fn dependency_source_jars_collects_only_declared_sources() {
        let m = Manifest {
            dependencies: BTreeMap::from([
                (
                    "withsrc".to_string(),
                    Dependency::Jar(JarDependency {
                        jar: "file:///abs/a.jar".to_string(),
                        sources: Some("file:///abs/a-sources.jar".to_string()),
                        recursive: None,
                    }),
                ),
                ("nosrc".to_string(), jar_dep("file:///abs/b.jar")),
            ]),
            ..Default::default()
        };
        let (sources, errors) = m.dependency_source_jars(Path::new("/proj"));
        assert_eq!(
            sources,
            vec![(
                "withsrc".to_string(),
                DependencySource::Path(PathBuf::from("/abs/a-sources.jar"))
            )]
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn jar_and_path_forms_resolve() {
        let jar = jar_dep("libs/lib.jar");
        assert_eq!(
            DependencySource::from_jar(&jar, "lib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/proj/libs/lib.jar"
            ))))
        );
        assert_eq!(
            SourceDependency::from_dependency(&jar, "lib", Path::new("/proj")),
            None
        );

        let path = Dependency::Path(PathDependency {
            path: "../sibling".to_string(),
            dir: Some("src".to_string()),
        });
        assert_eq!(
            SourceDependency::from_dependency(&path, "lib", Path::new("/proj")),
            Some(Ok(SourceDependency::Path(PathSource {
                root: PathBuf::from("/proj/../sibling"),
                dir: Some("src".to_string()),
            })))
        );
        assert_eq!(
            DependencySource::from_jar(&path, "lib", Path::new("/proj")),
            None
        );
    }

    #[test]
    fn git_refs_classify() {
        let make = |branch, tag, rev| {
            Dependency::Git(GitDependency {
                git: "https://example.com/r.git".to_string(),
                branch,
                tag,
                rev,
                dir: None,
            })
        };
        let git_ref =
            |d: &Dependency| match SourceDependency::from_dependency(d, "r", Path::new("/proj")) {
                Some(Ok(SourceDependency::Git(g))) => g.reference,
                other => panic!("expected git source, got {other:?}"),
            };
        assert_eq!(git_ref(&make(None, None, None)), GitRef::Default);
        assert_eq!(
            git_ref(&make(Some("main".to_string()), None, None)),
            GitRef::Branch("main".to_string())
        );
        assert_eq!(
            git_ref(&make(None, Some("v1".to_string()), None)),
            GitRef::Tag("v1".to_string())
        );
        assert_eq!(
            git_ref(&make(None, None, Some("abc123".to_string()))),
            GitRef::Rev("abc123".to_string())
        );
    }

    #[test]
    fn dependency_source_dirs_collects_git_and_path_only() {
        let m = Manifest {
            dependencies: BTreeMap::from([
                ("fromjar".to_string(), jar_dep("libs/lib.jar")),
                (
                    "fromgit".to_string(),
                    Dependency::Git(GitDependency {
                        git: "https://example.com/r.git".to_string(),
                        branch: None,
                        tag: Some("v1".to_string()),
                        rev: None,
                        dir: None,
                    }),
                ),
                (
                    "frompath".to_string(),
                    Dependency::Path(PathDependency {
                        path: "../sibling".to_string(),
                        dir: None,
                    }),
                ),
            ]),
            ..Default::default()
        };
        let (dirs, errors) = m.dependency_source_dirs(Path::new("/proj"));
        assert!(errors.is_empty());
        assert_eq!(
            dirs,
            vec![
                (
                    "fromgit".to_string(),
                    SourceDependency::Git(GitSource {
                        url: "https://example.com/r.git".to_string(),
                        reference: GitRef::Tag("v1".to_string()),
                        dir: None,
                    })
                ),
                (
                    "frompath".to_string(),
                    SourceDependency::Path(PathSource {
                        root: PathBuf::from("/proj/../sibling"),
                        dir: None,
                    })
                ),
            ]
        );
        // The classpath classifier sees only the jar, never the source forms.
        let (jars, errors) = m.dependency_sources(Path::new("/proj"));
        assert!(errors.is_empty());
        assert_eq!(
            jars,
            vec![(
                "fromjar".to_string(),
                DependencySource::Path(PathBuf::from("/proj/libs/lib.jar"))
            )]
        );
    }
}
