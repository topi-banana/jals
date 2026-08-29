//! Typed, declarative tasks recorded by a build script for later host execution.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::fmt;

use jals_storage::RelativePath;
use rhai::{Array, Dynamic, Engine, EvalAltResult, INT, ImmutableString, Position};
use serde::{Deserialize, Serialize};

/// Limits for one declarative build-task graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_field_names)] // `max_*` names each cap a distinct quantity; the prefix is intentional.
pub struct TaskPlanLimits {
    pub(crate) max_tasks: usize,
    pub(crate) max_edges: usize,
    pub(crate) max_literal_bytes: usize,
    pub(crate) max_terminals: usize,
    pub(crate) max_publication_roots: usize,
    pub(crate) max_path_bytes: usize,
    pub(crate) max_path_depth: usize,
    /// Ceiling on any single fetch's declared size.
    ///
    /// A fetch buffers up to its byte count before the digest is checked, and the count can come
    /// from the fetched JSON itself (`tasks.json_u64`). Without a ceiling, a compromised or
    /// mistaken upstream could name a size that exhausts memory or disk.
    pub(crate) max_fetch_bytes: u64,
}

/// Stable index of a task node in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(u32);

impl TaskId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The typed value produced by a task node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskValueKind {
    Url,
    Digest,
    ByteCount,
    Json,
    Text,
    Jar,
    SourceTree,
    /// Fetched bytes with no interpretation — an image, a font, a data file.
    ///
    /// Separate from [`Jar`](Self::Jar) because a jar is a thing this crate opens: it can be put on
    /// a classpath, remapped, merged, unpacked. None of that is true of a PNG, and a value kind
    /// that admitted both would let `add_classpath` take one.
    Blob,
    /// A tree of arbitrary files, addressed by relative path.
    ///
    /// Distinct from [`SourceTree`](Self::SourceTree) because their *sinks* are different, not
    /// their shape: a source tree is published into the project's own source roots, and a file tree
    /// is materialized into a directory some process reads. Letting one stand for the other would
    /// let a native library be published as project source.
    FileTree,
}

/// Digest algorithm used to authenticate fetched bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskDigestAlgorithm {
    Sha1,
    Sha256,
}

impl TaskDigestAlgorithm {
    const fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

/// Format expected from a verified fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskFetchKind {
    /// Bytes fetched and not interpreted.
    Bytes,
    Json,
    Jar,
    Text,
}

/// The grammar a [`TaskNodeKind::RemapJar`]'s mapping text is written in.
///
/// The plan's own spelling of `jals_classpath::MappingFormat`: this crate is the portable IR and
/// does not depend on the implementation, and the wire name here is frozen by written cache records
/// while that enum's is frozen by nothing. Keeping them separate is the same split
/// [`TaskPublishIntent`] makes between its serde tag and its script keyword.
///
/// `Proguard` stays a unit variant so its wire form stays the bare string `"proguard"` that records
/// written before tiny v2 existed carry; serde encodes the struct variant beside it as
/// `{"tiny-v2": {…}}`, which those records never contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskMappingFormat {
    /// The ProGuard-style text Mojang publishes per Minecraft release.
    Proguard,
    /// The tab-separated tiny v2 text Fabric publishes, read through one pair of its namespaces.
    ///
    /// The pair travels with the format because it is part of *which* renaming a node performs, so
    /// it reaches the cache key the same way the format does — two pairs over one mapping text are
    /// two different jars.
    TinyV2 {
        /// The namespace a deobfuscating remap reads names from.
        from: String,
        /// The namespace it writes names to.
        to: String,
    },
}

/// Which way a [`TaskNodeKind::RemapJar`] applies its mapping file.
///
/// One file describes a pair of namespaces; this says which of them the jar being remapped is
/// written in. It is part of the node rather than inferred because a jar in the wrong namespace
/// remaps to nothing and still produces a plausible archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskRemapDirection {
    /// Obfuscated → official: a shipped library becomes the names a project is written against.
    Deobfuscate,
    /// Official → obfuscated: compiled output becomes the names its runtime loads.
    Reobfuscate,
}

/// One value-producing node in a task plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskNodeKind {
    HttpsUrl {
        value: String,
    },
    ProjectJar {
        path: String,
    },
    /// UTF-8 text read from the immutable project snapshot.
    ///
    /// The counterpart of `ProjectJar` for the one other kind of bytes a plan consumes. Without it
    /// a mapping file checked into the repository cannot be used at all — the only producer of
    /// [`TaskValueKind::Text`] was an HTTPS fetch, which is the wrong shape for something already
    /// under version control.
    ProjectText {
        path: String,
    },
    Digest {
        algorithm: TaskDigestAlgorithm,
        value: String,
    },
    ByteCount {
        value: u64,
    },
    Fetch {
        kind: TaskFetchKind,
        url: TaskId,
        digest: TaskId,
        max_bytes: TaskId,
    },
    JsonAt {
        json: TaskId,
        path: Vec<String>,
    },
    JsonFindString {
        json: TaskId,
        path: Vec<String>,
        field: String,
        value: String,
    },
    JsonUrl {
        json: TaskId,
        path: Vec<String>,
    },
    JsonDigest {
        json: TaskId,
        path: Vec<String>,
        algorithm: TaskDigestAlgorithm,
    },
    JsonU64 {
        json: TaskId,
        path: Vec<String>,
    },
    ExtractJava {
        jar: TaskId,
        prefix: String,
    },
    NestedJar {
        jar: TaskId,
        member: String,
    },
    RemapJar {
        jar: TaskId,
        mappings: TaskId,
        format: TaskMappingFormat,
        direction: TaskRemapDirection,
        /// Jars read for their class hierarchy only — never remapped, never in the output.
        ///
        /// A jar whose supertypes live elsewhere cannot be remapped correctly from itself alone:
        /// an inherited member resolves against a supertype nobody declared, misses, and keeps its
        /// source name in an otherwise remapped archive.
        hierarchy: Vec<TaskId>,
    },
    MergeJars {
        base: TaskId,
        overlay: TaskId,
    },
    DecompileJava {
        jar: TaskId,
        prefix: String,
    },
    /// Every member of an archive below `prefix`, whatever its extension.
    ///
    /// The unfiltered sibling of `ExtractJava`. What a runtime directory holds is decided by the
    /// thing being run — a `.so`, a `.png`, a `.json` — so filtering by extension here would be
    /// this crate deciding what some other program needs.
    ExtractFiles {
        archive: TaskId,
        prefix: String,
    },
    /// One fetched blob as a one-file tree, at the path a runtime expects to find it under.
    ///
    /// The composition partner of `MergeTrees`: a store addressed by digest — a Minecraft asset
    /// index, a Maven-style layout — is many individually fetched files whose *placement* the
    /// consumer decides, and this is the smallest node that expresses "this blob goes here".
    PlaceFile {
        blob: TaskId,
        path: String,
    },
    /// Two file trees as one, with `overlay` winning a shared path.
    ///
    /// Needed because a runtime directory is usually assembled from several archives — the natives
    /// a launcher extracts come one jar per library — and a terminal takes one tree.
    MergeTrees {
        base: TaskId,
        overlay: TaskId,
    },
}

impl TaskNodeKind {
    const fn output_kind(&self) -> TaskValueKind {
        match self {
            Self::HttpsUrl { .. } | Self::JsonUrl { .. } => TaskValueKind::Url,
            Self::Digest { .. } | Self::JsonDigest { .. } => TaskValueKind::Digest,
            Self::ByteCount { .. } | Self::JsonU64 { .. } => TaskValueKind::ByteCount,
            Self::Fetch {
                kind: TaskFetchKind::Json,
                ..
            }
            | Self::JsonAt { .. }
            | Self::JsonFindString { .. } => TaskValueKind::Json,
            Self::Fetch {
                kind: TaskFetchKind::Text,
                ..
            }
            | Self::ProjectText { .. } => TaskValueKind::Text,
            Self::Fetch {
                kind: TaskFetchKind::Jar,
                ..
            }
            | Self::ProjectJar { .. }
            | Self::RemapJar { .. }
            | Self::MergeJars { .. }
            | Self::NestedJar { .. } => TaskValueKind::Jar,
            Self::ExtractJava { .. } | Self::DecompileJava { .. } => TaskValueKind::SourceTree,
            Self::ExtractFiles { .. } | Self::MergeTrees { .. } | Self::PlaceFile { .. } => {
                TaskValueKind::FileTree
            }
            Self::Fetch {
                kind: TaskFetchKind::Bytes,
                ..
            } => TaskValueKind::Blob,
        }
    }

    fn inputs(&self) -> Vec<(TaskId, TaskValueKind)> {
        match self {
            Self::HttpsUrl { .. }
            | Self::ProjectJar { .. }
            | Self::ProjectText { .. }
            | Self::Digest { .. }
            | Self::ByteCount { .. } => Vec::new(),
            Self::Fetch {
                url,
                digest,
                max_bytes,
                ..
            } => vec![
                (*url, TaskValueKind::Url),
                (*digest, TaskValueKind::Digest),
                (*max_bytes, TaskValueKind::ByteCount),
            ],
            Self::JsonAt { json, .. }
            | Self::JsonFindString { json, .. }
            | Self::JsonUrl { json, .. }
            | Self::JsonDigest { json, .. }
            | Self::JsonU64 { json, .. } => vec![(*json, TaskValueKind::Json)],
            Self::ExtractJava { jar, .. } | Self::DecompileJava { jar, .. } => {
                vec![(*jar, TaskValueKind::Jar)]
            }
            Self::NestedJar { jar, .. } => vec![(*jar, TaskValueKind::Jar)],
            Self::RemapJar {
                jar,
                mappings,
                hierarchy,
                ..
            } => {
                let mut inputs = vec![(*jar, TaskValueKind::Jar), (*mappings, TaskValueKind::Text)];
                inputs.extend(hierarchy.iter().map(|id| (*id, TaskValueKind::Jar)));
                inputs
            }
            Self::MergeJars { base, overlay } => {
                vec![(*base, TaskValueKind::Jar), (*overlay, TaskValueKind::Jar)]
            }
            Self::ExtractFiles { archive, .. } => vec![(*archive, TaskValueKind::Jar)],
            Self::PlaceFile { blob, .. } => vec![(*blob, TaskValueKind::Blob)],
            Self::MergeTrees { base, overlay } => vec![
                (*base, TaskValueKind::FileTree),
                (*overlay, TaskValueKind::FileTree),
            ],
        }
    }

    /// Input node IDs in semantic argument order.
    pub fn input_ids(&self) -> Vec<TaskId> {
        self.inputs().into_iter().map(|(id, _)| id).collect()
    }

    fn literal_bytes(&self) -> usize {
        match self {
            Self::HttpsUrl { value }
            | Self::ProjectJar { path: value }
            | Self::ProjectText { path: value }
            | Self::Digest { value, .. } => value.len(),
            Self::MergeTrees { .. }
            | Self::ByteCount { .. }
            | Self::Fetch { .. }
            | Self::JsonU64 { .. }
            | Self::MergeJars { .. } => 0,
            // A remap's only literals are the namespace names a tiny v2 format selects.
            Self::RemapJar { format, .. } => match format {
                TaskMappingFormat::Proguard => 0,
                TaskMappingFormat::TinyV2 { from, to } => from.len() + to.len(),
            },
            Self::JsonAt { path, .. }
            | Self::JsonUrl { path, .. }
            | Self::JsonDigest { path, .. } => path.iter().map(String::len).sum(),
            Self::JsonFindString {
                path, field, value, ..
            } => path.iter().map(String::len).sum::<usize>() + field.len() + value.len(),
            Self::ExtractJava { prefix, .. }
            | Self::DecompileJava { prefix, .. }
            | Self::ExtractFiles { prefix, .. } => prefix.len(),
            Self::PlaceFile { path, .. } => path.len(),
            Self::NestedJar { member, .. } => member.len(),
        }
    }
}

/// A node with its canonical declaration-order identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskNode {
    pub id: TaskId,
    pub kind: TaskNodeKind,
}

/// How a source tree is published into the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskPublishMode {
    ReplaceRoot,
}

/// What a published source tree is *for*, which is the whole of what a consumer does with it.
///
/// Deliberately not a [`TaskPublishMode`] variant: that axis is how the destination is written, and
/// it applies identically either way — a `replace-root` publication owns its destination whoever
/// ends up reading it. The two would multiply rather than merge.
///
/// The distinction only becomes visible when the project is a *dependency*. As the root a
/// publication is written to disk and compiles like an authored file in both cases; a consumer sees
/// either types it compiles or types it reads, and nothing in the task graph could infer which was
/// meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskPublishIntent {
    /// The tree carries types nothing else does, so a consumer compiles it.
    Compile,
    /// The tree is a *view* of types the classpath already defines, so a consumer only reads it.
    /// Handing `javac` both a decompiled tree and the JAR it came from is how a working build
    /// acquires duplicates.
    Navigation,
}

impl TaskPublishIntent {
    /// The intent a build script spelled, or `None` for anything else.
    ///
    /// Written out rather than derived from the serde representation because the two are
    /// independent surfaces: the wire name is frozen by every cache record already written under
    /// it, and the script keyword is frozen by every `build.rhai` in the wild.
    ///
    /// Private because the script keyword has exactly one reader — the Rhai binding below. A
    /// consumer of a `TaskPlan` receives the parsed intent and never the word.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "compile" => Some(Self::Compile),
            "navigation" => Some(Self::Navigation),
            _ => None,
        }
    }
}

/// A side effect requested from the host after all value nodes succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TaskTerminal {
    AddClasspath {
        jar: TaskId,
    },
    /// Expand every `.jar` member of `jar` onto the root classpath (used for library bundlers).
    AddNestedClasspath {
        jar: TaskId,
    },
    PublishTree {
        owner: String,
        tree: TaskId,
        destination: String,
        mode: TaskPublishMode,
        intent: TaskPublishIntent,
    },
    /// Materialize `tree` as a directory a `[[test-target]]` names with `{dir:<name>}`.
    ///
    /// A terminal rather than a value, for the reason every terminal is one: it is where something
    /// *leaves* the plan. What it leaves as is a directory on disk, which is the one thing a plan
    /// cannot describe — its address is the digest of its contents, and nothing knows that until
    /// the task has run.
    AddRuntimeDir {
        name: String,
        tree: TaskId,
    },
}

impl TaskTerminal {
    const fn input(&self) -> (TaskId, TaskValueKind) {
        match self {
            Self::AddClasspath { jar } | Self::AddNestedClasspath { jar } => {
                (*jar, TaskValueKind::Jar)
            }
            Self::PublishTree { tree, .. } => (*tree, TaskValueKind::SourceTree),
            Self::AddRuntimeDir { tree, .. } => (*tree, TaskValueKind::FileTree),
        }
    }

    /// The single value node consumed by this terminal.
    pub const fn input_id(&self) -> TaskId {
        self.input().0
    }

    const fn literal_bytes(&self) -> usize {
        match self {
            Self::AddClasspath { .. } | Self::AddNestedClasspath { .. } => 0,
            Self::PublishTree {
                owner, destination, ..
            } => owner.len() + destination.len(),
            Self::AddRuntimeDir { name, .. } => name.len(),
        }
    }
}

/// Canonical task graph recorded by one successful build-script evaluation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPlan {
    pub nodes: Vec<TaskNode>,
    pub terminals: Vec<TaskTerminal>,
}

impl TaskPlan {
    pub const fn new() -> Self {
        Self {
            nodes: Vec::new(),
            terminals: Vec::new(),
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.terminals.is_empty()
    }

    pub fn node(&self, id: TaskId) -> Option<&TaskNode> {
        self.nodes.get(id.index())
    }

    pub(crate) fn validate(&self, limits: TaskPlanLimits) -> Result<(), TaskPlanError> {
        if self.nodes.len() > limits.max_tasks {
            return Err(TaskPlanError::Limit("task count"));
        }
        if self.terminals.len() > limits.max_terminals {
            return Err(TaskPlanError::Limit("terminal count"));
        }
        let mut cost = PlanCost::default();
        for (index, node) in self.nodes.iter().enumerate() {
            if node.id.index() != index {
                return Err(TaskPlanError::NonCanonicalNodeId);
            }
            cost = cost.add(self.node_cost(index, &node.kind, limits)?)?;
        }
        for terminal in &self.terminals {
            cost = cost.add(self.terminal_cost(terminal, limits)?)?;
        }
        cost.check(limits)
    }

    /// Validate the node at `index` against the nodes before it, returning what it adds to the
    /// plan's totals.
    ///
    /// Split out of [`Self::validate`] so a builder can check one declaration at a time. Nodes are
    /// append-only and may only reference earlier ones, so an already-valid prefix stays valid.
    fn node_cost(
        &self,
        index: usize,
        kind: &TaskNodeKind,
        limits: TaskPlanLimits,
    ) -> Result<PlanCost, TaskPlanError> {
        let inputs = kind.inputs();
        let edges = inputs.len();
        for (input, expected) in inputs {
            let Some(dependency) = self.node(input) else {
                return Err(TaskPlanError::MissingInput(input));
            };
            if input.index() >= index {
                return Err(TaskPlanError::ForwardReference(input));
            }
            let actual = dependency.kind.output_kind();
            if actual != expected {
                return Err(TaskPlanError::TypeMismatch {
                    task: input,
                    expected,
                    actual,
                });
            }
        }
        Self::validate_node(kind, limits)?;
        Ok(PlanCost {
            edges,
            literal_bytes: kind.literal_bytes(),
            publication_roots: 0,
        })
    }

    /// Validate the terminal at `index`, returning what it adds to the plan's totals.
    fn terminal_cost(
        &self,
        terminal: &TaskTerminal,
        limits: TaskPlanLimits,
    ) -> Result<PlanCost, TaskPlanError> {
        let (input, expected) = terminal.input();
        let Some(node) = self.node(input) else {
            return Err(TaskPlanError::MissingInput(input));
        };
        let actual = node.kind.output_kind();
        if actual != expected {
            return Err(TaskPlanError::TypeMismatch {
                task: input,
                expected,
                actual,
            });
        }
        let mut publication_roots = 0;
        if let TaskTerminal::AddRuntimeDir { name, .. } = terminal {
            // The name is spelled inside `{dir:…}` and nowhere else, so it takes the vocabulary
            // that placeholder can express: anything with a `}` in it could not be written, and
            // anything empty could not be referred to.
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(TaskPlanError::InvalidRuntimeDirName);
            }
        }
        if let TaskTerminal::PublishTree {
            owner, destination, ..
        } = terminal
        {
            publication_roots = 1;
            if owner.is_empty() {
                return Err(TaskPlanError::InvalidOwner);
            }
            Self::validate_path(destination, limits, false)?;
        }
        Ok(PlanCost {
            edges: 1,
            literal_bytes: terminal.literal_bytes(),
            publication_roots,
        })
    }

    fn validate_node(kind: &TaskNodeKind, limits: TaskPlanLimits) -> Result<(), TaskPlanError> {
        match kind {
            TaskNodeKind::HttpsUrl { value } => {
                if !value.starts_with("https://")
                    || value.bytes().any(|byte| byte.is_ascii_whitespace())
                {
                    return Err(TaskPlanError::InvalidHttpsUrl);
                }
            }
            TaskNodeKind::ProjectJar { path } | TaskNodeKind::ProjectText { path } => {
                Self::validate_path(path, limits, false)?;
            }
            TaskNodeKind::Digest { algorithm, value } => {
                if value.len() != algorithm.hex_len()
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return Err(TaskPlanError::InvalidDigest);
                }
            }
            TaskNodeKind::ByteCount { value } if *value == 0 || *value > limits.max_fetch_bytes => {
                return Err(TaskPlanError::InvalidByteCount);
            }
            TaskNodeKind::ExtractJava { prefix, .. }
            | TaskNodeKind::DecompileJava { prefix, .. }
            | TaskNodeKind::ExtractFiles { prefix, .. } => {
                Self::validate_path(prefix, limits, true)?;
            }
            TaskNodeKind::NestedJar { member, .. }
            | TaskNodeKind::PlaceFile { path: member, .. } => {
                Self::validate_path(member, limits, false)?;
            }
            TaskNodeKind::JsonAt { path, .. }
            | TaskNodeKind::JsonFindString { path, .. }
            | TaskNodeKind::JsonUrl { path, .. }
            | TaskNodeKind::JsonDigest { path, .. }
            | TaskNodeKind::JsonU64 { path, .. } => {
                if path.iter().any(String::is_empty) {
                    return Err(TaskPlanError::InvalidJsonPath);
                }
            }
            TaskNodeKind::ByteCount { .. }
            | TaskNodeKind::Fetch { .. }
            | TaskNodeKind::RemapJar { .. }
            | TaskNodeKind::MergeJars { .. }
            | TaskNodeKind::MergeTrees { .. } => {}
        }
        Ok(())
    }

    fn validate_path(
        value: &str,
        limits: TaskPlanLimits,
        allow_root: bool,
    ) -> Result<(), TaskPlanError> {
        if value.len() > limits.max_path_bytes
            || Self::path_depth(value) > limits.max_path_depth
            || (!allow_root && value.is_empty())
        {
            return Err(TaskPlanError::InvalidPath);
        }
        let path = RelativePath::parse(value).map_err(|_| TaskPlanError::InvalidPath)?;
        if !allow_root && path.is_root() {
            return Err(TaskPlanError::InvalidPath);
        }
        Ok(())
    }

    fn path_depth(path: &str) -> usize {
        if path.is_empty() {
            0
        } else {
            path.bytes()
                .filter(|byte| *byte == b'/')
                .count()
                .saturating_add(1)
        }
    }
}

/// Invalid or over-limit task plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskPlanError {
    Limit(&'static str),
    NonCanonicalNodeId,
    MissingInput(TaskId),
    ForwardReference(TaskId),
    TypeMismatch {
        task: TaskId,
        expected: TaskValueKind,
        actual: TaskValueKind,
    },
    InvalidHttpsUrl,
    InvalidDigest,
    InvalidByteCount,
    InvalidJsonPath,
    InvalidPath,
    InvalidOwner,
    /// A `tasks.add_runtime_dir` name a `{dir:…}` placeholder could not spell.
    InvalidRuntimeDirName,
}

impl fmt::Display for TaskPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit(name) => write!(f, "build-task {name} exceeds its configured limit"),
            Self::NonCanonicalNodeId => f.write_str("build-task node IDs are not canonical"),
            Self::MissingInput(task) => write!(f, "build task references missing node {}", task.0),
            Self::ForwardReference(task) => {
                write!(
                    f,
                    "build task contains a forward reference to node {}",
                    task.0
                )
            }
            Self::TypeMismatch {
                task,
                expected,
                actual,
            } => write!(
                f,
                "build-task node {} has type {actual:?}, expected {expected:?}",
                task.0
            ),
            Self::InvalidHttpsUrl => f.write_str("build task requires a valid HTTPS URL"),
            Self::InvalidDigest => f.write_str("build task requires a canonical digest"),
            Self::InvalidByteCount => {
                f.write_str("build-task byte count must be non-zero and within the fetch limit")
            }
            Self::InvalidJsonPath => f.write_str("build-task JSON path contains an empty segment"),
            Self::InvalidPath => f.write_str("build task contains an invalid portable path"),
            Self::InvalidOwner => f.write_str("build-task publication owner must not be empty"),
            Self::InvalidRuntimeDirName => f.write_str(
                "a runtime directory name may hold only letters, digits, `-` and `_`: it is spelled \
                 inside a `{dir:…}` placeholder",
            ),
        }
    }
}

impl core::error::Error for TaskPlanError {}

#[derive(Debug, Clone, Copy)]
struct TaskHandle {
    id: TaskId,
}

macro_rules! handle {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy)]
        struct $name(TaskHandle);
    };
}

handle!(UrlTask);
handle!(DigestTask);
handle!(ByteCountTask);
handle!(JsonTask);
handle!(TextTask);
handle!(JarTask);
handle!(SourceTreeTask);
handle!(FileTreeTask);
handle!(BlobTask);

/// A mapping grammar as a script value — the optional third argument of `tasks.remap_jar`.
///
/// Not a `handle!` type: every one of those names a node the plan will execute, and a format is not
/// a step. It is a value rather than a pair of loose strings because a namespace pair means nothing
/// without the format that names it — `tasks.tiny_v2("official", "named")` is the only way to write
/// one, so a script cannot pair namespaces with a grammar that has none.
#[derive(Debug, Clone)]
struct MappingFormatValue(TaskMappingFormat);

/// Running totals over a plan's nodes and terminals.
///
/// Keeping these lets a builder validate one declaration at a time. Re-deriving them per
/// declaration made recording a plan quadratic: with the default 4096-task limit, a script could
/// spend minutes of CPU inside a native call that Rhai's operation counter never sees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PlanCost {
    edges: usize,
    literal_bytes: usize,
    publication_roots: usize,
}

impl PlanCost {
    fn add(self, other: Self) -> Result<Self, TaskPlanError> {
        Ok(Self {
            edges: self
                .edges
                .checked_add(other.edges)
                .ok_or(TaskPlanError::Limit("edge count"))?,
            literal_bytes: self
                .literal_bytes
                .checked_add(other.literal_bytes)
                .ok_or(TaskPlanError::Limit("literal bytes"))?,
            publication_roots: self
                .publication_roots
                .checked_add(other.publication_roots)
                .ok_or(TaskPlanError::Limit("publication root count"))?,
        })
    }

    const fn check(self, limits: TaskPlanLimits) -> Result<(), TaskPlanError> {
        if self.edges > limits.max_edges {
            return Err(TaskPlanError::Limit("edge count"));
        }
        if self.literal_bytes > limits.max_literal_bytes {
            return Err(TaskPlanError::Limit("literal bytes"));
        }
        if self.publication_roots > limits.max_publication_roots {
            return Err(TaskPlanError::Limit("publication root count"));
        }
        Ok(())
    }
}

/// Rhai-facing task graph builder. It records data only and never performs task effects.
#[derive(Clone)]
pub(crate) struct TasksApi {
    plan: Rc<RefCell<TaskPlan>>,
    limits: TaskPlanLimits,
    /// Totals for everything already accepted, so each declaration costs O(1).
    cost: Rc<Cell<PlanCost>>,
}

impl TasksApi {
    pub(crate) fn new(limits: TaskPlanLimits) -> Self {
        Self {
            plan: Rc::new(RefCell::new(TaskPlan::new())),
            limits,
            cost: Rc::new(Cell::new(PlanCost::default())),
        }
    }

    pub(crate) fn finish(self) -> Result<TaskPlan, TaskPlanError> {
        let plan = Rc::try_unwrap(self.plan)
            .map_err(|_| TaskPlanError::NonCanonicalNodeId)?
            .into_inner();
        plan.validate(self.limits)?;
        Ok(plan)
    }

    fn push(&self, kind: TaskNodeKind) -> RhaiResult<TaskHandle> {
        let mut plan = self
            .plan
            .try_borrow_mut()
            .map_err(|_| Self::rhai_error("reentrant build-task declaration"))?;
        if plan.nodes.len() >= self.limits.max_tasks {
            return Err(Self::rhai_error(
                "build-task count exceeds its configured limit",
            ));
        }
        let id = TaskId(
            u32::try_from(plan.nodes.len())
                .map_err(|_| Self::rhai_error("build-task count cannot be represented"))?,
        );
        let index = plan.nodes.len();
        self.accept(plan.node_cost(index, &kind, self.limits))?;
        plan.nodes.push(TaskNode { id, kind });
        Ok(TaskHandle { id })
    }

    fn terminal(&self, terminal: TaskTerminal) -> RhaiResult<()> {
        let mut plan = self
            .plan
            .try_borrow_mut()
            .map_err(|_| Self::rhai_error("reentrant build-task terminal declaration"))?;
        if plan.terminals.len() >= self.limits.max_terminals {
            return Err(Self::rhai_error(
                "build-task terminal count exceeds its configured limit",
            ));
        }
        self.accept(plan.terminal_cost(&terminal, self.limits))?;
        plan.terminals.push(terminal);
        Ok(())
    }

    /// Fold one declaration's cost into the running totals, leaving them unchanged if it is
    /// rejected — a script may catch the error and keep building.
    fn accept(&self, added: Result<PlanCost, TaskPlanError>) -> RhaiResult<()> {
        let total = added
            .and_then(|added| self.cost.get().add(added))
            .and_then(|total| total.check(self.limits).map(|()| total))
            .map_err(|error| Self::rhai_error(error.to_string()))?;
        self.cost.set(total);
        Ok(())
    }
}

type RhaiResult<T> = Result<T, Box<EvalAltResult>>;

impl TasksApi {
    #[allow(clippy::unnecessary_box_returns)]
    fn rhai_error(message: impl Into<String>) -> Box<EvalAltResult> {
        Box::new(EvalAltResult::ErrorRuntime(
            Dynamic::from(message.into()),
            Position::NONE,
        ))
    }

    fn path_from_array(path: Array, operation: &str) -> RhaiResult<Vec<String>> {
        path.into_iter()
            .map(|value| {
                value
                    .try_cast::<ImmutableString>()
                    .map(ImmutableString::into_owned)
                    .ok_or_else(|| {
                        Self::rhai_error(format!("{operation} requires a string path array"))
                    })
            })
            .collect()
    }

    fn https_url(api: &mut Self, value: ImmutableString) -> RhaiResult<UrlTask> {
        api.push(TaskNodeKind::HttpsUrl {
            value: value.into_owned(),
        })
        .map(UrlTask)
    }

    fn project_jar(api: &mut Self, path: ImmutableString) -> RhaiResult<JarTask> {
        api.push(TaskNodeKind::ProjectJar {
            path: path.into_owned(),
        })
        .map(JarTask)
    }

    fn digest(
        api: &Self,
        value: ImmutableString,
        algorithm: TaskDigestAlgorithm,
    ) -> RhaiResult<DigestTask> {
        api.push(TaskNodeKind::Digest {
            algorithm,
            value: value.into_owned(),
        })
        .map(DigestTask)
    }

    fn sha1(api: &mut Self, value: ImmutableString) -> RhaiResult<DigestTask> {
        Self::digest(api, value, TaskDigestAlgorithm::Sha1)
    }

    fn sha256(api: &mut Self, value: ImmutableString) -> RhaiResult<DigestTask> {
        Self::digest(api, value, TaskDigestAlgorithm::Sha256)
    }

    fn bytes(api: &mut Self, value: INT) -> RhaiResult<ByteCountTask> {
        let value = u64::try_from(value)
            .map_err(|_| Self::rhai_error("tasks.bytes requires a positive byte count"))?;
        api.push(TaskNodeKind::ByteCount { value })
            .map(ByteCountTask)
    }

    fn fetch(
        api: &Self,
        url: UrlTask,
        digest: DigestTask,
        max_bytes: ByteCountTask,
        kind: TaskFetchKind,
    ) -> RhaiResult<TaskHandle> {
        api.push(TaskNodeKind::Fetch {
            kind,
            url: url.0.id,
            digest: digest.0.id,
            max_bytes: max_bytes.0.id,
        })
    }

    fn fetch_json(
        api: &mut Self,
        url: UrlTask,
        digest: DigestTask,
        max_bytes: ByteCountTask,
    ) -> RhaiResult<JsonTask> {
        Self::fetch(api, url, digest, max_bytes, TaskFetchKind::Json).map(JsonTask)
    }

    fn fetch_jar(
        api: &mut Self,
        url: UrlTask,
        digest: DigestTask,
        max_bytes: ByteCountTask,
    ) -> RhaiResult<JarTask> {
        Self::fetch(api, url, digest, max_bytes, TaskFetchKind::Jar).map(JarTask)
    }

    fn fetch_text(
        api: &mut Self,
        url: UrlTask,
        digest: DigestTask,
        max_bytes: ByteCountTask,
    ) -> RhaiResult<TextTask> {
        Self::fetch(api, url, digest, max_bytes, TaskFetchKind::Text).map(TextTask)
    }

    fn json_at(api: &mut Self, json: JsonTask, path: Array) -> RhaiResult<JsonTask> {
        api.push(TaskNodeKind::JsonAt {
            json: json.0.id,
            path: Self::path_from_array(path, "tasks.json_at")?,
        })
        .map(JsonTask)
    }

    fn json_find_string(
        api: &mut Self,
        json: JsonTask,
        path: Array,
        field: ImmutableString,
        value: ImmutableString,
    ) -> RhaiResult<JsonTask> {
        api.push(TaskNodeKind::JsonFindString {
            json: json.0.id,
            path: Self::path_from_array(path, "tasks.json_find_string")?,
            field: field.into_owned(),
            value: value.into_owned(),
        })
        .map(JsonTask)
    }

    fn json_url(api: &mut Self, json: JsonTask, path: Array) -> RhaiResult<UrlTask> {
        api.push(TaskNodeKind::JsonUrl {
            json: json.0.id,
            path: Self::path_from_array(path, "tasks.json_url")?,
        })
        .map(UrlTask)
    }

    fn json_digest(
        api: &Self,
        json: JsonTask,
        path: Array,
        algorithm: TaskDigestAlgorithm,
        operation: &str,
    ) -> RhaiResult<DigestTask> {
        api.push(TaskNodeKind::JsonDigest {
            json: json.0.id,
            path: Self::path_from_array(path, operation)?,
            algorithm,
        })
        .map(DigestTask)
    }

    fn json_sha1(api: &mut Self, json: JsonTask, path: Array) -> RhaiResult<DigestTask> {
        Self::json_digest(
            api,
            json,
            path,
            TaskDigestAlgorithm::Sha1,
            "tasks.json_sha1",
        )
    }

    fn json_sha256(api: &mut Self, json: JsonTask, path: Array) -> RhaiResult<DigestTask> {
        Self::json_digest(
            api,
            json,
            path,
            TaskDigestAlgorithm::Sha256,
            "tasks.json_sha256",
        )
    }

    fn json_u64(api: &mut Self, json: JsonTask, path: Array) -> RhaiResult<ByteCountTask> {
        api.push(TaskNodeKind::JsonU64 {
            json: json.0.id,
            path: Self::path_from_array(path, "tasks.json_u64")?,
        })
        .map(ByteCountTask)
    }

    fn extract_java(
        api: &mut Self,
        jar: JarTask,
        prefix: ImmutableString,
    ) -> RhaiResult<SourceTreeTask> {
        api.push(TaskNodeKind::ExtractJava {
            jar: jar.0.id,
            prefix: prefix.into_owned(),
        })
        .map(SourceTreeTask)
    }

    fn extract_files(
        api: &mut Self,
        archive: JarTask,
        prefix: ImmutableString,
    ) -> RhaiResult<FileTreeTask> {
        api.push(TaskNodeKind::ExtractFiles {
            archive: archive.0.id,
            prefix: prefix.into_owned(),
        })
        .map(FileTreeTask)
    }

    fn fetch_bytes(
        api: &mut Self,
        url: UrlTask,
        digest: DigestTask,
        max_bytes: ByteCountTask,
    ) -> RhaiResult<BlobTask> {
        api.push(TaskNodeKind::Fetch {
            kind: TaskFetchKind::Bytes,
            url: url.0.id,
            digest: digest.0.id,
            max_bytes: max_bytes.0.id,
        })
        .map(BlobTask)
    }

    fn place(api: &mut Self, path: ImmutableString, blob: BlobTask) -> RhaiResult<FileTreeTask> {
        api.push(TaskNodeKind::PlaceFile {
            blob: blob.0.id,
            path: path.into_owned(),
        })
        .map(FileTreeTask)
    }

    fn merge_trees(
        api: &mut Self,
        base: FileTreeTask,
        overlay: FileTreeTask,
    ) -> RhaiResult<FileTreeTask> {
        api.push(TaskNodeKind::MergeTrees {
            base: base.0.id,
            overlay: overlay.0.id,
        })
        .map(FileTreeTask)
    }

    fn nested_jar(api: &mut Self, jar: JarTask, member: ImmutableString) -> RhaiResult<JarTask> {
        api.push(TaskNodeKind::NestedJar {
            jar: jar.0.id,
            member: member.into_owned(),
        })
        .map(JarTask)
    }

    /// `tasks.proguard()` — the ProGuard-style grammar, which names no namespaces of its own.
    ///
    /// Registered even though it is what `tasks.remap_jar(jar, mappings)` already means, so that a
    /// script naming its format never has to drop back to the two-argument spelling to say the
    /// default one.
    const fn proguard(_api: &mut Self) -> MappingFormatValue {
        MappingFormatValue(TaskMappingFormat::Proguard)
    }

    /// `tasks.tiny_v2(from, to)` — the tiny v2 grammar, read through one pair of its namespaces.
    ///
    /// The pair is checked here as well as in the manifest because this is a second way into the
    /// same node: a script reaches `TaskNodeKind::RemapJar` without passing `[mappings]` at all.
    fn tiny_v2(
        _api: &mut Self,
        from: ImmutableString,
        to: ImmutableString,
    ) -> RhaiResult<MappingFormatValue> {
        if from.is_empty() || to.is_empty() {
            return Err(Self::rhai_error(
                "tasks.tiny_v2 needs two namespace names, e.g. \
                 tasks.tiny_v2(\"official\", \"named\")",
            ));
        }
        if from == to {
            return Err(Self::rhai_error(
                "tasks.tiny_v2 names the two namespaces a remap translates between, so naming one \
                 twice renames nothing",
            ));
        }
        Ok(MappingFormatValue(TaskMappingFormat::TinyV2 {
            from: from.into_owned(),
            to: to.into_owned(),
        }))
    }

    /// `tasks.remap_jar(jar, mappings)` — deobfuscate a jar that closes over its own hierarchy,
    /// reading ProGuard-style text.
    ///
    /// The two-argument spelling keeps meaning exactly what it always has. A grammar that names its
    /// own namespaces cannot be written this way, which is what the third argument below is for.
    fn remap_jar(api: &mut Self, jar: JarTask, mappings: TextTask) -> RhaiResult<JarTask> {
        let format = MappingFormatValue(TaskMappingFormat::Proguard);
        Self::remap_jar_as(api, jar, mappings, format)
    }

    /// `tasks.remap_jar(jar, mappings, format)` — the same step over a stated grammar.
    ///
    /// The direction stays deobfuscating and the hierarchy stays empty, as in the two-argument
    /// form: a script fetching a game jar and its mappings is asking for exactly that, and the
    /// manifest's `remap` keys are where the other direction and an extra hierarchy are said.
    fn remap_jar_as(
        api: &mut Self,
        jar: JarTask,
        mappings: TextTask,
        format: MappingFormatValue,
    ) -> RhaiResult<JarTask> {
        api.push(TaskNodeKind::RemapJar {
            jar: jar.0.id,
            mappings: mappings.0.id,
            format: format.0,
            direction: TaskRemapDirection::Deobfuscate,
            hierarchy: Vec::new(),
        })
        .map(JarTask)
    }

    fn merge_jars(api: &mut Self, base: JarTask, overlay: JarTask) -> RhaiResult<JarTask> {
        api.push(TaskNodeKind::MergeJars {
            base: base.0.id,
            overlay: overlay.0.id,
        })
        .map(JarTask)
    }

    fn decompile_java(
        api: &mut Self,
        jar: JarTask,
        prefix: ImmutableString,
    ) -> RhaiResult<SourceTreeTask> {
        api.push(TaskNodeKind::DecompileJava {
            jar: jar.0.id,
            prefix: prefix.into_owned(),
        })
        .map(SourceTreeTask)
    }

    fn add_classpath(api: &mut Self, jar: JarTask) -> RhaiResult<()> {
        api.terminal(TaskTerminal::AddClasspath { jar: jar.0.id })
    }

    fn add_nested_classpath(api: &mut Self, jar: JarTask) -> RhaiResult<()> {
        api.terminal(TaskTerminal::AddNestedClasspath { jar: jar.0.id })
    }

    fn add_runtime_dir(
        api: &mut Self,
        name: ImmutableString,
        tree: FileTreeTask,
    ) -> RhaiResult<()> {
        api.terminal(TaskTerminal::AddRuntimeDir {
            name: name.into_owned(),
            tree: tree.0.id,
        })
    }

    /// The four-argument form every `build.rhai` written before the intent existed spells.
    ///
    /// Registered rather than left absent: Rhai resolves an overload by arity, so without this the
    /// whole of what a script author meets is `Function not found: publish_tree (…)` — a signature
    /// dump, when the thing they have to do is add one word. Every argument is discarded; the
    /// error is the entire body. This is deliberately *not* a default intent, which is the
    /// ambiguity the fifth argument exists to remove.
    fn publish_tree_without_intent(
        _api: &mut Self,
        _owner: ImmutableString,
        _tree: SourceTreeTask,
        _destination: ImmutableString,
        _mode: ImmutableString,
    ) -> RhaiResult<()> {
        Err(Self::rhai_error(
            "tasks.publish_tree needs a fifth argument saying what a consumer does with the tree: \
             `compile` (a consumer compiles it) or `navigation` (a consumer only reads it; the \
             classpath defines these types)",
        ))
    }

    /// `intent` has no default on purpose. What a consumer does with a published tree is the one
    /// thing the task graph cannot infer — a tree with a JAR behind it and a tree that is the only
    /// carrier of its package are written identically — and a script that does not say is a script
    /// whose author has not decided.
    fn publish_tree(
        api: &mut Self,
        owner: ImmutableString,
        tree: SourceTreeTask,
        destination: ImmutableString,
        mode: &str,
        intent: &str,
    ) -> RhaiResult<()> {
        if mode != "replace-root" {
            return Err(Self::rhai_error(
                "tasks.publish_tree supports only the `replace-root` mode",
            ));
        }
        let Some(intent) = TaskPublishIntent::parse(intent) else {
            return Err(Self::rhai_error(
                "tasks.publish_tree needs an intent of `compile` (a consumer compiles this tree) \
                 or `navigation` (a consumer only reads it; the classpath defines these types)",
            ));
        };
        api.terminal(TaskTerminal::PublishTree {
            owner: owner.into_owned(),
            tree: tree.0.id,
            destination: destination.into_owned(),
            mode: TaskPublishMode::ReplaceRoot,
            intent,
        })
    }

    pub(crate) fn register_rhai(engine: &mut Engine) {
        engine
            .register_type_with_name::<Self>("Tasks")
            .register_type_with_name::<UrlTask>("UrlTask")
            .register_type_with_name::<DigestTask>("DigestTask")
            .register_type_with_name::<ByteCountTask>("ByteCountTask")
            .register_type_with_name::<JsonTask>("JsonTask")
            .register_type_with_name::<TextTask>("TextTask")
            .register_type_with_name::<JarTask>("JarTask")
            .register_type_with_name::<SourceTreeTask>("SourceTreeTask")
            .register_type_with_name::<FileTreeTask>("FileTreeTask")
            .register_type_with_name::<BlobTask>("BlobTask")
            .register_type_with_name::<MappingFormatValue>("MappingFormat")
            .register_fn("https_url", Self::https_url)
            .register_fn("project_jar", Self::project_jar)
            .register_fn("sha1", Self::sha1)
            .register_fn("sha256", Self::sha256)
            .register_fn("bytes", Self::bytes)
            .register_fn("fetch_json", Self::fetch_json)
            .register_fn("fetch_jar", Self::fetch_jar)
            .register_fn("fetch_text", Self::fetch_text)
            .register_fn("json_at", Self::json_at)
            .register_fn("json_find_string", Self::json_find_string)
            .register_fn("json_url", Self::json_url)
            .register_fn("json_sha1", Self::json_sha1)
            .register_fn("json_sha256", Self::json_sha256)
            .register_fn("json_u64", Self::json_u64)
            .register_fn("extract_java", Self::extract_java)
            .register_fn("fetch_bytes", Self::fetch_bytes)
            .register_fn("place", Self::place)
            .register_fn("extract_files", Self::extract_files)
            .register_fn("merge_trees", Self::merge_trees)
            .register_fn("nested_jar", Self::nested_jar)
            .register_fn("proguard", Self::proguard)
            .register_fn("tiny_v2", Self::tiny_v2)
            .register_fn("remap_jar", Self::remap_jar)
            .register_fn("remap_jar", Self::remap_jar_as)
            .register_fn("merge_jars", Self::merge_jars)
            .register_fn("decompile_java", Self::decompile_java)
            .register_fn("add_classpath", Self::add_classpath)
            .register_fn("add_nested_classpath", Self::add_nested_classpath)
            .register_fn("add_runtime_dir", Self::add_runtime_dir)
            .register_fn("publish_tree", Self::publish_tree_without_intent)
            .register_fn("publish_tree", Self::publish_tree);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> TaskPlanLimits {
        TaskPlanLimits {
            max_tasks: 32,
            max_edges: 64,
            max_literal_bytes: 4096,
            max_terminals: 8,
            max_publication_roots: 4,
            max_path_bytes: 256,
            max_path_depth: 16,
            max_fetch_bytes: 1_048_576,
        }
    }

    /// A plan that extracts a directory of files out of an archive and hands it to a runtime
    /// directory — the shape a launcher's natives take.
    fn runtime_dir_plan(name: &str) -> TaskPlan {
        TaskPlan {
            nodes: vec![
                TaskNode {
                    id: TaskId(0),
                    kind: TaskNodeKind::ProjectJar {
                        path: "vendor/runtime.zip".to_owned(),
                    },
                },
                TaskNode {
                    id: TaskId(1),
                    kind: TaskNodeKind::ExtractFiles {
                        archive: TaskId(0),
                        prefix: "natives".to_owned(),
                    },
                },
            ],
            terminals: vec![TaskTerminal::AddRuntimeDir {
                name: name.to_owned(),
                tree: TaskId(1),
            }],
        }
    }

    #[test]
    fn a_file_tree_reaches_a_runtime_directory() {
        assert_eq!(runtime_dir_plan("natives").validate(limits()), Ok(()));
    }

    #[test]
    fn a_runtime_directory_name_a_placeholder_cannot_spell_is_refused() {
        // Every one of these would either be unwritable inside `{dir:…}` or unreferrable.
        for bad in ["", "with space", "a/b", "close}brace", "dots.here"] {
            assert_eq!(
                runtime_dir_plan(bad).validate(limits()),
                Err(TaskPlanError::InvalidRuntimeDirName),
                "`{bad}` should not be a runtime directory name"
            );
        }
    }

    #[test]
    fn a_source_tree_is_not_a_file_tree() {
        // The two carry the same shape and have different sinks, so the plan keeps them apart:
        // a `.java` extraction must not become a directory a process is pointed at, and a
        // directory of natives must not be published into the project's sources.
        let plan = TaskPlan {
            nodes: vec![
                TaskNode {
                    id: TaskId(0),
                    kind: TaskNodeKind::ProjectJar {
                        path: "vendor/sources.jar".to_owned(),
                    },
                },
                TaskNode {
                    id: TaskId(1),
                    kind: TaskNodeKind::ExtractJava {
                        jar: TaskId(0),
                        prefix: "net/example".to_owned(),
                    },
                },
            ],
            terminals: vec![TaskTerminal::AddRuntimeDir {
                name: "natives".to_owned(),
                tree: TaskId(1),
            }],
        };
        assert!(
            matches!(
                plan.validate(limits()),
                Err(TaskPlanError::TypeMismatch { .. })
            ),
            "a source tree must not satisfy a runtime directory"
        );
    }

    #[test]
    fn merging_two_file_trees_type_checks_and_merging_a_jar_does_not() {
        let mut plan = runtime_dir_plan("natives");
        plan.nodes.push(TaskNode {
            id: TaskId(2),
            kind: TaskNodeKind::ExtractFiles {
                archive: TaskId(0),
                prefix: "extra".to_owned(),
            },
        });
        plan.nodes.push(TaskNode {
            id: TaskId(3),
            kind: TaskNodeKind::MergeTrees {
                base: TaskId(1),
                overlay: TaskId(2),
            },
        });
        plan.terminals = vec![TaskTerminal::AddRuntimeDir {
            name: "natives".to_owned(),
            tree: TaskId(3),
        }];
        assert_eq!(plan.validate(limits()), Ok(()));

        // The archive itself is a `Jar`, not a tree.
        plan.nodes[3].kind = TaskNodeKind::MergeTrees {
            base: TaskId(0),
            overlay: TaskId(2),
        };
        assert!(matches!(
            plan.validate(limits()),
            Err(TaskPlanError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn plan_rejects_forward_and_wrong_typed_references() {
        let plan = TaskPlan {
            nodes: vec![TaskNode {
                id: TaskId(0),
                kind: TaskNodeKind::ExtractJava {
                    jar: TaskId(0),
                    prefix: "net/example".to_owned(),
                },
            }],
            terminals: Vec::new(),
        };
        assert_eq!(
            plan.validate(limits()),
            Err(TaskPlanError::ForwardReference(TaskId(0)))
        );
    }

    #[test]
    fn plan_round_trips_canonically() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());
        engine
            .run_with_scope(
                &mut scope,
                r#"
                    let url = tasks.https_url("https://example.invalid/sources.jar");
                    let digest = tasks.sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                    let jar = tasks.fetch_jar(url, digest, tasks.bytes(1024));
                    let sources = tasks.extract_java(jar, "net/example");
                    tasks.publish_tree("example", sources, "src/main/java/net/example", "replace-root", "navigation");
                "#,
            )
            .unwrap();
        drop(scope);
        let plan = api.finish().unwrap();
        let bytes = serde_json::to_vec(&plan).unwrap();
        let decoded: TaskPlan = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, plan);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
    }

    /// A plan's wire names are frozen by the cache records already written against them, so adding
    /// a format must leave the old one's encoding byte-identical. Serde encodes a unit variant as a
    /// bare string and a struct variant as a single-key object, so the two coexist — but that is a
    /// property of how the enum is *written*, and turning `Proguard` into `Proguard {}` for symmetry
    /// would silently invalidate every remap record in every user's cache.
    #[test]
    fn adding_a_mapping_format_leaves_the_existing_wire_name_alone() {
        assert_eq!(
            serde_json::to_string(&TaskMappingFormat::Proguard).unwrap(),
            "\"proguard\""
        );
        let decoded: TaskMappingFormat = serde_json::from_str("\"proguard\"").unwrap();
        assert_eq!(decoded, TaskMappingFormat::Proguard);

        let tiny = TaskMappingFormat::TinyV2 {
            from: "official".to_owned(),
            to: "named".to_owned(),
        };
        let encoded = serde_json::to_string(&tiny).unwrap();
        assert_eq!(encoded, r#"{"tiny-v2":{"from":"official","to":"named"}}"#);
        assert_eq!(
            serde_json::from_str::<TaskMappingFormat>(&encoded).unwrap(),
            tiny
        );
    }

    /// The namespace pair is the one thing a tiny v2 file cannot say about itself, so a script has
    /// to — and it reaches the plan as an operand of the format rather than as a loose pair, which
    /// is what stops it being written beside a grammar that has no namespaces.
    #[test]
    fn a_script_selects_the_namespace_pair_a_tiny_file_is_read_through() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());
        engine
            .run_with_scope(
                &mut scope,
                r#"
                    let jar = tasks.project_jar("vendor/game.jar");
                    let digest = tasks.sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                    let url = tasks.https_url("https://example.invalid/yarn.tiny");
                    let mappings = tasks.fetch_text(url, digest, tasks.bytes(1024));
                    let named = tasks.remap_jar(jar, mappings, tasks.tiny_v2("official", "named"));
                    tasks.add_classpath(named);
                "#,
            )
            .unwrap();
        drop(scope);
        let plan = api.finish().unwrap();
        let remap = plan
            .nodes
            .iter()
            .find_map(|node| match &node.kind {
                TaskNodeKind::RemapJar { format, .. } => Some(format),
                _ => None,
            })
            .expect("the script declared a remap");
        assert_eq!(
            remap,
            &TaskMappingFormat::TinyV2 {
                from: "official".to_owned(),
                to: "named".to_owned(),
            }
        );
    }

    /// The two-argument spelling predates the format argument and is what every existing script
    /// writes, so it has to keep meaning exactly what it meant.
    #[test]
    fn a_script_that_names_no_format_still_reads_proguard() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());
        engine
            .run_with_scope(
                &mut scope,
                r#"
                    let jar = tasks.project_jar("vendor/game.jar");
                    let digest = tasks.sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                    let url = tasks.https_url("https://example.invalid/client.txt");
                    let mappings = tasks.fetch_text(url, digest, tasks.bytes(1024));
                    tasks.add_classpath(tasks.remap_jar(jar, mappings));
                "#,
            )
            .unwrap();
        drop(scope);
        let plan = api.finish().unwrap();
        assert!(plan.nodes.iter().any(|node| matches!(
            &node.kind,
            TaskNodeKind::RemapJar {
                format: TaskMappingFormat::Proguard,
                direction: TaskRemapDirection::Deobfuscate,
                ..
            }
        )));
    }

    #[test]
    fn a_script_cannot_read_a_namespace_into_itself() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api);
        // A second way into the same node, so the check the manifest applies has to exist here too.
        // Asserted on `tiny_v2` alone rather than through a whole remap, so what fails is the pair
        // and not some other argument of the step it would have been passed to.
        let mut pair = |from: &str, to: &str| {
            engine.eval_with_scope::<Dynamic>(
                &mut scope,
                &format!("tasks.tiny_v2(\"{from}\", \"{to}\")"),
            )
        };
        assert!(pair("official", "named").is_ok());
        assert!(pair("named", "named").is_err());
        assert!(pair("", "named").is_err());
    }

    /// What a consumer does with a published tree is the one thing the graph cannot infer, so the
    /// script has to say — and the two answers have to reach the plan as two different terminals,
    /// or nothing downstream could route on them.
    #[test]
    fn a_publication_says_what_a_consumer_does_with_it() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());
        engine
            .run_with_scope(
                &mut scope,
                r#"
                    let jar = tasks.project_jar("sources.jar");
                    let tree = tasks.extract_java(jar, "net/example");
                    tasks.publish_tree("view", tree, "src/main/java/net/example", "replace-root",
                                       "navigation");
                    tasks.publish_tree("carrier", tree, "src/main/java/org/vendor", "replace-root",
                                       "compile");
                "#,
            )
            .unwrap();
        let error = engine
            .run_with_scope(
                &mut scope,
                r#"
                    let jar = tasks.project_jar("other.jar");
                    let tree = tasks.extract_java(jar, "net/other");
                    tasks.publish_tree("bad", tree, "src/main/java/net/other", "replace-root",
                                       "read-only");
                "#,
            )
            .unwrap_err();
        assert!(error.to_string().contains("intent"), "{error}");

        drop(scope);
        let plan = api.finish().unwrap();
        let intents: Vec<_> = plan
            .terminals
            .iter()
            .filter_map(|terminal| match terminal {
                TaskTerminal::PublishTree { owner, intent, .. } => Some((owner.as_str(), *intent)),
                _ => None,
            })
            .collect();
        assert_eq!(
            intents,
            [
                ("view", TaskPublishIntent::Navigation),
                ("carrier", TaskPublishIntent::Compile),
            ]
        );
    }

    /// The fifth argument is a breaking change to every `build.rhai` in existence, so the four-
    /// argument form is registered to fail rather than left to Rhai's overload resolution. What an
    /// author meets has to be the word they must add — a signature dump is not a migration.
    #[test]
    fn the_pre_intent_publish_tree_says_what_to_add() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let mut scope = rhai::Scope::new();
        scope.push("tasks", TasksApi::new(limits()));
        let error = engine
            .run_with_scope(
                &mut scope,
                r#"
                    let jar = tasks.project_jar("sources.jar");
                    let tree = tasks.extract_java(jar, "net/example");
                    tasks.publish_tree("old", tree, "src/main/java/net/example", "replace-root");
                "#,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("fifth argument"), "{error}");
        // Named, because the whole point is that a reader does not have to go and look them up.
        assert!(
            error.contains("compile") && error.contains("navigation"),
            "{error}"
        );
    }

    /// A fetch buffers up to its declared byte count *before* the digest is checked, so an
    /// unbounded count is an out-of-memory switch. `tasks.json_u64` can even take the number from
    /// the fetched document, putting it under whoever serves that document.
    #[test]
    fn rejects_a_byte_count_over_the_fetch_limit() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());

        // `limits()` allows 1 MiB.
        engine
            .run_with_scope(&mut scope, "tasks.bytes(1048576);")
            .expect("a byte count at the limit is accepted");
        let error = engine
            .run_with_scope(&mut scope, "tasks.bytes(1048577);")
            .unwrap_err();
        assert!(error.to_string().contains("byte count"));

        drop(scope);
        let plan = api.finish().unwrap();
        assert_eq!(plan.nodes.len(), 1);
    }

    /// Declarations are checked incrementally, so a rejected one must leave the running totals
    /// untouched — a script can catch the error and keep building, and `finish` revalidates the
    /// whole plan, so the two views have to agree.
    #[test]
    fn a_rejected_declaration_does_not_disturb_the_plan() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let api = TasksApi::new(limits());
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());
        engine
            .run_with_scope(
                &mut scope,
                r#"
                    let caught = 0;
                    // Over the 4096-byte literal budget, and an escaping path: both rejected.
                    for i in 0..8 {
                        try { tasks.project_jar("../escape.jar"); } catch (error) { caught += 1; }
                    }
                    if caught != 8 { throw "expected every bad declaration to be rejected"; }
                    let jar = tasks.project_jar("sources.jar");
                    let sources = tasks.extract_java(jar, "net/example");
                    tasks.publish_tree("example", sources, "src/main/java/net/example", "replace-root", "navigation");
                "#,
            )
            .unwrap();
        drop(scope);

        let plan = api.finish().unwrap();
        assert_eq!(plan.nodes.len(), 2, "rejected nodes must not be recorded");
        // `finish` revalidates from scratch; agreeing with it is the point of the running totals.
        assert_eq!(plan.validate(limits()), Ok(()));
    }

    /// The per-declaration checks must reject exactly what a whole-plan validation would.
    #[test]
    fn incremental_limits_match_whole_plan_validation() {
        let mut engine = Engine::new();
        TasksApi::register_rhai(&mut engine);
        let tight = TaskPlanLimits {
            max_tasks: 3,
            ..limits()
        };
        let api = TasksApi::new(tight);
        let mut scope = rhai::Scope::new();
        scope.push("tasks", api.clone());
        let error = engine
            .run_with_scope(
                &mut scope,
                r#"
                    tasks.project_jar("a.jar");
                    tasks.project_jar("b.jar");
                    tasks.project_jar("c.jar");
                    tasks.project_jar("d.jar");
                "#,
            )
            .unwrap_err();
        assert!(error.to_string().contains("build-task count"));
        drop(scope);

        let plan = api.finish().unwrap();
        assert_eq!(plan.nodes.len(), 3);
        assert_eq!(plan.validate(tight), Ok(()));
    }
}
