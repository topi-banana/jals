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
    /// and any diagnostics come out in a stable order. Each value is a [`Dependency`] — one of the
    /// `jar` / `git` / `path` forms, chosen by serde at parse time. The host (`jals-cli`/`jals-lsp`)
    /// resolves each entry — downloading `jar`s onto the classpath, cloning/reading `git`/`path`
    /// source into the editor index; this crate only classifies the specs (see
    /// [`Manifest::dependency_sources`] / [`Manifest::dependency_source_dirs`]), staying pure.
    pub dependencies: BTreeMap<String, Dependency>,
}

/// A single `[dependencies]` entry, in exactly one of three forms.
///
/// This is both the parsed model **and** its classification — there is no separate "kind". serde
/// deserializes the right variant directly from TOML (`#[serde(untagged)]`): the form is chosen by
/// which fields are present, and each variant's `#[serde(deny_unknown_fields)]` makes the forms
/// mutually exclusive — `{ jar, git }` matches no variant (so co-occurring forms, a missing form, and
/// fields misplaced onto the wrong form are all rejected at parse time, as a TOML error). The three
/// forms:
/// - **`jar`** — a compiled `.jar` (binary classes for analysis *and* compilation), with an optional
///   companion `sources` jar (the library's `.java`, for editor navigation only). See [`JarDependency`].
/// - **`git`** — a git repository checked out for its `.java` source, pinned with at most one of
///   `branch` / `tag` / `rev` (analysis + editor navigation only; never a compile input). See
///   [`GitDependency`].
/// - **`path`** — a local directory tree of `.java` source (analysis + editor navigation only). See
///   [`PathDependency`].
///
/// The host resolves each form differently: a [`Jar`](Dependency::Jar) is downloaded/read and put on
/// the classpath, a [`Git`](Dependency::Git) is cloned, a [`Path`](Dependency::Path) is read in place;
/// the latter two contribute `.java` source for analysis + navigation only. The resolution accessors
/// ([`jar_source`](Dependency::jar_source) / [`sources_source`](Dependency::sources_source) /
/// [`source_dependency`](Dependency::source_dependency)) classify the raw values into the host-facing
/// [`DependencySource`] / [`SourceDependency`], applying the few checks serde cannot (empty values,
/// URL scheme, at-most-one git ref).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    /// A compiled `.jar` (binary classes for analysis/compilation) with an optional companion
    /// `sources` jar.
    Jar(JarDependency),
    /// A git repository checked out for its `.java` source (analysis + editor navigation only).
    Git(GitDependency),
    /// A local directory tree of `.java` source (analysis + editor navigation only).
    Path(PathDependency),
}

/// The `jar` form of a [`Dependency`]: a compiled `.jar` and its optional companion `sources` jar.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct JarDependency {
    /// A `.jar` location: an `https://`/`http://` URL (the host downloads it), a `file://` URL, or a
    /// bare path (relative to the manifest directory). Resolved to a local `.jar` by the host, never
    /// here — see [`Dependency::jar_source`].
    pub jar: String,
    /// An optional companion **sources** `.jar` (the `-sources.jar` of Maven convention), located the
    /// same way as [`jar`](JarDependency::jar) (URL / `file://` / bare path). It carries the library's
    /// `.java` sources, used only for editor navigation (go-to-definition into the real source); it is
    /// never a compile or analysis input. Resolved by the host — see [`Dependency::sources_source`].
    pub sources: Option<String>,
}

/// The `git` form of a [`Dependency`]: a repository to clone for its `.java` source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GitDependency {
    /// A git repository URL to check out for its `.java` source (an `https://`/`http://`/`git://`/`ssh`
    /// clone target). The checked-out sources are an analysis + editor-navigation input only — they are
    /// never compiled. Pin the commit with at most one of [`branch`](GitDependency::branch) /
    /// [`tag`](GitDependency::tag) / [`rev`](GitDependency::rev); with none, the repo's default branch
    /// is used.
    pub git: String,
    /// The git branch to check out (mutually exclusive with `tag`/`rev`).
    pub branch: Option<String>,
    /// The git tag to check out (mutually exclusive with `branch`/`rev`).
    pub tag: Option<String>,
    /// The git revision (commit SHA) to check out (mutually exclusive with `branch`/`tag`).
    pub rev: Option<String>,
    /// The source root *within* the repo (e.g. `core/src/main/java`), for a non-standard layout; omit
    /// to let the host auto-detect it (`src/main/java` → `src` → the root itself).
    pub dir: Option<String>,
}

/// The `path` form of a [`Dependency`]: a local directory tree of `.java` source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PathDependency {
    /// A local directory tree of `.java` source, relative to the manifest directory. The sources are an
    /// analysis + editor-navigation input only — never compiled.
    pub path: String,
    /// The source root *within* the directory (e.g. `core/src/main/java`), for a non-standard layout;
    /// omit to let the host auto-detect it (`src/main/java` → `src` → the root itself).
    pub dir: Option<String>,
}

/// A `git` dependency's classified spec: the clone URL, which commit to check out, and an optional
/// source-root subdirectory. The host clones the URL, checks out the [`reference`](GitSource::reference),
/// then reads the `.java` under [`dir`](GitSource::dir) (or the auto-detected source root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// The repository URL to clone.
    pub url: String,
    /// Which commit to check out.
    pub reference: GitRef,
    /// The source root within the repo (`dir = "..."`); `None` to auto-detect.
    pub dir: Option<String>,
}

/// Which commit of a [`GitSource`] to check out: the default branch, or a named branch / tag / commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitRef {
    /// No `branch`/`tag`/`rev` given: check out the repository's default branch.
    Default,
    /// A named branch (`branch = "..."`).
    Branch(String),
    /// A named tag (`tag = "..."`).
    Tag(String),
    /// A specific revision / commit SHA (`rev = "..."`).
    Rev(String),
}

impl GitRef {
    /// The `git checkout` argument this ref pins to — the branch / tag / revision to check out, or
    /// `None` for [`Default`](GitRef::Default) (leave the clone on the repository's default branch).
    pub fn checkout_arg(&self) -> Option<&str> {
        match self {
            GitRef::Default => None,
            GitRef::Branch(b) | GitRef::Tag(b) | GitRef::Rev(b) => Some(b),
        }
    }
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

/// A `git` / `path` dependency whose `.java` source the host indexes for analysis and navigation —
/// the resolved source-form of a [`Dependency`], collected by [`Manifest::dependency_source_dirs`]. (A
/// `jar` dependency is never one of these; its classes come from the classpath.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDependency {
    /// A git repository to clone and read `.java` from.
    Git(GitSource),
    /// A local directory to read `.java` from.
    Path(PathSource),
}

/// Where a dependency's jar is obtained, classified purely from its spec (no I/O), so the host knows
/// whether to download it or read it off disk. Produced by [`Dependency::jar_source`] and
/// [`Dependency::sources_source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    /// An `https://`/`http://` URL the host must download.
    Url(String),
    /// A local `.jar` path — from a `file://` URL or a bare path, the latter resolved against the
    /// manifest directory when relative.
    Path(PathBuf),
}

/// A `[dependencies]` entry whose value could not be classified, found by the resolution accessors
/// ([`Dependency::jar_source`] / [`Dependency::sources_source`] / [`Dependency::source_dependency`]).
/// Carries the dependency name for an actionable message.
///
/// These are the checks serde cannot express at parse time. The *structural* errors — a missing form,
/// co-occurring forms (`{ jar, git }`), or a field misplaced onto the wrong form (`branch` without
/// `git`) — are rejected earlier, when [`Dependency`]'s untagged variants fail to match, surfacing as
/// a TOML parse error rather than a `DependencyError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyError {
    /// A field expected to carry a value (`jar` / `sources` / `git` / `path` / a git ref) is present
    /// but empty.
    Empty {
        /// The dependency's name.
        name: String,
        /// Which field was empty (e.g. `"jar"`, `"git"`, `"path"`).
        field: &'static str,
    },
    /// A jar-location field (`jar` or `sources`) uses an unsupported URL scheme (only
    /// `https`/`http`/`file` are known).
    UnknownScheme {
        /// The dependency's name.
        name: String,
        /// Which field carried the bad value (`"jar"` or `"sources"`).
        field: &'static str,
        /// The offending value.
        value: String,
    },
    /// More than one of `branch` / `tag` / `rev` was given for a `git` dependency; at most one is
    /// allowed.
    ConflictingGitRef {
        /// The dependency's name.
        name: String,
    },
}

impl fmt::Display for DependencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DependencyError::Empty { name, field } => {
                write!(f, "dependency `{name}` has an empty `{field}`")
            }
            DependencyError::UnknownScheme { name, field, value } => write!(
                f,
                "dependency `{name}` has an unsupported `{field}` URL scheme `{value}` \
                 (expected `https://`, `http://`, `file://`, or a path)"
            ),
            DependencyError::ConflictingGitRef { name } => write!(
                f,
                "git dependency `{name}` specifies more than one of `branch`, `tag`, `rev` \
                 (use at most one)"
            ),
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
    /// The compiled-jar classpath source of a `jar` dependency, or `None` for a `git`/`path` source
    /// dependency (which contributes no classpath jar). `name` labels errors; `manifest_dir` is joined
    /// onto a bare relative path, exactly like [`Manifest::classpath_entries`].
    ///
    /// # Errors
    /// Returns [`DependencyError`] when the `jar` value is empty or uses an unsupported URL scheme.
    pub fn jar_source(
        &self,
        name: &str,
        manifest_dir: &Path,
    ) -> Option<Result<DependencySource, DependencyError>> {
        match self {
            Dependency::Jar(jar) => Some(classify(&jar.jar, name, "jar", manifest_dir)),
            Dependency::Git(_) | Dependency::Path(_) => None,
        }
    }

    /// The optional companion **sources** jar of a `jar` dependency, classified the same way as the
    /// `jar` value (a URL to download or a path to read). `None` when this is not a `jar` dependency or
    /// it declares no `sources`. `name` labels errors; `manifest_dir` is joined onto a bare relative
    /// path.
    ///
    /// # Errors
    /// Returns [`DependencyError`] when the `sources` value is empty or uses an unsupported URL scheme.
    pub fn sources_source(
        &self,
        name: &str,
        manifest_dir: &Path,
    ) -> Option<Result<DependencySource, DependencyError>> {
        match self {
            Dependency::Jar(jar) => jar
                .sources
                .as_deref()
                .map(|sources| classify(sources, name, "sources", manifest_dir)),
            Dependency::Git(_) | Dependency::Path(_) => None,
        }
    }

    /// The resolved `.java` source tree of a `git`/`path` dependency, or `None` for a `jar` dependency
    /// (whose classes come from the classpath). `name` labels errors; `manifest_dir` is joined onto a
    /// bare relative `path`.
    ///
    /// # Errors
    /// Returns [`DependencyError`] when the `git`/`path` value is empty or (for `git`) more than one of
    /// `branch` / `tag` / `rev` is set.
    pub fn source_dependency(
        &self,
        name: &str,
        manifest_dir: &Path,
    ) -> Option<Result<SourceDependency, DependencyError>> {
        match self {
            Dependency::Jar(_) => None,
            Dependency::Git(git) => Some(git.source(name)),
            Dependency::Path(path) => Some(path.source(name, manifest_dir)),
        }
    }
}

impl GitDependency {
    /// Resolve this `git` dependency into a [`SourceDependency::Git`]: a non-empty clone URL and a
    /// single pinned [`GitRef`] from at most one of `branch` / `tag` / `rev`.
    ///
    /// # Errors
    /// [`DependencyError::Empty`] for an empty `git` URL or git ref, [`DependencyError::ConflictingGitRef`]
    /// when more than one of `branch` / `tag` / `rev` is set.
    fn source(&self, name: &str) -> Result<SourceDependency, DependencyError> {
        if self.git.is_empty() {
            return Err(DependencyError::Empty {
                name: name.into(),
                field: "git",
            });
        }
        Ok(SourceDependency::Git(GitSource {
            url: self.git.clone(),
            reference: self.git_ref(name)?,
            dir: self.dir.clone(),
        }))
    }

    /// Classify the pinned commit from at most one of `branch` / `tag` / `rev`.
    ///
    /// # Errors
    /// [`DependencyError::ConflictingGitRef`] when more than one is set, [`DependencyError::Empty`]
    /// when the one set is empty.
    fn git_ref(&self, name: &str) -> Result<GitRef, DependencyError> {
        let refs: [(&'static str, Option<&str>); 3] = [
            ("branch", self.branch.as_deref()),
            ("tag", self.tag.as_deref()),
            ("rev", self.rev.as_deref()),
        ];
        let present: Vec<(&'static str, &str)> = refs
            .iter()
            .filter_map(|(f, v)| v.map(|v| (*f, v)))
            .collect();
        match present.as_slice() {
            [] => Ok(GitRef::Default),
            [(field, value)] => {
                if value.is_empty() {
                    return Err(DependencyError::Empty {
                        name: name.into(),
                        field,
                    });
                }
                Ok(match *field {
                    "branch" => GitRef::Branch(value.to_string()),
                    "tag" => GitRef::Tag(value.to_string()),
                    _ => GitRef::Rev(value.to_string()),
                })
            }
            _ => Err(DependencyError::ConflictingGitRef { name: name.into() }),
        }
    }
}

impl PathDependency {
    /// Resolve this `path` dependency into a [`SourceDependency::Path`]: a non-empty directory resolved
    /// against `manifest_dir`, plus the optional source-root `dir`.
    ///
    /// # Errors
    /// [`DependencyError::Empty`] when the `path` value is empty.
    fn source(&self, name: &str, manifest_dir: &Path) -> Result<SourceDependency, DependencyError> {
        if self.path.is_empty() {
            return Err(DependencyError::Empty {
                name: name.into(),
                field: "path",
            });
        }
        Ok(SourceDependency::Path(PathSource {
            root: manifest_dir.join(&self.path),
            dir: self.dir.clone(),
        }))
    }
}

/// Classify a jar-location string (a `jar` or `sources` value) into a [`DependencySource`] without any
/// I/O, shared by [`Dependency::jar_source`] and [`Dependency::sources_source`]. `field` names the
/// source field for error messages; `manifest_dir` is joined onto a bare relative path (mirrors
/// [`Manifest::classpath_entries`]).
fn classify(
    value: &str,
    name: &str,
    field: &'static str,
    manifest_dir: &Path,
) -> Result<DependencySource, DependencyError> {
    if value.is_empty() {
        return Err(DependencyError::Empty {
            name: name.into(),
            field,
        });
    }
    if let Some(rest) = value.strip_prefix("file://") {
        // `file:///abs/path` -> `/abs/path`. A full file-URL decode (percent-encoding, Windows
        // drive letters) can come later; this is enough for the common Unix absolute-path form.
        return Ok(DependencySource::Path(PathBuf::from(rest)));
    }
    if value.starts_with("https://") || value.starts_with("http://") {
        return Ok(DependencySource::Url(value.to_string()));
    }
    if value.contains("://") {
        return Err(DependencyError::UnknownScheme {
            name: name.into(),
            field,
            value: value.to_string(),
        });
    }
    // No scheme: a path relative to the manifest directory (mirrors `classpath_entries`).
    Ok(DependencySource::Path(manifest_dir.join(value)))
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

        // `[dependencies]`: the *structural* shape of each entry — exactly one of `jar` / `git` /
        // `path`, with only that form's fields — is already enforced by serde when the manifest is
        // parsed (the untagged `Dependency` variants, each `deny_unknown_fields`), so a malformed
        // entry never reaches here. What remains are the value-level checks serde cannot express: an
        // empty value, an unsupported URL scheme, conflicting git refs. These are hard errors, like
        // Cargo; runtime I/O failures (a download that fails, a missing local jar / repo) are soft
        // warnings handled later by the host's resolver. The `manifest_dir` is irrelevant to the error
        // cases, so a placeholder is fine.
        for (name, dep) in &self.dependencies {
            if let Some(result) = dep.jar_source(name, Path::new(".")) {
                result.map_err(ValidationError::Dependency)?;
            }
            if let Some(result) = dep.sources_source(name, Path::new(".")) {
                result.map_err(ValidationError::Dependency)?;
            }
            if let Some(result) = dep.source_dependency(name, Path::new(".")) {
                result.map_err(ValidationError::Dependency)?;
            }
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

    /// Classify every **`jar`** `[dependencies]` entry into a host-resolvable [`DependencySource`],
    /// paired with its name (used for cache filenames and diagnostics), separating the ones that
    /// classified from any [`DependencyError`]s. The binary-classpath half of dependency resolution.
    /// Pure — no I/O; the host downloads `Url`s and reads `Path`s.
    ///
    /// Source-form (`git` / `path`) dependencies are **skipped** here — they carry no jar for the
    /// classpath; their `.java` source is collected separately by
    /// [`dependency_source_dirs`](Manifest::dependency_source_dirs). A classification error (a
    /// malformed entry) is still surfaced.
    ///
    /// In practice [`Manifest::validate`] (run by [`Manifest::from_file`]) already rejects the error
    /// cases, so a manifest loaded through `from_file` yields an empty error list; the errors are
    /// surfaced here too for callers that classify a `Manifest` they built or parsed directly.
    pub fn dependency_sources(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>) {
        // `jar_source` is `None` for `git`/`path` source deps (no classpath jar).
        self.collect_dependencies(manifest_dir, Dependency::jar_source)
    }

    /// Classify every `[dependencies]` entry through `classify`, collecting the `Some(Ok)` values
    /// (each paired with its dependency name) and the `Some(Err)`s into separate vectors; a `None`
    /// (a form this `classify` does not apply to) is skipped. The shared spine of
    /// [`dependency_sources`](Manifest::dependency_sources),
    /// [`dependency_source_dirs`](Manifest::dependency_source_dirs), and
    /// [`dependency_source_jars`](Manifest::dependency_source_jars).
    fn collect_dependencies<T>(
        &self,
        manifest_dir: &Path,
        classify: impl Fn(&Dependency, &str, &Path) -> Option<Result<T, DependencyError>>,
    ) -> (Vec<(String, T)>, Vec<DependencyError>) {
        let mut oks = Vec::new();
        let mut errors = Vec::new();
        for (name, dep) in &self.dependencies {
            match classify(dep, name, manifest_dir) {
                Some(Ok(value)) => oks.push((name.clone(), value)),
                Some(Err(err)) => errors.push(err),
                None => {}
            }
        }
        (oks, errors)
    }

    /// Classify every **source-form** (`git` / `path`) `[dependencies]` entry into a host-resolvable
    /// [`SourceDependency`], paired with its name, separating the ones that classified from any
    /// [`DependencyError`]s — the source-tree counterpart of
    /// [`dependency_sources`](Manifest::dependency_sources). `jar` dependencies are skipped (their
    /// classes come from the classpath). Pure (no I/O): the host clones each `git` repo / reads each
    /// `path` directory, locates the `.java` source root, and feeds those sources to the editor for
    /// analysis and go-to-definition — they are never a compile input.
    pub fn dependency_source_dirs(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, SourceDependency)>, Vec<DependencyError>) {
        // `source_dependency` is `None` for a `jar` dependency (its classes come from the classpath).
        self.collect_dependencies(manifest_dir, Dependency::source_dependency)
    }

    /// Classify the **sources** jar of every `[dependencies]` entry that declares one, paired with its
    /// name, separating the ones that classified from any [`DependencyError`]s — the sources-jar
    /// counterpart of [`dependency_sources`](Manifest::dependency_sources). Entries without a `sources`
    /// field are simply absent. Pure (no I/O): the host resolves each [`DependencySource`] to a local
    /// sources jar (downloading remote ones), extracts its `.java`, and feeds those to the editor's
    /// go-to-definition — they are never a compile or analysis input.
    pub fn dependency_source_jars(
        &self,
        manifest_dir: &Path,
    ) -> (Vec<(String, DependencySource)>, Vec<DependencyError>) {
        self.collect_dependencies(manifest_dir, Dependency::sources_source)
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

    /// A `jar`-form dependency with no companion `sources` jar.
    fn jar_dep(jar: &str) -> Dependency {
        Dependency::Jar(JarDependency {
            jar: jar.to_string(),
            sources: None,
        })
    }

    /// A one-entry `[dependencies]` manifest, for the `validate` tests.
    fn manifest_with_dep(name: &str, dep: Dependency) -> Manifest {
        Manifest {
            dependencies: BTreeMap::from([(name.to_string(), dep)]),
            ..Default::default()
        }
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
        // serde picks the `Jar` variant directly from the present fields.
        assert_eq!(
            m.dependencies.get("testlib"),
            Some(&jar_dep("https://example.com/lib.jar"))
        );
        assert_eq!(
            m.dependencies.get("otherlib"),
            Some(&jar_dep("file:///abs/path/lib.jar"))
        );
    }

    #[test]
    fn dependency_jar_classifies_https_as_url() {
        let dep = jar_dep("https://example.com/lib.jar");
        assert_eq!(
            dep.jar_source("testlib", Path::new("/proj")),
            Some(Ok(DependencySource::Url(
                "https://example.com/lib.jar".to_string()
            )))
        );
    }

    #[test]
    fn dependency_jar_classifies_file_url_as_path() {
        let dep = jar_dep("file:///abs/path/lib.jar");
        assert_eq!(
            dep.jar_source("otherlib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/abs/path/lib.jar"
            ))))
        );
    }

    #[test]
    fn dependency_jar_classifies_bare_path_relative_to_manifest_dir() {
        let dep = jar_dep("libs/lib.jar");
        assert_eq!(
            dep.jar_source("locallib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/proj/libs/lib.jar"
            ))))
        );
    }

    #[test]
    fn validate_rejects_empty_jar() {
        let m = manifest_with_dep("bad", jar_dep(""));
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(DependencyError::Empty {
                name: "bad".to_string(),
                field: "jar",
            }))
        );
    }

    #[test]
    fn validate_rejects_unknown_scheme() {
        let m = manifest_with_dep("bad", jar_dep("ftp://example.com/lib.jar"));
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(
                DependencyError::UnknownScheme {
                    name: "bad".to_string(),
                    field: "jar",
                    value: "ftp://example.com/lib.jar".to_string(),
                }
            ))
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
    fn parses_dependency_sources_field() {
        let m: Manifest = toml::from_str(
            r#"
            [dependencies]
            testlib = { jar = "libs/lib.jar", sources = "libs/lib-sources.jar" }
            "#,
        )
        .unwrap();
        assert_eq!(
            m.dependencies.get("testlib"),
            Some(&Dependency::Jar(JarDependency {
                jar: "libs/lib.jar".to_string(),
                sources: Some("libs/lib-sources.jar".to_string()),
            }))
        );
    }

    #[test]
    fn sources_source_is_none_without_a_sources_field() {
        let dep = jar_dep("libs/lib.jar");
        assert_eq!(dep.sources_source("lib", Path::new("/proj")), None);
    }

    #[test]
    fn sources_source_classifies_like_jar() {
        let dep = Dependency::Jar(JarDependency {
            jar: "libs/lib.jar".to_string(),
            sources: Some("https://example.com/lib-sources.jar".to_string()),
        });
        assert_eq!(
            dep.sources_source("lib", Path::new("/proj")),
            Some(Ok(DependencySource::Url(
                "https://example.com/lib-sources.jar".to_string()
            )))
        );

        let local = Dependency::Jar(JarDependency {
            jar: "libs/lib.jar".to_string(),
            sources: Some("libs/lib-sources.jar".to_string()),
        });
        assert_eq!(
            local.sources_source("lib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/proj/libs/lib-sources.jar"
            ))))
        );
    }

    #[test]
    fn validate_rejects_unknown_scheme_in_sources() {
        let m = manifest_with_dep(
            "bad",
            Dependency::Jar(JarDependency {
                jar: "libs/lib.jar".to_string(),
                sources: Some("ftp://example.com/lib-sources.jar".to_string()),
            }),
        );
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(
                DependencyError::UnknownScheme {
                    name: "bad".to_string(),
                    field: "sources",
                    value: "ftp://example.com/lib-sources.jar".to_string(),
                }
            ))
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
    fn parses_git_and_path_dependency_fields() {
        let m: Manifest = toml::from_str(
            r#"
            [dependencies]
            fromgit = { git = "https://github.com/x/y", tag = "v1.2", dir = "core/src/main/java" }
            frompath = { path = "../sibling" }
            "#,
        )
        .unwrap();
        assert_eq!(
            m.dependencies.get("fromgit"),
            Some(&Dependency::Git(GitDependency {
                git: "https://github.com/x/y".to_string(),
                branch: None,
                tag: Some("v1.2".to_string()),
                rev: None,
                dir: Some("core/src/main/java".to_string()),
            }))
        );
        assert_eq!(
            m.dependencies.get("frompath"),
            Some(&Dependency::Path(PathDependency {
                path: "../sibling".to_string(),
                dir: None,
            }))
        );
    }

    #[test]
    fn jar_and_path_forms_resolve() {
        let jar = jar_dep("libs/lib.jar");
        assert_eq!(
            jar.jar_source("lib", Path::new("/proj")),
            Some(Ok(DependencySource::Path(PathBuf::from(
                "/proj/libs/lib.jar"
            ))))
        );
        assert_eq!(jar.source_dependency("lib", Path::new("/proj")), None);

        let path = Dependency::Path(PathDependency {
            path: "../sibling".to_string(),
            dir: Some("src".to_string()),
        });
        assert_eq!(
            path.source_dependency("lib", Path::new("/proj")),
            Some(Ok(SourceDependency::Path(PathSource {
                root: PathBuf::from("/proj/../sibling"),
                dir: Some("src".to_string()),
            })))
        );
        assert_eq!(path.jar_source("lib", Path::new("/proj")), None);
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
        let git_ref = |d: &Dependency| match d.source_dependency("r", Path::new("/proj")) {
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
    fn parse_rejects_multiple_forms() {
        // Co-occurring primary forms match no untagged variant (each `deny_unknown_fields`), so the
        // manifest fails to *parse* — the unification's structural guarantee, in place of the old
        // `validate`-time `MultipleForms`.
        let parsed: Result<Manifest, _> = toml::from_str(
            r#"
            [dependencies]
            bad = { jar = "libs/lib.jar", git = "https://example.com/r.git" }
            "#,
        );
        assert!(parsed.is_err(), "jar + git together must not parse");
    }

    #[test]
    fn parse_rejects_no_form() {
        // An empty entry matches no variant (each requires its primary field).
        let parsed: Result<Manifest, _> = toml::from_str(
            r#"
            [dependencies]
            bad = {}
            "#,
        );
        assert!(parsed.is_err(), "an entry with no form must not parse");
    }

    #[test]
    fn parse_rejects_misplaced_fields() {
        // `branch` only makes sense with `git`: on a `path` entry it matches no variant.
        let on_path: Result<Manifest, _> = toml::from_str(
            r#"
            [dependencies]
            bad = { path = "../sibling", branch = "main" }
            "#,
        );
        assert!(on_path.is_err(), "branch on a path dep must not parse");

        // `sources` only makes sense with `jar`: on a `git` entry it matches no variant.
        let on_git: Result<Manifest, _> = toml::from_str(
            r#"
            [dependencies]
            bad = { git = "https://example.com/r.git", sources = "whatever.jar" }
            "#,
        );
        assert!(on_git.is_err(), "sources on a git dep must not parse");
    }

    #[test]
    fn validate_rejects_conflicting_git_refs() {
        // branch + tag both parse as valid `GitDependency` fields; the at-most-one-ref rule is a
        // value-level check, surfaced by `validate` (and `source_dependency`).
        let m = manifest_with_dep(
            "r",
            Dependency::Git(GitDependency {
                git: "https://example.com/r.git".to_string(),
                branch: Some("main".to_string()),
                tag: Some("v1".to_string()),
                rev: None,
                dir: None,
            }),
        );
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(
                DependencyError::ConflictingGitRef {
                    name: "r".to_string()
                }
            ))
        );
    }

    #[test]
    fn validate_accepts_git_and_path_forms() {
        let m: Manifest = toml::from_str(
            r#"
            [dependencies]
            g = { git = "https://github.com/x/y", rev = "deadbeef" }
            p = { path = "../sibling" }
            "#,
        )
        .unwrap();
        assert_eq!(m.validate(), Ok(()));
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
