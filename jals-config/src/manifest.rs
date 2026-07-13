//! Project manifest, deserialized from `jals.toml`.
//!
//! A `jals.toml` describes a Java project the way `Cargo.toml` describes a Rust crate. Every key is
//! optional; omitted keys fall back to [`Manifest::default`], which encodes the Maven-style
//! `src/main/java` -> `target/classes` layout. Keys are kebab-case and grouped into `[package]`,
//! `[build]`, and `[run]` sections.
//!
//! This module owns the pure, `no_std` half: the serde model, structural [`validate`](Manifest::validate),
//! the [`FromStr`] parse-from-text entry point, and the pure classified type [`GitRef`]. The host-only,
//! `std::path`-based resolution (classpath / source-root / dependency-source / invocation / scaffold) lives
//! in `jals-build`'s `ManifestExt`.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
use core::str::FromStr;

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
    /// `[run] main-class`; otherwise the run target is selected from these (see `jals-build`'s
    /// `resolve_run_target`).
    pub bin: Vec<Bin>,
    /// External dependencies (`[dependencies]`), keyed by name (the Java analogue of Cargo's
    /// `[dependencies]`). A `BTreeMap` so iteration order is deterministic — the resolved classpath
    /// and any diagnostics come out in a stable order. Each value is a [`Dependency`] — one of the
    /// `jar` / `git` / `path` forms, chosen by serde at parse time. The host (`jals-cli`/`jals-lsp`)
    /// resolves each entry — downloading `jar`s onto the classpath, cloning/reading `git`/`path`
    /// source into the editor index; this crate only models and validates the specs, staying pure
    /// (`jals-build`'s `ManifestExt` classifies them into host-facing path sources).
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
/// the latter two contribute `.java` source for analysis + navigation only. The value-level checks
/// serde cannot express (empty values, URL scheme, at-most-one git ref) are applied by
/// [`Dependency::validate`]; `jals-build`'s `ManifestExt` classifies the raw values into host-facing
/// path sources.
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
    /// here (see `jals-build`'s `ManifestExt`).
    pub jar: String,
    /// An optional companion **sources** `.jar` (the `-sources.jar` of Maven convention), located the
    /// same way as [`jar`](JarDependency::jar) (URL / `file://` / bare path). It carries the library's
    /// `.java` sources, used only for editor navigation (go-to-definition into the real source); it is
    /// never a compile or analysis input. Resolved by the host.
    pub sources: Option<String>,
    /// Whether to recursively unpack the jar's **bundled jars** (`*.jar` members nested inside it, as in
    /// a Spring-Boot-style fat jar's `BOOT-INF/lib/*.jar`) onto the classpath. With `recursive = true`
    /// the host extracts every nested jar — at any depth — and adds them as classpath entries, so the
    /// bundled libraries are loaded for both compilation and analysis; the default (`None`/`false`) reads
    /// only the jar's own top-level `.class` files. A bundled-jar-less jar is unaffected.
    pub recursive: Option<bool>,
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

/// Which commit of a git dependency to check out: the default branch, or a named branch / tag / commit.
///
/// The pure classification of a [`GitDependency`]'s `branch` / `tag` / `rev` (see
/// [`GitDependency::git_ref`]); `jals-build`'s `GitSource` pairs it with the resolved clone URL.
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
            Self::Default => None,
            Self::Branch(b) | Self::Tag(b) | Self::Rev(b) => Some(b),
        }
    }
}

/// A `[dependencies]` entry whose value could not be classified.
///
/// Found by [`Dependency::validate`] (and by `jals-build`'s `ManifestExt` classifiers). Carries the
/// dependency name for an actionable message.
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
            Self::Empty { name, field } => {
                write!(f, "dependency `{name}` has an empty `{field}`")
            }
            Self::UnknownScheme { name, field, value } => write!(
                f,
                "dependency `{name}` has an unsupported `{field}` URL scheme `{value}` \
                 (expected `https://`, `http://`, `file://`, or a path)"
            ),
            Self::ConflictingGitRef { name } => write!(
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
    /// The Java language edition the project targets (Cargo's `[package] edition`).
    ///
    /// Purely a *language-feature gate* for analysis (the linter / LSP): it does not affect
    /// compilation — the `javac` version knobs remain `[build] release`/`source`/`target`. When
    /// unset, no edition-gated feature is flagged. See [`Edition`].
    pub edition: Option<Edition>,
    /// The Java language system (platform implementation) the project targets (Cargo's
    /// `[package] rust-version` slot). See [`JavaVersion`].
    pub java_version: Option<JavaVersion>,
}

/// The Java language edition a project targets (`[package] edition`).
///
/// Values are the Java feature releases (`java8`–`java25`) whose syntax `jals` targets. It drives
/// language-feature gating in the linter (e.g. compact source files with a top-level `main` are a
/// preview feature in Java 24 but a permanent feature in Java 25); it is *not* passed to `javac`.
///
/// The set is a closed enum rather than a free numeric parse so that non-release notations (e.g. a
/// jals-specific `java25+jals` dialect) can later join as variants with an explicit
/// `#[serde(rename = "…")]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edition {
    /// Java 8.
    Java8,
    /// Java 9.
    Java9,
    /// Java 10.
    Java10,
    /// Java 11.
    Java11,
    /// Java 12.
    Java12,
    /// Java 13.
    Java13,
    /// Java 14.
    Java14,
    /// Java 15.
    Java15,
    /// Java 16.
    Java16,
    /// Java 17.
    Java17,
    /// Java 18.
    Java18,
    /// Java 19.
    Java19,
    /// Java 20.
    Java20,
    /// Java 21.
    Java21,
    /// Java 22.
    Java22,
    /// Java 23.
    Java23,
    /// Java 24, where compact source files / instance main methods are still a preview feature.
    Java24,
    /// Java 25, where compact source files / instance main methods are a permanent feature.
    Java25,
}

impl Edition {
    /// The Java feature-release version this edition targets (e.g. `24` for [`Edition::Java24`]).
    pub const fn feature_version(self) -> u32 {
        match self {
            Self::Java8 => 8,
            Self::Java9 => 9,
            Self::Java10 => 10,
            Self::Java11 => 11,
            Self::Java12 => 12,
            Self::Java13 => 13,
            Self::Java14 => 14,
            Self::Java15 => 15,
            Self::Java16 => 16,
            Self::Java17 => 17,
            Self::Java18 => 18,
            Self::Java19 => 19,
            Self::Java20 => 20,
            Self::Java21 => 21,
            Self::Java22 => 22,
            Self::Java23 => 23,
            Self::Java24 => 24,
            Self::Java25 => 25,
        }
    }
}

/// The Java language system a project targets (`[package] java-version`).
///
/// Which platform implementation the project is written against, next to the syntax version in
/// [`Package::edition`] — the split Cargo makes between `edition` and `rust-version`.
///
/// Currently parsed and threaded through to the assembled project inputs only; no analysis behaves
/// differently yet. It reserves the seam for system-dependent analysis (e.g. gating lints on the
/// API subset a `teavm` target actually supports). An unknown value is a TOML parse error (serde
/// unknown variant), like an unknown [`Edition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JavaVersion {
    /// The Oracle JDK.
    OracleJdk,
    /// `OpenJDK`.
    OpenJdk,
    /// `TeaVM` (Java compiled to JavaScript / `WebAssembly`).
    TeaVm,
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
    /// selected from the bins instead (see `jals-build`'s `resolve_run_target`).
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
        Self {
            source_dirs: alloc::vec!["src/main/java".to_owned()],
            classes_dir: "target/classes".to_owned(),
            release: None,
            source: None,
            target: None,
            classpath: Vec::new(),
            javac_flags: Vec::new(),
        }
    }
}

impl Dependency {
    /// Validate a jar-location string without building any path.
    ///
    /// The string is a `jar` or `sources` value: non-empty and, if it carries a URL scheme, a known
    /// one (`file` / `https` / `http`). This is the value-level check serde cannot express, shared by
    /// [`Dependency::validate`] and `jals-build`'s classpath classifier (which reuses it before
    /// resolving the value to a `PathBuf` / URL). `field` names the source field for error messages.
    ///
    /// # Errors
    /// [`DependencyError::Empty`] for an empty value, [`DependencyError::UnknownScheme`] for a URL
    /// with an unrecognised scheme.
    pub fn validate_jar_location(
        value: &str,
        name: &str,
        field: &'static str,
    ) -> Result<(), DependencyError> {
        if value.is_empty() {
            return Err(DependencyError::Empty {
                name: name.to_owned(),
                field,
            });
        }
        if value.starts_with("file://")
            || value.starts_with("https://")
            || value.starts_with("http://")
        {
            return Ok(());
        }
        if value.contains("://") {
            return Err(DependencyError::UnknownScheme {
                name: name.to_owned(),
                field,
                value: value.to_owned(),
            });
        }
        // No scheme: a bare (manifest-relative) path — always valid at this layer.
        Ok(())
    }

    /// Apply the value-level checks serde cannot express, without any I/O or path building: a `jar`'s
    /// `jar` (and optional `sources`) is a non-empty known-scheme location, a `git`'s URL is non-empty
    /// with at most one `branch` / `tag` / `rev`, and a `path`'s directory is non-empty. `name` labels
    /// errors.
    ///
    /// # Errors
    /// Returns the first [`DependencyError`] found (empty value, unsupported URL scheme, or conflicting
    /// git refs).
    pub fn validate(&self, name: &str) -> Result<(), DependencyError> {
        match self {
            Self::Jar(jar) => {
                Self::validate_jar_location(&jar.jar, name, "jar")?;
                if let Some(sources) = &jar.sources {
                    Self::validate_jar_location(sources, name, "sources")?;
                }
                Ok(())
            }
            Self::Git(git) => {
                if git.git.is_empty() {
                    return Err(DependencyError::Empty {
                        name: name.to_owned(),
                        field: "git",
                    });
                }
                git.git_ref(name).map(|_| ())
            }
            Self::Path(path) => {
                if path.path.is_empty() {
                    return Err(DependencyError::Empty {
                        name: name.to_owned(),
                        field: "path",
                    });
                }
                Ok(())
            }
        }
    }
}

impl GitDependency {
    /// Classify the pinned commit from at most one of `branch` / `tag` / `rev`. Pure — [`GitRef`]
    /// holds only strings; `jals-build`'s `GitSource` pairs the result with the clone URL.
    ///
    /// # Errors
    /// [`DependencyError::ConflictingGitRef`] when more than one is set, [`DependencyError::Empty`]
    /// when the one set is empty.
    pub fn git_ref(&self, name: &str) -> Result<GitRef, DependencyError> {
        // Validate the one ref that is set, rejecting an empty value. Matching the tuple of options
        // directly picks the variant in one step — no separate "how many are set?" count and no second
        // match on a field name to re-derive which kind it is.
        let non_empty = |value: &str, field| {
            (!value.is_empty())
                .then(|| value.to_owned())
                .ok_or_else(|| DependencyError::Empty {
                    name: name.to_owned(),
                    field,
                })
        };
        match (
            self.branch.as_deref(),
            self.tag.as_deref(),
            self.rev.as_deref(),
        ) {
            (None, None, None) => Ok(GitRef::Default),
            (Some(branch), None, None) => Ok(GitRef::Branch(non_empty(branch, "branch")?)),
            (None, Some(tag), None) => Ok(GitRef::Tag(non_empty(tag, "tag")?)),
            (None, None, Some(rev)) => Ok(GitRef::Rev(non_empty(rev, "rev")?)),
            _ => Err(DependencyError::ConflictingGitRef {
                name: name.to_owned(),
            }),
        }
    }
}

impl Manifest {
    /// The synthetic manifest name used for error reporting when parsing from text (no real path).
    pub(crate) const IN_MEMORY_NAME: &'static str = "jals.toml";

    /// Structurally validate the manifest, independent of any filesystem (pure, so it stays
    /// `wasm32`-compatible and is called by the [`FromStr`] impl right after parsing, and by
    /// `jals-build`'s `ManifestExt::from_file`).
    ///
    /// Checks the `[[bin]]` table: every bin needs a non-empty `name` and `main-class`, names must
    /// be unique, and `[package] default-run` (when set) must name a declared bin. Also applies each
    /// `[dependencies]` entry's value-level checks (see [`Dependency::validate`]).
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
        // warnings handled later by the host's resolver.
        for (name, dep) in &self.dependencies {
            dep.validate(name).map_err(ValidationError::Dependency)?;
        }

        Ok(())
    }

    /// The project's target Java feature version (from `[package] edition`), if declared. The single
    /// projection of the manifest's [`Edition`] into the bare feature release the analysis layers
    /// consume — the host feeds it to the edition-gated lint rules (e.g. `compact-source-file`) as
    /// the lint config's target version; `None` when no edition is declared, leaving those gates off.
    pub fn target_java_version(&self) -> Option<u32> {
        self.package.edition.map(Edition::feature_version)
    }

    /// The project's declared Java language system (from `[package] java-version`), if any. Threaded
    /// through to the assembled project inputs; no analysis consumes it yet (see [`JavaVersion`]).
    pub const fn java_version(&self) -> Option<JavaVersion> {
        self.package.java_version
    }

    /// The names of every `jar` `[dependencies]` entry that opted into recursive **bundled-jar**
    /// unpacking (`recursive = true`). The host pairs these names with the resolved jars to decide
    /// which jars to scan for nested `*.jar` members. Pure — no I/O; only the `jar` form carries
    /// `recursive`, so `git`/`path` entries are never present.
    pub fn recursive_jar_dependencies(&self) -> BTreeSet<&str> {
        self.dependencies
            .iter()
            .filter_map(|(name, dep)| match dep {
                Dependency::Jar(jar) if jar.recursive == Some(true) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Parse and validate a manifest from its TOML text, with no filesystem access — the
/// `wasm32`-friendly entry point for hosts that already hold the text (e.g. the browser playground
/// parsing a `jals.toml` editor buffer, or `jals-build`'s `ManifestExt::from_file` after reading the
/// file). Parse / validation errors are keyed to a synthetic `jals.toml` name (there is no real path).
impl FromStr for Manifest {
    type Err = ManifestParseError;

    fn from_str(text: &str) -> Result<Self, ManifestParseError> {
        let manifest: Self = toml::from_str(text).map_err(|source| ManifestParseError::Parse {
            path: Self::IN_MEMORY_NAME.to_owned(),
            source,
        })?;
        manifest
            .validate()
            .map_err(|source| ManifestParseError::Invalid {
                path: Self::IN_MEMORY_NAME.to_owned(),
                source,
            })?;
        Ok(manifest)
    }
}

/// An error parsing or validating a manifest from text.
///
/// `no_std`: it holds a rendered path `String` and wraps [`toml::de::Error`] (the parse failure) or
/// [`ValidationError`] (the structural failure). `jals-build`'s host-side `ManifestError` re-stamps
/// these with the real `PathBuf` and adds the `std::io` read failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestParseError {
    /// The text contained invalid TOML.
    Parse {
        /// The path (or synthetic name) that failed to parse.
        path: String,
        /// The underlying parse error.
        source: toml::de::Error,
    },
    /// The text parsed but is structurally invalid (see [`ValidationError`]).
    Invalid {
        /// The path (or synthetic name) that failed validation.
        path: String,
        /// The validation failure.
        source: ValidationError,
    },
}

impl fmt::Display for ManifestParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { path, source } => {
                write!(f, "failed to parse manifest {path}: {source}")
            }
            Self::Invalid { path, source } => {
                write!(f, "invalid manifest {path}: {source}")
            }
        }
    }
}

impl Error for ManifestParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse { source, .. } => Some(source),
            Self::Invalid { source, .. } => Some(source),
        }
    }
}

/// A structural problem in a manifest, found by [`Manifest::validate`] (independent of the file it
/// came from — the host [`ManifestParseError`] / `jals-build`'s `ManifestError` add the path).
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
    /// or conflicting git refs. Wraps the classification [`DependencyError`] so the two layers share a
    /// single message and the variant set never drifts apart.
    Dependency(DependencyError),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateBin { name } => {
                write!(f, "duplicate `[[bin]]` name `{name}`")
            }
            Self::UnknownDefaultRun { name, available } => write!(
                f,
                "`[package] default-run` is `{name}`, which is not a declared bin (available: {})",
                available.join(", ")
            ),
            Self::EmptyBinField { field } => {
                write!(f, "a `[[bin]]` has an empty `{field}`")
            }
            Self::Dependency(err) => write!(f, "{err}"),
        }
    }
}

impl Error for ValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Dependency(err) => Some(err),
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
        assert_eq!(m.build.source_dirs, alloc::vec!["src/main/java".to_owned()]);
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
            alloc::vec!["src/main/java".to_owned(), "generated".to_owned()]
        );
        assert_eq!(m.build.classes_dir, "out");
        assert_eq!(m.build.release, Some(21));
        assert_eq!(m.build.classpath, alloc::vec!["libs/guava.jar".to_owned()]);
        assert_eq!(m.build.javac_flags, alloc::vec!["-Xlint:all".to_owned()]);
        assert_eq!(m.run.main_class.as_deref(), Some("com.example.Main"));
        // No `[[bin]]`: the bin list is empty and selection falls back to `[run] main-class`.
        assert!(m.bin.is_empty());
        assert_eq!(m.package.default_run, None);
        // No `[package] edition`: it is absent, disabling edition-gated feature checks.
        assert_eq!(m.package.edition, None);
        // No `[package] java-version` either.
        assert_eq!(m.package.java_version, None);
    }

    #[test]
    fn parses_package_edition() {
        let m: Manifest = toml::from_str("[package]\nedition = \"java8\"\n").unwrap();
        assert_eq!(m.package.edition, Some(Edition::Java8));
        assert_eq!(m.package.edition.unwrap().feature_version(), 8);

        let m: Manifest = toml::from_str("[package]\nedition = \"java24\"\n").unwrap();
        assert_eq!(m.package.edition, Some(Edition::Java24));
        assert_eq!(m.package.edition.unwrap().feature_version(), 24);

        let m: Manifest = toml::from_str("[package]\nedition = \"java25\"\n").unwrap();
        assert_eq!(m.package.edition, Some(Edition::Java25));
        assert_eq!(m.package.edition.unwrap().feature_version(), 25);
    }

    #[test]
    fn rejects_unknown_edition() {
        // An edition outside the modelled set is a TOML parse error (serde unknown variant), so no
        // dedicated `validate` check is needed.
        assert!(toml::from_str::<Manifest>("[package]\nedition = \"java7\"\n").is_err());
        assert!(toml::from_str::<Manifest>("[package]\nedition = \"java26\"\n").is_err());
    }

    #[test]
    fn parses_package_java_version() {
        let m: Manifest = toml::from_str("[package]\njava-version = \"oraclejdk\"\n").unwrap();
        assert_eq!(m.package.java_version, Some(JavaVersion::OracleJdk));

        let m: Manifest = toml::from_str("[package]\njava-version = \"openjdk\"\n").unwrap();
        assert_eq!(m.package.java_version, Some(JavaVersion::OpenJdk));
        assert_eq!(m.java_version(), Some(JavaVersion::OpenJdk));

        let m: Manifest = toml::from_str("[package]\njava-version = \"teavm\"\n").unwrap();
        assert_eq!(m.package.java_version, Some(JavaVersion::TeaVm));
    }

    #[test]
    fn rejects_unknown_java_version() {
        // Like `edition`, an unknown language system is a TOML parse error (serde unknown
        // variant), so no dedicated `validate` check is needed.
        assert!(toml::from_str::<Manifest>("[package]\njava-version = \"graalvm\"\n").is_err());
    }

    #[test]
    fn partial_manifest_falls_back_to_defaults() {
        // Only [package].name is given; the absent [build]/[run] tables keep their defaults, and a
        // present-but-partial table fills missing keys from the struct's Default (serde container
        // `default`).
        let m: Manifest = toml::from_str("[package]\nname = \"x\"\n").unwrap();
        assert_eq!(m.package.name.as_deref(), Some("x"));
        assert_eq!(m.build.source_dirs, alloc::vec!["src/main/java".to_owned()]);
        assert_eq!(m.build.classes_dir, "target/classes");
        assert_eq!(m.run.main_class, None);
        assert!(m.bin.is_empty());
    }

    #[test]
    fn source_and_target_without_release() {
        let m: Manifest = toml::from_str("[build]\nsource = 17\ntarget = 17\n").unwrap();
        assert_eq!(m.build.release, None);
        assert_eq!(m.build.source, Some(17));
        assert_eq!(m.build.target, Some(17));
        // The omitted keys still come from the Maven default.
        assert_eq!(m.build.source_dirs, alloc::vec!["src/main/java".to_owned()]);
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

    #[test]
    fn validate_accepts_unique_bins_and_valid_default_run() {
        let mut m = manifest_with_bins(alloc::vec![bin("a", "A"), bin("b", "B")]);
        m.package.default_run = Some("b".to_owned());
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_duplicate_bin_names() {
        let m = manifest_with_bins(alloc::vec![bin("dup", "A"), bin("dup", "B")]);
        assert_eq!(
            m.validate(),
            Err(ValidationError::DuplicateBin {
                name: "dup".to_owned()
            })
        );
    }

    #[test]
    fn validate_rejects_unknown_default_run() {
        let mut m = manifest_with_bins(alloc::vec![bin("a", "A")]);
        m.package.default_run = Some("ghost".to_owned());
        assert_eq!(
            m.validate(),
            Err(ValidationError::UnknownDefaultRun {
                name: "ghost".to_owned(),
                available: alloc::vec!["a".to_owned()],
            })
        );
    }

    #[test]
    fn validate_rejects_empty_bin_fields() {
        let m = manifest_with_bins(alloc::vec![bin("", "A")]);
        assert_eq!(
            m.validate(),
            Err(ValidationError::EmptyBinField { field: "name" })
        );

        let m = manifest_with_bins(alloc::vec![bin("a", "")]);
        assert_eq!(
            m.validate(),
            Err(ValidationError::EmptyBinField {
                field: "main-class"
            })
        );
    }

    /// A `jar`-form dependency with no companion `sources` jar and no bundled-jar recursion.
    fn jar_dep(jar: &str) -> Dependency {
        Dependency::Jar(JarDependency {
            jar: jar.to_owned(),
            sources: None,
            recursive: None,
        })
    }

    /// A one-entry `[dependencies]` manifest, for the `validate` tests.
    fn manifest_with_dep(name: &str, dep: Dependency) -> Manifest {
        Manifest {
            dependencies: BTreeMap::from([(name.to_owned(), dep)]),
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
    fn validate_rejects_empty_jar() {
        let m = manifest_with_dep("bad", jar_dep(""));
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(DependencyError::Empty {
                name: "bad".to_owned(),
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
                    name: "bad".to_owned(),
                    field: "jar",
                    value: "ftp://example.com/lib.jar".to_owned(),
                }
            ))
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
                jar: "libs/lib.jar".to_owned(),
                sources: Some("libs/lib-sources.jar".to_owned()),
                recursive: None,
            }))
        );
    }

    #[test]
    fn validate_rejects_unknown_scheme_in_sources() {
        let m = manifest_with_dep(
            "bad",
            Dependency::Jar(JarDependency {
                jar: "libs/lib.jar".to_owned(),
                sources: Some("ftp://example.com/lib-sources.jar".to_owned()),
                recursive: None,
            }),
        );
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(
                DependencyError::UnknownScheme {
                    name: "bad".to_owned(),
                    field: "sources",
                    value: "ftp://example.com/lib-sources.jar".to_owned(),
                }
            ))
        );
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
                git: "https://github.com/x/y".to_owned(),
                branch: None,
                tag: Some("v1.2".to_owned()),
                rev: None,
                dir: Some("core/src/main/java".to_owned()),
            }))
        );
        assert_eq!(
            m.dependencies.get("frompath"),
            Some(&Dependency::Path(PathDependency {
                path: "../sibling".to_owned(),
                dir: None,
            }))
        );
    }

    #[test]
    fn git_refs_classify() {
        let make = |branch, tag, rev| GitDependency {
            git: "https://example.com/r.git".to_owned(),
            branch,
            tag,
            rev,
            dir: None,
        };
        assert_eq!(make(None, None, None).git_ref("r"), Ok(GitRef::Default));
        assert_eq!(
            make(Some("main".to_owned()), None, None).git_ref("r"),
            Ok(GitRef::Branch("main".to_owned()))
        );
        assert_eq!(
            make(None, Some("v1".to_owned()), None).git_ref("r"),
            Ok(GitRef::Tag("v1".to_owned()))
        );
        assert_eq!(
            make(None, None, Some("abc123".to_owned())).git_ref("r"),
            Ok(GitRef::Rev("abc123".to_owned()))
        );
        // More than one ref set is a conflict.
        assert_eq!(
            make(Some("m".to_owned()), Some("v".to_owned()), None).git_ref("r"),
            Err(DependencyError::ConflictingGitRef {
                name: "r".to_owned()
            })
        );
    }

    #[test]
    fn parse_rejects_multiple_forms() {
        // Co-occurring primary forms match no untagged variant (each `deny_unknown_fields`), so the
        // manifest fails to *parse* — the unification's structural guarantee.
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
            r"
            [dependencies]
            bad = {}
            ",
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

        // `recursive` is a `jar`-only flag: on a `git` entry it matches no variant.
        let recursive_on_git: Result<Manifest, _> = toml::from_str(
            r#"
            [dependencies]
            bad = { git = "https://example.com/r.git", recursive = true }
            "#,
        );
        assert!(
            recursive_on_git.is_err(),
            "recursive on a git dep must not parse"
        );
    }

    #[test]
    fn parses_recursive_flag() {
        let m: Manifest = toml::from_str(
            r#"
            [dependencies]
            fat = { jar = "libs/fat.jar", recursive = true }
            "#,
        )
        .unwrap();
        assert_eq!(
            m.dependencies.get("fat"),
            Some(&Dependency::Jar(JarDependency {
                jar: "libs/fat.jar".to_owned(),
                sources: None,
                recursive: Some(true),
            }))
        );
    }

    #[test]
    fn recursive_jar_dependencies_collects_flagged() {
        let m = Manifest {
            dependencies: BTreeMap::from([
                (
                    "fat".to_owned(),
                    Dependency::Jar(JarDependency {
                        jar: "libs/fat.jar".to_owned(),
                        sources: None,
                        recursive: Some(true),
                    }),
                ),
                // A plain jar (no flag) and an explicit `recursive = false` are both excluded.
                ("plain".to_owned(), jar_dep("libs/plain.jar")),
                (
                    "off".to_owned(),
                    Dependency::Jar(JarDependency {
                        jar: "libs/off.jar".to_owned(),
                        sources: None,
                        recursive: Some(false),
                    }),
                ),
                // `git`/`path` forms never carry the flag.
                (
                    "src".to_owned(),
                    Dependency::Path(PathDependency {
                        path: "../sibling".to_owned(),
                        dir: None,
                    }),
                ),
            ]),
            ..Default::default()
        };
        assert_eq!(
            m.recursive_jar_dependencies(),
            BTreeSet::from(["fat"]),
            "only the `recursive = true` jar dep is collected"
        );
    }

    #[test]
    fn validate_rejects_conflicting_git_refs() {
        // branch + tag both parse as valid `GitDependency` fields; the at-most-one-ref rule is a
        // value-level check, surfaced by `validate`.
        let m = manifest_with_dep(
            "r",
            Dependency::Git(GitDependency {
                git: "https://example.com/r.git".to_owned(),
                branch: Some("main".to_owned()),
                tag: Some("v1".to_owned()),
                rev: None,
                dir: None,
            }),
        );
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(
                DependencyError::ConflictingGitRef {
                    name: "r".to_owned()
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
    fn from_str_parses_and_validates() {
        // The `FromStr` entry point (used by the playground and `ManifestExt::from_file`) parses then
        // validates, keying errors to the synthetic name.
        let ok: Manifest = "[package]\nname = \"x\"\n".parse().unwrap();
        assert_eq!(ok.package.name.as_deref(), Some("x"));

        let invalid = "[[bin]]\nname = \"\"\nmain-class = \"X\"\n".parse::<Manifest>();
        assert!(matches!(invalid, Err(ManifestParseError::Invalid { .. })));

        let bad_toml = "not = = toml".parse::<Manifest>();
        assert!(matches!(bad_toml, Err(ManifestParseError::Parse { .. })));
    }
}
