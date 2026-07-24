//! Project manifest, deserialized from `jals.toml`.
//!
//! A `jals.toml` describes a Java project the way `Cargo.toml` describes a Rust crate. Every key is
//! optional; omitted keys fall back to [`Manifest::default`], which encodes the Maven-style
//! `src/main/java` -> `target/classes` layout. Keys are kebab-case and grouped into `[package]`,
//! `[features]`, `[build]`, `[run]`, and `[toolchain]` sections.
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

use jals_storage::{DirKey, FileKey};
use serde::Deserialize;

use crate::toolchain::Toolchain;

const MANAGED_BUILD_ROOT: &str = "target/jals/build";

/// The reserved `[features]` key whose list is the build-feature set enabled when the command
/// line selects none (Cargo's `default` feature). A resolution directive, never itself a queryable
/// feature — [`Manifest::resolve_build_features`] expands it and drops the name from the result.
///
/// Reserved *everywhere* a feature can be written, not just in a `[features]` table:
/// [`Dependency::validate_features`] rejects it in a `[dependencies] features` list too, and
/// [`FeatureRef::parse`] rejects `dep/default`, so the name can never reach a resolved set by any
/// route. Whether a dependency's own `default` list applies is said with
/// [`default-features`](Dependency::default_features), never by naming the directive.
const DEFAULT_BUILD_FEATURE: &str = "default";

/// The separator of the cross-package `<dependency>/<feature>` form (Cargo's `serde/std`).
///
/// Reserved in a feature *name*: a `[features]` key carrying it would be unreachable, since every
/// list entry containing it is read as a cross-package reference instead (see [`FeatureRef`]).
const FEATURE_SEPARATOR: char = '/';

/// A parsed `jals.toml` project manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Manifest {
    /// Project metadata (`[package]`). Informational for now.
    pub package: Package,
    /// Cargo-style **build features** (`[features]`): named, additive build-time toggles a build
    /// script reads to vary what it produces.
    ///
    /// Top-level like Cargo's `[features]`, and distinct from `[package] features` (the closed
    /// [`Feature`] enum, a language-*analysis* gate): these are open-ended, user-defined names
    /// selected on the command line (`--features` / `--all-features` / `--no-default-features`) and
    /// queried by the build script via `build.feature("…")`. Each key maps to the other features it
    /// enables (the Cargo `feature = ["…"]` form); the reserved `default` key lists the features
    /// enabled when none are selected. Resolved additively — closed under the enables map — by
    /// [`Manifest::resolve_build_features`]. A `default`/`enables` name that is not itself a
    /// declared key is a [`ValidationError::UndeclaredBuildFeature`]. Empty when omitted, so an
    /// existing manifest keeps building exactly as before.
    ///
    /// A list entry may also be Cargo's cross-package `<dependency>/<feature>` form (see
    /// [`FeatureRef`]): enabling the declaring feature then enables `<feature>` in the
    /// `[dependencies]` entry `<dependency>`. Such an entry is *routed*, never queryable — it is
    /// absent from this project's own resolved set, and
    /// [`ResolvedBuildFeatures::dependencies`] is where it comes out. The named dependency must be
    /// a declared `git`/`path` entry: a `jar` runs no build script that could read the feature.
    ///
    /// A feature reaches a dependency by exactly two routes — a
    /// [`features`](Dependency::features) list on the `[dependencies]` entry, and a cross-package
    /// entry here — and both are written in *this* manifest. The declaring project's own selection
    /// never leaks implicitly.
    pub features: BTreeMap<String, Vec<String>>,
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
    /// Toolchain selection (`[toolchain]`): which `javac` compiles the project and which `java` runs
    /// it, chosen independently (see [`Toolchain`] and its [`Compiler`](crate::Compiler) /
    /// [`Runtime`](crate::Runtime) enums). Defaults to the system tools when omitted, so an existing
    /// manifest is unaffected. This crate only models the selection; `jals-build` matches each enum
    /// to a backend, and its `native` feature resolves the [`ToolSpec`](crate::ToolSpec) view of a
    /// program-selecting variant to a program path (JDK discovery / `PATH`).
    pub toolchain: Toolchain,
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
/// Both source forms also carry [`features`](Dependency::features) — the build features to enable in
/// that dependency, Cargo's per-dependency `features = ["…"]` — and
/// [`default-features`](Dependency::default_features), which suppresses that dependency's own
/// `[features] default` list. A `jar` has no build script, so neither field exists on that form and
/// writing one is a parse error like any misplaced field.
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
    /// The **build features** to enable in this dependency (Cargo's per-dependency `features`). See
    /// [`Dependency::features`] for the semantics, which are the same for both source forms.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether this dependency resolves its own `[features] default` list (Cargo's
    /// `default-features`). See [`Dependency::default_features`], which reads the `None` default.
    #[serde(default)]
    pub default_features: Option<bool>,
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
    /// The **build features** to enable in this dependency (Cargo's per-dependency `features`). See
    /// [`Dependency::features`] for the semantics, which are the same for both source forms.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether this dependency resolves its own `[features] default` list (Cargo's
    /// `default-features`). See [`Dependency::default_features`], which reads the `None` default.
    #[serde(default)]
    pub default_features: Option<bool>,
}

/// Which commit of a git dependency to check out: the default branch, or a named branch / tag / commit.
///
/// The pure classification of a [`GitDependency`]'s `branch` / `tag` / `rev` (see
/// [`GitDependency::git_ref`]); `jals-classpath`'s native plan pairs it with the clone URL.
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
    /// A `git` field's value starts with `-`, so the `git` CLI would read it as an option rather
    /// than a URL or ref.
    OptionLike {
        /// The dependency's name.
        name: String,
        /// Which field carried the bad value (`"git"`, `"branch"`, `"tag"`, or `"rev"`).
        field: &'static str,
        /// The offending value.
        value: String,
    },
    /// A `features` list names the reserved `default` feature, which is a declaring table's
    /// resolution directive and never a queryable feature (see [`Dependency::validate_features`]).
    ReservedFeature {
        /// The dependency's name.
        name: String,
        /// The reserved feature name that was listed.
        feature: String,
    },
    /// A `features` list names a cross-package `<dependency>/<feature>` reference, which belongs in
    /// a `[features]` table (see [`Dependency::validate_features`]).
    CrossFeature {
        /// The dependency's name.
        name: String,
        /// The offending entry, as written.
        feature: String,
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
            Self::OptionLike { name, field, value } => write!(
                f,
                "git dependency `{name}` has a `{field}` starting with `-` (`{value}`), which the \
                 `git` CLI would read as an option rather than a value"
            ),
            Self::ReservedFeature { name, feature } => write!(
                f,
                "dependency `{name}` lists the reserved feature `{feature}` in `features` \
                 (`{feature}` is a `[features]` resolution directive, never a queryable feature; \
                 use `default-features`)"
            ),
            Self::CrossFeature { name, feature } => write!(
                f,
                "dependency `{name}` lists `{feature}` in `features`, which names features of \
                 `{name}` itself (write a `<dependency>/<feature>` reference in `[features]`)"
            ),
        }
    }
}

impl Error for DependencyError {}

/// One entry of a build-feature list, classified: a feature of the package that wrote the list, or
/// Cargo's cross-package `<dependency>/<feature>` form (`serde/std`).
///
/// The single place the `/` form is read. Every list that can carry one — a `[features]` value and
/// the command-line selection — classifies through [`parse`](FeatureRef::parse), so the shape rules
/// cannot drift apart between where a manifest is validated and where a selection is resolved.
/// A `[dependencies] features` list is deliberately *not* one of them: those name features of that
/// dependency only, as in Cargo, and [`Dependency::validate_features`] rejects a `/` outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureRef<'a> {
    /// A feature of the package whose list this is.
    Local(&'a str),
    /// `<dependency>/<feature>`: enable `feature` in the `[dependencies]` entry `dependency`.
    Dependency {
        /// The `[dependencies]` key the feature is enabled in.
        dependency: &'a str,
        /// The feature to enable there.
        feature: &'a str,
    },
}

impl<'a> FeatureRef<'a> {
    /// Classify one feature-list entry by its shape alone.
    ///
    /// Pure name analysis: whether the named dependency exists is a whole-manifest question
    /// ([`Manifest::validate`]), and whether a local name is declared is deliberately *not* checked
    /// at all here — a dependency node receives opaque names it may not declare.
    ///
    /// # Errors
    /// [`FeatureRefError`] for an empty entry or side, a second `/`, or `dep/default` — the
    /// reserved directive is never enableable by name (see [`DEFAULT_BUILD_FEATURE`]).
    pub fn parse(entry: &'a str) -> Result<Self, FeatureRefError> {
        if entry.is_empty() {
            return Err(FeatureRefError::Empty);
        }
        let Some((dependency, feature)) = entry.split_once(FEATURE_SEPARATOR) else {
            return Ok(Self::Local(entry));
        };
        if dependency.is_empty() || feature.is_empty() {
            return Err(FeatureRefError::Empty);
        }
        if feature.contains(FEATURE_SEPARATOR) {
            return Err(FeatureRefError::NestedSeparator);
        }
        if feature == DEFAULT_BUILD_FEATURE {
            return Err(FeatureRefError::ReservedFeature);
        }
        Ok(Self::Dependency {
            dependency,
            feature,
        })
    }
}

/// A feature-list entry whose *shape* is not a valid [`FeatureRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureRefError {
    /// The entry, or one side of its `/`, is empty.
    Empty,
    /// A second `/`: a cross-package reference names exactly one dependency and one feature, and
    /// forwarding through an intermediate package is written in *that* package's `[features]`.
    NestedSeparator,
    /// The dependency-side feature is the reserved `default`, which is a resolution directive
    /// rather than an enableable feature; use `default-features` to control it.
    ReservedFeature,
}

impl fmt::Display for FeatureRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("expected `<feature>` or `<dependency>/<feature>`"),
            Self::NestedSeparator => {
                f.write_str("expected at most one `/` (`<dependency>/<feature>`)")
            }
            Self::ReservedFeature => write!(
                f,
                "`{DEFAULT_BUILD_FEATURE}` is a resolution directive, never an enableable feature \
                 (use `default-features` on the `[dependencies]` entry)"
            ),
        }
    }
}

impl Error for FeatureRefError {}

/// A build-feature selection that could not be resolved by [`Manifest::resolve_build_features`].
///
/// The value-level checks the command line cannot express: a `--features` name that is not declared
/// in `[features]`, or a `--features dep/feat` whose dependency cannot receive one. Internal-consistency
/// problems — a `default`/`enables` list that references an undeclared feature or dependency — are a
/// [`ValidationError`] found at manifest validation instead, so a well-formed manifest's closure
/// never hits an undeclared name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildFeatureError {
    /// `--features` named a feature not declared in `[features]`.
    UnknownSelected {
        /// The undeclared feature name.
        name: String,
    },
    /// `--features dep/feat` named a dependency that is not a declared `git`/`path` entry (a `jar`
    /// runs no build script that could read a feature).
    UnknownSelectedDependency {
        /// The whole selection entry, as written.
        name: String,
        /// The dependency half that could not receive a feature.
        dependency: String,
    },
    /// `--features` carried an entry whose shape is neither a feature name nor a well-formed
    /// `<dependency>/<feature>` reference.
    InvalidSelected {
        /// The whole selection entry, as written.
        name: String,
        /// Why the shape was rejected.
        reason: FeatureRefError,
    },
}

impl fmt::Display for BuildFeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSelected { name } => write!(
                f,
                "`--features {name}` names a feature not declared in `[features]`"
            ),
            Self::UnknownSelectedDependency { name, dependency } => write!(
                f,
                "`--features {name}` names `{dependency}`, which is not a declared `git`/`path` \
                 `[dependencies]` entry"
            ),
            Self::InvalidSelected { name, reason } => {
                write!(f, "`--features {name}` is malformed: {reason}")
            }
        }
    }
}

impl Error for BuildFeatureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidSelected { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

/// The build features one project resolved to: the set its own build script queries, plus what it
/// forwards to each dependency.
///
/// The two halves are deliberately separate values rather than one set of strings. A cross-package
/// `<dependency>/<feature>` entry is *routed*, never queryable — keeping it out of
/// [`features`](Self::features) is what makes `build.feature("render/vulkan")` false by construction
/// instead of by a filter someone has to remember. Both halves are ordered, so a build script's
/// fingerprint over either is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedBuildFeatures {
    features: BTreeSet<String>,
    dependencies: BTreeMap<String, BTreeSet<String>>,
}

impl ResolvedBuildFeatures {
    /// The features this project's own build script queries with `build.feature("…")`.
    pub const fn features(&self) -> &BTreeSet<String> {
        &self.features
    }

    /// Consume this resolution for its own queryable set, dropping what it forwards.
    pub fn into_features(self) -> BTreeSet<String> {
        self.features
    }

    /// The features forwarded to one `[dependencies]` entry, or `None` when it receives none here.
    pub fn dependency(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.dependencies.get(name)
    }

    /// Every `[dependencies]` entry this project forwards features to, in name order.
    pub fn dependencies(&self) -> impl ExactSizeIterator<Item = (&str, &BTreeSet<String>)> {
        self.dependencies
            .iter()
            .map(|(name, features)| (name.as_str(), features))
    }
}

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
    /// The language [`Feature`]s the project enables (`[package] features = ["…"]`).
    ///
    /// Additive-only, but a **closed** set — not the top-level [`[features]`](Manifest::features)
    /// map, which is the open-ended Cargo `[features]` analogue selected on the command line and
    /// read by the build script. A Java **release preset** (`"java25"`, …)
    /// selects everything that release stabilized — each preset implies the one before it, so
    /// `java25 ⊇ java24 ⊇ …` holds with nothing else declared — while an **individual feature**
    /// name (`"module-imports"`, …) turns on a single otherwise-preview construct, the Java
    /// analogue of one `--enable-preview` flag. Purely an *analysis* gate (the linter / LSP): it
    /// does not affect compilation — the `javac` version knobs remain
    /// `[build] release`/`source`/`target`. Resolved (closed under [`Feature::implies`]) by
    /// [`Manifest::feature_set`]; an unknown name is a TOML parse error (serde unknown variant).
    /// Empty when omitted, leaving every feature gate off.
    pub features: Vec<Feature>,
}

/// A single selectable language capability — the unit `[package] features` lists and the analysis
/// layers query.
///
/// A feature is **not** a gate on the *parser* (which always accepts every construct, losslessly and
/// error-resiliently); it is an *analysis* gate the linter / LSP consult to decide whether a construct
/// is permitted under the project's declared feature set. Variants come in two kinds:
///
/// - **Java release presets** (`Java8`–`Java25`, spelled `"java8"`–`"java25"`): each
///   [`implies`](Feature::implies) its [`predecessor`](Feature::predecessor) preset plus every
///   feature that release stabilized, so listing one selects the whole release — Rust-feature style
///   (`java25 = ["java24", …]`), except the release's feature list is *derived* from each feature's
///   single [`stabilized_in`](Feature::stabilized_in) introduction point, never enumerated per
///   release. A preset is an ordinary variant like any other feature; no version number exists in
///   the model.
/// - **Individual language features** (`ModuleImports`, `CompactSourceFiles`, …): each records, in
///   exactly one place ([`Feature::stabilized_in`]), the release preset at which it became a permanent
///   (non-preview) feature — or `None` for a jals-specific dialect feature that no Java release
///   stabilizes and that is therefore only ever turned on explicitly.
///
/// The set is a closed enum: an unknown name in `[package] features` is a TOML parse error (an
/// unknown variant); non-release notations (e.g. a jals-specific `java25-jals` dialect preset) can
/// later join as variants. Set membership lives in [`FeatureSet`], which keys off the typed variant —
/// no bit index is ever exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// The Java 8 release preset.
    Java8,
    /// The Java 9 release preset.
    Java9,
    /// The Java 10 release preset.
    Java10,
    /// The Java 11 release preset.
    Java11,
    /// The Java 12 release preset.
    Java12,
    /// The Java 13 release preset.
    Java13,
    /// The Java 14 release preset.
    Java14,
    /// The Java 15 release preset.
    Java15,
    /// The Java 16 release preset.
    Java16,
    /// The Java 17 release preset.
    Java17,
    /// The Java 18 release preset.
    Java18,
    /// The Java 19 release preset.
    Java19,
    /// The Java 20 release preset.
    Java20,
    /// The Java 21 release preset.
    Java21,
    /// The Java 22 release preset.
    Java22,
    /// The Java 23 release preset.
    Java23,
    /// The Java 24 release preset, where compact source files / module imports are still previews.
    Java24,
    /// The Java 25 release preset, which stabilizes compact source files and module imports.
    Java25,
    /// JEP 511: module import declarations (`import module M;`), permanent in Java 25 (a preview
    /// before). Gated by the linter's `module-import` rule.
    ModuleImports,
    /// JEP 512: compact source files and instance `main` methods (top-level members), permanent in
    /// Java 25 (a preview before). Gated by the linter's `compact-source-file` rule.
    CompactSourceFiles,
    /// jals dialect: grouped imports (`import java.util.{HashMap, ArrayList};`). Not valid Java at
    /// any release (`stabilized_in` = `None`) — on only when explicitly listed in `[package]
    /// features`. Gated by the linter's `grouped-import` rule; the compile frontend desugars it to
    /// plain imports.
    GroupedImports,
    /// jals dialect: attributes (`#[cfg(feature = "x")]`). Not valid Java at any release
    /// (`stabilized_in` = `None`) — on only when explicitly listed in `[package] features`. Gated
    /// by the linter's `attribute` rule; the compile frontend strips every attribute and applies
    /// `cfg` conditional compilation against the resolved `[features]` build-feature set.
    Attributes,
}

impl Feature {
    /// Every feature, in declaration order (release presets first, in release order). Load-bearing,
    /// not documentation: deserialization parses a name by looking it up here, so a variant omitted
    /// from this list cannot appear in any `[package] features` (and the exhaustive `match`es in
    /// [`predecessor`](Feature::predecessor) / [`stabilized_in`](Feature::stabilized_in) /
    /// [`config_name`](Feature::config_name) already force a stop for a new variant). The
    /// declaration-order invariant is const-asserted below.
    pub const ALL: [Self; 22] = [
        Self::Java8,
        Self::Java9,
        Self::Java10,
        Self::Java11,
        Self::Java12,
        Self::Java13,
        Self::Java14,
        Self::Java15,
        Self::Java16,
        Self::Java17,
        Self::Java18,
        Self::Java19,
        Self::Java20,
        Self::Java21,
        Self::Java22,
        Self::Java23,
        Self::Java24,
        Self::Java25,
        Self::ModuleImports,
        Self::CompactSourceFiles,
        Self::GroupedImports,
        Self::Attributes,
    ];

    /// Every feature's [`config_name`](Feature::config_name), parallel to [`ALL`](Feature::ALL) —
    /// the `expected one of …` list a `[package] features` parse error names.
    const NAMES: [&'static str; Self::ALL.len()] = {
        let mut names = [""; Self::ALL.len()];
        let mut i = 0;
        while i < names.len() {
            names[i] = Self::ALL[i].config_name();
            i += 1;
        }
        names
    };

    /// This feature's bit in a [`FeatureSet`] — derived from the declaration order, never stored.
    const fn bit(self) -> u64 {
        1 << self as u64
    }

    /// The release preset immediately before this one (`Java25` → `Java24`), or `None` for the
    /// oldest preset ([`Java8`](Feature::Java8)) and for an individual language feature.
    ///
    /// The single edge each preset contributes to the `java25 ⊇ java24 ⊇ …` chain —
    /// [`implies`](Feature::implies) follows it, and [`FeatureSet::resolve`]'s closure walks it
    /// transitively, so no per-release membership list (and no version number) exists anywhere.
    pub const fn predecessor(self) -> Option<Self> {
        match self {
            Self::Java9 => Some(Self::Java8),
            Self::Java10 => Some(Self::Java9),
            Self::Java11 => Some(Self::Java10),
            Self::Java12 => Some(Self::Java11),
            Self::Java13 => Some(Self::Java12),
            Self::Java14 => Some(Self::Java13),
            Self::Java15 => Some(Self::Java14),
            Self::Java16 => Some(Self::Java15),
            Self::Java17 => Some(Self::Java16),
            Self::Java18 => Some(Self::Java17),
            Self::Java19 => Some(Self::Java18),
            Self::Java20 => Some(Self::Java19),
            Self::Java21 => Some(Self::Java20),
            Self::Java22 => Some(Self::Java21),
            Self::Java23 => Some(Self::Java22),
            Self::Java24 => Some(Self::Java23),
            Self::Java25 => Some(Self::Java24),
            Self::Java8
            | Self::ModuleImports
            | Self::CompactSourceFiles
            | Self::GroupedImports
            | Self::Attributes => None,
        }
    }

    /// The release preset at which this language feature became a permanent (non-preview) feature,
    /// `None` for a release preset itself and for a dialect feature no Java release stabilizes
    /// (enabled only by listing it in `[package] features`).
    ///
    /// This is the **single** place a feature's release fact lives. Adding a [`Feature`] variant
    /// fails to compile until this `match` is extended, so [`implies`](Feature::implies) — and thus
    /// the whole release ⊇ chain — can never silently drift out of step with the feature set.
    pub const fn stabilized_in(self) -> Option<Self> {
        match self {
            Self::ModuleImports | Self::CompactSourceFiles => Some(Self::Java25),
            // A release preset itself, and the jals dialect features (`GroupedImports`,
            // `Attributes`) no Java release stabilizes — all return `None`.
            Self::Java8
            | Self::Java9
            | Self::Java10
            | Self::Java11
            | Self::Java12
            | Self::Java13
            | Self::Java14
            | Self::Java15
            | Self::Java16
            | Self::Java17
            | Self::Java18
            | Self::Java19
            | Self::Java20
            | Self::Java21
            | Self::Java22
            | Self::Java23
            | Self::Java24
            | Self::Java25
            | Self::GroupedImports
            | Self::Attributes => None,
        }
    }

    /// Whether this is a **jals dialect** feature: a construct no Java release defines, which
    /// compiles only because the jals compile frontend desugars it away before `javac` runs.
    ///
    /// The distinction a release gate cannot express. A release-gated feature is real Java that a
    /// project may simply not have opted into describing, so a project declaring no
    /// `[package] features` opting out of the gate is harmless. A dialect construct is never valid
    /// Java at any release, so the same silence would leave the user with no jals-side signal at
    /// all and a raw `javac` syntax error — which is why [`FeatureSet::permits`] does not extend
    /// its empty-set exemption here.
    ///
    /// Exhaustive with no `_` arm, like the release facts above: a new [`Feature`] has to decide
    /// which side of this line it falls on rather than defaulting to "Java".
    pub const fn is_dialect(self) -> bool {
        match self {
            Self::GroupedImports | Self::Attributes => true,
            Self::Java8
            | Self::Java9
            | Self::Java10
            | Self::Java11
            | Self::Java12
            | Self::Java13
            | Self::Java14
            | Self::Java15
            | Self::Java16
            | Self::Java17
            | Self::Java18
            | Self::Java19
            | Self::Java20
            | Self::Java21
            | Self::Java22
            | Self::Java23
            | Self::Java24
            | Self::Java25
            | Self::ModuleImports
            | Self::CompactSourceFiles => false,
        }
    }

    /// The features this one directly *implies* — the edges [`FeatureSet::resolve`] closes over.
    ///
    /// A release preset implies its [`predecessor`](Feature::predecessor) preset plus every
    /// feature stabilized in this release (read back off each feature's
    /// [`stabilized_in`](Feature::stabilized_in)) — exactly Cargo's `java25 = ["java24", …]`,
    /// except the release's own feature list is *derived* from the per-feature `stabilized_in`
    /// fact, never enumerated per release. Earlier releases' features arrive transitively through
    /// the predecessor chain, so the `java25 ⊇ java24 ⊇ …` monotonicity holds **by construction**.
    /// An individual feature implies nothing today; a feature-to-feature dependency would be added
    /// here.
    pub fn implies(self) -> FeatureSet {
        Self::ALL
            .into_iter()
            .filter(|feature| {
                Some(*feature) == self.predecessor() || feature.stabilized_in() == Some(self)
            })
            .fold(FeatureSet::default(), FeatureSet::with)
    }

    /// The kebab-case name this feature carries in `[package] features` — the **single** name
    /// table: deserialization parses by it (see the [`Deserialize`] impl below), and the linter
    /// names the feature in a diagnostic's "enable it with …" hint through it, so the two can
    /// never disagree.
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::Java8 => "java8",
            Self::Java9 => "java9",
            Self::Java10 => "java10",
            Self::Java11 => "java11",
            Self::Java12 => "java12",
            Self::Java13 => "java13",
            Self::Java14 => "java14",
            Self::Java15 => "java15",
            Self::Java16 => "java16",
            Self::Java17 => "java17",
            Self::Java18 => "java18",
            Self::Java19 => "java19",
            Self::Java20 => "java20",
            Self::Java21 => "java21",
            Self::Java22 => "java22",
            Self::Java23 => "java23",
            Self::Java24 => "java24",
            Self::Java25 => "java25",
            Self::ModuleImports => "module-imports",
            Self::CompactSourceFiles => "compact-source-files",
            Self::GroupedImports => "grouped-imports",
            Self::Attributes => "attributes",
        }
    }
}

// [`Feature::ALL`] must list every variant in declaration order: deserialization looks names up in
// it, and [`Feature::bit`] packs the declaration index into the [`FeatureSet`] word.
const _: () = {
    assert!(
        Feature::ALL.len() <= u64::BITS as usize,
        "FeatureSet's u64 is full; widen its storage"
    );
    let mut i = 0;
    while i < Feature::ALL.len() {
        assert!(
            Feature::ALL[i] as usize == i,
            "Feature::ALL must be in declaration order"
        );
        i += 1;
    }
};

// Deserialized by [`Feature::config_name`] lookup over [`Feature::ALL`] rather than a serde derive,
// so the parsed names and the names the linter's hints print are one table — and a variant omitted
// from `ALL` fails to parse at all instead of silently dropping out of [`Feature::implies`].
impl<'de> Deserialize<'de> for Feature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Self::ALL
            .into_iter()
            .find(|feature| feature.config_name() == name)
            .ok_or_else(|| serde::de::Error::unknown_variant(&name, &Self::NAMES))
    }
}

/// The resolved set of language [`Feature`]s enabled for a project — the value the analysis layers
/// query with [`contains`](FeatureSet::contains).
///
/// An [`empty`](FeatureSet::is_empty) set (a project that declares no `[package] features`) leaves
/// every feature gate off.
///
/// A small bitset newtype: no integer is exposed, membership is the typed [`Feature`] enum.
/// Constructed only by [`resolve`](FeatureSet::resolve) — the `[package] features` list closed
/// under [`Feature::implies`]. `Copy` (one word), so hosts thread it around freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeatureSet(u64);

impl FeatureSet {
    /// Resolve the effective feature set: every listed feature, closed under [`Feature::implies`]
    /// to a fixpoint — so listing `java25` pulls in `java24`, …, `java8`, and everything those
    /// releases stabilize.
    pub fn resolve(features: &[Feature]) -> Self {
        let mut set = features.iter().copied().fold(Self::default(), Self::with);
        loop {
            let closed = Feature::ALL
                .into_iter()
                .filter(|feature| set.contains(*feature))
                .fold(set, |acc, feature| acc.union(feature.implies()));
            if closed == set {
                return set;
            }
            set = closed;
        }
    }

    /// This set with `feature` enabled.
    const fn with(self, feature: Feature) -> Self {
        Self(self.0 | feature.bit())
    }

    /// The union of this set and `other`.
    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `feature` is enabled in this set.
    pub const fn contains(self, feature: Feature) -> bool {
        self.0 & feature.bit() != 0
    }

    /// Whether no feature is enabled — the set of a project that declares no `[package] features`,
    /// which leaves every feature gate off (the gated lint rules report nothing).
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether a use of `feature` is permitted: an [`empty`](FeatureSet::is_empty) set permits
    /// every *Java* feature (a project that declares no `[package] features` opts out of
    /// release-based feature gating), otherwise the feature must be enabled. The single owner of
    /// the empty-set exemption every feature-gate consumer (the gated lint rules) relies on.
    ///
    /// A [`dialect`](Feature::is_dialect) feature is exempt from the exemption: it must be
    /// [`contain`](FeatureSet::contains)ed, empty set or not. Silence there would be a lie —
    /// nothing else would report the construct either, because the build path keys the desugaring
    /// off `contains`, so an undeclared dialect construct reaches `javac` verbatim and fails as a
    /// plain syntax error.
    pub const fn permits(self, feature: Feature) -> bool {
        if feature.is_dialect() {
            return self.contains(feature);
        }
        self.is_empty() || self.contains(feature)
    }
}

/// Compilation settings (`[build]`).
///
/// Unknown keys are rejected. Every nested shape here already denies them, and the failure mode
/// without it is bad: misspelling `script` as `scripts` silently disables the build script, so the
/// generated sources never appear and `javac` fails with an unrelated "cannot find symbol".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Build {
    /// Optional project build script.
    pub script: Option<BuildScript>,
    /// Which frontend lowers the project's sources to the Java that the backend compiles.
    ///
    /// Defaults to [`FrontendKind::Vanilla`], the identity lowering, so an existing manifest
    /// keeps compiling exactly the sources it always did.
    pub frontend: FrontendKind,
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

/// A project build script selected by its `type` field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BuildScript {
    /// A Rhai build script stored at a project-relative portable file path.
    Rhai {
        /// The script file, relative to the project root.
        file: String,
    },
}

impl BuildScript {
    /// The value of the script's serialized `type` tag.
    pub const fn tag_name(&self) -> &'static str {
        match self {
            Self::Rhai { .. } => "rhai",
        }
    }
}

/// The compile frontend, selected by its `type` field (`[build.frontend]`).
///
/// A frontend turns the project's authored sources into the Java sources the backend compiles.
/// Only the identity lowering exists today; this is the seam a macro frontend fills, and it is
/// tagged from the start so that adding one is a new variant rather than a schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FrontendKind {
    /// Emit every source unchanged: the authored sources *are* the Java sources.
    ///
    /// A struct variant (`{}`) rather than a unit variant on purpose: serde's
    /// `deny_unknown_fields` is honored for a struct variant of an internally-tagged enum but
    /// silently ignored for a unit one, so a typo like `[build.frontend] typ = "vanilla"` would
    /// otherwise be accepted — the very footgun `deny_unknown_fields` exists to catch.
    Vanilla {},
}

impl Default for FrontendKind {
    fn default() -> Self {
        Self::Vanilla {}
    }
}

impl FrontendKind {
    /// The value of the frontend's serialized `type` tag.
    ///
    /// Also its cache identity. A stable string rather than the enum discriminant, so adding or
    /// reordering variants can never renumber a shipped frontend's cache keys.
    pub const fn tag_name(&self) -> &'static str {
        match self {
            Self::Vanilla {} => "vanilla",
        }
    }
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
            script: None,
            frontend: FrontendKind::Vanilla {},
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
    /// with at most one `branch` / `tag` / `rev`, a `path`'s directory is non-empty, and — on both
    /// source forms — every `features` name passes [`validate_features`](Dependency::validate_features).
    /// `name` labels errors.
    ///
    /// # Errors
    /// Returns the first [`DependencyError`] found (empty value, unsupported URL scheme, conflicting
    /// git refs, or a bad `features` name).
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
                // These values become `git` CLI arguments. A leading `-` makes the CLI read them
                // as options instead: `branch = "-f"` turns `git checkout --quiet -f` into a
                // no-op that *succeeds*, silently resolving the dependency to the default branch
                // rather than the requested one. Reject the shape rather than rely on argument
                // order downstream.
                for (field, value) in [
                    ("git", Some(&git.git)),
                    ("branch", git.branch.as_ref()),
                    ("tag", git.tag.as_ref()),
                    ("rev", git.rev.as_ref()),
                ] {
                    if let Some(value) = value
                        && value.starts_with('-')
                    {
                        return Err(DependencyError::OptionLike {
                            name: name.to_owned(),
                            field,
                            value: value.clone(),
                        });
                    }
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
        }?;
        self.validate_features(name)
    }

    /// The **build features** this entry enables in the dependency project — Cargo's per-dependency
    /// `features = ["…"]`, declared on the `git` / `path` forms.
    ///
    /// Features are *per package*: these are names of features **of the dependency**, exactly as in
    /// Cargo, so the cross-package `<dependency>/<feature>` form is rejected here
    /// ([`Dependency::validate_features`]) — routing a feature onward is written in the receiving
    /// project's own `[features]`. They are one of the two inputs to that project's resolution; the
    /// other is whatever a [`[features]`](Manifest::features) entry of this manifest forwards to it.
    /// A `jar` has no build script, so the field does not exist on that form at all (writing it is a
    /// parse error) and this returns an empty slice.
    pub fn features(&self) -> &[String] {
        match self {
            Self::Jar(_) => &[],
            Self::Git(git) => &git.features,
            Self::Path(path) => &path.features,
        }
    }

    /// Whether this entry lets the dependency resolve its own `[features] default` list (Cargo's
    /// `default-features`, defaulting to `true`).
    ///
    /// Only *this* edge's answer. Where several entries reach one project, the sets unify and so
    /// does this flag — additively, as in Cargo: one entry asking for the defaults turns them on for
    /// the shared node, whatever the others said. A `jar` receives no features at all, so its answer
    /// is never read.
    pub fn default_features(&self) -> bool {
        match self {
            Self::Jar(_) => true,
            Self::Git(git) => git.default_features.unwrap_or(true),
            Self::Path(path) => path.default_features.unwrap_or(true),
        }
    }

    /// Whether a `<dependency>/<feature>` reference may name this entry: only a source form, since
    /// a `jar` contributes compiled classes and runs no build script that could read a feature.
    ///
    /// The single spelling of that rule, asked once where a manifest is validated and once where a
    /// `--features` selection is resolved. Exhaustive on purpose: a future dependency form has to
    /// answer here rather than inherit whichever side of the question the two callers assumed.
    const fn accepts_features(&self) -> bool {
        match self {
            Self::Jar(_) => false,
            Self::Git(_) | Self::Path(_) => true,
        }
    }

    /// The value-level checks on [`features`](Dependency::features): every name is non-empty, none
    /// is the reserved `default`, and none carries the cross-package `/`.
    ///
    /// `default` is a resolution directive, never a queryable feature (see
    /// [`Manifest::resolve_build_features`], which drops it); `default-features` is how this entry
    /// says whether the dependency resolves its own list. A `/` is rejected because this list names
    /// features of the dependency itself — `a/b` here would read as a package the dependency's own
    /// manifest never mentions, so the name is a mistake rather than a route.
    ///
    /// # Errors
    /// [`DependencyError::Empty`] for an empty name, [`DependencyError::ReservedFeature`] for
    /// `default`, [`DependencyError::CrossFeature`] for a name containing `/`.
    pub fn validate_features(&self, name: &str) -> Result<(), DependencyError> {
        for feature in self.features() {
            if feature.is_empty() {
                return Err(DependencyError::Empty {
                    name: name.to_owned(),
                    field: "features",
                });
            }
            if feature == DEFAULT_BUILD_FEATURE {
                return Err(DependencyError::ReservedFeature {
                    name: name.to_owned(),
                    feature: feature.clone(),
                });
            }
            if feature.contains(FEATURE_SEPARATOR) {
                return Err(DependencyError::CrossFeature {
                    name: name.to_owned(),
                    feature: feature.clone(),
                });
            }
        }
        Ok(())
    }
}

impl GitDependency {
    /// Classify the pinned commit from at most one of `branch` / `tag` / `rev`. Pure — [`GitRef`]
    /// holds only strings; `jals-classpath`'s native plan pairs the result with the clone URL.
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
    /// `[dependencies]` entry's value-level checks (see [`Dependency::validate`]) and requires a
    /// configured build script to name a non-root portable project file outside every directory
    /// removed by `jals clean`.
    ///
    /// # Errors
    /// Returns [`ValidationError`] describing the first problem found.
    pub fn validate(&self) -> Result<(), ValidationError> {
        // `classes-dir` may legitimately be a host path (`invocation` supports an absolute one),
        // so a value that is not a portable project path is left to the host. The one shape that
        // must be rejected here is the project root: `RelativePath::parse` maps the empty string
        // to it, and `jals clean` removes this directory recursively, so accepting it would
        // delete the whole project.
        let classes_dir = DirKey::parse(&self.build.classes_dir).ok();
        if classes_dir.as_ref().is_some_and(|dir| dir.path().is_root()) {
            return Err(ValidationError::InvalidClassesDir {
                dir: self.build.classes_dir.clone(),
            });
        }
        if let Some(BuildScript::Rhai { file }) = &self.build.script {
            let script = FileKey::parse(file)
                .map_err(|_| ValidationError::InvalidBuildScriptFile { file: file.clone() })?;
            // A script is always a portable project path, so it cannot be inside a `classes-dir`
            // that is not one.
            if classes_dir
                .as_ref()
                .is_some_and(|dir| script.path().starts_with(dir.path()))
            {
                return Err(ValidationError::BuildScriptInClassesDir {
                    file: file.clone(),
                    classes_dir: self.build.classes_dir.clone(),
                });
            }
            let managed_root = DirKey::parse(MANAGED_BUILD_ROOT)
                .map_err(|_| ValidationError::InvalidBuildScriptFile { file: file.clone() })?;
            if script.path().starts_with(managed_root.path()) {
                return Err(ValidationError::BuildScriptInManagedRoot { file: file.clone() });
            }
        }

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

        // `[features]`: every local name a `default`/`enables` list references must itself be a
        // declared feature, and every `<dependency>/<feature>` entry must name a dependency that can
        // actually receive one. The command-line `--features` selection is checked later against the
        // same declared set by `resolve_build_features`; this is the manifest-internal half serde
        // cannot express (an open-ended map has no closed variant set to reject unknown names at
        // parse time).
        for (feature, enables) in &self.features {
            // A key containing `/` could never be enabled: every list entry carrying one is read as
            // a cross-package reference, so the feature would be declared and unreachable.
            if feature.contains(FEATURE_SEPARATOR) {
                return Err(ValidationError::InvalidBuildFeatureName {
                    feature: feature.clone(),
                });
            }
            for entry in enables {
                self.validate_feature_entry(feature, entry)?;
            }
        }

        Ok(())
    }

    /// Check one `[features]` list entry of the declared feature `feature` against what this
    /// manifest declares: a local name must be a declared feature, and a cross-package reference
    /// must name a dependency that can receive one.
    fn validate_feature_entry(&self, feature: &str, entry: &str) -> Result<(), ValidationError> {
        let reference =
            FeatureRef::parse(entry).map_err(|reason| ValidationError::InvalidFeatureRef {
                feature: feature.to_owned(),
                entry: entry.to_owned(),
                reason,
            })?;
        match reference {
            FeatureRef::Local(name) => {
                if self.features.contains_key(name) {
                    Ok(())
                } else {
                    Err(ValidationError::UndeclaredBuildFeature {
                        feature: feature.to_owned(),
                        enables: name.to_owned(),
                    })
                }
            }
            // Routing to something with no build script is always a mistake, and the graph relies
            // on it: a `jar` name reaching the router would be ambiguous, since a jar with a
            // companion `sources` archive contributes two edges under one name.
            FeatureRef::Dependency { dependency, .. } => match self.dependencies.get(dependency) {
                Some(dep) if dep.accepts_features() => Ok(()),
                Some(_) => Err(ValidationError::BinaryFeatureDependency {
                    feature: feature.to_owned(),
                    entry: entry.to_owned(),
                    dependency: dependency.to_owned(),
                }),
                None => Err(ValidationError::UndeclaredFeatureDependency {
                    feature: feature.to_owned(),
                    entry: entry.to_owned(),
                    dependency: dependency.to_owned(),
                }),
            },
        }
    }

    /// The project's resolved language [`FeatureSet`] — the `[package] features` list closed under
    /// [`Feature::implies`] (see [`FeatureSet::resolve`]) — empty when the project declares no
    /// features, leaving every feature gate off. The single projection the analysis layers consume:
    /// the host feeds it to the feature-gated lint rules as the lint config's feature set
    /// (`compact-source-file` / `module-import` fire for a feature the set lacks).
    pub fn feature_set(&self) -> FeatureSet {
        FeatureSet::resolve(&self.package.features)
    }

    /// Resolve the effective **build features** of this project from an already-chosen `seed`: the
    /// additive closure of `seed` over `[features]`, with cross-package entries routed out.
    ///
    /// The one closure every project goes through, whoever chose the seed. At the root that is the
    /// command line ([`resolve_build_features`](Self::resolve_build_features), which calls this);
    /// for a dependency it is what the `[dependencies]` entries aimed at it asked for, unioned with
    /// what their `[features]` tables forwarded. `defaults` says whether this project's own
    /// `default` list joins the seed — the root's `--no-default-features`, and a dependency's
    /// [`default-features`](Dependency::default_features).
    ///
    /// A `<dependency>/<feature>` entry lands in [`ResolvedBuildFeatures::dependencies`] instead of
    /// the queryable set, so `build.feature("render/vulkan")` is false by construction. The reserved
    /// `default` key is a directive, not a feature, so it never appears in the result either. A seed
    /// name this manifest does not declare stays queryable and simply expands to nothing: a
    /// dependency legitimately receives names from a project that knows more about it than its own
    /// (possibly absent) `[features]` table does.
    pub fn expand_build_features<I>(&self, seed: I, defaults: bool) -> ResolvedBuildFeatures
    where
        I: IntoIterator<Item = String>,
    {
        let declared = &self.features;
        let mut pending: Vec<String> = seed.into_iter().collect();
        if defaults && let Some(list) = declared.get(DEFAULT_BUILD_FEATURE) {
            pending.extend(list.iter().cloned());
        }

        // Close over the enables map with a worklist: a name expands exactly once, when it first
        // joins the set, so the fixpoint costs one pass per reachable feature. An undeclared name
        // still joins and simply expands to nothing.
        let mut resolved = ResolvedBuildFeatures::default();
        while let Some(entry) = pending.pop() {
            // A malformed entry cannot route anywhere, and `validate` has already rejected every
            // shape a manifest can write, so treat the leftovers as opaque local names rather than
            // inventing a failure mode a dependency's arriving set would have to handle.
            if let Ok(FeatureRef::Dependency {
                dependency,
                feature,
            }) = FeatureRef::parse(&entry)
            {
                resolved
                    .dependencies
                    .entry(dependency.to_owned())
                    .or_default()
                    .insert(feature.to_owned());
                continue;
            }
            if resolved.features.contains(&entry) {
                continue;
            }
            if let Some(enables) = declared.get(&entry) {
                pending.extend(enables.iter().cloned());
            }
            resolved.features.insert(entry);
        }

        // `default` is a resolution directive, not a queryable feature — drop it after expansion.
        resolved.features.remove(DEFAULT_BUILD_FEATURE);
        resolved
    }

    /// Resolve the **root** project's build features for a run: the command-line selection, closed
    /// over `[features]` by [`expand_build_features`](Self::expand_build_features).
    ///
    /// Mirrors Cargo's `--features` / `--all-features` / `--no-default-features`:
    /// - the base set is every declared feature (`all`) or nothing, plus the `default` key's list
    ///   unless `no_default`;
    /// - the `selected` entries are unioned in and the whole is closed to a fixpoint (like
    ///   [`FeatureSet::resolve`]);
    /// - `all` takes precedence over `no_default` — `default` is itself a declared key, so its list
    ///   expands from the base set even when the directive is suppressed.
    ///
    /// A `selected` entry may be a cross-package `<dependency>/<feature>` reference, as in Cargo's
    /// `--features serde/std`; it is routed rather than enabled here.
    ///
    /// # Errors
    /// [`BuildFeatureError::UnknownSelected`] if a `selected` feature is not declared,
    /// [`BuildFeatureError::UnknownSelectedDependency`] if a cross-package entry names something
    /// other than a `git`/`path` dependency, [`BuildFeatureError::InvalidSelected`] for a malformed
    /// entry. A manifest validated by [`Manifest::validate`] guarantees every `default`/`enables`
    /// reference is sound, so the closure itself never encounters a bad name.
    pub fn resolve_build_features(
        &self,
        selected: &[String],
        all: bool,
        no_default: bool,
    ) -> Result<ResolvedBuildFeatures, BuildFeatureError> {
        for name in selected {
            match FeatureRef::parse(name) {
                Ok(FeatureRef::Local(local)) => {
                    if !self.features.contains_key(local) {
                        return Err(BuildFeatureError::UnknownSelected { name: name.clone() });
                    }
                }
                Ok(FeatureRef::Dependency { dependency, .. }) => {
                    if !self
                        .dependencies
                        .get(dependency)
                        .is_some_and(Dependency::accepts_features)
                    {
                        return Err(BuildFeatureError::UnknownSelectedDependency {
                            name: name.clone(),
                            dependency: dependency.to_owned(),
                        });
                    }
                }
                Err(reason) => {
                    return Err(BuildFeatureError::InvalidSelected {
                        name: name.clone(),
                        reason,
                    });
                }
            }
        }
        // The base set, then the explicit selection on top.
        let mut seed: Vec<String> = if all {
            self.features.keys().cloned().collect()
        } else {
            Vec::new()
        };
        seed.extend(selected.iter().cloned());
        Ok(self.expand_build_features(seed, !no_default))
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
    /// `[build] classes-dir` is not a non-root portable project directory path.
    InvalidClassesDir {
        /// The invalid compiler output directory.
        dir: String,
    },
    /// A `[build] script.file` is not a non-root portable project file path.
    InvalidBuildScriptFile {
        /// The invalid script file value.
        file: String,
    },
    /// A `[build] script.file` is inside `target/jals/build`, which `jals clean` owns and removes.
    BuildScriptInManagedRoot {
        /// The unsafe script file value.
        file: String,
    },
    /// A `[build] script.file` is inside `classes-dir`, which `jals clean` owns and removes.
    BuildScriptInClassesDir {
        /// The unsafe script file value.
        file: String,
        /// The compiler output directory containing the script.
        classes_dir: String,
    },
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
    /// A `[features]` entry's `default`/`enables` list references a feature that is not itself a
    /// declared key.
    UndeclaredBuildFeature {
        /// The declared feature (or `default`) whose list carries the bad reference.
        feature: String,
        /// The referenced name that is not a declared feature.
        enables: String,
    },
    /// A `[features]` **key** contains `/`, which is reserved for the cross-package
    /// `<dependency>/<feature>` form — such a feature could never be enabled by name.
    InvalidBuildFeatureName {
        /// The unreachable feature name.
        feature: String,
    },
    /// A `[features]` list entry is neither a feature name nor a well-formed
    /// `<dependency>/<feature>` reference (see [`FeatureRef::parse`]).
    InvalidFeatureRef {
        /// The declared feature (or `default`) whose list carries the bad entry.
        feature: String,
        /// The offending entry, as written.
        entry: String,
        /// Why the shape was rejected.
        reason: FeatureRefError,
    },
    /// A `<dependency>/<feature>` entry names a dependency this manifest does not declare.
    UndeclaredFeatureDependency {
        /// The declared feature (or `default`) whose list carries the entry.
        feature: String,
        /// The offending entry, as written.
        entry: String,
        /// The undeclared dependency name.
        dependency: String,
    },
    /// A `<dependency>/<feature>` entry names a `jar` dependency, which runs no build script that
    /// could read a feature.
    BinaryFeatureDependency {
        /// The declared feature (or `default`) whose list carries the entry.
        feature: String,
        /// The offending entry, as written.
        entry: String,
        /// The `jar` dependency name.
        dependency: String,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidClassesDir { dir } => write!(
                f,
                "invalid `[build] classes-dir` `{dir}`: expected a non-root portable project directory path"
            ),
            Self::InvalidBuildScriptFile { file } => write!(
                f,
                "invalid `[build] script.file` `{file}`: expected a non-root portable file path"
            ),
            Self::BuildScriptInManagedRoot { file } => write!(
                f,
                "invalid `[build] script.file` `{file}`: scripts must be outside `target/jals/build`, which `jals clean` removes"
            ),
            Self::BuildScriptInClassesDir { file, classes_dir } => write!(
                f,
                "invalid `[build] script.file` `{file}`: scripts must be outside `[build] classes-dir` `{classes_dir}`, which `jals clean` removes"
            ),
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
            Self::UndeclaredBuildFeature { feature, enables } => write!(
                f,
                "`[features] {feature}` enables `{enables}`, which is not a declared feature"
            ),
            Self::InvalidBuildFeatureName { feature } => write!(
                f,
                "`[features]` declares `{feature}`, but `/` is reserved for the \
                 `<dependency>/<feature>` form, so the feature could never be enabled"
            ),
            Self::InvalidFeatureRef {
                feature,
                entry,
                reason,
            } => write!(f, "`[features] {feature}` enables `{entry}`: {reason}"),
            Self::UndeclaredFeatureDependency {
                feature,
                entry,
                dependency,
            } => write!(
                f,
                "`[features] {feature}` enables `{entry}`, but `{dependency}` is not a declared \
                 `[dependencies]` entry"
            ),
            Self::BinaryFeatureDependency {
                feature,
                entry,
                dependency,
            } => write!(
                f,
                "`[features] {feature}` enables `{entry}`, but `{dependency}` is a `jar` \
                 dependency, which runs no build script that could read a feature"
            ),
        }
    }
}

impl Error for ValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Dependency(err) => Some(err),
            Self::InvalidFeatureRef { reason, .. } => Some(reason),
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
        assert_eq!(m.build.script, None);
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
    fn parses_build_script_inline_and_dotted_syntax() {
        let m: Manifest = r#"
            [build]
            script = { type = "rhai", file = "build.rhai" }
            "#
        .parse()
        .unwrap();
        assert_eq!(
            m.build.script,
            Some(BuildScript::Rhai {
                file: "build.rhai".to_owned(),
            })
        );

        let m: Manifest = "build.script = { type = \"rhai\", file = \"scripts/build.rhai\" }\n"
            .parse()
            .unwrap();
        assert_eq!(
            m.build.script,
            Some(BuildScript::Rhai {
                file: "scripts/build.rhai".to_owned(),
            })
        );
    }

    #[test]
    fn parses_build_script_table_syntax() {
        let m: Manifest = r#"
            [build.script]
            type = "rhai"
            file = "build.rhai"
            "#
        .parse()
        .unwrap();
        assert_eq!(
            m.build.script,
            Some(BuildScript::Rhai {
                file: "build.rhai".to_owned(),
            })
        );
    }

    #[test]
    fn build_script_tag_name_matches_serde_tag() {
        let script = BuildScript::Rhai {
            file: "build.rhai".to_owned(),
        };
        assert_eq!(script.tag_name(), "rhai");
    }

    #[test]
    fn build_script_rejects_missing_and_unknown_fields() {
        for text in [
            "[build]\nscript = { file = \"build.rhai\" }\n",
            "[build]\nscript = { type = \"rhai\" }\n",
            "[build]\nscript = { type = \"unknown\", file = \"build.rhai\" }\n",
            "[build]\nscript = { type = \"rhai\", file = \"build.rhai\", extra = true }\n",
            "[build.script]\ntype = \"rhai\"\nfile = \"build.rhai\"\nextra = true\n",
        ] {
            assert!(
                toml::from_str::<Manifest>(text).is_err(),
                "invalid build script parsed: {text}"
            );
        }
    }

    #[test]
    fn build_script_file_must_be_a_non_root_portable_file_key() {
        for file in ["../build.rhai", ""] {
            let text =
                alloc::format!("[build]\nscript = {{ type = \"rhai\", file = \"{file}\" }}\n");
            let error = text.parse::<Manifest>().unwrap_err();
            let ManifestParseError::Invalid { source, .. } = error else {
                panic!("invalid build script file must be a validation error");
            };
            assert_eq!(
                source,
                ValidationError::InvalidBuildScriptFile {
                    file: file.to_owned(),
                }
            );
            assert_eq!(
                alloc::format!("{source}"),
                alloc::format!(
                    "invalid `[build] script.file` `{file}`: expected a non-root portable file path"
                )
            );
        }
    }

    #[test]
    fn build_script_file_must_be_outside_the_managed_build_root() {
        for file in [
            "target/jals/build",
            "target/jals/build/build.rhai",
            "target/jals/build/rhai/out/build.rhai",
        ] {
            let text =
                alloc::format!("[build]\nscript = {{ type = \"rhai\", file = \"{file}\" }}\n");
            let error = text.parse::<Manifest>().unwrap_err();
            let ManifestParseError::Invalid { source, .. } = error else {
                panic!("managed build script file must be a validation error");
            };
            assert_eq!(
                source,
                ValidationError::BuildScriptInManagedRoot {
                    file: file.to_owned(),
                }
            );
            assert!(source.to_string().contains("`jals clean` removes"));
        }
    }

    #[test]
    fn build_script_file_must_be_outside_the_classes_dir() {
        let error = r#"
            [build]
            classes-dir = "out"
            script = { type = "rhai", file = "out/build.rhai" }
            "#
        .parse::<Manifest>()
        .unwrap_err();
        let ManifestParseError::Invalid { source, .. } = error else {
            panic!("script inside classes-dir must be a validation error");
        };
        assert_eq!(
            source,
            ValidationError::BuildScriptInClassesDir {
                file: "out/build.rhai".to_owned(),
                classes_dir: "out".to_owned(),
            }
        );
        assert!(source.to_string().contains("`jals clean` removes"));
    }

    /// `invocation` supports an absolute `classes-dir`, and `source-dirs` may reach outside the
    /// project. Requiring a portable project path for either would break projects that worked
    /// before build scripts existed.
    #[test]
    fn accepts_a_host_path_classes_dir() {
        for dir in ["/tmp/out", "../out"] {
            let manifest: Manifest = alloc::format!("[build]\nclasses-dir = \"{dir}\"\n")
                .parse()
                .unwrap_or_else(|error| {
                    panic!("`classes-dir = {dir:?}` must stay accepted: {error}")
                });
            assert_eq!(manifest.build.classes_dir, dir);
        }
    }

    /// Misspelling `script` used to parse cleanly and leave `script: None`, so the build script
    /// never ran and the only symptom was `javac` failing on the sources it would have generated.
    #[test]
    fn rejects_an_unknown_build_key() {
        let error = r#"
            [build]
            scripts = { type = "rhai", file = "build.rhai" }
            "#
        .parse::<Manifest>()
        .unwrap_err();
        assert!(
            matches!(error, ManifestParseError::Parse { .. }),
            "an unknown `[build]` key must not parse: {error:?}"
        );
    }

    /// `jals clean` removes `classes-dir` recursively, so a value resolving to the project root
    /// would delete the entire project — including files `jals` never generated. The empty string
    /// parses to the root, so it has to be rejected here rather than relied on to fail later.
    #[test]
    fn rejects_a_root_classes_dir() {
        let error = r#"
            [build]
            classes-dir = ""
            "#
        .parse::<Manifest>()
        .unwrap_err();
        let ManifestParseError::Invalid { source, .. } = error else {
            panic!("a root classes-dir must be a validation error");
        };
        assert_eq!(
            source,
            ValidationError::InvalidClassesDir { dir: String::new() }
        );
        assert!(source.to_string().contains("non-root"));
    }

    #[test]
    fn build_script_is_optional_for_legacy_build_configuration() {
        let m: Manifest = toml::from_str(
            r#"
            [build]
            source-dirs = ["src", "generated"]
            classes-dir = "out"
            release = 21
            source = 17
            target = 17
            classpath = ["lib/a.jar"]
            javac-flags = ["-Xlint:all"]
            "#,
        )
        .unwrap();

        assert_eq!(m.build.script, None);
        assert_eq!(m.build.source_dirs, alloc::vec!["src", "generated"]);
        assert_eq!(m.build.classes_dir, "out");
        assert_eq!(m.build.release, Some(21));
        assert_eq!(m.build.source, Some(17));
        assert_eq!(m.build.target, Some(17));
        assert_eq!(m.build.classpath, alloc::vec!["lib/a.jar"]);
        assert_eq!(m.build.javac_flags, alloc::vec!["-Xlint:all"]);
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
        // No `[package] features`: every feature gate stays off.
        assert!(m.package.features.is_empty());
        assert!(m.feature_set().is_empty());
    }

    #[test]
    fn parses_package_features() {
        // `[package] features` lists language features by their kebab-case names — release presets
        // and individual features side by side.
        let m: Manifest = toml::from_str(
            "[package]\nfeatures = [\"module-imports\", \"compact-source-files\"]\n",
        )
        .unwrap();
        assert_eq!(
            m.package.features,
            alloc::vec![Feature::ModuleImports, Feature::CompactSourceFiles]
        );

        let m: Manifest =
            toml::from_str("[package]\nfeatures = [\"java8\", \"java25\"]\n").unwrap();
        assert_eq!(
            m.package.features,
            alloc::vec![Feature::Java8, Feature::Java25]
        );
    }

    #[test]
    fn rejects_unknown_feature() {
        // An unknown feature name is a TOML parse error (serde unknown variant), so no dedicated
        // `validate` check is needed — including releases outside the modelled preset range.
        assert!(toml::from_str::<Manifest>("[package]\nfeatures = [\"teleportation\"]\n").is_err());
        assert!(toml::from_str::<Manifest>("[package]\nfeatures = [\"java7\"]\n").is_err());
        assert!(toml::from_str::<Manifest>("[package]\nfeatures = [\"java26\"]\n").is_err());
    }

    #[test]
    fn feature_set_empty_without_features() {
        // No `[package] features`: the resolved set is empty, so every gate stays off.
        assert!(Manifest::default().feature_set().is_empty());
    }

    #[test]
    fn feature_set_expands_release_presets() {
        // java24: the two Java-25 features are still previews, so neither is enabled — but the
        // implied earlier presets are in the set (the java24 ⊇ … ⊇ java8 chain).
        let m: Manifest = toml::from_str("[package]\nfeatures = [\"java24\"]\n").unwrap();
        let fs = m.feature_set();
        assert!(!fs.contains(Feature::ModuleImports));
        assert!(!fs.contains(Feature::CompactSourceFiles));
        assert!(fs.contains(Feature::Java8));

        // java25 stabilizes both, so the preset pulls them in.
        let m: Manifest = toml::from_str("[package]\nfeatures = [\"java25\"]\n").unwrap();
        let fs = m.feature_set();
        assert!(fs.contains(Feature::ModuleImports));
        assert!(fs.contains(Feature::CompactSourceFiles));
        assert!(fs.contains(Feature::Java24));
    }

    #[test]
    fn feature_set_unions_preset_and_individual_features() {
        // `java24` plus an explicit `module-imports`: that one feature turns on without moving to
        // the next release preset, while `compact-source-files` stays off.
        let m: Manifest =
            toml::from_str("[package]\nfeatures = [\"java24\", \"module-imports\"]\n").unwrap();
        let fs = m.feature_set();
        assert!(fs.contains(Feature::ModuleImports));
        assert!(!fs.contains(Feature::CompactSourceFiles));
    }

    #[test]
    fn feature_set_from_individual_features_without_preset() {
        // Individual features may be opted into without any release preset at all.
        let m: Manifest =
            toml::from_str("[package]\nfeatures = [\"compact-source-files\"]\n").unwrap();
        let fs = m.feature_set();
        assert!(fs.contains(Feature::CompactSourceFiles));
        assert!(!fs.contains(Feature::ModuleImports));
    }

    #[test]
    fn grouped_imports_is_an_independent_dialect_feature() {
        // Enabled only by explicit opt-in; no release preset implies it (`stabilized_in` = None).
        let m: Manifest = toml::from_str("[package]\nfeatures = [\"grouped-imports\"]\n").unwrap();
        assert!(m.feature_set().contains(Feature::GroupedImports));

        // The newest release preset does NOT pull it in — it is not part of any Java version.
        let m: Manifest = toml::from_str("[package]\nfeatures = [\"java25\"]\n").unwrap();
        assert!(!m.feature_set().contains(Feature::GroupedImports));
        assert_eq!(Feature::GroupedImports.stabilized_in(), None);

        // Combinable with any other feature (the bitset unions them; nothing is implied away).
        let m: Manifest =
            toml::from_str("[package]\nfeatures = [\"java25\", \"grouped-imports\"]\n").unwrap();
        let fs = m.feature_set();
        assert!(fs.contains(Feature::GroupedImports));
        assert!(fs.contains(Feature::ModuleImports));
        assert!(fs.contains(Feature::CompactSourceFiles));
    }

    #[test]
    fn attributes_is_an_independent_dialect_feature() {
        // Enabled only by explicit opt-in; no release preset implies it (`stabilized_in` = None).
        let m: Manifest = toml::from_str("[package]\nfeatures = [\"attributes\"]\n").unwrap();
        assert!(m.feature_set().contains(Feature::Attributes));

        // The newest release preset does NOT pull it in — it is not part of any Java version.
        let m: Manifest = toml::from_str("[package]\nfeatures = [\"java25\"]\n").unwrap();
        assert!(!m.feature_set().contains(Feature::Attributes));
        assert_eq!(Feature::Attributes.stabilized_in(), None);

        // Combinable with the other dialect feature and a release preset.
        let m: Manifest = toml::from_str(
            "[package]\nfeatures = [\"java25\", \"grouped-imports\", \"attributes\"]\n",
        )
        .unwrap();
        let fs = m.feature_set();
        assert!(fs.contains(Feature::Attributes));
        assert!(fs.contains(Feature::GroupedImports));
        assert!(fs.contains(Feature::ModuleImports));

        // Like every dialect feature, the empty-set exemption does not apply.
        assert!(!FeatureSet::resolve(&[]).permits(Feature::Attributes));
        assert!(FeatureSet::resolve(&[Feature::Attributes]).permits(Feature::Attributes));
    }

    #[test]
    fn empty_feature_set_does_not_permit_a_dialect_feature() {
        // The empty-set exemption covers Java features only. A project that declares no
        // `[package] features` still cannot compile a dialect construct — the build path keys
        // desugaring off `contains`, so `javac` would see the raw syntax — and staying silent
        // would leave that with no jals-side report at all.
        let empty = FeatureSet::resolve(&[]);
        assert!(empty.is_empty());
        assert!(!empty.permits(Feature::GroupedImports));
        // Java features keep the exemption.
        assert!(empty.permits(Feature::ModuleImports));
        assert!(empty.permits(Feature::CompactSourceFiles));

        // Opting in permits it, and a non-empty set that lacks it still does not.
        assert!(FeatureSet::resolve(&[Feature::GroupedImports]).permits(Feature::GroupedImports));
        assert!(!FeatureSet::resolve(&[Feature::Java25]).permits(Feature::GroupedImports));
    }

    #[test]
    fn only_jals_constructs_are_dialect_features() {
        // Every Java release preset and release-gated feature is Java; `grouped-imports` and
        // `attributes` are the constructs javac has never heard of.
        for feature in Feature::ALL {
            assert_eq!(
                feature.is_dialect(),
                matches!(feature, Feature::GroupedImports | Feature::Attributes),
                "unexpected dialect classification for `{}`",
                feature.config_name()
            );
        }
    }

    #[test]
    fn release_presets_are_monotonic() {
        // Each preset's resolved set is a superset of its predecessor's — the `java25 ⊇ java24 ⊇ …`
        // chain, carried by the single `predecessor` edge in `implies` and the transitive closure
        // in `resolve`, checked here end to end for every predecessor pair.
        for feature in Feature::ALL {
            let Some(previous) = feature.predecessor() else {
                continue;
            };
            let earlier = FeatureSet::resolve(&[previous]);
            let later = FeatureSet::resolve(&[feature]);
            assert!(
                Feature::ALL
                    .into_iter()
                    .all(|f| !earlier.contains(f) || later.contains(f)),
                "{feature:?} must include everything {previous:?} does"
            );
        }
    }

    #[test]
    fn feature_config_name_matches_serde() {
        // Every feature parses from its `config_name` — deserialization *is* a `config_name` lookup,
        // so this is an end-to-end smoke test of the manifest wiring (and pins `ALL` as the complete
        // parseable set).
        for feature in Feature::ALL {
            let toml = alloc::format!("[package]\nfeatures = [\"{}\"]\n", feature.config_name());
            let m: Manifest = toml::from_str(&toml).unwrap();
            assert_eq!(m.package.features, alloc::vec![feature]);
        }
    }

    #[test]
    fn parses_build_features() {
        // `[features]` is an open-ended map: feature name -> the features it enables.
        let m: Manifest =
            toml::from_str("[features]\ndefault = [\"server\"]\nserver = []\nclient = []\n")
                .unwrap();
        assert_eq!(
            m.features.get("default"),
            Some(&alloc::vec!["server".to_owned()])
        );
        assert!(m.features.contains_key("server"));
        assert!(m.features.contains_key("client"));
    }

    #[test]
    fn build_features_reject_the_old_build_section() {
        // Features moved from `[build.features]` to the top-level `[features]` (Cargo's layout).
        // `Build`'s `deny_unknown_fields` turns a manifest left at the old location into a parse
        // error rather than a silently empty feature set, so the migration is loud.
        assert!(toml::from_str::<Manifest>("[build.features]\nserver = []\n").is_err());
    }

    #[test]
    fn build_features_default_selection() {
        // No selection resolves to the `default` list, closed, with the directive name dropped.
        let m: Manifest =
            toml::from_str("[features]\ndefault = [\"server\"]\nserver = []\nclient = []\n")
                .unwrap();
        let set = m.resolve_build_features(&[], false, false).unwrap();
        assert_eq!(set.into_features(), BTreeSet::from(["server".to_owned()]));
    }

    #[test]
    fn build_features_are_additive() {
        // `--features client` keeps the default `server` (a feature never subtracts) -> both.
        let m: Manifest =
            toml::from_str("[features]\ndefault = [\"server\"]\nserver = []\nclient = []\n")
                .unwrap();
        let set = m
            .resolve_build_features(&["client".to_owned()], false, false)
            .unwrap();
        assert_eq!(
            set.into_features(),
            BTreeSet::from(["client".to_owned(), "server".to_owned()])
        );
    }

    #[test]
    fn build_features_all_and_no_default() {
        let m: Manifest =
            toml::from_str("[features]\ndefault = [\"server\"]\nserver = []\nclient = []\n")
                .unwrap();
        // `--all-features`: every declared feature, the `default` directive dropped.
        let all = m.resolve_build_features(&[], true, false).unwrap();
        assert_eq!(
            all.into_features(),
            BTreeSet::from(["client".to_owned(), "server".to_owned()])
        );
        // `--no-default-features --features client`: client alone.
        let client_only = m
            .resolve_build_features(&["client".to_owned()], false, true)
            .unwrap();
        assert_eq!(
            client_only.into_features(),
            BTreeSet::from(["client".to_owned()])
        );
        // `--all-features` overrides `--no-default-features`.
        let both = m.resolve_build_features(&[], true, true).unwrap();
        assert_eq!(
            both.into_features(),
            BTreeSet::from(["client".to_owned(), "server".to_owned()])
        );
    }

    #[test]
    fn build_features_closure_over_enables() {
        // A feature that enables others pulls them in transitively.
        let m: Manifest =
            toml::from_str("[features]\ndefault = []\nfull = [\"a\"]\na = [\"b\"]\nb = []\n")
                .unwrap();
        let set = m
            .resolve_build_features(&["full".to_owned()], false, false)
            .unwrap();
        assert_eq!(
            set.into_features(),
            BTreeSet::from(["a".to_owned(), "b".to_owned(), "full".to_owned()])
        );
    }

    #[test]
    fn build_features_unknown_selected_is_error() {
        let m: Manifest = toml::from_str("[features]\nserver = []\n").unwrap();
        assert_eq!(
            m.resolve_build_features(&["client".to_owned()], false, false),
            Err(BuildFeatureError::UnknownSelected {
                name: "client".to_owned()
            })
        );
    }

    #[test]
    fn build_features_undeclared_reference_is_validate_error() {
        // `default` references `server`, which is not itself a declared feature.
        let m: Manifest = toml::from_str("[features]\ndefault = [\"server\"]\n").unwrap();
        assert_eq!(
            m.validate(),
            Err(ValidationError::UndeclaredBuildFeature {
                feature: "default".to_owned(),
                enables: "server".to_owned(),
            })
        );
    }

    /// A manifest with one `path` dependency and one `jar`, for the cross-package form.
    fn cross_manifest(features: &str) -> Manifest {
        toml::from_str(&alloc::format!(
            "[features]\n{features}\n\
             [dependencies]\n\
             render = {{ path = \"../render\" }}\n\
             gson = {{ jar = \"libs/gson.jar\" }}\n"
        ))
        .unwrap()
    }

    #[test]
    fn feature_refs_classify_by_shape() {
        assert_eq!(FeatureRef::parse("gpu"), Ok(FeatureRef::Local("gpu")));
        assert_eq!(
            FeatureRef::parse("render/vulkan"),
            Ok(FeatureRef::Dependency {
                dependency: "render",
                feature: "vulkan",
            })
        );
        // Empty either side, and a second `/`: forwarding through an intermediate package is that
        // package's own business, so `a/b/c` is a typo rather than a chain.
        assert_eq!(FeatureRef::parse(""), Err(FeatureRefError::Empty));
        assert_eq!(FeatureRef::parse("render/"), Err(FeatureRefError::Empty));
        assert_eq!(FeatureRef::parse("/vulkan"), Err(FeatureRefError::Empty));
        assert_eq!(
            FeatureRef::parse("a/b/c"),
            Err(FeatureRefError::NestedSeparator)
        );
        // The directive is never enableable by name, from any direction.
        assert_eq!(
            FeatureRef::parse("render/default"),
            Err(FeatureRefError::ReservedFeature)
        );
    }

    #[test]
    fn cross_package_features_are_routed_not_queryable() {
        // `gpu` is this project's feature; `render/vulkan` is a directive aimed at the dependency,
        // so it must not come back as something `build.feature("…")` could see.
        let m =
            cross_manifest("default = [\"gpu\"]\ngpu = [\"render/vulkan\", \"fast\"]\nfast = []");
        m.validate().unwrap();
        let resolved = m.resolve_build_features(&[], false, false).unwrap();
        assert_eq!(
            resolved.features(),
            &BTreeSet::from(["fast".to_owned(), "gpu".to_owned()])
        );
        assert_eq!(
            resolved.dependency("render"),
            Some(&BTreeSet::from(["vulkan".to_owned()]))
        );
        assert_eq!(resolved.dependency("gson"), None);
    }

    #[test]
    fn cross_package_features_are_selectable_on_the_command_line() {
        // Cargo's `--features serde/std`: routed exactly like a `[features]` entry, and the local
        // set stays empty because nothing local was selected.
        let m = cross_manifest("gpu = []");
        let resolved = m
            .resolve_build_features(&["render/vulkan".to_owned()], false, true)
            .unwrap();
        assert!(resolved.features().is_empty());
        assert_eq!(
            resolved.dependency("render"),
            Some(&BTreeSet::from(["vulkan".to_owned()]))
        );
        // A dependency that cannot run a build script, and one that does not exist at all, are the
        // same error: neither can receive a feature.
        assert_eq!(
            m.resolve_build_features(&["gson/pretty".to_owned()], false, false),
            Err(BuildFeatureError::UnknownSelectedDependency {
                name: "gson/pretty".to_owned(),
                dependency: "gson".to_owned(),
            })
        );
        assert_eq!(
            m.resolve_build_features(&["nope/x".to_owned()], false, false),
            Err(BuildFeatureError::UnknownSelectedDependency {
                name: "nope/x".to_owned(),
                dependency: "nope".to_owned(),
            })
        );
        assert_eq!(
            m.resolve_build_features(&["render/default".to_owned()], false, false),
            Err(BuildFeatureError::InvalidSelected {
                name: "render/default".to_owned(),
                reason: FeatureRefError::ReservedFeature,
            })
        );
    }

    #[test]
    fn all_features_reaches_cross_package_entries() {
        // `--all-features` seeds every declared key, so whatever they route is routed too.
        let m = cross_manifest("gpu = [\"render/vulkan\"]\naudio = [\"render/openal\"]");
        assert_eq!(
            m.resolve_build_features(&[], true, false)
                .unwrap()
                .dependency("render"),
            Some(&BTreeSet::from(["openal".to_owned(), "vulkan".to_owned()]))
        );
    }

    #[test]
    fn cross_package_features_validate_their_dependency() {
        assert_eq!(
            cross_manifest("gpu = [\"missing/x\"]").validate(),
            Err(ValidationError::UndeclaredFeatureDependency {
                feature: "gpu".to_owned(),
                entry: "missing/x".to_owned(),
                dependency: "missing".to_owned(),
            })
        );
        // A jar contributes compiled classes and runs no build script, so nothing there could read
        // the feature — reject it rather than resolve to a silent no-op.
        assert_eq!(
            cross_manifest("gpu = [\"gson/pretty\"]").validate(),
            Err(ValidationError::BinaryFeatureDependency {
                feature: "gpu".to_owned(),
                entry: "gson/pretty".to_owned(),
                dependency: "gson".to_owned(),
            })
        );
        assert_eq!(
            cross_manifest("gpu = [\"render/\"]").validate(),
            Err(ValidationError::InvalidFeatureRef {
                feature: "gpu".to_owned(),
                entry: "render/".to_owned(),
                reason: FeatureRefError::Empty,
            })
        );
    }

    #[test]
    fn a_feature_name_carrying_the_separator_is_rejected() {
        // Every list entry containing `/` is read as a cross-package reference, so such a key could
        // never be enabled — reject the declaration instead of leaving dead config behind.
        let m: Manifest = toml::from_str("[features]\n\"a/b\" = []\n").unwrap();
        assert_eq!(
            m.validate(),
            Err(ValidationError::InvalidBuildFeatureName {
                feature: "a/b".to_owned(),
            })
        );
    }

    #[test]
    fn a_dependency_features_list_rejects_the_cross_package_form() {
        // That list names features of the dependency itself, as in Cargo. Routing onward is written
        // in the receiving project's own `[features]`, so `a/b` here is a mistake, not a route.
        let m: Manifest = toml::from_str(
            "[dependencies]\nrender = { path = \"../render\", features = [\"shader/spirv\"] }\n",
        )
        .unwrap();
        assert_eq!(
            m.validate(),
            Err(ValidationError::Dependency(DependencyError::CrossFeature {
                name: "render".to_owned(),
                feature: "shader/spirv".to_owned(),
            }))
        );
    }

    #[test]
    fn default_features_defaults_to_true_and_is_source_only() {
        let m: Manifest = toml::from_str(
            "[dependencies]\n\
             plain = { path = \"../plain\" }\n\
             bare = { path = \"../bare\", default-features = false }\n",
        )
        .unwrap();
        assert!(m.dependencies["plain"].default_features());
        assert!(!m.dependencies["bare"].default_features());
        // A jar has no build script, so the key does not exist on that form at all.
        assert!(
            toml::from_str::<Manifest>(
                "[dependencies]\nlib = { jar = \"libs/x.jar\", default-features = false }\n"
            )
            .is_err()
        );
    }

    #[test]
    fn expand_build_features_closes_an_arriving_set() {
        // What a dependency node goes through: an arriving set closed over its own table, with its
        // `default` list applied only when an incoming edge allows it. Names it never declared stay
        // queryable — a project may know more about a dependency than its own table does.
        let m: Manifest = toml::from_str(
            "[features]\ndefault = [\"soft\"]\nsoft = []\nvulkan = [\"shader/spirv\", \"gpu\"]\n\
             gpu = []\n[dependencies]\nshader = { path = \"../shader\" }\n",
        )
        .unwrap();
        m.validate().unwrap();
        let with_defaults = m.expand_build_features(["vulkan".to_owned()], true);
        assert_eq!(
            with_defaults.features(),
            &BTreeSet::from(["gpu".to_owned(), "soft".to_owned(), "vulkan".to_owned()])
        );
        assert_eq!(
            with_defaults.dependency("shader"),
            Some(&BTreeSet::from(["spirv".to_owned()]))
        );
        let suppressed = m.expand_build_features(["vulkan".to_owned(), "opaque".to_owned()], false);
        assert_eq!(
            suppressed.features(),
            &BTreeSet::from(["gpu".to_owned(), "opaque".to_owned(), "vulkan".to_owned()])
        );
    }

    #[test]
    fn parses_toolchain_section() {
        use crate::toolchain::{Compiler, Distribution, Runtime};

        let m: Manifest = toml::from_str(
            "[toolchain]\n\
             compiler = { distribution = { name = \"temurin\", version = 21 } }\n\
             runtime = { path = \"/opt/jdk-17/bin/java\" }\n",
        )
        .unwrap();
        assert_eq!(
            m.toolchain.compiler,
            Compiler::Distribution(Distribution {
                name: Some("temurin".into()),
                version: Some(21),
            })
        );
        assert_eq!(
            m.toolchain.runtime,
            Runtime::Path("/opt/jdk-17/bin/java".into())
        );

        // No [toolchain] table: both selections default to the system tools.
        let m = Manifest::default();
        assert_eq!(m.toolchain.compiler, Compiler::System);
        assert_eq!(m.toolchain.runtime, Runtime::System);
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
            fromgit = { git = "https://github.com/x/y", tag = "v1.2", dir = "core/src/main/java", features = ["hello"] }
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
                features: vec!["hello".to_owned()],
                default_features: None,
            }))
        );
        assert_eq!(
            m.dependencies.get("frompath"),
            Some(&Dependency::Path(PathDependency {
                path: "../sibling".to_owned(),
                dir: None,
                features: Vec::new(),
                default_features: None,
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
            features: Vec::new(),
            default_features: None,
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

    /// These values are passed to the `git` CLI. A leading `-` makes git read them as options:
    /// `git checkout --quiet -f` exits 0 without switching refs, so the dependency would silently
    /// resolve to the default branch instead of the one the manifest asked for.
    #[test]
    fn rejects_option_like_git_values() {
        for (field, toml) in [
            ("git", r#"repo = { git = "--upload-pack=touch /tmp/x" }"#),
            (
                "branch",
                r#"repo = { git = "https://example.invalid/r.git", branch = "-f" }"#,
            ),
            (
                "tag",
                r#"repo = { git = "https://example.invalid/r.git", tag = "--x" }"#,
            ),
            (
                "rev",
                r#"repo = { git = "https://example.invalid/r.git", rev = "-abc" }"#,
            ),
        ] {
            let manifest: Manifest =
                toml::from_str(&alloc::format!("[dependencies]\n{toml}\n")).unwrap();
            let error = manifest
                .dependencies
                .get("repo")
                .unwrap()
                .validate("repo")
                .unwrap_err();
            assert!(
                matches!(&error, DependencyError::OptionLike { field: f, .. } if *f == field),
                "`{field}` must be rejected, got {error:?}"
            );
        }
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
                        features: Vec::new(),
                        default_features: None,
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
                features: Vec::new(),
                default_features: None,
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
    fn dependency_features_are_read_off_both_source_forms() {
        // The graph builders reach every form through this one accessor, so a `jar` — which has no
        // build script to query them — has to answer with an empty list rather than be special-cased
        // at each call site.
        let m: Manifest = toml::from_str(
            r#"
            [dependencies]
            g = { git = "https://github.com/x/y", features = ["hello"] }
            p = { path = "../sibling", features = ["hello", "world"] }
            j = { jar = "libs/x.jar" }
            "#,
        )
        .unwrap();
        assert_eq!(m.validate(), Ok(()));
        assert_eq!(m.dependencies["g"].features(), ["hello"]);
        assert_eq!(m.dependencies["p"].features(), ["hello", "world"]);
        assert!(m.dependencies["j"].features().is_empty());
    }

    #[test]
    fn jar_dependency_rejects_features_at_parse() {
        // A jar contributes compiled classes and never runs a build script, so `features` on it
        // could only ever be a silent no-op. `JarDependency` simply does not carry the field, which
        // the untagged variants' `deny_unknown_fields` turns into a parse error for free.
        let parsed: Result<Manifest, _> = toml::from_str(
            "[dependencies]\nj = { jar = \"libs/x.jar\", features = [\"hello\"] }\n",
        );
        assert!(
            parsed.is_err(),
            "`features` on a jar dependency should not parse"
        );
    }

    #[test]
    fn dependency_features_reject_empty_and_reserved_names() {
        let empty: Manifest =
            toml::from_str("[dependencies]\np = { path = \"../s\", features = [\"\"] }\n").unwrap();
        assert_eq!(
            empty.validate(),
            Err(ValidationError::Dependency(DependencyError::Empty {
                name: "p".to_owned(),
                field: "features",
            }))
        );

        // Nothing expands a dependency's list, so accepting `default` here would be the one way to
        // make `build.feature("default")` true anywhere.
        let reserved: Manifest =
            toml::from_str("[dependencies]\np = { path = \"../s\", features = [\"default\"] }\n")
                .unwrap();
        assert_eq!(
            reserved.validate(),
            Err(ValidationError::Dependency(
                DependencyError::ReservedFeature {
                    name: "p".to_owned(),
                    feature: "default".to_owned(),
                }
            ))
        );
    }

    #[test]
    fn absent_frontend_defaults_to_vanilla() {
        let m: Manifest = toml::from_str("[build]\nrelease = 21\n").unwrap();
        assert_eq!(m.build.frontend, FrontendKind::Vanilla {});
    }

    #[test]
    fn explicit_vanilla_frontend_parses() {
        // The internally-tagged enum with `deny_unknown_fields` has serde edge cases, so the
        // explicit stanza is exercised directly rather than assumed to inherit BuildScript's
        // behavior.
        let m: Manifest = toml::from_str("[build.frontend]\ntype = \"vanilla\"\n").unwrap();
        assert_eq!(m.build.frontend, FrontendKind::Vanilla {});
        assert_eq!(m.build.frontend.tag_name(), "vanilla");
    }

    #[test]
    fn unknown_frontend_type_is_a_parse_error() {
        // The closed enum is the extension seam: an unimplemented frontend must be rejected at
        // parse, not silently accepted or defaulted.
        let parsed: Result<Manifest, _> = toml::from_str("[build.frontend]\ntype = \"macro\"\n");
        assert!(parsed.is_err(), "unknown frontend type should not parse");
    }

    #[test]
    fn frontend_rejects_unknown_field() {
        let parsed: Result<Manifest, _> =
            toml::from_str("[build.frontend]\ntype = \"vanilla\"\nbogus = 1\n");
        assert!(parsed.is_err(), "unknown frontend field should not parse");
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
