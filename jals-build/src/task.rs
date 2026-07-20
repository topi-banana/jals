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
pub struct TaskPlanLimits {
    pub max_tasks: usize,
    pub max_edges: usize,
    pub max_literal_bytes: usize,
    pub max_terminals: usize,
    pub max_publication_roots: usize,
    pub max_path_bytes: usize,
    pub max_path_depth: usize,
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
}

/// Digest algorithm used to authenticate fetched bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskDigestAlgorithm {
    Sha1,
    Sha256,
}

impl TaskDigestAlgorithm {
    pub const fn hex_len(self) -> usize {
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
    Json,
    Jar,
    Text,
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
    },
    MergeJars {
        base: TaskId,
        overlay: TaskId,
    },
    DecompileJava {
        jar: TaskId,
        prefix: String,
    },
}

impl TaskNodeKind {
    pub const fn output_kind(&self) -> TaskValueKind {
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
            } => TaskValueKind::Text,
            Self::Fetch {
                kind: TaskFetchKind::Jar,
                ..
            }
            | Self::ProjectJar { .. }
            | Self::RemapJar { .. }
            | Self::MergeJars { .. }
            | Self::NestedJar { .. } => TaskValueKind::Jar,
            Self::ExtractJava { .. } | Self::DecompileJava { .. } => TaskValueKind::SourceTree,
        }
    }

    fn inputs(&self) -> Vec<(TaskId, TaskValueKind)> {
        match self {
            Self::HttpsUrl { .. }
            | Self::ProjectJar { .. }
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
            Self::RemapJar { jar, mappings } => {
                vec![(*jar, TaskValueKind::Jar), (*mappings, TaskValueKind::Text)]
            }
            Self::MergeJars { base, overlay } => {
                vec![(*base, TaskValueKind::Jar), (*overlay, TaskValueKind::Jar)]
            }
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
            | Self::Digest { value, .. } => value.len(),
            Self::ByteCount { .. }
            | Self::Fetch { .. }
            | Self::JsonU64 { .. }
            | Self::RemapJar { .. }
            | Self::MergeJars { .. } => 0,
            Self::JsonAt { path, .. }
            | Self::JsonUrl { path, .. }
            | Self::JsonDigest { path, .. } => path.iter().map(String::len).sum(),
            Self::JsonFindString {
                path, field, value, ..
            } => path.iter().map(String::len).sum::<usize>() + field.len() + value.len(),
            Self::ExtractJava { prefix, .. } | Self::DecompileJava { prefix, .. } => prefix.len(),
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
    },
}

impl TaskTerminal {
    const fn input(&self) -> (TaskId, TaskValueKind) {
        match self {
            Self::AddClasspath { jar } | Self::AddNestedClasspath { jar } => {
                (*jar, TaskValueKind::Jar)
            }
            Self::PublishTree { tree, .. } => (*tree, TaskValueKind::SourceTree),
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

    pub fn validate(&self, limits: TaskPlanLimits) -> Result<(), TaskPlanError> {
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
            TaskNodeKind::ProjectJar { path } => {
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
            TaskNodeKind::ByteCount { value } if *value == 0 => {
                return Err(TaskPlanError::InvalidByteCount);
            }
            TaskNodeKind::ExtractJava { prefix, .. }
            | TaskNodeKind::DecompileJava { prefix, .. } => {
                Self::validate_path(prefix, limits, true)?;
            }
            TaskNodeKind::NestedJar { member, .. } => {
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
            | TaskNodeKind::MergeJars { .. } => {}
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
            Self::InvalidByteCount => f.write_str("build-task byte limit must be non-zero"),
            Self::InvalidJsonPath => f.write_str("build-task JSON path contains an empty segment"),
            Self::InvalidPath => f.write_str("build task contains an invalid portable path"),
            Self::InvalidOwner => f.write_str("build-task publication owner must not be empty"),
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

    fn nested_jar(api: &mut Self, jar: JarTask, member: ImmutableString) -> RhaiResult<JarTask> {
        api.push(TaskNodeKind::NestedJar {
            jar: jar.0.id,
            member: member.into_owned(),
        })
        .map(JarTask)
    }

    fn remap_jar(api: &mut Self, jar: JarTask, mappings: TextTask) -> RhaiResult<JarTask> {
        api.push(TaskNodeKind::RemapJar {
            jar: jar.0.id,
            mappings: mappings.0.id,
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

    fn publish_tree(
        api: &mut Self,
        owner: ImmutableString,
        tree: SourceTreeTask,
        destination: ImmutableString,
        mode: &str,
    ) -> RhaiResult<()> {
        if mode != "replace-root" {
            return Err(Self::rhai_error(
                "tasks.publish_tree supports only the `replace-root` mode",
            ));
        }
        api.terminal(TaskTerminal::PublishTree {
            owner: owner.into_owned(),
            tree: tree.0.id,
            destination: destination.into_owned(),
            mode: TaskPublishMode::ReplaceRoot,
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
            .register_fn("nested_jar", Self::nested_jar)
            .register_fn("remap_jar", Self::remap_jar)
            .register_fn("merge_jars", Self::merge_jars)
            .register_fn("decompile_java", Self::decompile_java)
            .register_fn("add_classpath", Self::add_classpath)
            .register_fn("add_nested_classpath", Self::add_nested_classpath)
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
        }
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
                    tasks.publish_tree("example", sources, "src/main/java/net/example", "replace-root");
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
                    tasks.publish_tree("example", sources, "src/main/java/net/example", "replace-root");
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
