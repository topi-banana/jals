//! The single-owner language service actor.
//!
//! async-lsp's router requires every request handler to return a `Send` future, while all of
//! `jals-editor`'s analysis state is deliberately `!Send` (see `jals-exec`'s execution model).
//! The server therefore splits in two: the [`ServerState`](crate::server) frontend owns nothing
//! but a [`Cmd`] sender and per-request reply channels, and this actor — one local task spawned
//! next to the main loop — owns every document, workspace, and cache, and processes commands
//! strictly in arrival order. FIFO processing is what makes a `didChange` visible to every query
//! enqueued after it; no locks, no shared state.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use async_lsp::lsp_types::{
    CompletionResponse, Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentHighlight, DocumentSymbolResponse, FileChangeType, FoldingRange,
    GotoDefinitionResponse, Hover, Location, MessageType, NumberOrString, Position,
    PrepareRenameResponse, PublishDiagnosticsParams, Range, SelectionRange, SemanticToken,
    SemanticTokens, SemanticTokensDelta, SemanticTokensFullDeltaResult, SemanticTokensResult,
    ShowMessageParams, SignatureHelp, TextEdit, Url, WorkspaceEdit, notification,
};
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use futures::FutureExt;
use jals_build::{
    ManifestExt,
    build_script::{BuildScriptEnvironment, BuildScriptLimits, BuildScriptSession},
};
use jals_config::{
    BuildScript, Dependency, DependencyScope, FeatureSet, Manifest, ResolvedBuildFeatures,
};
use jals_editor::{
    EditorHost, FoldingHost, Folds, Ident, LineIndex, Outline, SelectionChains, SelectionHost,
};
use jals_exec::Exec;
use jals_project::{
    BuildTaskHost, GraphOutcome, GraphResolveError, NativeProjectAssembly, ProjectAnchor,
    ProjectAssembly, ProjectDiagnostic, ProjectDiagnosticCode, ProjectDiagnosticSeverity,
    ProjectDiagnostics, ProjectScript, RootBuildScriptOptions, ScriptFile, ScriptOutcome,
};
use jals_storage::{DirKey, FileKey, NativeScope, NativeStorage, RelativePath};
use tokio::sync::{mpsc, oneshot};

use crate::formatting::Formatting;
use crate::host::LspHost;
use crate::state::{DetachedWorkspaces, DocumentStore, OpenDocument, ProjectWorkspace, UriConfigs};

/// The reply channel of one request command: the response payload, or a protocol error the
/// frontend forwards verbatim.
pub(crate) type Reply<T> = oneshot::Sender<Result<T, ResponseError>>;

/// One unit of work for the actor: an LSP notification's parameters, a request's parameters plus
/// its reply channel, or an actor-internal completion message.
///
/// Every frontend-visible variant is `Send`-safe (`lsp_types` payloads and channel endpoints), so
/// the router's handlers can build and send them freely; [`WorkspaceReady`](Cmd::WorkspaceReady)
/// carries `!Send` analysis state, which is fine because the whole command channel lives and dies
/// on the one `LocalSet` thread.
pub(crate) enum Cmd {
    // -- Notifications (no reply) --
    DidOpen(DidOpenTextDocumentParams),
    DidChange(DidChangeTextDocumentParams),
    DidClose(DidCloseTextDocumentParams),
    DidChangeWatchedFiles(DidChangeWatchedFilesParams),
    // -- Requests (reply through the oneshot channel) --
    DocumentSymbol {
        uri: Url,
        reply: Reply<Option<DocumentSymbolResponse>>,
    },
    DocumentHighlight {
        uri: Url,
        position: Position,
        reply: Reply<Option<Vec<DocumentHighlight>>>,
    },
    Definition {
        uri: Url,
        position: Position,
        reply: Reply<Option<GotoDefinitionResponse>>,
    },
    References {
        uri: Url,
        position: Position,
        include_declaration: bool,
        reply: Reply<Option<Vec<Location>>>,
    },
    PrepareRename {
        uri: Url,
        position: Position,
        reply: Reply<Option<PrepareRenameResponse>>,
    },
    Rename {
        uri: Url,
        position: Position,
        new_name: String,
        reply: Reply<Option<WorkspaceEdit>>,
    },
    Completion {
        uri: Url,
        position: Position,
        reply: Reply<Option<CompletionResponse>>,
    },
    Hover {
        uri: Url,
        position: Position,
        reply: Reply<Option<Hover>>,
    },
    SignatureHelp {
        uri: Url,
        position: Position,
        reply: Reply<Option<SignatureHelp>>,
    },
    Formatting {
        uri: Url,
        reply: Reply<Option<Vec<TextEdit>>>,
    },
    SemanticTokensFull {
        uri: Url,
        reply: Reply<Option<SemanticTokensResult>>,
    },
    SemanticTokensFullDelta {
        uri: Url,
        previous_result_id: String,
        reply: Reply<Option<SemanticTokensFullDeltaResult>>,
    },
    FoldingRange {
        uri: Url,
        reply: Reply<Option<Vec<FoldingRange>>>,
    },
    SelectionRange {
        uri: Url,
        positions: Vec<Position>,
        reply: Reply<Option<Vec<SelectionRange>>>,
    },
    /// The client's build-feature selection changed (`initialize` options or a
    /// `workspace/didChangeConfiguration`): store it and reassemble every open workspace under
    /// the new selection when it differs.
    SetFeatureSelection(FeatureSelection),
    // -- Actor-internal --
    /// A spawned workspace assembly finished (see [`Actor::ensure_workspace_for`]): the parts to
    /// build the project's [`ProjectWorkspace`] from, or the error that makes it fall back to a
    /// bare workspace. Re-enters through the same queue so it serializes with everything else.
    WorkspaceReady {
        root: PathBuf,
        generation: u64,
        assembled: Result<Box<AssembledWorkspace>, Box<WorkspaceAssemblyFailure>>,
    },
}

/// The client's build-feature selection — the LSP analogue of `--features` /
/// `--all-features` / `--no-default-features` on `jals build`. Read from
/// `initializationOptions` and `workspace/didChangeConfiguration` settings (under a `jals` key:
/// `{"jals": {"features": [...], "allFeatures": bool, "noDefaultFeatures": bool}}`, or the same
/// keys at the top level). The default (nothing selected) resolves the manifest's own `default`
/// list — the same selection a plain `jals build` uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FeatureSelection {
    features: Vec<String>,
    all_features: bool,
    no_default_features: bool,
}

impl FeatureSelection {
    /// Parse a selection out of client-provided JSON. Absent or malformed keys fall back to the
    /// default — a client that sends unrelated options simply keeps the manifest's `default`
    /// selection.
    pub(crate) fn from_json(value: &serde_json::Value) -> Self {
        let scope = value.get("jals").unwrap_or(value);
        let features = scope
            .get("features")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let flag = |key: &str| {
            scope
                .get(key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        Self {
            features,
            all_features: flag("allFeatures"),
            no_default_features: flag("noDefaultFeatures"),
        }
    }
}

/// Everything a spawned assembly task produced for one project: the opened aggregate plus the
/// resolved analysis/navigation inputs, ready for [`ProjectWorkspace::load_storage`].
pub(crate) struct AssembledWorkspace {
    storage: NativeStorage,
    source_roots: Vec<DirKey>,
    project_sources: Vec<FileKey>,
    classpath_classes: Vec<jals_classfile::ClassFile>,
    feature_set: FeatureSet,
    /// The root's resolved build features under the client's [`FeatureSelection`] — what each
    /// project file's `#[cfg(feature = "…")]` evaluates against when the `attributes` dialect
    /// is on.
    build_features: BTreeSet<String>,
    library_sources: Vec<FileKey>,
    source_dep_sources: Vec<FileKey>,
    materialized: BTreeMap<FileKey, PathBuf>,
    watch_policy: ProjectWatchPolicy,
    /// The script `[build] script` names, kept apart from the diagnostics anchored to it.
    ///
    /// An *empty* diagnostic vector for the script's URI is meaningful — it clears what a previous
    /// run published — so which file to clear cannot be derived from the diagnostics themselves.
    configured_script: Option<FileKey>,
    /// [`ProjectAnchor::Script`] diagnostics, in this protocol's shape.
    script_diagnostics: Vec<Diagnostic>,
    /// [`ProjectAnchor::Manifest`] diagnostics, in this protocol's shape.
    project_diagnostics: Vec<Diagnostic>,
}

/// A hard graph or host failure. A graph-free root fallback is available when the root manifest
/// and storage were valid; it is installed only for an initial load, never over a last-good
/// workspace.
pub(crate) struct WorkspaceAssemblyFailure {
    message: String,
    fallback: Option<Box<AssembledWorkspace>>,
    project_diagnostics: Vec<Diagnostic>,
}

impl std::fmt::Debug for WorkspaceAssemblyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceAssemblyFailure")
            .field("message", &self.message)
            .field("has_fallback", &self.fallback.is_some())
            .field("project_diagnostics", &self.project_diagnostics)
            .finish()
    }
}

/// Files that invalidate one successfully assembled build script. An empty rerun set means the
/// script did not narrow its inputs, so any non-output project file remains conservative.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildWatchPolicy {
    script: FileKey,
    rerun_files: BTreeSet<FileKey>,
}

/// What a dependency graph's preprocessing pass needs to run build scripts, borrowed as one value.
///
/// Grouped because the three travel together and are meaningless apart: the environment is already
/// scoped to the root project, `features` is the same resolution its queryable half came from (the
/// other half is what the root forwards into the graph), and `limits` bounds every script the pass
/// runs. Both the real assembly and its root-only fallback pass exactly this.
struct GraphScriptInputs<'a> {
    environment: &'a BuildScriptEnvironment,
    features: &'a ResolvedBuildFeatures,
    limits: &'a BuildScriptLimits,
}

/// Deterministic classification of host changes for one assembled project. Source roots and exact
/// project files can be refreshed in place; classpath/dependency/external inputs require lowering
/// and assembly again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectWatchPolicy {
    source_roots: Vec<DirKey>,
    project_sources: BTreeSet<FileKey>,
    reassemble_inputs: Vec<PathBuf>,
    build_script: Option<BuildWatchPolicy>,
}

impl ProjectWatchPolicy {
    const fn script(&self) -> Option<&FileKey> {
        match &self.build_script {
            Some(policy) => Some(&policy.script),
            None => None,
        }
    }
}

/// The server's whole answer to "how does a project diagnostic look". Severity, range, code, and
/// source are decided here and nowhere else; the assembly decided *what* to say and *where*, and
/// this decides only how LSP spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchedProjectAction {
    Ignore,
    Refresh,
    Reassemble,
}

/// One `jals.toml` project's slot: dependency assembly still in flight (queries on its files fall
/// back to the one-file path, exactly like a manifest-less file), or the loaded workspace.
enum WorkspaceSlot {
    Loading {
        root: PathBuf,
        assembly: WorkspaceAssembly,
    },
    Ready {
        workspace: Box<ProjectWorkspace>,
        assembly: Option<WorkspaceAssembly>,
        watch_policy: Option<ProjectWatchPolicy>,
    },
}

/// One project assembly currently running off the actor queue. A second watched change marks it
/// dirty instead of starting overlapping work; completion then schedules one fresh replacement.
#[derive(Clone, Copy)]
struct WorkspaceAssembly {
    generation: u64,
    rerun_requested: bool,
}

impl WorkspaceSlot {
    fn project_root(&self) -> &Path {
        match self {
            Self::Loading { root, .. } => root,
            Self::Ready { workspace, .. } => workspace.project_root(),
        }
    }

    fn ready(&self) -> Option<&ProjectWorkspace> {
        match self {
            Self::Loading { .. } => None,
            Self::Ready { workspace, .. } => Some(workspace),
        }
    }

    fn ready_mut(&mut self) -> Option<&mut ProjectWorkspace> {
        match self {
            Self::Loading { .. } => None,
            Self::Ready { workspace, .. } => Some(workspace),
        }
    }

    /// Mark an active assembly dirty. Returns whether an assembly is already active.
    const fn request_rerun(&mut self) -> bool {
        let assembly = match self {
            Self::Loading { assembly, .. } => Some(assembly),
            Self::Ready { assembly, .. } => assembly.as_mut(),
        };
        if let Some(assembly) = assembly {
            assembly.rerun_requested = true;
            true
        } else {
            false
        }
    }

    const fn assembly(&self) -> Option<WorkspaceAssembly> {
        match self {
            Self::Loading { assembly, .. } => Some(*assembly),
            Self::Ready { assembly, .. } => *assembly,
        }
    }

    const fn replace_assembly(&mut self, assembly: WorkspaceAssembly) {
        match self {
            Self::Loading {
                assembly: current, ..
            } => *current = assembly,
            Self::Ready {
                assembly: current, ..
            } => *current = Some(assembly),
        }
    }

    const fn watch_policy(&self) -> Option<&ProjectWatchPolicy> {
        match self {
            Self::Loading { .. } => None,
            Self::Ready { watch_policy, .. } => watch_policy.as_ref(),
        }
    }
}

/// The language service: the actor task's exclusive state — the client handle, open documents,
/// memoized config discovery (one cache each for the formatter's `jalsfmt.toml` and the linter's
/// `jalslint.toml`), one workspace per open `jals.toml` project, and the semantic-tokens delta
/// baselines.
pub(crate) struct Actor {
    client: ClientSocket,
    exec: Exec,
    /// A clone of the frontend's sender, handed to spawned workspace-assembly tasks so their
    /// completion re-enters the command queue as [`Cmd::WorkspaceReady`].
    commands: mpsc::UnboundedSender<Cmd>,
    store: DocumentStore,
    discovery: UriConfigs<jals_config::fmt::Config>,
    lint_discovery: UriConfigs<jals_config::lint::Config>,
    /// One slot per `jals.toml` project a client has a file open in. Populated lazily on
    /// `didOpen` by walking up from the file to its manifest (see
    /// [`ensure_workspace_for`](Self::ensure_workspace_for)), so the server only ever indexes a
    /// real project's source roots, never a whole git checkout.
    workspaces: Vec<WorkspaceSlot>,
    /// Every open document that no slot above owns — one that belongs to no manifest, or whose
    /// project is still assembling — grouped by parent directory. Held beside the project slots
    /// rather than among them because a slot is identified by its manifest root and drives
    /// assembly, watching, and reassembly generations off it, none of which a directory without a
    /// manifest has. Together the two make [`workspace_for`](Self::workspace_for) total over the
    /// open Java documents, which is what leaves `None` meaning "no analysis" and nothing else.
    detached: DetachedWorkspaces,
    /// The last semantic-tokens response published per document — its `result_id` and the
    /// delta-encoded token array — so a `textDocument/semanticTokens/full/delta` request can be
    /// answered with just the edits turning the client's copy into the current one. Evicted on
    /// `did_close`; a `previous_result_id` the cache no longer holds falls back to a full
    /// response.
    semantic_tokens_cache: HashMap<Url, (String, Vec<SemanticToken>)>,
    /// Monotonic counter minting a fresh `result_id` for each semantic-tokens response.
    semantic_tokens_result_id: u64,
    /// Monotonic identity for workspace assembly tasks. Completions only apply to the generation
    /// currently recorded by their project slot.
    workspace_assembly_generation: u64,
    /// The client's build-feature selection (`initialize` options / configuration), fed into
    /// every workspace assembly's `resolve_build_features`.
    feature_selection: FeatureSelection,
}

impl Actor {
    /// Run `work` to completion, catching a panic so one poisoned command cannot take the whole
    /// language service down: the actor logs to stderr (stdout is the LSP transport) and keeps
    /// serving the queue.
    async fn guard(work: impl Future<Output = ()>) {
        if AssertUnwindSafe(work).catch_unwind().await.is_err() {
            eprintln!("jals-lsp: a language service command panicked; continuing");
        }
    }

    /// Answer one request command: skip it entirely when the client already gave up (a
    /// `$/cancelRequest` dropped the frontend's reply receiver — checked *before* starting,
    /// never by dropping in-flight work), and turn a panic into an `INTERNAL_ERROR` reply so
    /// the request resolves instead of hanging.
    async fn respond<T>(reply: Reply<T>, work: impl Future<Output = Result<T, ResponseError>>) {
        if reply.is_closed() {
            return;
        }
        let outcome = AssertUnwindSafe(work)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| {
                eprintln!("jals-lsp: a language service request panicked; continuing");
                Err(ResponseError::new(
                    ErrorCode::INTERNAL_ERROR,
                    "the language service panicked while answering",
                ))
            });
        let _ = reply.send(outcome);
    }

    pub(crate) fn new(
        client: ClientSocket,
        exec: Exec,
        commands: mpsc::UnboundedSender<Cmd>,
    ) -> Self {
        Self {
            client,
            exec,
            commands,
            store: DocumentStore::default(),
            discovery: UriConfigs::default(),
            lint_discovery: UriConfigs::default(),
            workspaces: Vec::new(),
            detached: DetachedWorkspaces::default(),
            semantic_tokens_cache: HashMap::new(),
            semantic_tokens_result_id: 0,
            workspace_assembly_generation: 0,
            feature_selection: FeatureSelection::default(),
        }
    }

    /// The actor loop: FIFO over the command queue, with one refinement — a burst of `didChange`
    /// events for the same document is coalesced (see [`did_change`](Self::did_change)), so
    /// diagnostics are computed once for the newest text instead of once per keystroke. Commands
    /// the coalescer set aside are processed from `pending` before the channel is polled again,
    /// preserving their original order.
    pub(crate) async fn run(mut self, mut receiver: mpsc::UnboundedReceiver<Cmd>) {
        let mut pending = VecDeque::new();
        loop {
            let cmd = match pending.pop_front() {
                Some(cmd) => cmd,
                None => match receiver.recv().await {
                    Some(cmd) => cmd,
                    None => return,
                },
            };
            match cmd {
                Cmd::DidChange(params) => {
                    self.did_change(params, &mut receiver, &mut pending).await;
                }
                cmd => self.process(cmd).await,
            }
        }
    }

    /// Apply one `didChange`, opportunistically coalescing a contiguous burst: everything already
    /// queued is drained into `pending`, then adjacent changes for this same document are applied
    /// in order (each event's splices are relative to the previous state). Coalescing stops at the
    /// first intervening command so requests observe the document version at which they arrived.
    async fn did_change(
        &mut self,
        params: DidChangeTextDocumentParams,
        receiver: &mut mpsc::UnboundedReceiver<Cmd>,
        pending: &mut VecDeque<Cmd>,
    ) {
        let uri = params.text_document.uri.clone();
        Self::guard(self.apply_change(params)).await;
        while let Ok(cmd) = receiver.try_recv() {
            pending.push_back(cmd);
        }
        while matches!(
            pending.front(),
            Some(Cmd::DidChange(next)) if next.text_document.uri == uri
        ) {
            let Some(Cmd::DidChange(next)) = pending.pop_front() else {
                unreachable!("front just matched a didChange");
            };
            Self::guard(self.apply_change(next)).await;
        }
        Self::guard(self.refresh_and_publish(&uri)).await;
    }

    /// Dispatch one command. `didChange` is normally routed through the coalescer in
    /// [`run`](Self::run); the plain arm here (splice + overlay + diagnostics) keeps dispatch
    /// total for direct drivers such as tests.
    async fn process(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::DidOpen(params) => Self::guard(self.did_open(params)).await,
            Cmd::DidChange(params) => {
                let uri = params.text_document.uri.clone();
                Self::guard(async {
                    self.apply_change(params).await;
                    self.refresh_and_publish(&uri).await;
                })
                .await;
            }
            Cmd::DidClose(params) => Self::guard(self.did_close(params)).await,
            Cmd::DidChangeWatchedFiles(params) => {
                Self::guard(self.watched_files_changed(&params)).await;
            }
            Cmd::SetFeatureSelection(selection) => {
                Self::guard(async { self.set_feature_selection(selection) }).await;
            }
            Cmd::WorkspaceReady {
                root,
                generation,
                assembled,
            } => {
                Self::guard(self.workspace_ready(root, generation, assembled)).await;
            }
            Cmd::DocumentSymbol { uri, reply } => {
                Self::respond(reply, async { Ok(self.document_symbol(&uri)) }).await;
            }
            Cmd::DocumentHighlight {
                uri,
                position,
                reply,
            } => Self::respond(reply, self.document_highlight(&uri, position)).await,
            Cmd::Definition {
                uri,
                position,
                reply,
            } => Self::respond(reply, self.definition(&uri, position)).await,
            Cmd::References {
                uri,
                position,
                include_declaration,
                reply,
            } => Self::respond(reply, self.references(&uri, position, include_declaration)).await,
            Cmd::PrepareRename {
                uri,
                position,
                reply,
            } => Self::respond(reply, self.prepare_rename(&uri, position)).await,
            Cmd::Rename {
                uri,
                position,
                new_name,
                reply,
            } => Self::respond(reply, self.rename(&uri, position, &new_name)).await,
            Cmd::Completion {
                uri,
                position,
                reply,
            } => Self::respond(reply, self.completion(&uri, position)).await,
            Cmd::Hover {
                uri,
                position,
                reply,
            } => Self::respond(reply, self.hover(&uri, position)).await,
            Cmd::SignatureHelp {
                uri,
                position,
                reply,
            } => Self::respond(reply, self.signature_help(&uri, position)).await,
            Cmd::Formatting { uri, reply } => {
                Self::respond(reply, self.formatting(&uri)).await;
            }
            Cmd::SemanticTokensFull { uri, reply } => {
                Self::respond(reply, async {
                    Ok(self.semantic_tokens_full_response(&uri).await)
                })
                .await;
            }
            Cmd::SemanticTokensFullDelta {
                uri,
                previous_result_id,
                reply,
            } => {
                Self::respond(reply, async {
                    Ok(self
                        .semantic_tokens_delta_response(&uri, &previous_result_id)
                        .await)
                })
                .await;
            }
            Cmd::FoldingRange { uri, reply } => {
                Self::respond(reply, async { Ok(self.folding_range(&uri)) }).await;
            }
            Cmd::SelectionRange {
                uri,
                positions,
                reply,
            } => Self::respond(reply, async { Ok(self.selection_range(&uri, &positions)) }).await,
        }
    }

    // ---- Document lifecycle ---------------------------------------------------------------------

    async fn did_open(&mut self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let uri = doc.uri;
        self.store.upsert(uri.clone(), doc.text, doc.version).await;
        // Discover (and index, once) the `jals.toml` project this file belongs to, so cross-file
        // resolution works without ever walking a non-project folder.
        self.ensure_workspace_for(&uri).await;
        self.refresh_and_publish(&uri).await;
    }

    /// Splice one `didChange` into the stored document. The workspace overlay and diagnostics are
    /// refreshed separately ([`refresh_and_publish`](Self::refresh_and_publish)), once per
    /// coalesced burst.
    async fn apply_change(&mut self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        self.store
            .apply_changes(&uri, &params.content_changes, params.text_document.version)
            .await;
    }

    /// Reflect the (possibly coalesced) new text into the owning workspace's index and republish
    /// diagnostics.
    async fn refresh_and_publish(&mut self, uri: &Url) {
        if self.is_assembly_diagnostic_uri(uri) {
            return;
        }
        self.route_document(uri).await;
        self.publish_diagnostics(uri).await;
    }

    async fn did_close(&mut self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let assembly_diagnostics_are_authoritative = self.is_assembly_diagnostic_uri(&uri);
        self.store.remove(&uri);
        // Drop the cached semantic-tokens baseline; a reopened document starts a fresh result id.
        self.semantic_tokens_cache.remove(&uri);
        // A detached group *is* its open documents, so a closed one leaves it — otherwise every
        // directory the user ever looked at would keep every text and CST it ever held, and a
        // deleted sibling would go on resolving. The survivors are republished because losing a
        // sibling can unresolve a name in them. A project workspace keeps its overlay instead:
        // that aggregate reclaims it on the next reassembly, and its membership is the manifest's
        // to decide, not the editor's.
        let survivors = self.detached.forget(std::slice::from_ref(&uri)).await;
        self.republish(survivors).await;
        // Clear stale diagnostics for the now-closed document.
        if !assembly_diagnostics_are_authoritative {
            let _ =
                self.client
                    .notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                        uri,
                        diagnostics: Vec::new(),
                        version: None,
                    });
        }
    }

    fn is_assembly_diagnostic_uri(&self, uri: &Url) -> bool {
        self.is_script_diagnostic_uri(uri) || self.is_project_diagnostic_uri(uri)
    }

    /// Rhai files are never Java diagnostic inputs. The exact configured script is also protected
    /// when it uses another extension, so assembly diagnostics remain authoritative while open.
    fn is_script_diagnostic_uri(&self, uri: &Url) -> bool {
        let Ok(path) = uri.to_file_path() else {
            return false;
        };
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rhai"))
        {
            return true;
        }
        self.workspaces.iter().any(|slot| {
            slot.watch_policy()
                .and_then(ProjectWatchPolicy::script)
                .is_some_and(|script| script.path().to_host_path(slot.project_root()) == path)
        })
    }

    /// Project-graph diagnostics are published against the root manifest because graph node
    /// metadata intentionally exposes no host path. Keep an open manifest's one-file Java
    /// fallback from replacing that authoritative diagnostic set.
    fn is_project_diagnostic_uri(&self, uri: &Url) -> bool {
        let Ok(path) = uri.to_file_path() else {
            return false;
        };
        self.workspaces
            .iter()
            .any(|slot| path == slot.project_root().join("jals.toml"))
    }

    async fn watched_files_changed(&mut self, params: &DidChangeWatchedFilesParams) {
        // A created/changed/deleted config file can affect discovery for any directory at or
        // below it (including shadowing); drop the whole memo for the affected tool and
        // rediscover lazily on the next request that needs it.
        if params
            .changes
            .iter()
            .any(|e| UriConfigs::<jals_config::fmt::Config>::is_config_file(&e.uri))
        {
            self.discovery.clear();
        }
        if params
            .changes
            .iter()
            .any(|e| UriConfigs::<jals_config::lint::Config>::is_config_file(&e.uri))
        {
            self.lint_discovery.clear();
        }
        let changed: Vec<_> = params
            .changes
            .iter()
            .filter_map(|event| event.uri.to_file_path().ok().map(|path| (path, event.typ)))
            .collect();
        let actions: Vec<_> = self
            .workspaces
            .iter()
            .filter_map(|slot| {
                let root = slot.project_root();
                let action = Self::watched_project_action(root, slot.watch_policy(), &changed);
                let action = if slot.assembly().is_some() && action == WatchedProjectAction::Refresh
                {
                    // A replacement may have changed its declared inputs since the old policy.
                    WatchedProjectAction::Reassemble
                } else {
                    action
                };
                (action != WatchedProjectAction::Ignore).then(|| (root.to_path_buf(), action))
            })
            .collect();
        for (root, action) in actions {
            match action {
                WatchedProjectAction::Ignore => {}
                WatchedProjectAction::Refresh => self.refresh_workspace_from_disk(&root).await,
                WatchedProjectAction::Reassemble => self.request_workspace_reassembly(&root),
            }
        }
    }

    /// Classify a watched-file batch for one loaded project. Generated writes and cache feedback
    /// are ignored, but deleting generated output rebuilds the workspace so stale symbols cannot
    /// survive. Every other project change is at least a lightweight refresh.
    fn watched_project_action(
        root: &Path,
        policy: Option<&ProjectWatchPolicy>,
        changed: &[(PathBuf, FileChangeType)],
    ) -> WatchedProjectAction {
        let build_root = root.join("target/jals/build");
        let cache_root = root.join("target/jals/cache");
        let manifest = root.join("jals.toml");
        // `NativeStorage` never snapshots `.git`, so a change there cannot affect analysis. The
        // client watches `**/*` and VS Code's default excludes stop at `.git/objects`, so without
        // this every `git status` / `git commit` writes `.git/index` and would trigger a full
        // reassembly — re-running the build script for a change the workspace cannot even see.
        let git_root = root.join(".git");
        let mut saw_refreshable_source = false;
        for (path, change_type) in changed {
            if path.starts_with(&git_root) {
                continue;
            }
            if path.starts_with(&build_root) {
                if *change_type == FileChangeType::DELETED {
                    return WatchedProjectAction::Reassemble;
                }
                continue;
            }
            if path.starts_with(&cache_root) {
                continue;
            }
            if *path == manifest {
                return WatchedProjectAction::Reassemble;
            }

            let Some(policy) = policy else {
                if path.starts_with(root) {
                    return WatchedProjectAction::Reassemble;
                }
                continue;
            };
            if policy
                .reassemble_inputs
                .iter()
                .any(|input| path.starts_with(input))
            {
                return WatchedProjectAction::Reassemble;
            }
            if !path.starts_with(root) {
                continue;
            }
            let key =
                RelativePath::from_host_path(root, path).and_then(|path| FileKey::new(path).ok());
            if let Some(build) = &policy.build_script
                && (*path == build.script.path().to_host_path(root)
                    || build.rerun_files.is_empty()
                    || key
                        .as_ref()
                        .is_some_and(|key| build.rerun_files.contains(key)))
            {
                return WatchedProjectAction::Reassemble;
            }
            let refreshable = key.as_ref().is_some_and(|key| {
                policy.project_sources.contains(key)
                    || (key.has_extension("java")
                        && policy
                            .source_roots
                            .iter()
                            .any(|source| key.path().starts_with(source.path())))
            });
            if refreshable {
                saw_refreshable_source = true;
            } else {
                // Classpath, source dependencies, and unknown/non-Java inputs require re-lowering.
                return WatchedProjectAction::Reassemble;
            }
        }
        if saw_refreshable_source {
            WatchedProjectAction::Refresh
        } else {
            WatchedProjectAction::Ignore
        }
    }

    async fn refresh_workspace_from_disk(&mut self, root: &Path) {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|slot| slot.project_root() == root)
        else {
            return;
        };
        let WorkspaceSlot::Ready { workspace, .. } = &mut self.workspaces[index] else {
            self.request_workspace_reassembly(root);
            return;
        };
        workspace.refresh().await;

        let open: Vec<Url> = self.store.uris().cloned().collect();
        for uri in open {
            if uri.to_file_path().is_ok_and(|path| path.starts_with(root)) {
                self.refresh_and_publish(&uri).await;
            }
        }
    }

    // ---- Workspace lifecycle --------------------------------------------------------------------

    /// Ensure a workspace slot exists for the `jals.toml` project `uri` belongs to.
    ///
    /// Walks up from the file's directory to find its manifest. A file under no manifest is left
    /// for one-file resolution, and an existing slot (ready *or* still loading) is reused, so a
    /// second open under the same root never spawns a duplicate assembly. Otherwise a `Loading`
    /// slot is inserted immediately and the dependency assembly — storage snapshot, classpath
    /// resolution over HTTP, navigation-source staging — runs on a spawned task that reports back
    /// through [`Cmd::WorkspaceReady`]; until then, queries on the project's files fall back to
    /// the one-file path (same as manifest-less files). Every assembly reparses the manifest; an
    /// unparsable manifest indexes the project root as a lone source root, no classpath.
    async fn ensure_workspace_for(&mut self, uri: &Url) {
        let Some(dir) = uri
            .to_file_path()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            return;
        };
        let Some(manifest_path) = Manifest::discover_path(&dir).await else {
            return;
        };
        let root = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        if self
            .workspaces
            .iter()
            .any(|slot| slot.project_root() == root)
        {
            return;
        }
        let generation = self.next_workspace_assembly_generation();
        self.workspaces.push(WorkspaceSlot::Loading {
            root: root.clone(),
            assembly: WorkspaceAssembly {
                generation,
                rerun_requested: false,
            },
        });
        self.spawn_workspace_assembly(root, generation);
    }

    const fn next_workspace_assembly_generation(&mut self) -> u64 {
        self.workspace_assembly_generation += 1;
        self.workspace_assembly_generation
    }

    /// Start one detached assembly. Manifest parsing deliberately happens inside every run so a
    /// watched jals.toml edit cannot reuse stale configuration.
    fn spawn_workspace_assembly(&self, root: PathBuf, generation: u64) {
        let exec = self.exec.clone();
        let commands = self.commands.clone();
        let selection = self.feature_selection.clone();
        let blocked_files: Vec<_> = self
            .store
            .uris()
            .filter_map(|uri| uri.to_file_path().ok())
            .filter_map(|path| RelativePath::from_host_path(&root, &path))
            .filter_map(|path| FileKey::new(path).ok())
            .collect();
        drop(self.exec.spawn(async move {
            let manifest_path = root.join("jals.toml");
            let assembled = match Manifest::from_file(&manifest_path).await {
                Ok(manifest) => {
                    // Every other command path is wrapped in `catch_unwind`; this one is spawned
                    // and detached, so a panic here would simply never send `WorkspaceReady`. The
                    // slot then stays `Loading` (all queries silently degrade to single-file for
                    // the rest of the session) or keeps a stale assembly whose rerun flag is never
                    // cleared (no watcher event can ever reassemble it again). Turn a panic into
                    // an ordinary failure so the slot always reaches a terminal state.
                    let assemble =
                        core::panic::AssertUnwindSafe(AssembledWorkspace::assemble_with_blocked(
                            &manifest,
                            &root,
                            exec,
                            &blocked_files,
                            &selection,
                        ));
                    futures::FutureExt::catch_unwind(assemble)
                        .await
                        .unwrap_or_else(|_| {
                            let message =
                                format!("assembling project {} panicked", manifest_path.display());
                            Err(WorkspaceAssemblyFailure {
                                project_diagnostics: vec![AssembledWorkspace::host_diagnostic(
                                    ProjectDiagnosticCode::ProjectAssembly,
                                    message.clone(),
                                )],
                                message,
                                fallback: None,
                            })
                        })
                }
                Err(error) => {
                    let message = format!(
                        "reading project manifest {} failed: {error}",
                        manifest_path.display()
                    );
                    Err(WorkspaceAssemblyFailure {
                        project_diagnostics: vec![AssembledWorkspace::host_diagnostic(
                            ProjectDiagnosticCode::ProjectManifest,
                            message.clone(),
                        )],
                        message,
                        fallback: None,
                    })
                }
            };
            let _ = commands.send(Cmd::WorkspaceReady {
                root,
                generation,
                assembled: assembled.map(Box::new).map_err(Box::new),
            });
        }));
    }

    /// Store a new build-feature selection and reassemble every open workspace under it: the
    /// resolved features feed each project's dependency graph *and* its `#[cfg(...)]` analysis,
    /// so a change re-runs the same path a watched `jals.toml` edit does. A selection equal to
    /// the current one is a no-op, so clients may push their configuration unconditionally.
    fn set_feature_selection(&mut self, selection: FeatureSelection) {
        if self.feature_selection == selection {
            return;
        }
        self.feature_selection = selection;
        let roots: Vec<PathBuf> = self
            .workspaces
            .iter()
            .map(|slot| slot.project_root().to_path_buf())
            .collect();
        for root in roots {
            self.request_workspace_reassembly(&root);
        }
    }

    /// Queue a replacement assembly for one project. Repeated changes while one run is active are
    /// collapsed into a single follow-up run, so script executions never overlap for a project.
    fn request_workspace_reassembly(&mut self, root: &Path) {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|slot| slot.project_root() == root)
        else {
            return;
        };
        if self.workspaces[index].request_rerun() {
            return;
        }
        let generation = self.next_workspace_assembly_generation();
        self.workspaces[index].replace_assembly(WorkspaceAssembly {
            generation,
            rerun_requested: false,
        });
        self.spawn_workspace_assembly(root.to_path_buf(), generation);
    }

    /// Finish a spawned assembly: reject stale results, schedule a requested follow-up, or build
    /// and install the workspace (falling back to a bare one when assembly failed). Open documents
    /// under the root are then replayed into the fresh index and their diagnostics republished.
    async fn workspace_ready(
        &mut self,
        root: PathBuf,
        generation: u64,
        assembled: Result<Box<AssembledWorkspace>, Box<WorkspaceAssemblyFailure>>,
    ) {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|slot| slot.project_root() == root)
        else {
            return;
        };
        let Some(active) = self.workspaces[index].assembly() else {
            return;
        };
        if active.generation != generation {
            return;
        }
        if active.rerun_requested {
            let generation = self.next_workspace_assembly_generation();
            self.workspaces[index].replace_assembly(WorkspaceAssembly {
                generation,
                rerun_requested: false,
            });
            self.spawn_workspace_assembly(root, generation);
            return;
        }

        let previous_script = self.workspaces[index]
            .watch_policy()
            .and_then(ProjectWatchPolicy::script)
            .cloned();
        let (parts, publish_project_diagnostics) = match assembled {
            Ok(parts) => (parts, true),
            Err(mut failure) => {
                eprintln!(
                    "jals-lsp: assembling project inputs for {} failed: {}",
                    root.display(),
                    failure.message
                );
                let mut diagnostics = failure.project_diagnostics.clone();
                if let Some(fallback) = &failure.fallback {
                    diagnostics.extend(fallback.project_diagnostics.clone());
                }
                if let Some(params) = Self::project_diagnostic_publication(&root, diagnostics) {
                    let _ = self
                        .client
                        .notify::<notification::PublishDiagnostics>(params);
                }
                if let WorkspaceSlot::Ready { assembly, .. } = &mut self.workspaces[index] {
                    *assembly = None;
                    return;
                }

                let Some(parts) = failure.fallback.take() else {
                    for params in Self::build_script_diagnostic_publications(
                        &root,
                        previous_script.as_ref(),
                        None,
                        Vec::new(),
                    ) {
                        let _ = self
                            .client
                            .notify::<notification::PublishDiagnostics>(params);
                    }
                    let workspace = ProjectWorkspace::bare(&root, self.exec.clone()).await;
                    return self.install_workspace(index, root, workspace, None).await;
                };
                (parts, false)
            }
        };
        let AssembledWorkspace {
            storage,
            source_roots,
            project_sources,
            classpath_classes,
            feature_set,
            build_features,
            library_sources,
            source_dep_sources,
            materialized,
            watch_policy,
            configured_script,
            script_diagnostics,
            project_diagnostics,
        } = *parts;
        for params in Self::build_script_diagnostic_publications(
            &root,
            previous_script.as_ref(),
            configured_script.as_ref(),
            script_diagnostics,
        ) {
            let _ = self
                .client
                .notify::<notification::PublishDiagnostics>(params);
        }
        if publish_project_diagnostics
            && let Some(params) = Self::project_diagnostic_publication(&root, project_diagnostics)
        {
            let _ = self
                .client
                .notify::<notification::PublishDiagnostics>(params);
        }
        let workspace = ProjectWorkspace::load_storage(
            root.clone(),
            storage,
            source_roots,
            project_sources,
            &classpath_classes,
            library_sources,
            source_dep_sources,
            materialized,
            feature_set,
            build_features,
        )
        .await;
        self.install_workspace(index, root, workspace, Some(watch_policy))
            .await;
    }

    async fn install_workspace(
        &mut self,
        index: usize,
        root: PathBuf,
        workspace: ProjectWorkspace,
        watch_policy: Option<ProjectWatchPolicy>,
    ) {
        self.workspaces[index] = WorkspaceSlot::Ready {
            workspace: Box::new(workspace),
            assembly: None,
            watch_policy,
        };
        let under_root: Vec<Url> = self
            .store
            .uris()
            .filter(|uri| uri.to_file_path().is_ok_and(|path| path.starts_with(&root)))
            .cloned()
            .collect();
        // Evict the whole batch before replaying it. Every one of these documents was answered by
        // a detached group until now, and dropping them one at a time would rebuild a shrinking
        // group once per file.
        let survivors = self.detached.forget(&under_root).await;
        self.republish(survivors).await;
        for uri in under_root {
            self.refresh_and_publish(&uri).await;
        }
    }

    /// Shape the replace/clear notifications for one installed assembly. The previous script is
    /// cleared when its path changes or the manifest removes it; the current script is always
    /// published, including an empty vector that clears warnings/errors after a clean rerun.
    fn build_script_diagnostic_publications(
        root: &Path,
        previous_script: Option<&FileKey>,
        configured_script: Option<&FileKey>,
        diagnostics: Vec<Diagnostic>,
    ) -> Vec<PublishDiagnosticsParams> {
        let mut publications = Vec::new();
        if previous_script != configured_script
            && let Some(previous) = previous_script
            && let Some(clear) =
                Self::diagnostic_publication(previous.path().to_host_path(root), Vec::new())
        {
            publications.push(clear);
        }
        if let Some(script) = configured_script
            && let Some(current) =
                Self::diagnostic_publication(script.path().to_host_path(root), diagnostics)
        {
            publications.push(current);
        }
        publications
    }

    fn project_diagnostic_publication(
        root: &Path,
        diagnostics: Vec<Diagnostic>,
    ) -> Option<PublishDiagnosticsParams> {
        Self::diagnostic_publication(root.join("jals.toml"), diagnostics)
    }

    fn diagnostic_publication(
        path: PathBuf,
        diagnostics: Vec<Diagnostic>,
    ) -> Option<PublishDiagnosticsParams> {
        Some(PublishDiagnosticsParams {
            uri: Url::from_file_path(path).ok()?,
            diagnostics,
            version: None,
        })
    }

    /// The analysis that answers for `uri`.
    ///
    /// A loaded project slot that owns it, else the detached group it was routed into. Total over
    /// the open documents this server treats as Java, because [`route_document`](Self::route_document)
    /// puts every one of them in exactly one of those two places — so `None` here means this
    /// server holds no analysis for that URI, and never "it is not a project file".
    ///
    /// Project first: a document opened before its project finished assembling is mounted
    /// detached, and the project workspace takes over the moment it is installed.
    fn workspace_for(&self, uri: &Url) -> Option<OpenDocument<'_>> {
        self.workspaces
            .iter()
            .filter_map(WorkspaceSlot::ready)
            .find_map(|workspace| workspace.open(uri))
            .or_else(|| self.detached.open(uri))
    }

    /// Route the open document at `uri` to the analysis that answers for it, and reflect its
    /// current text there.
    ///
    /// A loaded project workspace that owns it takes it; anything else — a file under no manifest,
    /// an unsaved buffer, a project file whose assembly is still in flight — is mounted into the
    /// detached group for its directory. This is the one place that decision is made.
    ///
    /// Membership changes republish the affected group: gaining or losing a sibling changes what
    /// the *other* documents in it resolve, and nothing else would go back and say so. An edit to
    /// a document already routed changes only that document, and republishes nothing extra.
    async fn route_document(&mut self, uri: &Url) {
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        let owned = self
            .workspaces
            .iter_mut()
            .filter_map(WorkspaceSlot::ready_mut)
            .find(|workspace| workspace.owns_uri(uri));
        let republish = if let Some(workspace) = owned {
            workspace.set_overlay(uri, &doc).await;
            // It may have been answered detached until now — while its project was assembling, or
            // before a source root grew to cover it.
            self.detached.forget(std::slice::from_ref(uri)).await
        } else {
            self.detached.mount(uri, &doc.content).await
        };
        self.republish(republish).await;
    }

    /// Publish diagnostics for documents whose analysis changed for a reason of their own — a
    /// sibling arriving or leaving their group — rather than an edit to themselves.
    ///
    /// Not observed by any test: the actor's tests drive a `ClientSocket::new_closed()`, which
    /// swallows every notification, so they can only assert that the *analysis* changed (by asking
    /// it again) and not that the client was told. Changing this is therefore a change no test
    /// will catch.
    async fn republish(&mut self, uris: Vec<Url>) {
        for uri in uris {
            self.publish_diagnostics(&uri).await;
        }
    }

    /// Compute and push diagnostics for `uri` (a no-op if the document is not open).
    ///
    /// The assembly policy (syntax + lint + cross-file resolution, ordering, suppression) is
    /// [`jals_editor::FileDiagnostics`], driven through the analysis that owns the document —
    /// which folds in that analysis's index and resolved feature set. A file under no manifest is
    /// answered by its detached group, so in-file subtyping and stdlib-classified exceptions check
    /// there exactly as they do in a project.
    async fn publish_diagnostics(&mut self, uri: &Url) {
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        let config = self.lint_discovery.for_uri(uri).await;
        let diagnostics = match self.workspace_for(uri) {
            Some(workspace) => workspace.diagnostics(&config).await,
            None => Vec::new(),
        };
        let _ = self
            .client
            .notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version: Some(doc.version),
            });
    }

    // ---- Requests -------------------------------------------------------------------------------
    //
    // Each request answers through the workspace that owns `uri`, falling back to the one-file
    // project over the open document for files outside any indexed workspace (and for workspace
    // queries that answer `None`), exactly as before the actor split.

    fn document_symbol(&self, uri: &Url) -> Option<DocumentSymbolResponse> {
        self.store.get(uri).map(|doc| {
            DocumentSymbolResponse::Nested(
                LspHost.symbols(&doc.content, Outline::of(&doc.content.parse.syntax())),
            )
        })
    }

    /// A file in the project index highlights cross-file type names precisely through the
    /// workspace; any other document falls back to the one-file project over the open document
    /// alone (a lexical match for such a name).
    async fn document_highlight(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<Vec<DocumentHighlight>>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(Some(workspace.document_highlight(position).await))
    }

    /// A file in the project index resolves cross-file (and file-locally) through the workspace.
    /// A `None` answer falls back to one-file resolution against the open document alone.
    async fn definition(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<GotoDefinitionResponse>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(workspace
            .definition(position)
            .await
            .map(GotoDefinitionResponse::Scalar))
    }

    /// A file in an indexed project finds references project-wide through the workspace (a
    /// project type used from any source file); any other document falls back to one-file
    /// references over the open document alone.
    async fn references(
        &self,
        uri: &Url,
        position: Position,
        include_declaration: bool,
    ) -> Result<Option<Vec<Location>>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(Some(
            workspace.references(position, include_declaration).await,
        ))
    }

    /// A file in an indexed project validates project types project-wide through the workspace;
    /// any other document falls back to one-file renamability over the open document alone.
    async fn prepare_rename(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<PrepareRenameResponse>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(workspace
            .prepare_rename(position)
            .await
            .map(PrepareRenameResponse::Range))
    }

    /// A file in an indexed project renames project types project-wide through the workspace;
    /// any other document falls back to a one-file rename over the open document alone.
    async fn rename(
        &self,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>, ResponseError> {
        // Reject a new name that is not a single legal Java identifier before producing any
        // edit, so the editor surfaces the error instead of writing broken source.
        if !Ident::is_valid_java_identifier(new_name).await {
            return Err(ResponseError::new(
                ErrorCode::INVALID_PARAMS,
                format!("`{new_name}` is not a valid Java identifier"),
            ));
        }
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(workspace.rename(position, new_name).await)
    }

    /// A file in the project index completes members with cross-file type names through the
    /// workspace; any other document falls back to a one-file index of the open document.
    async fn completion(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<CompletionResponse>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(Some(CompletionResponse::Array(
            workspace.completions(position).await,
        )))
    }

    /// A file in the project index infers with cross-file type names through the workspace; any
    /// other document falls back to one-file inference against the open document alone.
    async fn hover(&self, uri: &Url, position: Position) -> Result<Option<Hover>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(workspace.hover(position).await)
    }

    /// A file in the project index resolves overloads with cross-file type names through the
    /// workspace; any other document falls back to a one-file index of the open document.
    async fn signature_help(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<SignatureHelp>, ResponseError> {
        let Some(workspace) = self.workspace_for(uri) else {
            return Ok(None);
        };
        Ok(workspace.signature_help(position).await)
    }

    async fn formatting(&mut self, uri: &Url) -> Result<Option<Vec<TextEdit>>, ResponseError> {
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        let config = self.discovery.for_uri(uri).await;
        // The dialect the *owning project* enabled, not the server's: a document outside every
        // workspace has no manifest to answer for it, and the empty set is what the formatter
        // reads as "do not write dialect syntax".
        let features = self
            .workspace_for(uri)
            .map_or_else(jals_config::FeatureSet::default, |workspace| {
                workspace.feature_set()
            });
        let formatted = Formatting::formatting_edits(&doc.content, &config, features).await;
        // No edits is the *same* response as "already formatted", so a refusal has to say so out of
        // band or the command looks like it did nothing. `window/showMessage` rather than a
        // diagnostic: the fail-safe's subject is the whole file, and there is no range to point at.
        if formatted.fell_back {
            let _ = self
                .client
                .notify::<notification::ShowMessage>(ShowMessageParams {
                    typ: MessageType::WARNING,
                    message: format!(
                        "jals: the formatter could not vouch for its output for {uri}, so the \
                         document was left unchanged. This is a bug in jals-fmt, not in the source.",
                    ),
                });
        }
        Ok(Some(formatted.edits))
    }

    fn folding_range(&self, uri: &Url) -> Option<Vec<FoldingRange>> {
        self.store.get(uri).map(|doc| {
            Folds::of(
                &doc.content.parse.syntax(),
                &doc.content.text,
                &doc.content.line_index,
            )
            .into_iter()
            .map(|fold| LspHost.fold(fold))
            .collect()
        })
    }

    fn selection_range(&self, uri: &Url, positions: &[Position]) -> Option<Vec<SelectionRange>> {
        self.store.get(uri).map(|doc| {
            let root = doc.content.parse.syntax();
            positions
                .iter()
                .map(|position| {
                    let offset = LspHost.offset(&doc.content, position);
                    LspHost.selection(&doc.content, SelectionChains::at(&root, offset))
                })
                .collect()
        })
    }

    // ---- Semantic tokens ------------------------------------------------------------------------

    /// The document's delta-encoded semantic tokens, classified against the index of whichever
    /// analysis owns it. `None` if this server holds no analysis for `uri`.
    async fn compute_semantic_tokens(&self, uri: &Url) -> Option<Vec<SemanticToken>> {
        Some(self.workspace_for(uri)?.semantic_tokens().await?.data)
    }

    /// Mint a fresh `result_id` for a semantic-tokens response.
    fn next_semantic_tokens_result_id(&mut self) -> String {
        self.semantic_tokens_result_id += 1;
        self.semantic_tokens_result_id.to_string()
    }

    /// The full semantic-tokens response for `uri`, tagged with a fresh `result_id` and cached as
    /// the baseline for a later `full/delta`. `None` if the document is not open.
    async fn semantic_tokens_full_response(&mut self, uri: &Url) -> Option<SemanticTokensResult> {
        let data = self.compute_semantic_tokens(uri).await?;
        let result_id = self.next_semantic_tokens_result_id();
        self.semantic_tokens_cache
            .insert(uri.clone(), (result_id.clone(), data.clone()));
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: Some(result_id),
            data,
        }))
    }

    /// The `full/delta` response for `uri` against the client's `previous_result_id`: just the
    /// edits since that baseline when the server still holds it, otherwise the full token set.
    /// Either way a fresh `result_id` is minted and cached. `None` if the document is not open.
    async fn semantic_tokens_delta_response(
        &mut self,
        uri: &Url,
        previous_result_id: &str,
    ) -> Option<SemanticTokensFullDeltaResult> {
        let data = self.compute_semantic_tokens(uri).await?;
        let result_id = self.next_semantic_tokens_result_id();
        // If the client still holds the baseline we cached under `previous_result_id`, compute
        // the edits turning it into the current tokens — borrowing it in place, before we
        // overwrite the cache below, so a stale/evicted id costs no clone of the previous token
        // array.
        let edits = self
            .semantic_tokens_cache
            .get(uri)
            .filter(|(cached_id, _)| *cached_id == previous_result_id)
            .map(|(_, cached_data)| LspHost::tokens_delta(cached_data, &data));
        self.semantic_tokens_cache
            .insert(uri.clone(), (result_id.clone(), data.clone()));
        Some(match edits {
            // A matching baseline: reply with just the edits turning it into the current tokens.
            Some(edits) => SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta {
                result_id: Some(result_id),
                edits,
            }),
            // No matching baseline (evicted, or a stale id): reply with the full token set.
            None => SemanticTokensFullDeltaResult::Tokens(SemanticTokens {
                result_id: Some(result_id),
                data,
            }),
        })
    }
}

impl AssembledWorkspace {
    /// A failure the *host* observed about running the procedure, rather than one the procedure
    /// reported as a value: a panicked assembly, an unreadable manifest, storage that would not open.
    ///
    /// Spelled from the same vocabulary and mapped through the same function as everything else this
    /// server publishes, so a client sees one shape regardless of which side noticed.
    fn host_diagnostic(code: ProjectDiagnosticCode, message: String) -> Diagnostic {
        Self::lsp_diagnostic(
            &ProjectDiagnostic {
                anchor: ProjectAnchor::Manifest,
                span: None,
                severity: ProjectDiagnosticSeverity::Error,
                code,
                message,
            },
            None,
        )
    }

    /// One [`ProjectDiagnostic`] in this protocol's shape.
    ///
    /// `anchor_text` is the text of *this diagnostic's own* [`ProjectAnchor`], or `None` when this
    /// server does not hold that file. The caller picks it through the same match that routes the
    /// diagnostic to a URI, so the pairing this used to guard on is now structural: there is no
    /// second place left for a manifest-anchored span to be resolved against the script's text.
    fn lsp_diagnostic(diagnostic: &ProjectDiagnostic, anchor_text: Option<&str>) -> Diagnostic {
        // This protocol cannot say "no location", so it always names one. Without the text there is
        // nothing to convert against at all, and the head of the file is this protocol's own
        // fallback rather than a placement rule.
        let range = anchor_text.map_or_else(
            || Range::new(Position::new(0, 0), Position::new(0, 1)),
            |source| {
                let span = diagnostic.placement_in(source);
                let index = LineIndex::new(source);
                let start = index.position(source, span.start);
                let end = index.position(source, span.end);
                Range::new(
                    Position::new(start.line, start.character),
                    Position::new(end.line, end.character),
                )
            },
        );

        Diagnostic {
            range,
            severity: Some(match diagnostic.severity {
                ProjectDiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
                ProjectDiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
                ProjectDiagnosticSeverity::Info => DiagnosticSeverity::INFORMATION,
            }),
            code: Some(NumberOrString::String(diagnostic.code.as_str().to_owned())),
            source: Some(
                match diagnostic.anchor {
                    ProjectAnchor::Script(_) => "jals-build",
                    ProjectAnchor::Manifest => "jals-project",
                }
                .to_owned(),
            ),
            // The parenthetical is this host's, and about this host: it answers "why are you
            // telling me instead of doing it?", a question a `jals build` reader never asks. One
            // message string in this protocol, so it goes inline rather than on its own line.
            message: diagnostic.code.remedy().map_or_else(
                || diagnostic.message.clone(),
                |remedy| {
                    format!(
                        "{}; {remedy} (the language server never does)",
                        diagnostic.message
                    )
                },
            ),
            ..Diagnostic::default()
        }
    }

    /// Assemble one project's full analysis + navigation inputs against a fresh aggregate:
    /// run its optional build script, snapshot the effective manifest's scopes, resolve the
    /// classpath (async HTTP through the native fetch adapter), and stage/materialize navigation
    /// sources. Runs on a spawned task, off the actor's queue; stderr is safe to log on (the LSP
    /// protocol owns stdout, not stderr).
    #[cfg(test)]
    async fn assemble(
        manifest: &Manifest,
        root: &Path,
        exec: Exec,
    ) -> Result<Self, WorkspaceAssemblyFailure> {
        Self::assemble_with_blocked(manifest, root, exec, &[], &FeatureSelection::default()).await
    }

    async fn assemble_with_blocked(
        manifest: &Manifest,
        root: &Path,
        exec: Exec,
        blocked_files: &[FileKey],
        selection: &FeatureSelection,
    ) -> Result<Self, WorkspaceAssemblyFailure> {
        // Scripts receive a complete project snapshot because project.read/project.walk_files can
        // address any project-relative input. Script and generated-output I/O stays entirely in
        // jals-storage; successful output is already in this aggregate's new revision.
        let configured_script = manifest
            .build
            .script
            .as_ref()
            .and_then(|script| match script {
                BuildScript::Rhai { file } => FileKey::parse(file).ok(),
            });
        let has_build_script = configured_script.is_some();
        let scopes = if has_build_script {
            vec![NativeScope::all(RelativePath::ROOT)]
        } else {
            let mut scopes = jals_classpath::NativeProjectPlan::snapshot_scopes(manifest, root);
            scopes.push(NativeScope::all(
                RelativePath::parse("target/jals/build/tasks/ownership-v1.json")
                    .expect("ownership path is portable"),
            ));
            scopes
        };
        let mut storage = NativeStorage::for_project_scoped(root, scopes, exec.clone())
            .await
            .map_err(|error| {
                let message = format!("opening project storage failed: {error}");
                WorkspaceAssemblyFailure {
                    project_diagnostics: vec![Self::host_diagnostic(
                        ProjectDiagnosticCode::ProjectStorage,
                        message.clone(),
                    )],
                    message,
                    fallback: None,
                }
            })?;
        let mut effective_manifest = manifest.clone();
        let mut build_script_watch = configured_script.clone().map(|script| BuildWatchPolicy {
            script,
            rerun_files: BTreeSet::new(),
        });
        let mut project_sources = BTreeSet::new();
        // The root project resolves the client's feature selection (`initialize` options /
        // configuration) — defaulting to the manifest's own `default` list, the same selection a
        // plain `jals build` uses, so what the editor analyses matches what the build produces.
        // Dependency nodes resolve their own sets during graph preprocessing, from the
        // `[dependencies]` entries pointing at them plus whatever this selection forwards. An
        // invalid selection (an unknown feature name) degrades to the default rather than
        // failing the whole workspace; the mistake still surfaces on the command line.
        let features = manifest
            .resolve_build_features(
                &selection.features,
                selection.all_features,
                selection.no_default_features,
            )
            .unwrap_or_else(|error| {
                eprintln!("jals-lsp: invalid feature selection ({error}); using defaults");
                manifest
                    .resolve_build_features(&[], false, false)
                    .unwrap_or_default()
            });
        let environment =
            BuildScriptEnvironment::new().for_project(manifest, features.features().clone());
        let limits = BuildScriptLimits::default();
        let scripts = GraphScriptInputs {
            environment: &environment,
            features: &features,
            limits: &limits,
        };
        // Analysis consumes what the user's own build already fetched and verified into the cache;
        // it does not fetch. Opening a folder runs whatever `build.rhai` it contains, and nobody
        // reviews a repository before opening it in an editor — reaching the network on that signal
        // alone would let an unread script pull (and send) whatever it likes the moment a project is
        // opened. `jals build` populates the cache, and the server picks it up from there.
        //
        // Stated here because the policy is part of the capability: every phase handed this fetcher
        // inherits the refusal, which is what the separate `network` field beside it could not do.
        let fetcher = jals_classpath::ReqwestFetcher::for_project(
            root.to_path_buf(),
            jals_classpath::NetworkPolicy::Offline,
        );
        // The script's text, read once and unconditionally when one is configured: it costs an
        // in-memory read of a file this aggregate already holds, and a failure that turns out to
        // carry a position has nowhere to get it from afterwards.
        let script_text = configured_script
            .as_ref()
            .and_then(|script| storage.view().file_text(script).ok().map(ToOwned::to_owned));
        let (script, script_error) = match ProjectAssembly::script(
            &exec,
            &fetcher,
            &mut storage,
            &mut BuildScriptSession::new(),
            RootBuildScriptOptions {
                manifest,
                environment: &environment,
                limits: &limits,
                host: BuildTaskHost::Project,
                blocked_files,

                publications: jals_project::SourcePublication::Apply,
            },
        )
        .await
        {
            Ok(script) => {
                if let Some(output) = script.output() {
                    build_script_watch
                        .as_mut()
                        .expect("a configured script has a watch policy")
                        .rerun_files
                        .clone_from(&output.rerun_files);
                    project_sources.clone_from(&output.generated_sources);
                }
                // The manifest retained here is what `watch_policy` classifies host changes
                // against, so the script's classpath directives have to land on it and not only
                // inside the assembly.
                script.augment_classpath(&mut effective_manifest);
                (script, None)
            }
            // A failed script never stops the workspace: ordinary analysis of what is already on
            // disk is still worth having, and the failure is reported as a diagnostic rather than
            // by refusing to load. The narrative goes to stderr; the diagnostics are assembled
            // once, below, from the error itself.
            Err(error) => {
                eprintln!(
                    "jals-lsp: build script for {} failed; continuing with ordinary project \
                     analysis: {error}",
                    root.display()
                );
                (ProjectScript::skipped(), Some(error))
            }
        };
        // Both halves outlive the assembly calls below, so the outcome can borrow either.
        let script_outcome = match (&script_error, script.output()) {
            (Some(error), _) => ScriptOutcome::Failed(error),
            (None, Some(output)) => ScriptOutcome::Ran(output),
            (None, None) => ScriptOutcome::Skipped,
        };
        let script_file = configured_script.as_ref().map(|key| ScriptFile {
            key,
            text: script_text.as_deref(),
        });
        let assembly =
            match Self::assemble_graph(&script, &effective_manifest, root, &mut storage, &scripts)
                .await
            {
                Ok(assembly) => assembly,
                Err(failure) => {
                    let message = failure.error.to_string();
                    // The root-only fallback below rediscovers without `[dependencies]`, so every
                    // warning about a dependency is reported here or nowhere. The script phase is
                    // deliberately `Skipped`: the fallback's own `finish_assembly` reports it, and
                    // `workspace_ready` concatenates both sets — reporting it here too would
                    // publish every script warning twice on exactly this path.
                    let project_diagnostics = ProjectDiagnostics::assemble(
                        ScriptOutcome::Skipped,
                        GraphOutcome::Failed(&failure),
                        None,
                    )
                    .iter()
                    .map(|diagnostic| Self::lsp_diagnostic(diagnostic, None))
                    .collect();
                    let mut root_only = effective_manifest.clone();
                    root_only.dependencies.clear();
                    let fallback_assembly = match Self::assemble_graph(
                        &script,
                        &root_only,
                        root,
                        &mut storage,
                        &scripts,
                    )
                    .await
                    {
                        Ok(assembly) => assembly,
                        Err(fallback_error) => {
                            return Err(WorkspaceAssemblyFailure {
                                message: format!(
                                    "{message}; root-only fallback failed: {fallback_error}"
                                ),
                                fallback: None,
                                project_diagnostics,
                            });
                        }
                    };
                    let fallback = Self::finish_assembly(
                        storage,
                        &effective_manifest,
                        root,
                        project_sources,
                        build_script_watch,
                        configured_script.clone(),
                        script_outcome,
                        script_file,
                        fallback_assembly,
                        features.into_features(),
                    )
                    .await;
                    return Err(WorkspaceAssemblyFailure {
                        message,
                        fallback: Some(Box::new(fallback)),
                        project_diagnostics,
                    });
                }
            };
        Ok(Self::finish_assembly(
            storage,
            &effective_manifest,
            root,
            project_sources,
            build_script_watch,
            configured_script.clone(),
            script_outcome,
            script_file,
            assembly,
            features.into_features(),
        )
        .await)
    }

    /// The graph phase, under the server's own policy.
    ///
    /// Analysis never reaches the network: opening a folder must not clone a remote the user has not
    /// asked about, and a dependency's build tasks run under the same rule for the same reason. Git
    /// dependencies and task artifacts resolve from what a real `jals build` already acquired and
    /// verified.
    async fn assemble_graph(
        script: &ProjectScript,
        manifest: &Manifest,
        root: &Path,
        storage: &mut NativeStorage,
        scripts: &GraphScriptInputs<'_>,
    ) -> Result<NativeProjectAssembly, GraphResolveError> {
        let exec = storage.exec().clone();
        script
            .resolve_native(
                manifest,
                root,
                storage,
                jals_project::GraphPreprocess {
                    exec: &exec,
                    // Offline, and now that the policy rides the capability, that holds for the
                    // input resolution this phase ends in as well — not just for discovery and the
                    // dependency task plans, which is as far as the old `network` field reached.
                    fetcher: &jals_classpath::ReqwestFetcher::for_project(
                        root.to_path_buf(),
                        jals_classpath::NetworkPolicy::Offline,
                    ),
                    environment: scripts.environment,
                    root_features: scripts.features,
                    limits: scripts.limits,
                },
                // Every open file gets an answer, and a `[test] source-dirs` tree is open like
                // any other — so the scope is the one that carries the types those files name.
                DependencyScope::Test,
                jals_classpath::ProjectInputOptions::Editor,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_assembly(
        mut storage: NativeStorage,
        effective_manifest: &Manifest,
        root: &Path,
        project_sources: BTreeSet<FileKey>,
        build_script_watch: Option<BuildWatchPolicy>,
        configured_script: Option<FileKey>,
        script_outcome: ScriptOutcome<'_>,
        script_file: Option<ScriptFile<'_>>,
        assembly: NativeProjectAssembly,
        build_features: BTreeSet<String>,
    ) -> Self {
        // Everything the procedure had to say, ordered and severity-assigned once. What is left
        // here is splitting it by the file it is anchored to, because LSP publishes per URI.
        //
        // No `ProjectDiagnostics::has_errors` gate, deliberately, and this is the one host that
        // has none: the CLI and the browser stop on an error because they were asked to produce
        // something, and this server was asked to be useful *while* the project is wrong. The
        // errors are published against their anchors and the workspace loads anyway.
        let assembled = ProjectDiagnostics::assemble(
            script_outcome,
            GraphOutcome::Resolved(assembly.report()),
            script_file,
        );
        let script_text = script_file.and_then(|file| file.text);
        let mut script_diagnostics = Vec::new();
        let mut project_diagnostics = Vec::new();
        for diagnostic in &assembled {
            // stderr is a plain string with no severity channel of its own, so it carries one.
            eprintln!(
                "jals-lsp: {}: {}: {}",
                root.display(),
                diagnostic.severity.lead(),
                diagnostic.message
            );
            // One match for both halves of the same question: which URI this is published to, and
            // whose text it is placed against. Answering them apart is how the placement rule and
            // the routing rule drifted three hundred lines away from each other.
            let (anchor_text, bucket) = match diagnostic.anchor {
                ProjectAnchor::Script(_) => (script_text, &mut script_diagnostics),
                // No manifest text here, deliberately. This server reads `jals.toml` through
                // `Manifest::from_file`, which drops it, and the project snapshot captures it only
                // when a configured build script forces whole-root scoping — so reading it back
                // would place a manifest diagnostic on its first line in some projects and at the
                // head of the file in others, for a reason nothing in the manifest states.
                ProjectAnchor::Manifest => (None, &mut project_diagnostics),
            };
            bucket.push(Self::lsp_diagnostic(diagnostic, anchor_text));
        }

        let NativeProjectAssembly {
            inputs,
            source_roots,
            watch_paths,
            ..
        } = assembly;

        // Navigation sources are cache artifacts, not host paths. Mount them as overlay files in
        // the same aggregate so the editor reads them from this exact revision, and materialize
        // each one out of the cache so its definition targets are real, openable files.
        let mut materialized = BTreeMap::new();
        let mut mounts = Vec::new();
        let mut library_sources = Vec::new();
        for source in &inputs.library_sources {
            if let Some(key) =
                Self::stage_artifact(&storage, "library", source, &mut mounts, &mut materialized)
                    .await
            {
                library_sources.push(key);
            }
        }
        let mut source_dep_sources = Vec::new();
        for source in &inputs.source_dep_sources {
            match source {
                jals_classpath::SourceFile::Project(key) => source_dep_sources.push(key.clone()),
                jals_classpath::SourceFile::Artifact(source) => {
                    if let Some(key) = Self::stage_artifact(
                        &storage,
                        "source-dependency",
                        source,
                        &mut mounts,
                        &mut materialized,
                    )
                    .await
                    {
                        source_dep_sources.push(key);
                    }
                }
            }
        }
        // One revision bump and tree rebuild for the whole batch — mounting the sources one
        // `set_overlay` at a time rebuilds the merged tree per file, quadratic in mount count.
        // On failure the mounts are simply absent and the workspace loads without them.
        let revision = storage.revision();
        if let Err(error) = storage.set_overlays(revision, mounts) {
            eprintln!("jals-lsp: mounting dependency sources failed: {error}");
        }
        let watch_policy = Self::watch_policy(
            effective_manifest,
            root,
            &source_roots,
            &project_sources,
            build_script_watch,
            &watch_paths,
        );
        Self {
            storage,
            source_roots,
            project_sources: project_sources.into_iter().collect(),
            classpath_classes: inputs.classpath_classes,
            feature_set: inputs.feature_set,
            build_features,
            library_sources,
            source_dep_sources,
            materialized,
            watch_policy,
            configured_script,
            script_diagnostics,
            project_diagnostics,
        }
    }

    fn watch_policy(
        manifest: &Manifest,
        root: &Path,
        source_roots: &[DirKey],
        project_sources: &BTreeSet<FileKey>,
        build_script: Option<BuildWatchPolicy>,
        graph_watch_paths: &[PathBuf],
    ) -> ProjectWatchPolicy {
        fn normalize(path: &Path) -> PathBuf {
            let mut normalized = PathBuf::new();
            for component in path.components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir if normalized.pop() => {}
                    _ => normalized.push(component.as_os_str()),
                }
            }
            normalized
        }

        fn local_path(root: &Path, value: &str) -> Option<PathBuf> {
            let path = Path::new(value);
            if path.is_absolute() {
                return Some(normalize(path));
            }
            if let Ok(url) = Url::parse(value) {
                return (url.scheme() == "file")
                    .then(|| url.to_file_path().ok())
                    .flatten();
            }
            Some(normalize(&root.join(path)))
        }

        let mut reassemble_inputs = Vec::new();
        for source in &manifest.build.source_dirs {
            if let Some(path) = local_path(root, source)
                && !path.starts_with(root)
            {
                reassemble_inputs.push(path);
            }
        }
        reassemble_inputs.extend(
            manifest
                .build
                .classpath
                .iter()
                .filter_map(|path| local_path(root, path)),
        );
        for dependency in manifest.dependencies.values() {
            match dependency {
                Dependency::Jar(jar) => {
                    reassemble_inputs.extend(
                        core::iter::once(&jar.jar)
                            .chain(jar.sources.iter())
                            .filter_map(|path| local_path(root, path)),
                    );
                }
                Dependency::Path(path) => {
                    if let Some(path) = local_path(root, &path.path) {
                        reassemble_inputs.push(path);
                    }
                }
                Dependency::Git(_) => {}
            }
        }
        reassemble_inputs.extend(graph_watch_paths.iter().cloned());
        reassemble_inputs.sort();
        reassemble_inputs.dedup();

        let mut source_roots = source_roots.to_vec();
        source_roots.sort();
        source_roots.dedup();
        ProjectWatchPolicy {
            source_roots,
            project_sources: project_sources.clone(),
            reassemble_inputs,
            build_script,
        }
    }

    /// Stage a cached navigation source for mounting into the aggregate's overlay under
    /// `.jals/<kind>/…`, returning its overlay key. `None` skips an artifact that is missing
    /// from the cache or whose path cannot be addressed. The artifact is also materialized to a
    /// real file under the cache root and recorded in `materialized`, so go-to-definition
    /// targets resolve to a `file://` URL the client can actually open. The caller commits the
    /// staged batch with one `set_overlays`.
    async fn stage_artifact(
        storage: &NativeStorage,
        kind: &str,
        source: &jals_classpath::LibrarySource,
        mounts: &mut Vec<(FileKey, Vec<u8>)>,
        materialized: &mut BTreeMap<FileKey, PathBuf>,
    ) -> Option<FileKey> {
        let bytes = storage
            .artifacts()
            .lookup(&source.key)
            .await
            .ok()
            .flatten()?;
        let mount_root = DirKey::parse(&format!(".jals/{kind}")).ok()?;
        let key = mount_root.file_at(&source.path).ok()?;
        mounts.push((key.clone(), bytes));
        // Best-effort: a failed materialization keeps the mount (analysis still works), it only
        // degrades navigation into this one file.
        if let Ok(target) = storage
            .artifacts()
            .materialize_file(&source.key, &source.path)
            .await
        {
            materialized.insert(key.clone(), target);
        }
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{
        FileChangeType, FileEvent, TextDocumentContentChangeEvent, TextDocumentIdentifier,
        TextDocumentItem, VersionedTextDocumentIdentifier,
    };
    use jals_build::build_script::{BuildScriptDiagnostic, BuildScriptError};
    use jals_exec::block_on_inline;
    use jals_project::RootBuildScriptError;

    use super::*;

    /// An actor over the inline executor and a closed client socket, plus its command channel.
    /// The inline executor drives spawned assemblies to completion synchronously, so a
    /// `WorkspaceReady` is already queued when `did_open` returns — tests drain it with
    /// [`drain`].
    fn actor() -> (
        Actor,
        mpsc::UnboundedReceiver<Cmd>,
        mpsc::UnboundedSender<Cmd>,
    ) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let actor = Actor::new(ClientSocket::new_closed(), Exec::inline(), sender.clone());
        (actor, receiver, sender)
    }

    fn changed(path: &Path) -> (PathBuf, FileChangeType) {
        (path.to_path_buf(), FileChangeType::CHANGED)
    }

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn diagnostic_code(diagnostic: &Diagnostic) -> Option<&str> {
        match diagnostic.code.as_ref() {
            Some(NumberOrString::String(code)) => Some(code),
            Some(NumberOrString::Number(_)) | None => None,
        }
    }

    fn scripted_dependency_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "jals.toml",
            "[build]\nsource-dirs = [\"src\"]\n\
             [dependencies]\ngenerated = { path = \"dependency\" }\n",
        );
        write(
            dir.path(),
            "dependency/jals.toml",
            "[build]\nsource-dirs = [\"src\"]\n\
             script = { type = \"rhai\", file = \"build.rhai\" }\n",
        );
        write(
            dir.path(),
            "dependency/build.rhai",
            r#"
                let source = output.write_text(
                    "p/Generated.java",
                    "package p; public class Generated {}\n",
                );
                build.add_source(source);
                build.add_javac_arg("-dependency-directive-must-not-propagate");
                build.add_jvm_arg("-dependency-directive-must-not-propagate");
            "#,
        );
        write(
            dir.path(),
            "src/Main.java",
            "package p; class Main { Generated value; }",
        );
        dir
    }

    /// Process every command already queued (e.g. a `WorkspaceReady` from an inline assembly).
    async fn drain(actor: &mut Actor, receiver: &mut mpsc::UnboundedReceiver<Cmd>) {
        while let Ok(cmd) = receiver.try_recv() {
            actor.process(cmd).await;
        }
    }

    async fn open(
        actor: &mut Actor,
        receiver: &mut mpsc::UnboundedReceiver<Cmd>,
        path: std::path::PathBuf,
        text: &str,
    ) {
        std::fs::write(&path, text).unwrap();
        actor
            .process(Cmd::DidOpen(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(path).unwrap(),
                    language_id: "java".into(),
                    version: 1,
                    text: text.into(),
                },
            }))
            .await;
        drain(actor, receiver).await;
    }

    /// Open `uri` with `text` without writing anything to disk — the only way to open a document
    /// that has no host path at all.
    async fn open_uri(
        actor: &mut Actor,
        receiver: &mut mpsc::UnboundedReceiver<Cmd>,
        uri: &Url,
        text: &str,
    ) {
        actor
            .process(Cmd::DidOpen(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "java".into(),
                    version: 1,
                    text: text.into(),
                },
            }))
            .await;
        drain(actor, receiver).await;
    }

    async fn close(actor: &mut Actor, uri: &Url) {
        actor
            .process(Cmd::DidClose(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            }))
            .await;
    }

    /// The codes of the diagnostics the owning analysis reports for `uri`.
    async fn codes(actor: &Actor, uri: &Url) -> Vec<String> {
        let Some(workspace) = actor.workspace_for(uri) else {
            return Vec::new();
        };
        workspace
            .diagnostics(&jals_config::lint::Config::default())
            .await
            .iter()
            .filter_map(|diagnostic| diagnostic_code(diagnostic).map(str::to_owned))
            .collect()
    }

    // ---- Routing: every open document reaches an analysis --------------------------------------

    /// The invariant the whole routing seam buys: a document the server treats as Java is always
    /// answered by *some* workspace. Whether it is a project's or a detached group's is the
    /// server's business, not the request handler's — which is why no handler asks any more.
    #[test]
    fn every_open_java_document_has_a_workspace() {
        block_on_inline(async {
            let (mut actor, mut receiver, _sender) = actor();

            // A project, opened but not yet assembled: the slot is still `Loading`.
            let project = tempfile::tempdir().unwrap();
            write(
                project.path(),
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\n",
            );
            write(project.path(), "src/Main.java", "class Main {}");
            let main = Url::from_file_path(project.path().join("src/Main.java")).unwrap();
            actor
                .process(Cmd::DidOpen(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: main.clone(),
                        language_id: "java".into(),
                        version: 1,
                        text: "class Main {}".into(),
                    },
                }))
                .await;
            assert!(
                actor.workspace_for(&main).is_some(),
                "a project file answers while its project is still assembling"
            );
            drain(&mut actor, &mut receiver).await;
            assert!(
                actor.workspace_for(&main).is_some(),
                "and still answers once the project is installed"
            );

            // A file under the project root but outside every source root: no source root covers
            // it, so the project workspace does not own it and it stays detached.
            let stray = project.path().join("Stray.java");
            open(&mut actor, &mut receiver, stray.clone(), "class Stray {}").await;

            // Two files in one directory under no manifest, and an unsaved buffer.
            let loose = tempfile::tempdir().unwrap();
            open(
                &mut actor,
                &mut receiver,
                loose.path().join("A.java"),
                "class A {}",
            )
            .await;
            open(
                &mut actor,
                &mut receiver,
                loose.path().join("B.java"),
                "class B {}",
            )
            .await;
            let untitled = Url::parse("untitled:Untitled-1").unwrap();
            open_uri(&mut actor, &mut receiver, &untitled, "class Draft {}").await;

            let open_uris: Vec<Url> = actor.store.uris().cloned().collect();
            assert_eq!(open_uris.len(), 5);
            for uri in open_uris {
                assert!(
                    actor.workspace_for(&uri).is_some(),
                    "no analysis answers for {uri}"
                );
            }
            // One group for the loose directory, one for the stray file's directory (the project
            // root), one for the buffer that has no directory at all.
            assert_eq!(actor.detached.len(), 3);
            assert!(actor.detached.holds(&Url::from_file_path(&stray).unwrap()));
            assert!(actor.detached.holds(&untitled));
        });
    }

    /// Two files in one directory are one Java package, so each is a typing authority for the
    /// other. This is the whole reason a detached group is a directory and not a document.
    #[test]
    fn siblings_in_one_directory_resolve_each_other() {
        block_on_inline(async {
            let (mut actor, mut receiver, _sender) = actor();
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("A.java");
            let a_uri = Url::from_file_path(&a).unwrap();
            let b = dir.path().join("B.java");
            let b_uri = Url::from_file_path(&b).unwrap();

            open(&mut actor, &mut receiver, a.clone(), "class A { B b; }").await;
            assert!(
                codes(&actor, &a_uri)
                    .await
                    .iter()
                    .any(|c| c == "cannot-resolve"),
                "nothing declares B yet"
            );

            open(&mut actor, &mut receiver, b, "class B {}").await;
            assert!(
                !codes(&actor, &a_uri)
                    .await
                    .iter()
                    .any(|c| c == "cannot-resolve"),
                "opening the sibling that declares B resolves it"
            );

            // And the reference navigates across the two files.
            let location = actor
                .workspace_for(&a_uri)
                .expect("A.java is routed")
                .definition(Position::new(0, "class A { ".len() as u32))
                .await
                .expect("B resolves to its declaration");
            assert_eq!(
                location.uri, b_uri,
                "the location carries the sibling's URI"
            );

            // A rename in the group rewrites the sibling too. This is the change most likely to
            // surprise: the edit touches a file the user never focused.
            let edit = actor
                .rename(
                    &a_uri,
                    Position::new(0, "class A { ".len() as u32),
                    "Renamed",
                )
                .await
                .expect("rename is answered")
                .expect("B is a renamable type of this group");
            let changes = edit.changes.expect("a plain-edit workspace edit");
            assert_eq!(
                changes.keys().collect::<BTreeSet<_>>(),
                BTreeSet::from([&a_uri, &b_uri]),
                "the reference and the declaration are in different files"
            );
            assert!(changes.values().flatten().all(|e| e.new_text == "Renamed"));

            // Closing the sibling takes the declaration away again.
            close(&mut actor, &b_uri).await;
            assert!(
                codes(&actor, &a_uri)
                    .await
                    .iter()
                    .any(|c| c == "cannot-resolve"),
                "closing the sibling unresolves B"
            );
        });
    }

    /// A group is exactly its open documents, so the last close takes the group with it — nothing
    /// is kept alive by a directory the user merely looked at once.
    #[test]
    fn a_detached_group_drops_when_its_last_document_closes() {
        block_on_inline(async {
            let (mut actor, mut receiver, _sender) = actor();
            let dir = tempfile::tempdir().unwrap();
            let a = Url::from_file_path(dir.path().join("A.java")).unwrap();
            let b = Url::from_file_path(dir.path().join("B.java")).unwrap();
            open(
                &mut actor,
                &mut receiver,
                dir.path().join("A.java"),
                "class A {}",
            )
            .await;
            open(
                &mut actor,
                &mut receiver,
                dir.path().join("B.java"),
                "class B {}",
            )
            .await;
            assert_eq!(actor.detached.len(), 1);

            close(&mut actor, &a).await;
            assert_eq!(actor.detached.len(), 1, "B.java still holds the group open");
            assert!(!actor.detached.holds(&a));
            assert!(actor.detached.holds(&b));

            close(&mut actor, &b).await;
            assert!(actor.detached.is_empty(), "the last close drops the group");
        });
    }

    /// An unsaved buffer has no host path, so its group is itself and its address is the URI the
    /// client opened it with — which a location has to come back carrying.
    #[test]
    fn an_untitled_document_answers_from_its_own_group() {
        block_on_inline(async {
            let (mut actor, mut receiver, _sender) = actor();
            let uri = Url::parse("untitled:Untitled-1").unwrap();
            let text = "class Draft { void run() { Draft d = new Draft(); } }";
            open_uri(&mut actor, &mut receiver, &uri, text).await;

            let location = actor
                .workspace_for(&uri)
                .expect("the buffer is routed")
                .definition(Position::new(0, text.find("Draft d").unwrap() as u32))
                .await
                .expect("the self-reference resolves");
            assert_eq!(
                location.uri, uri,
                "a mounted key renders back as the URI it was opened with"
            );
            let hover = actor
                .workspace_for(&uri)
                .unwrap()
                .hover(Position::new(0, text.find("new Draft").unwrap() as u32))
                .await
                .expect("an unsaved buffer still infers types");
            assert!(format!("{hover:?}").contains("Draft"), "{hover:?}");
        });
    }

    /// A project file opened before its assembly finishes is answered detached, then hands over.
    /// Both halves matter: the immediate answer, and that the detached mount does not linger.
    #[test]
    fn a_document_migrates_from_its_detached_group_to_its_project() {
        block_on_inline(async {
            let (mut actor, mut receiver, _sender) = actor();
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\n",
            );
            write(dir.path(), "src/Main.java", "class Main { Helper h; }");
            write(dir.path(), "src/Helper.java", "class Helper {}");
            let main = Url::from_file_path(dir.path().join("src/Main.java")).unwrap();

            actor
                .process(Cmd::DidOpen(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: main.clone(),
                        language_id: "java".into(),
                        version: 1,
                        text: "class Main { Helper h; }".into(),
                    },
                }))
                .await;
            assert!(
                actor.detached.holds(&main),
                "the assembly is still in flight, so the document is answered detached"
            );
            // Detached, only the open document is indexed, so its unopened sibling is unresolved.
            assert!(
                codes(&actor, &main)
                    .await
                    .iter()
                    .any(|c| c == "cannot-resolve")
            );

            drain(&mut actor, &mut receiver).await;
            assert!(
                !actor.detached.holds(&main),
                "installing the project workspace evicts the detached mount"
            );
            assert!(actor.detached.is_empty(), "and drops the emptied group");
            assert!(
                actor.workspace_for(&main).is_some(),
                "the project workspace answers now"
            );
            // The project indexes its whole source root, so the unopened sibling resolves.
            assert!(
                !codes(&actor, &main)
                    .await
                    .iter()
                    .any(|c| c == "cannot-resolve")
            );
        });
    }

    /// `did_open` builds at most one workspace per `jals.toml` project, reuses it for later files
    /// in the same project, and builds none for a file under no manifest — so opening a file in a
    /// manifestless folder never triggers a whole-tree index walk (the Helix freeze regression).
    /// Assembly happens on a spawned task that reports back as `Cmd::WorkspaceReady`; the slot is
    /// `Ready` once that command is processed.
    #[test]
    fn did_open_indexes_one_workspace_per_project() {
        fn project(name: &str) -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("jals.toml"),
                format!("[package]\nname = \"{name}\"\n[build]\nsource-dirs = [\"src\"]\n"),
            )
            .unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            dir
        }

        block_on_inline(async {
            let proj_a = project("a");
            let proj_b = project("b");
            let no_manifest = tempfile::tempdir().unwrap();

            let (mut actor, mut receiver, _sender) = actor();

            open(
                &mut actor,
                &mut receiver,
                proj_a.path().join("src/A.java"),
                "class A {}",
            )
            .await;
            assert_eq!(actor.workspaces.len(), 1, "first file builds one workspace");
            assert!(
                actor.workspaces[0].ready().is_some(),
                "the workspace is ready once WorkspaceReady is processed"
            );

            open(
                &mut actor,
                &mut receiver,
                proj_a.path().join("src/A2.java"),
                "class A2 {}",
            )
            .await;
            assert_eq!(
                actor.workspaces.len(),
                1,
                "a second file in the same project reuses the workspace"
            );

            open(
                &mut actor,
                &mut receiver,
                proj_b.path().join("src/B.java"),
                "class B {}",
            )
            .await;
            assert_eq!(
                actor.workspaces.len(),
                2,
                "a file in a different project adds a second workspace"
            );

            assert!(
                actor.detached.is_empty(),
                "every file so far belongs to a project"
            );
            open(
                &mut actor,
                &mut receiver,
                no_manifest.path().join("C.java"),
                "class C {}",
            )
            .await;
            assert_eq!(
                actor.workspaces.len(),
                2,
                "a file under no manifest builds no project workspace"
            );
            assert_eq!(
                actor.detached.len(),
                1,
                "it is answered by a detached group instead — one file mounted, no tree walk"
            );
        });
    }

    #[test]
    fn project_watch_policy_classifies_sources_dependencies_and_cache() {
        let root = Path::new("project");
        let manifest = root.join("jals.toml");
        let script_path = root.join("build.rhai");
        let input_path = root.join("schema/model.json");
        let source_path = root.join("src/Main.java");
        let output_path = root.join("target/jals/build/rhai/out/Generated.java");
        let cache_path = root.join("target/jals/cache/artifact");
        let classpath = root.join("lib/api.jar");
        let source_dependency = root.join("deps/lib/Lib.java");
        let external_dependency = PathBuf::from("external/lib/External.java");
        let script = FileKey::parse("build.rhai").unwrap();
        let input = FileKey::parse("schema/model.json").unwrap();
        let ordinary = ProjectWatchPolicy {
            source_roots: vec![DirKey::parse("src").unwrap()],
            project_sources: BTreeSet::new(),
            reassemble_inputs: vec![
                root.join("deps/lib"),
                classpath.clone(),
                PathBuf::from("external/lib"),
            ],
            build_script: None,
        };

        assert_eq!(
            Actor::watched_project_action(root, Some(&ordinary), &[changed(&source_path)],),
            WatchedProjectAction::Refresh,
            "authored Java under a source root refreshes in place"
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&ordinary), &[changed(&manifest)]),
            WatchedProjectAction::Reassemble
        );
        for path in [classpath, source_dependency, external_dependency] {
            assert_eq!(
                Actor::watched_project_action(root, Some(&ordinary), &[changed(&path)]),
                WatchedProjectAction::Reassemble,
                "classpath and source dependencies require reassembly: {path:?}"
            );
        }
        assert_eq!(
            Actor::watched_project_action(
                root,
                Some(&ordinary),
                &[changed(&output_path), changed(&cache_path)],
            ),
            WatchedProjectAction::Ignore,
            "generated output and cache feedback are ignored"
        );

        let conservative = ProjectWatchPolicy {
            build_script: Some(BuildWatchPolicy {
                script: script.clone(),
                rerun_files: BTreeSet::new(),
            }),
            ..ordinary.clone()
        };
        assert_eq!(
            Actor::watched_project_action(root, Some(&conservative), &[changed(&source_path)],),
            WatchedProjectAction::Reassemble,
            "an empty rerun set conservatively watches all project files"
        );

        let declared = ProjectWatchPolicy {
            build_script: Some(BuildWatchPolicy {
                script,
                rerun_files: BTreeSet::from([input]),
            }),
            ..ordinary
        };
        assert_eq!(
            Actor::watched_project_action(root, Some(&declared), &[changed(&script_path)],),
            WatchedProjectAction::Reassemble
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&declared), &[changed(&input_path)]),
            WatchedProjectAction::Reassemble
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&declared), &[changed(&source_path)],),
            WatchedProjectAction::Refresh,
            "unrelated files do not rerun a script with declared inputs"
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&declared), &[changed(&output_path)]),
            WatchedProjectAction::Ignore,
            "generated outputs do nothing"
        );
    }

    #[test]
    fn generated_output_deletion_reassembles_while_write_feedback_is_ignored() {
        let root = Path::new("project");
        let output = root.join("target/jals/build/rhai/out/Generated.java");
        let cache = root.join("target/jals/cache/artifact");

        for change_type in [FileChangeType::CREATED, FileChangeType::CHANGED] {
            assert_eq!(
                Actor::watched_project_action(root, None, &[(output.clone(), change_type)],),
                WatchedProjectAction::Ignore
            );
        }
        assert_eq!(
            Actor::watched_project_action(root, None, &[(output, FileChangeType::DELETED)],),
            WatchedProjectAction::Reassemble
        );
        assert_eq!(
            Actor::watched_project_action(root, None, &[(cache, FileChangeType::DELETED)]),
            WatchedProjectAction::Ignore
        );
    }

    /// The client watches `**/*`, and VS Code's default excludes stop at `.git/objects`, so
    /// `.git/index` and `.git/refs/**` reach the server. `NativeStorage` never snapshots `.git`,
    /// so those writes cannot affect analysis — but classifying them as "unknown" made every
    /// `git status` re-run the project's build script.
    #[test]
    fn git_metadata_writes_are_ignored() {
        let root = Path::new("project");
        for relative in [".git/index", ".git/refs/heads/main", ".git/HEAD"] {
            let path = root.join(relative);
            for change_type in [
                FileChangeType::CREATED,
                FileChangeType::CHANGED,
                FileChangeType::DELETED,
            ] {
                assert_eq!(
                    Actor::watched_project_action(root, None, &[(path.clone(), change_type)]),
                    WatchedProjectAction::Ignore,
                    "{relative} must not touch the workspace"
                );
            }
        }
    }

    #[test]
    fn manifest_watch_policy_tracks_external_path_dependencies() {
        let root = Path::new("/workspace/project");
        let manifest: Manifest = r#"
            [build]
            source-dirs = ["src"]
            classpath = ["lib/api.jar"]
            [dependencies]
            shared = { path = "../shared" }
            local = { path = "deps/local" }
        "#
        .parse()
        .unwrap();
        let policy = AssembledWorkspace::watch_policy(
            &manifest,
            root,
            &[DirKey::parse("src").unwrap()],
            &BTreeSet::new(),
            None,
            &[],
        );

        for path in [
            PathBuf::from("/workspace/shared/Shared.java"),
            root.join("deps/local/Local.java"),
            root.join("lib/api.jar"),
        ] {
            assert_eq!(
                Actor::watched_project_action(root, Some(&policy), &[changed(&path)]),
                WatchedProjectAction::Reassemble,
                "manifest-derived input must reassemble: {path:?}"
            );
        }
    }

    #[test]
    fn graph_watch_policy_tracks_transitive_local_dependency_inputs() {
        block_on_inline(async {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("root");
            let child = parent.path().join("child");
            let transitive = parent.path().join("transitive");
            write(
                &root,
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\n\
                 [dependencies]\nchild = { path = \"../child\" }\n",
            );
            write(&root, "src/Main.java", "class Main {}");
            write(
                &child,
                "jals.toml",
                "[dependencies]\ntransitive = { path = \"../transitive\" }\n",
            );
            write(&child, "src/Child.java", "class Child {}");
            write(
                &transitive,
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\n",
            );
            write(&transitive, "src/Transitive.java", "class Transitive {}");
            let manifest = Manifest::from_file(&root.join("jals.toml")).await.unwrap();
            let assembled = AssembledWorkspace::assemble(&manifest, &root, Exec::inline())
                .await
                .unwrap();

            let canonical_child = std::fs::canonicalize(&child).unwrap();
            let canonical_transitive = std::fs::canonicalize(&transitive).unwrap();
            // The policy records each resolved dependency root. Match by canonicalizing the
            // policy's own paths rather than by comparing spellings: a temporary directory is
            // reached through a symlink on macOS (`/var` → `/private/var`), so the unresolved path
            // would compare unequal, and Windows canonicalization adds the verbatim prefix that
            // the graph adapter strips before handing a watch path out.
            let watched_root = |canonical: &std::path::Path| {
                assembled
                    .watch_policy
                    .reassemble_inputs
                    .iter()
                    .find(|input| {
                        std::fs::canonicalize(input).is_ok_and(|input| input == canonical)
                    })
                    .cloned()
            };
            assert!(watched_root(&canonical_child).is_some());
            // Joined onto the path the policy itself holds, so the inputs below are spelled the way
            // a watcher would report them.
            let transitive_root = watched_root(&canonical_transitive)
                .expect("the transitive dependency root is watched");
            for path in [
                transitive_root.join("jals.toml"),
                transitive_root.join("build.rhai"),
                transitive_root.join("schema.rerun"),
                transitive_root.join("src/Transitive.java"),
                transitive_root.join("lib/local.jar"),
            ] {
                assert_eq!(
                    Actor::watched_project_action(
                        &root,
                        Some(&assembled.watch_policy),
                        &[changed(&path)],
                    ),
                    WatchedProjectAction::Reassemble,
                    "local dependency input must reassemble: {path:?}"
                );
            }
        });
    }

    #[test]
    fn build_script_diagnostics_shape_messages_and_clear_previous_state() {
        let root = tempfile::tempdir().unwrap();
        let script = FileKey::parse("build.rhai").unwrap();

        // A run that called `build.error` carries every diagnostic it emitted, each under its own
        // severity. The assembly decides that; this asserts the protocol shape it maps to.
        let reported = RootBuildScriptError::BuildScript(BuildScriptError::ReportedErrors(vec![
            BuildScriptDiagnostic::warning("generated fallback"),
            BuildScriptDiagnostic::error("generation failed"),
        ]));
        let shaped: Vec<Diagnostic> = ProjectDiagnostics::assemble(
            ScriptOutcome::Failed(&reported),
            GraphOutcome::NotReached,
            Some(ScriptFile {
                key: &script,
                text: None,
            }),
        )
        .iter()
        .map(|diagnostic| AssembledWorkspace::lsp_diagnostic(diagnostic, None))
        .collect();

        let publications =
            Actor::build_script_diagnostic_publications(root.path(), None, Some(&script), shaped);
        assert_eq!(publications.len(), 1);
        assert_eq!(publications[0].diagnostics.len(), 2);
        assert_eq!(
            publications[0].diagnostics[0].severity,
            Some(DiagnosticSeverity::WARNING)
        );
        assert_eq!(
            publications[0].diagnostics[1].severity,
            Some(DiagnosticSeverity::ERROR)
        );
        // Both messages stay bare: the protocol carries the severity in its own field.
        assert_eq!(publications[0].diagnostics[0].message, "generated fallback");
        assert_eq!(publications[0].diagnostics[1].message, "generation failed");
        // No text supplied here, so there is nothing to place against and this protocol's own
        // fallback stands. `finish_assembly` does supply it, and the placement rule then puts these
        // on the script's first line — which is what
        // `a_positioned_script_failure_points_at_the_line_it_names` covers.
        assert_eq!(
            publications[0].diagnostics[0].range,
            Range::new(Position::new(0, 0), Position::new(0, 1))
        );
        assert_eq!(
            publications[0].diagnostics[0].source.as_deref(),
            Some("jals-build")
        );

        // A clean rerun still publishes — an empty vector is what clears the previous run's
        // diagnostics, which is why the configured script travels beside them rather than being
        // read out of them.
        let publications = Actor::build_script_diagnostic_publications(
            root.path(),
            Some(&script),
            Some(&script),
            Vec::new(),
        );
        assert_eq!(publications.len(), 1);
        assert!(publications[0].diagnostics.is_empty());

        // A script removed from the manifest clears the file it used to be at.
        let publications = Actor::build_script_diagnostic_publications(
            root.path(),
            Some(&script),
            None,
            Vec::new(),
        );
        assert_eq!(publications.len(), 1);
        assert!(publications[0].diagnostics.is_empty());
    }

    #[test]
    fn a_positioned_script_failure_points_at_the_line_it_names() {
        // The byte span the assembly resolved, converted to this protocol's coordinates — the only
        // thing this server still does with a script position.
        let script = FileKey::parse("build.rhai").unwrap();
        let source = "let a = 1;\nlet b = 2;\n";
        let diagnostic = ProjectDiagnostic {
            anchor: ProjectAnchor::Script(script),
            span: Some(15..16),
            severity: ProjectDiagnosticSeverity::Error,
            code: ProjectDiagnosticCode::BuildScript,
            message: "syntax error".to_owned(),
        };
        assert_eq!(
            AssembledWorkspace::lsp_diagnostic(&diagnostic, Some(source)).range,
            Range::new(Position::new(1, 4), Position::new(1, 5))
        );
        // Without the text there is nothing to convert against, so it falls back rather than
        // guessing.
        assert_eq!(
            AssembledWorkspace::lsp_diagnostic(&diagnostic, None).range,
            Range::new(Position::new(0, 0), Position::new(0, 1))
        );

        // Span-less but with the text: the placement rule puts it on the anchor's first line, which
        // is a range a client can actually show. This used to collapse to the same one-character
        // stub as the no-text case, so `build.error("boom")` and "we could not read the script"
        // were indistinguishable on screen.
        let span_less = ProjectDiagnostic {
            span: None,
            ..diagnostic
        };
        assert_eq!(
            AssembledWorkspace::lsp_diagnostic(&span_less, Some(source)).range,
            Range::new(Position::new(0, 0), Position::new(0, 10))
        );
        // And the `\r` of a CRLF script stays out of it — highlighting it would draw a character
        // the author cannot see.
        assert_eq!(
            AssembledWorkspace::lsp_diagnostic(&span_less, Some("let a = 1;\r\nlet b = 2;\r\n"))
                .range,
            Range::new(Position::new(0, 0), Position::new(0, 10))
        );
    }

    #[test]
    fn the_offline_advisory_gains_this_host_s_remedy() {
        // The assembly states the condition and owns the sentence that clears it; offering that
        // sentence, and saying why this server is not the one running it, are the server's.
        let diagnostic = ProjectDiagnostic {
            anchor: ProjectAnchor::Manifest,
            span: None,
            severity: ProjectDiagnosticSeverity::Info,
            code: ProjectDiagnosticCode::DependencyCache,
            message: "some dependencies are not in the verified cache".to_owned(),
        };
        let shaped = AssembledWorkspace::lsp_diagnostic(&diagnostic, None);
        assert_eq!(shaped.severity, Some(DiagnosticSeverity::INFORMATION));
        assert!(shaped.message.contains("run `jals build`"));
        assert_eq!(shaped.source.as_deref(), Some("jals-project"));
    }

    #[test]
    fn open_script_keeps_build_diagnostics_authoritative() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("jals.toml"),
                "[build]\nsource-dirs = [\".\"]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n",
            )
            .unwrap();
            let script = "build.warning(\"from Rhai\");\n";
            let script_path = dir.path().join("build.rhai");
            let script_uri = Url::from_file_path(&script_path).unwrap();
            let (mut actor, mut receiver, _sender) = actor();

            open(&mut actor, &mut receiver, script_path, script).await;

            assert!(actor.is_script_diagnostic_uri(&script_uri));
            assert_eq!(
                actor.workspaces[0]
                    .watch_policy()
                    .and_then(ProjectWatchPolicy::script),
                Some(&FileKey::parse("build.rhai").unwrap())
            );
            actor.refresh_and_publish(&script_uri).await;
            assert!(
                actor.is_script_diagnostic_uri(&script_uri),
                "ordinary Java publication remains suppressed while the script is open"
            );
        });
    }

    #[test]
    fn failed_reassembly_preserves_last_good_workspace_and_watch_state() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            std::fs::write(
                dir.path().join("jals.toml"),
                "[build]\nsource-dirs = [\"src\"]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n",
            )
            .unwrap();
            std::fs::write(dir.path().join("build.rhai"), "build.warning(\"old\");\n").unwrap();
            let (mut actor, mut receiver, _sender) = actor();
            open(
                &mut actor,
                &mut receiver,
                dir.path().join("src/Main.java"),
                "class Main {}",
            )
            .await;
            assert!(actor.workspaces[0].watch_policy().is_some());

            let generation = actor.next_workspace_assembly_generation();
            actor.workspaces[0].replace_assembly(WorkspaceAssembly {
                generation,
                rerun_requested: false,
            });
            actor
                .workspace_ready(
                    dir.path().to_path_buf(),
                    generation,
                    Err(Box::new(WorkspaceAssemblyFailure {
                        message: "failed".into(),
                        fallback: None,
                        project_diagnostics: vec![AssembledWorkspace::host_diagnostic(
                            ProjectDiagnosticCode::DependencyAcquisition,
                            "failed".into(),
                        )],
                    })),
                )
                .await;

            assert!(
                actor.workspaces[0].watch_policy().is_some(),
                "a failed replacement retains the last-good script/input watches"
            );
            assert!(
                actor.workspaces[0].ready().is_some(),
                "a failed replacement retains the last-good index"
            );
            assert!(actor.workspaces[0].assembly().is_none());
        });
    }

    #[test]
    fn compile_and_runtime_diagnostics_use_exact_rhai_positions() {
        block_on_inline(async {
            let manifest: Manifest = r#"
                [build]
                script = { type = "rhai", file = "build.rhai" }
            "#
            .parse()
            .unwrap();
            for (script, expected) in [
                (
                    "let valid = 1;\nlet broken = ;\n",
                    Range::new(Position::new(1, 13), Position::new(1, 14)),
                ),
                (
                    "let valid = 1;\nthrow \"boom\";\n",
                    Range::new(Position::new(1, 0), Position::new(1, 1)),
                ),
                (
                    "let emoji = \"😀\"; throw \"boom\";\n",
                    Range::new(Position::new(0, 18), Position::new(0, 19)),
                ),
                // `build.error` is reported *by* the script rather than thrown by Rhai, so it
                // carries no position at all. End to end, that lands on the script's first line —
                // the placement rule running against the text `finish_assembly` supplies. It used
                // to land on a one-character stub at the head of the file, indistinguishable from a
                // script this server could not read.
                (
                    "build.error(\"boom\");\n",
                    Range::new(Position::new(0, 0), Position::new(0, 20)),
                ),
                // The same script with the line endings a Windows checkout has. The `\r` stays out
                // of the range: the script is read back verbatim from the project snapshot, so
                // without the rule the highlight would run one character past what the author can
                // see. Written through `std::fs::write` rather than a literal in the loop above so
                // no platform's newline translation can quietly undo the case.
                (
                    "build.error(\"boom\");\r\n",
                    Range::new(Position::new(0, 0), Position::new(0, 20)),
                ),
            ] {
                let dir = tempfile::tempdir().unwrap();
                std::fs::write(
                    dir.path().join("jals.toml"),
                    "[build]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n",
                )
                .unwrap();
                std::fs::write(dir.path().join("build.rhai"), script).unwrap();

                let assembled = AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline())
                    .await
                    .unwrap();
                assert_eq!(
                    assembled.configured_script,
                    Some(FileKey::parse("build.rhai").unwrap())
                );
                assert_eq!(assembled.script_diagnostics.len(), 1);
                assert_eq!(assembled.script_diagnostics[0].range, expected);
            }
        });
    }

    #[test]
    fn build_script_generated_java_is_indexed_on_initial_assembly() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            std::fs::write(
                dir.path().join("jals.toml"),
                "[package]\nname = \"generated\"\n[build]\nsource-dirs = [\"src\"]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n",
            )
            .unwrap();
            std::fs::write(
                dir.path().join("build.rhai"),
                r#"
                    let source = output.write_text(
                        "p/Generated.java",
                        "package p; public class Generated {}\n",
                    );
                    output.write_text(
                        "p/Sibling.java",
                        "package p; public class Sibling {}\n",
                    );
                    build.add_source(source);
                "#,
            )
            .unwrap();
            let main = "package p; class Main { Generated value; Sibling excluded; }";
            let main_path = dir.path().join("src/Main.java");
            let main_uri = Url::from_file_path(&main_path).unwrap();
            let generated_uri = Url::from_file_path(
                dir.path()
                    .join("target/jals/build/rhai/out/p/Generated.java"),
            )
            .unwrap();

            let (mut actor, mut receiver, _sender) = actor();
            open(&mut actor, &mut receiver, main_path, main).await;

            let location = actor
                .workspace_for(&main_uri)
                .expect("the project workspace loaded")
                .definition(Position::new(0, main.find("Generated").unwrap() as u32))
                .await
                .expect("the generated type resolves");
            assert_eq!(location.uri, generated_uri);
            assert!(
                actor
                    .workspace_for(&main_uri)
                    .unwrap()
                    .definition(Position::new(0, main.find("Sibling").unwrap() as u32),)
                    .await
                    .is_none(),
                "an unselected generated sibling is not a project source"
            );
        });
    }

    #[test]
    fn open_document_defers_exclusive_task_publication_before_fetch() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            let manifest_text = "[build]\nsource-dirs = [\"src\"]\nscript = { type = \"rhai\", file = \"build.rhai\" }\n";
            std::fs::write(dir.path().join("jals.toml"), manifest_text).unwrap();
            std::fs::write(dir.path().join("src/Main.java"), "class Main {}\n").unwrap();
            std::fs::write(
                dir.path().join("build.rhai"),
                r#"
                    let jar = tasks.fetch_jar(
                        tasks.https_url("https://example.invalid/sources.jar"),
                        tasks.sha256("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                        tasks.bytes(1024)
                    );
                    let sources = tasks.extract_java(jar, "generated");
                    tasks.publish_tree("sources", sources, "src/generated", "replace-root", "navigation");
                "#,
            )
            .unwrap();
            let manifest: Manifest = manifest_text.parse().unwrap();

            let assembled = AssembledWorkspace::assemble_with_blocked(
                &manifest,
                dir.path(),
                Exec::inline(),
                &[FileKey::parse("src/generated/A.java").unwrap()],
                &FeatureSelection::default(),
            )
            .await
            .unwrap();

            assert!(!dir.path().join("src/generated").exists());
            assert!(
                assembled
                    .script_diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains("publication is deferred"))
            );
        });
    }

    #[test]
    fn dependency_generated_java_is_indexed_as_a_stable_artifact_source() {
        block_on_inline(async {
            let dir = scripted_dependency_project();
            let main_path = dir.path().join("src/Main.java");
            let main = std::fs::read_to_string(&main_path).unwrap();
            let main_uri = Url::from_file_path(&main_path).unwrap();
            let (mut actor, mut receiver, _sender) = actor();

            open(&mut actor, &mut receiver, main_path, &main).await;

            let location = actor
                .workspace_for(&main_uri)
                .expect("the graph-backed workspace loaded")
                .definition(Position::new(0, main.find("Generated").unwrap() as u32))
                .await
                .expect("the generated dependency type resolves");
            let path = location.uri.to_file_path().unwrap();
            assert!(path.ends_with("p/Generated.java"));
            assert!(
                path.components()
                    .any(|component| component.as_os_str() == "dependencies"),
                "the materialized URI retains the stable node-token path: {path:?}"
            );
            assert!(
                !dir.path().join("dependency/target").exists(),
                "dependency preprocessing does not publish process-style output into its source"
            );
        });
    }

    #[test]
    fn dependency_source_identity_is_stable_across_reassembly() {
        block_on_inline(async {
            let dir = scripted_dependency_project();
            let manifest = Manifest::from_file(&dir.path().join("jals.toml"))
                .await
                .unwrap();

            let first = AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline())
                .await
                .unwrap();
            let first = first
                .source_dep_sources
                .iter()
                .find(|key| key.to_string().ends_with("p/Generated.java"))
                .cloned()
                .expect("the generated source is staged");
            let second = AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline())
                .await
                .unwrap();
            let second = second
                .source_dep_sources
                .iter()
                .find(|key| key.to_string().ends_with("p/Generated.java"))
                .cloned()
                .expect("the generated source is staged again");

            assert_eq!(first, second);
            assert!(
                first
                    .to_string()
                    .starts_with(".jals/source-dependency/dependencies/")
            );
        });
    }

    #[test]
    fn dependency_failures_are_structured_and_keep_an_initial_root_workspace() {
        block_on_inline(async {
            for (child_manifest, child_script, expected_code) in [
                ("[build]\nsource-dirs = [\n", None, "dependency-manifest"),
                (
                    "[build]\nsource-dirs = [\"src\"]\n\
                     script = { type = \"rhai\", file = \"build.rhai\" }\n",
                    Some("let = ;"),
                    "dependency-build-script",
                ),
            ] {
                let dir = tempfile::tempdir().unwrap();
                write(
                    dir.path(),
                    "jals.toml",
                    "[build]\nsource-dirs = [\"src\"]\n\
                     script = { type = \"rhai\", file = \"root.rhai\" }\n\
                     [dependencies]\nchild = { path = \"child\" }\n",
                );
                write(dir.path(), "root.rhai", "build.error(\"root diagnostic\");");
                write(dir.path(), "src/Main.java", "class Main { Missing value; }");
                write(dir.path(), "child/jals.toml", child_manifest);
                if let Some(script) = child_script {
                    write(dir.path(), "child/build.rhai", script);
                }
                let manifest = Manifest::from_file(&dir.path().join("jals.toml"))
                    .await
                    .unwrap();

                let Err(failure) =
                    AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline()).await
                else {
                    panic!("dependency failure unexpectedly assembled");
                };
                // The failure is reported after the warnings the earlier phases produced: the
                // dependency discovery warned about is usually the one preprocessing then failed
                // on, so the warnings are context for it and read first.
                let reported = failure
                    .project_diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
                    .expect("a graph failure is reported as an error");
                assert_eq!(diagnostic_code(reported), Some(expected_code));
                assert!(
                    failure
                        .project_diagnostics
                        .iter()
                        .take_while(|diagnostic| {
                            diagnostic.severity != Some(DiagnosticSeverity::ERROR)
                        })
                        .all(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::WARNING)),
                    "only warnings precede the failure they explain"
                );
                let fallback = failure
                    .fallback
                    .as_ref()
                    .expect("a valid root remains analyzable on initial load");
                assert_eq!(
                    fallback
                        .storage
                        .view()
                        .file_text(&FileKey::parse("src/Main.java").unwrap()),
                    Ok("class Main { Missing value; }")
                );

                let graph = Actor::project_diagnostic_publication(
                    dir.path(),
                    failure.project_diagnostics.clone(),
                )
                .unwrap();
                let script = Actor::build_script_diagnostic_publications(
                    dir.path(),
                    None,
                    fallback.configured_script.as_ref(),
                    fallback.script_diagnostics.clone(),
                );
                assert_eq!(
                    graph.uri,
                    Url::from_file_path(dir.path().join("jals.toml")).unwrap()
                );
                assert_eq!(script.len(), 1);
                assert_ne!(graph.uri, script[0].uri);
                assert_ne!(
                    graph.uri,
                    Url::from_file_path(dir.path().join("src/Main.java")).unwrap(),
                    "dependency diagnostics cannot replace ordinary Java diagnostics"
                );
                // The graph phase runs twice on this path — once for real, once root-only — and
                // `workspace_ready` publishes both sets. Only the fallback reports the script, so
                // `root diagnostic` is published once rather than once per traversal.
                assert_eq!(
                    script[0]
                        .diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.message.contains("root diagnostic"))
                        .count(),
                    1,
                    "the script phase is reported once across both graph traversals"
                );
                assert!(
                    failure
                        .project_diagnostics
                        .iter()
                        .all(|diagnostic| !diagnostic.message.contains("root diagnostic")),
                    "the failed traversal reports the graph, never the script"
                );
            }
        });
    }

    #[test]
    fn dependency_cycle_is_diagnosed_without_discarding_root_analysis() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\n\
                 [dependencies]\na = { path = \"a\" }\n",
            );
            write(
                dir.path(),
                "a/jals.toml",
                "[dependencies]\nb = { path = \"../b\" }\n",
            );
            write(
                dir.path(),
                "b/jals.toml",
                "[dependencies]\na-again = { path = \"../a\" }\n",
            );
            write(dir.path(), "src/Main.java", "class Main {}");
            let manifest = Manifest::from_file(&dir.path().join("jals.toml"))
                .await
                .unwrap();

            let Err(failure) =
                AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline()).await
            else {
                panic!("cycle unexpectedly assembled");
            };
            assert_eq!(
                diagnostic_code(&failure.project_diagnostics[0]),
                Some("dependency-cycle")
            );
            assert!(
                failure.project_diagnostics[0]
                    .message
                    .contains("dependency cycle")
            );
            assert!(failure.fallback.is_some());
        });
    }

    #[test]
    fn graph_warnings_join_project_resolution_diagnostics() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\n\
                 [dependencies]\nmissing = { path = \"missing\" }\n",
            );
            write(dir.path(), "src/Main.java", "class Main {}");
            let manifest = Manifest::from_file(&dir.path().join("jals.toml"))
                .await
                .unwrap();

            let assembled = AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline())
                .await
                .unwrap();
            assert!(assembled.project_diagnostics.iter().any(|diagnostic| {
                diagnostic_code(diagnostic) == Some("dependency-resolution")
                    && diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                    && diagnostic.message.contains("missing")
            }));
        });
    }

    /// A classpath entry that cannot be used used to reach only the server's stderr: `finish_assembly`
    /// printed `inputs.warnings` and turned none of them into a diagnostic, so nothing said so in
    /// the editor. They are part of what the assembly reports now, like every other channel.
    #[test]
    fn classpath_input_warnings_reach_the_client() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "jals.toml",
                "[build]\nsource-dirs = [\"src\"]\nclasspath = [\"../escape.class\"]\n",
            );
            write(dir.path(), "src/Main.java", "class Main {}");
            let manifest = Manifest::from_file(&dir.path().join("jals.toml"))
                .await
                .unwrap();

            let assembled = AssembledWorkspace::assemble(&manifest, dir.path(), Exec::inline())
                .await
                .unwrap();
            assert!(
                assembled.project_diagnostics.iter().any(|diagnostic| {
                    diagnostic_code(diagnostic) == Some("classpath-input")
                        && diagnostic.severity == Some(DiagnosticSeverity::WARNING)
                }),
                "{:?}",
                assembled.project_diagnostics
            );
        });
    }

    #[test]
    fn build_script_failure_keeps_ordinary_project_analysis() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            std::fs::write(
                dir.path().join("jals.toml"),
                "[build]\nsource-dirs = [\"src\"]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n",
            )
            .unwrap();
            std::fs::write(dir.path().join("build.rhai"), "let = ;").unwrap();
            std::fs::write(dir.path().join("src/Foo.java"), "package p; class Foo {}").unwrap();
            let main = "package p; class Main { Foo value; }";
            let main_path = dir.path().join("src/Main.java");
            let main_uri = Url::from_file_path(&main_path).unwrap();
            let foo_uri = Url::from_file_path(dir.path().join("src/Foo.java")).unwrap();

            let (mut actor, mut receiver, _sender) = actor();
            open(&mut actor, &mut receiver, main_path, main).await;

            let location = actor
                .workspace_for(&main_uri)
                .expect("script failure did not discard the workspace")
                .definition(Position::new(0, main.find("Foo").unwrap() as u32))
                .await
                .expect("ordinary project sources still resolve");
            assert_eq!(location.uri, foo_uri);
        });
    }

    #[test]
    fn watched_build_input_change_reruns_script_and_reassembles_workspace() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("src")).unwrap();
            std::fs::write(
                dir.path().join("jals.toml"),
                "[build]\nsource-dirs = [\"src\"]\n\
                 script = { type = \"rhai\", file = \"build.rhai\" }\n",
            )
            .unwrap();
            std::fs::write(
                dir.path().join("build.rhai"),
                r#"
                    let source = output.write_text(
                        "p/Model.java",
                        project.read_text("model.java.in"),
                    );
                    build.add_source(source);
                    build.rerun_if_changed("model.java.in");
                "#,
            )
            .unwrap();
            let model_path = dir.path().join("model.java.in");
            std::fs::write(&model_path, "package p; class First {}\n").unwrap();
            let main = "package p; class Main { Second value; }";
            let main_path = dir.path().join("src/Main.java");
            let main_uri = Url::from_file_path(&main_path).unwrap();

            let (mut actor, mut receiver, _sender) = actor();
            open(&mut actor, &mut receiver, main_path.clone(), main).await;
            let policy = actor.workspaces[0]
                .watch_policy()
                .and_then(|policy| policy.build_script.as_ref())
                .expect("the successful output installs a script watch policy");
            assert_eq!(
                policy.rerun_files,
                BTreeSet::from([FileKey::parse("model.java.in").unwrap()])
            );
            assert!(
                actor
                    .workspace_for(&main_uri)
                    .unwrap()
                    .definition(Position::new(0, main.find("Second").unwrap() as u32),)
                    .await
                    .is_none(),
                "the initial script output declares only First"
            );

            actor
                .process(Cmd::DidChangeWatchedFiles(DidChangeWatchedFilesParams {
                    changes: vec![FileEvent {
                        uri: Url::from_file_path(&main_path).unwrap(),
                        typ: FileChangeType::CHANGED,
                    }],
                }))
                .await;
            assert!(
                receiver.try_recv().is_err(),
                "an unrelated source change refreshes instead of reassembling"
            );

            actor.request_workspace_reassembly(dir.path());
            actor
                .process(Cmd::DidChangeWatchedFiles(DidChangeWatchedFilesParams {
                    changes: vec![FileEvent {
                        uri: Url::from_file_path(&main_path).unwrap(),
                        typ: FileChangeType::CHANGED,
                    }],
                }))
                .await;
            assert!(
                actor.workspaces[0]
                    .assembly()
                    .is_some_and(|assembly| assembly.rerun_requested),
                "an in-flight replacement reruns for inputs unknown to its old policy"
            );
            drain(&mut actor, &mut receiver).await;

            std::fs::write(&model_path, "package p; class Second {}\n").unwrap();
            actor
                .process(Cmd::DidChangeWatchedFiles(DidChangeWatchedFilesParams {
                    changes: vec![FileEvent {
                        uri: Url::from_file_path(&model_path).unwrap(),
                        typ: FileChangeType::CHANGED,
                    }],
                }))
                .await;
            drain(&mut actor, &mut receiver).await;

            let location = actor
                .workspace_for(&main_uri)
                .expect("the replacement workspace loaded")
                .definition(Position::new(0, main.find("Second").unwrap() as u32))
                .await
                .expect("the changed input reran the script");
            assert_eq!(
                location.uri,
                Url::from_file_path(dir.path().join("target/jals/build/rhai/out/p/Model.java"))
                    .unwrap()
            );
        });
    }

    #[test]
    fn semantic_tokens_delta_reflects_edits_and_falls_back_when_stale() {
        block_on_inline(async {
            let (mut actor, mut receiver, _sender) = actor();
            // A directory under no manifest: the document is answered by a detached group, which
            // is what every open document goes through. It is opened rather than pushed straight
            // into the store precisely so it is routed — an unrouted document has no analysis and
            // so no tokens to take a delta of.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("A.java");
            open(&mut actor, &mut receiver, path.clone(), "class A {}").await;
            let uri = Url::from_file_path(&path).unwrap();

            // A full request tags the response with a result id and caches it as the delta
            // baseline.
            let Some(SemanticTokensResult::Tokens(first)) =
                actor.semantic_tokens_full_response(&uri).await
            else {
                panic!("full request returns tokens");
            };
            let baseline = first.result_id.expect("full response carries a result id");

            // Edit the document, then ask for a delta against the baseline the client still
            // holds.
            open(&mut actor, &mut receiver, path, "class A { int x; }").await;
            match actor.semantic_tokens_delta_response(&uri, &baseline).await {
                Some(SemanticTokensFullDeltaResult::TokensDelta(delta)) => {
                    assert!(
                        !delta.edits.is_empty(),
                        "the added field changes the token stream"
                    );
                    assert_ne!(
                        delta.result_id.as_deref(),
                        Some(baseline.as_str()),
                        "each response mints a fresh result id"
                    );
                }
                other => panic!("expected a token delta, got {other:?}"),
            }

            // A `previous_result_id` the server no longer holds falls back to a full token set.
            assert!(matches!(
                actor
                    .semantic_tokens_delta_response(&uri, "does-not-exist")
                    .await,
                Some(SemanticTokensFullDeltaResult::Tokens(_))
            ));

            // Closing the document drops the cached baseline.
            actor.semantic_tokens_cache.remove(&uri);
            assert!(!actor.semantic_tokens_cache.contains_key(&uri));
        });
    }

    /// A contiguous burst of queued `didChange`s for one document coalesces, but an intervening
    /// request is answered before any later change is applied.
    #[test]
    fn didchange_bursts_stop_at_interleaved_requests() {
        fn change(uri: &Url, version: i32, text: &str) -> Cmd {
            Cmd::DidChange(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.to_owned(),
                }],
            })
        }

        block_on_inline(async {
            let (mut actor, mut receiver, sender) = actor();
            let uri = Url::parse("file:///no-manifest/A.java").unwrap();
            actor
                .store
                .upsert(uri.clone(), "class A {}".into(), 1)
                .await;

            // The client typed again, asked for a hover, then kept typing before the actor got to
            // the first change.
            sender.send(change(&uri, 3, "class C {}")).unwrap();
            let (reply, response) = oneshot::channel();
            sender
                .send(Cmd::Hover {
                    uri: uri.clone(),
                    position: Position::new(0, 6),
                    reply,
                })
                .unwrap();
            sender.send(change(&uri, 4, "class D {}")).unwrap();

            let mut pending = VecDeque::new();
            let Cmd::DidChange(first) = change(&uri, 2, "class B {}") else {
                unreachable!()
            };
            actor.did_change(first, &mut receiver, &mut pending).await;

            // Only the contiguous changes before the hover are applied.
            let doc = actor.store.get(&uri).unwrap();
            assert_eq!(&*doc.content.text, "class C {}");
            assert_eq!(doc.version, 3);

            // The hover remains ahead of the later change, preserving the request boundary.
            assert_eq!(
                pending.len(),
                2,
                "the hover and later change remain pending"
            );
            let cmd = pending.pop_front().unwrap();
            assert!(matches!(cmd, Cmd::Hover { .. }));
            actor.process(cmd).await;
            response
                .await
                .expect("the actor replied")
                .expect("hover is not an error");

            let cmd = pending.pop_front().unwrap();
            assert!(matches!(cmd, Cmd::DidChange(_)));
            actor.process(cmd).await;
            let doc = actor.store.get(&uri).unwrap();
            assert_eq!(&*doc.content.text, "class D {}");
            assert_eq!(doc.version, 4);
        });
    }

    /// A panicking request replies `INTERNAL_ERROR` instead of killing the actor loop.
    #[test]
    fn a_panicking_request_replies_internal_error_and_the_actor_continues() {
        block_on_inline(async {
            let (reply, response) = oneshot::channel::<Result<(), ResponseError>>();
            Actor::respond(reply, async { panic!("boom") }).await;
            let error = response
                .await
                .expect("a reply was sent")
                .expect_err("the panic surfaces as an error");
            assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        });
    }

    /// A request whose client already gave up (dropped receiver) is skipped entirely.
    #[test]
    fn a_cancelled_request_is_skipped() {
        block_on_inline(async {
            let (reply, response) = oneshot::channel::<Result<(), ResponseError>>();
            drop(response);
            let mut ran = false;
            Actor::respond(reply, async {
                ran = true;
                Ok(())
            })
            .await;
            assert!(!ran, "the computation never starts for a closed reply");
        });
    }

    #[test]
    fn feature_selection_parses_nested_and_flat_json() {
        let nested = serde_json::json!({
            "jals": { "features": ["fancy", "extra"], "noDefaultFeatures": true }
        });
        assert_eq!(
            FeatureSelection::from_json(&nested),
            FeatureSelection {
                features: vec!["fancy".to_owned(), "extra".to_owned()],
                all_features: false,
                no_default_features: true,
            }
        );
        let flat = serde_json::json!({ "allFeatures": true });
        assert_eq!(
            FeatureSelection::from_json(&flat),
            FeatureSelection {
                features: Vec::new(),
                all_features: true,
                no_default_features: false,
            }
        );
        // Unrelated options keep the default selection.
        assert_eq!(
            FeatureSelection::from_json(&serde_json::json!({"other": 1})),
            FeatureSelection::default()
        );
    }

    /// The `#[cfg]`-aware analysis end to end through the LSP layer: with the `attributes`
    /// dialect on, the manifest's `default` build features select which definition survives, a
    /// changed selection reassembles under the new one, and the disabled region is published as
    /// a faded (`Unnecessary`) hint.
    #[test]
    fn cfg_analysis_follows_the_feature_selection() {
        block_on_inline(async {
            let dir = tempfile::tempdir().unwrap();
            write(
                dir.path(),
                "jals.toml",
                "[package]\nfeatures = [\"attributes\"]\n[features]\nfancy = []\n\
                 [build]\nsource-dirs = [\"src\"]\n",
            );
            write(
                dir.path(),
                "src/Gated.java",
                "#[cfg(feature = \"fancy\")]\npublic class Gated {}\n",
            );
            let main = dir.path().join("src/Main.java");

            let (mut actor, mut receiver, _sender) = actor();
            open(
                &mut actor,
                &mut receiver,
                main.clone(),
                "public class Main { Gated g; }\n",
            )
            .await;
            let main_uri = Url::from_file_path(&main).unwrap();
            let config = jals_config::lint::Config::default();

            // `fancy` is not in the manifest's `default` list, so `Gated` is disabled and the
            // cross-file reference cannot resolve.
            let workspace = actor.workspaces[0].ready().expect("workspace is ready");
            let diags = workspace
                .open(&main_uri)
                .unwrap()
                .diagnostics(&config)
                .await;
            assert!(
                diags
                    .iter()
                    .any(|d| diagnostic_code(d) == Some("cannot-resolve")),
                "{diags:?}"
            );
            let gated_uri = Url::from_file_path(dir.path().join("src/Gated.java")).unwrap();
            let diags = workspace
                .open(&gated_uri)
                .unwrap()
                .diagnostics(&config)
                .await;
            assert!(
                diags.iter().any(|d| diagnostic_code(d) == Some("cfg")
                    && d.tags.as_ref().is_some_and(
                        |tags| tags.contains(&async_lsp::lsp_types::DiagnosticTag::UNNECESSARY)
                    )),
                "{diags:?}"
            );

            // Selecting `fancy` (as initialization options / settings would) reassembles the
            // workspace; the type is live again and nothing is faded.
            actor
                .process(Cmd::SetFeatureSelection(FeatureSelection::from_json(
                    &serde_json::json!({"jals": {"features": ["fancy"]}}),
                )))
                .await;
            drain(&mut actor, &mut receiver).await;
            let workspace = actor.workspaces[0].ready().expect("workspace reassembled");
            let diags = workspace
                .open(&main_uri)
                .unwrap()
                .diagnostics(&config)
                .await;
            assert!(
                !diags
                    .iter()
                    .any(|d| diagnostic_code(d) == Some("cannot-resolve")),
                "{diags:?}"
            );

            // An identical selection is a no-op (no reassembly spawned).
            let generation = actor.workspace_assembly_generation;
            actor
                .process(Cmd::SetFeatureSelection(FeatureSelection::from_json(
                    &serde_json::json!({"jals": {"features": ["fancy"]}}),
                )))
                .await;
            assert_eq!(actor.workspace_assembly_generation, generation);
        });
    }
}
