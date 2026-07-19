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
    CompletionItem, CompletionResponse, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentHighlight, DocumentSymbolResponse, FoldingRange,
    GotoDefinitionResponse, Hover, Location, Position, PrepareRenameResponse,
    PublishDiagnosticsParams, Range, SelectionRange, SemanticToken, SemanticTokens,
    SemanticTokensDelta, SemanticTokensFullDeltaResult, SemanticTokensResult, SignatureHelp,
    TextEdit, Url, WorkspaceEdit, notification,
};
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use futures::FutureExt;
use jals_build::{
    ManifestExt,
    build_script::{
        BuildScriptDiagnostic, BuildScriptEnvironment, BuildScriptError, BuildScriptLimits,
        BuildScriptOutput, BuildScriptPosition, BuildScriptSession, RHAI_OUTPUT_ROOT,
        execute_build_script,
    },
};
use jals_config::{BuildScript, Dependency, FeatureSet, Manifest};
use jals_editor::{
    EditorHost, FoldingHost, Folds, Ident, Outline, SelectionChains, SelectionHost,
    SemanticTokensHost, SignatureHelpUtf16, SingleFileProject,
};
use jals_exec::Exec;
use jals_storage::{DirKey, FileKey, NativeScope, NativeStorage, RelativePath};
use tokio::sync::{mpsc, oneshot};

use crate::formatting::Formatting;
use crate::host::LspHost;
use crate::state::{Document, DocumentStore, ProjectWorkspace, UriConfigs};

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
    // -- Actor-internal --
    /// A spawned workspace assembly finished (see [`Actor::ensure_workspace_for`]): the parts to
    /// build the project's [`ProjectWorkspace`] from, or the error that makes it fall back to a
    /// bare workspace. Re-enters through the same queue so it serializes with everything else.
    WorkspaceReady {
        root: PathBuf,
        generation: u64,
        assembled: Result<Box<AssembledWorkspace>, String>,
    },
}

/// Everything a spawned assembly task produced for one project: the opened aggregate plus the
/// resolved analysis/navigation inputs, ready for [`ProjectWorkspace::load_storage`].
pub(crate) struct AssembledWorkspace {
    storage: NativeStorage,
    source_roots: Vec<DirKey>,
    project_sources: Vec<FileKey>,
    classpath_classes: Vec<jals_classfile::ClassFile>,
    feature_set: FeatureSet,
    library_sources: Vec<FileKey>,
    source_dep_sources: Vec<FileKey>,
    materialized: BTreeMap<FileKey, PathBuf>,
    watch_policy: ProjectWatchPolicy,
    build_script_diagnostics: BuildScriptDiagnosticUpdate,
}

/// Files that invalidate one successfully assembled build script. An empty rerun set means the
/// script did not narrow its inputs, so any non-output project file remains conservative.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildWatchPolicy {
    script: FileKey,
    rerun_files: BTreeSet<FileKey>,
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

/// Diagnostics produced by the configured script during one assembly. `script = None` is also a
/// meaningful update: it clears diagnostics for a script removed from the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildScriptDiagnosticUpdate {
    script: Option<FileKey>,
    diagnostics: Vec<Diagnostic>,
}

impl BuildScriptDiagnosticUpdate {
    const fn new(script: Option<FileKey>) -> Self {
        Self {
            script,
            diagnostics: Vec::new(),
        }
    }

    fn push_reported(&mut self, diagnostic: &BuildScriptDiagnostic) {
        let severity = match diagnostic {
            BuildScriptDiagnostic::Warning(_) => DiagnosticSeverity::WARNING,
            BuildScriptDiagnostic::Error(_) => DiagnosticSeverity::ERROR,
        };
        self.diagnostics.push(Self::diagnostic(
            severity,
            diagnostic.message().to_owned(),
            None,
        ));
    }

    fn push_failure(&mut self, message: String, position: Option<BuildScriptPosition>) {
        self.diagnostics.push(Self::diagnostic(
            DiagnosticSeverity::ERROR,
            message,
            position,
        ));
    }

    fn diagnostic(
        severity: DiagnosticSeverity,
        message: String,
        position: Option<BuildScriptPosition>,
    ) -> Diagnostic {
        let range = position.map_or_else(
            || Range::new(Position::new(0, 0), Position::new(0, 1)),
            |position| {
                let line = position.line().saturating_sub(1);
                let character = position.column().saturating_sub(1);
                Range::new(
                    Position::new(line, character),
                    Position::new(line, character.saturating_add(1)),
                )
            },
        );
        Diagnostic {
            range,
            severity: Some(severity),
            source: Some("jals-build".to_owned()),
            message,
            ..Diagnostic::default()
        }
    }
}

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
    /// real project's source roots, never a whole git checkout. Empty for files that belong to no
    /// manifest, which fall back to one-file resolution.
    workspaces: Vec<WorkspaceSlot>,
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
            semantic_tokens_cache: HashMap::new(),
            semantic_tokens_result_id: 0,
            workspace_assembly_generation: 0,
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
    pub(crate) async fn process(&mut self, cmd: Cmd) {
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
            Cmd::DidClose(params) => Self::guard(async { self.did_close(params) }).await,
            Cmd::DidChangeWatchedFiles(params) => {
                Self::guard(self.watched_files_changed(&params)).await;
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
        if self.is_script_diagnostic_uri(uri) {
            return;
        }
        self.refresh_workspace_overlay(uri).await;
        self.publish_diagnostics(uri).await;
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let script_diagnostics_are_authoritative = self.is_script_diagnostic_uri(&uri);
        self.store.remove(&uri);
        // Drop the cached semantic-tokens baseline; a reopened document starts a fresh result id.
        self.semantic_tokens_cache.remove(&uri);
        // Clear stale diagnostics for the now-closed document.
        if !script_diagnostics_are_authoritative {
            let _ =
                self.client
                    .notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                        uri,
                        diagnostics: Vec::new(),
                        version: None,
                    });
        }
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
            .filter_map(|event| event.uri.to_file_path().ok())
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

    /// Classify a watched-file batch for one loaded project. Every non-output project change is at
    /// least a lightweight refresh; only the manifest and effective script inputs rebuild the
    /// dependency plan and rerun Rhai.
    fn watched_project_action(
        root: &Path,
        policy: Option<&ProjectWatchPolicy>,
        changed: &[PathBuf],
    ) -> WatchedProjectAction {
        let build_root = root.join("target/jals/build");
        let cache_root = root.join("target/jals/cache");
        let manifest = root.join("jals.toml");
        let mut saw_refreshable_source = false;
        for path in changed {
            if path.starts_with(&build_root) || path.starts_with(&cache_root) {
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
        drop(self.exec.spawn(async move {
            let manifest_path = root.join("jals.toml");
            let assembled = match Manifest::from_file(&manifest_path).await {
                Ok(manifest) => AssembledWorkspace::assemble(&manifest, &root, exec).await,
                Err(error) => Err(format!(
                    "reading project manifest {} failed: {error}",
                    manifest_path.display()
                )),
            };
            let _ = commands.send(Cmd::WorkspaceReady {
                root,
                generation,
                assembled: assembled.map(Box::new),
            });
        }));
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
        assembled: Result<Box<AssembledWorkspace>, String>,
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
        let (workspace, watch_policy) = match assembled {
            Ok(parts) => {
                let AssembledWorkspace {
                    storage,
                    source_roots,
                    project_sources,
                    classpath_classes,
                    feature_set,
                    library_sources,
                    source_dep_sources,
                    materialized,
                    watch_policy,
                    build_script_diagnostics,
                } = *parts;
                for params in Self::build_script_diagnostic_publications(
                    &root,
                    previous_script.as_ref(),
                    &build_script_diagnostics,
                ) {
                    let _ = self
                        .client
                        .notify::<notification::PublishDiagnostics>(params);
                }
                (
                    ProjectWorkspace::load_storage(
                        root.clone(),
                        storage,
                        source_roots,
                        project_sources,
                        &classpath_classes,
                        library_sources,
                        source_dep_sources,
                        materialized,
                        feature_set,
                    )
                    .await,
                    Some(watch_policy),
                )
            }
            Err(error) => {
                eprintln!(
                    "jals-lsp: assembling project inputs for {} failed: {error}",
                    root.display()
                );
                let cleared = BuildScriptDiagnosticUpdate::new(None);
                for params in Self::build_script_diagnostic_publications(
                    &root,
                    previous_script.as_ref(),
                    &cleared,
                ) {
                    let _ = self
                        .client
                        .notify::<notification::PublishDiagnostics>(params);
                }
                (ProjectWorkspace::bare(&root, self.exec.clone()).await, None)
            }
        };
        self.workspaces[index] = WorkspaceSlot::Ready {
            workspace: Box::new(workspace),
            assembly: None,
            watch_policy,
        };
        let open: Vec<Url> = self.store.uris().cloned().collect();
        for uri in open {
            if uri.to_file_path().is_ok_and(|path| path.starts_with(&root)) {
                self.refresh_and_publish(&uri).await;
            }
        }
    }

    /// Shape the replace/clear notifications for one installed assembly. The previous script is
    /// cleared when its path changes or the manifest removes it; the current script is always
    /// published, including an empty vector that clears warnings/errors after a clean rerun.
    fn build_script_diagnostic_publications(
        root: &Path,
        previous_script: Option<&FileKey>,
        update: &BuildScriptDiagnosticUpdate,
    ) -> Vec<PublishDiagnosticsParams> {
        fn params(
            root: &Path,
            script: &FileKey,
            diagnostics: Vec<Diagnostic>,
        ) -> Option<PublishDiagnosticsParams> {
            Some(PublishDiagnosticsParams {
                uri: Url::from_file_path(script.path().to_host_path(root)).ok()?,
                diagnostics,
                version: None,
            })
        }

        let mut publications = Vec::new();
        if previous_script != update.script.as_ref()
            && let Some(previous) = previous_script
            && let Some(clear) = params(root, previous, Vec::new())
        {
            publications.push(clear);
        }
        if let Some(script) = &update.script
            && let Some(current) = params(root, script, update.diagnostics.clone())
        {
            publications.push(current);
        }
        publications
    }

    /// The loaded workspace that owns `uri`, if any. A still-loading slot never matches: its
    /// files keep the one-file fallback until `WorkspaceReady`.
    fn workspace_for(&self, uri: &Url) -> Option<&ProjectWorkspace> {
        self.workspaces
            .iter()
            .filter_map(WorkspaceSlot::ready)
            .find(|workspace| workspace.owns_uri(uri))
    }

    /// Reflect the open document at `uri` into the project index of the workspace that owns it,
    /// if any.
    async fn refresh_workspace_overlay(&mut self, uri: &Url) {
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        if let Some(workspace) = self
            .workspaces
            .iter_mut()
            .filter_map(WorkspaceSlot::ready_mut)
            .find(|workspace| workspace.owns_uri(uri))
        {
            workspace.set_overlay(uri, &doc).await;
        }
    }

    /// Compute and push diagnostics for `uri` (a no-op if the document is not open).
    ///
    /// The assembly policy (syntax + lint + cross-file resolution, ordering, suppression) is
    /// [`jals_editor::FileDiagnostics`], driven through the owning workspace (which folds in the
    /// project index and its resolved feature set). A file outside any workspace runs the same
    /// policy over an index-aware one-file project ([`SingleFileProject`]), so in-file subtyping
    /// and stdlib-classified exceptions still check.
    async fn publish_diagnostics(&mut self, uri: &Url) {
        if self.is_script_diagnostic_uri(uri) {
            return;
        }
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        let config = self.lint_discovery.for_uri(uri).await;
        let workspace_diagnostics = match self.workspace_for(uri) {
            Some(workspace) => workspace.diagnostics(uri, &config).await,
            None => None,
        };
        let diagnostics = if let Some(diagnostics) = workspace_diagnostics {
            diagnostics
        } else {
            let project = SingleFileProject::new(&doc.content.parse).await;
            project
                .diagnostics(&doc.content.parse, &config)
                .await
                .into_iter()
                .map(|diagnostic| LspHost.diagnostic(&doc.content, diagnostic))
                .collect()
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
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(highlights) = workspace.document_highlight(uri, position).await
        {
            return Ok(Some(highlights));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Some(Self::fallback_highlights(&doc, position).await))
    }

    /// A file in the project index resolves cross-file (and file-locally) through the workspace.
    /// A `None` answer falls back to one-file resolution against the open document alone.
    async fn definition(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<GotoDefinitionResponse>, ResponseError> {
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(location) = workspace.goto_definition(uri, position).await
        {
            return Ok(Some(GotoDefinitionResponse::Scalar(location)));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Self::fallback_definition(&doc, uri, position)
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
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(locations) = workspace
                .references(uri, position, include_declaration)
                .await
        {
            return Ok(Some(locations));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Some(
            Self::fallback_references(&doc, uri, position, include_declaration).await,
        ))
    }

    /// A file in an indexed project validates project types project-wide through the workspace;
    /// any other document falls back to one-file renamability over the open document alone.
    async fn prepare_rename(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<PrepareRenameResponse>, ResponseError> {
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(range) = workspace.prepare_rename(uri, position).await
        {
            return Ok(Some(PrepareRenameResponse::Range(range)));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Self::fallback_prepare_rename(&doc, position)
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
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(edit) = workspace.rename(uri, position, new_name).await
        {
            return Ok(Some(edit));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Self::fallback_rename(&doc, uri, position, new_name).await)
    }

    /// A file in the project index completes members with cross-file type names through the
    /// workspace; any other document falls back to a one-file index of the open document.
    async fn completion(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<CompletionResponse>, ResponseError> {
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(items) = workspace.completions(uri, position).await
        {
            return Ok(Some(CompletionResponse::Array(items)));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Some(CompletionResponse::Array(
            Self::fallback_completions(&doc, position).await,
        )))
    }

    /// A file in the project index infers with cross-file type names through the workspace; any
    /// other document falls back to one-file inference against the open document alone.
    async fn hover(&self, uri: &Url, position: Position) -> Result<Option<Hover>, ResponseError> {
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(hover) = workspace.hover(uri, position).await
        {
            return Ok(Some(hover));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Self::fallback_hover(&doc, position).await)
    }

    /// A file in the project index resolves overloads with cross-file type names through the
    /// workspace; any other document falls back to a one-file index of the open document.
    async fn signature_help(
        &self,
        uri: &Url,
        position: Position,
    ) -> Result<Option<SignatureHelp>, ResponseError> {
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(help) = workspace.signature_help(uri, position).await
        {
            return Ok(Some(help));
        }
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        Ok(Self::fallback_signature_help(&doc, position).await)
    }

    async fn formatting(&mut self, uri: &Url) -> Result<Option<Vec<TextEdit>>, ResponseError> {
        let Some(doc) = self.store.get(uri) else {
            return Ok(None);
        };
        let config = self.discovery.for_uri(uri).await;
        Ok(Some(
            Formatting::formatting_edits(&doc.content, &config).await,
        ))
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

    // ---- One-file fallbacks ---------------------------------------------------------------------
    //
    // Requests on a document outside any indexed workspace drive the same [`SingleFileProject`]
    // query surface the workspace path uses, mapped through [`LspHost`]. A stdlib-aware one-file
    // index is built per request; targets outside the open document (a source-less stdlib member
    // keeps a reserved file id) are never mapped onto its text.

    /// Go-to-definition over the open document alone.
    async fn fallback_definition(
        doc: &Document,
        uri: &Url,
        position: Position,
    ) -> Option<Location> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        let target = project
            .queries()
            .definition(LspHost.offset(&doc.content, &position))
            .await?;
        (target.file == SingleFileProject::FILE).then(|| Location {
            uri: uri.clone(),
            range: LspHost.range(&doc.content, target.range),
        })
    }

    /// Find-references over the open document alone, each as a `Location` under `uri`.
    async fn fallback_references(
        doc: &Document,
        uri: &Url,
        position: Position,
        include_declaration: bool,
    ) -> Vec<Location> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        project
            .queries()
            .references(
                LspHost.offset(&doc.content, &position),
                include_declaration,
                [project.file()],
            )
            .into_iter()
            .map(|target| Location {
                uri: uri.clone(),
                range: LspHost.range(&doc.content, target.range),
            })
            .collect()
    }

    /// prepareRename over the open document alone.
    async fn fallback_prepare_rename(doc: &Document, position: Position) -> Option<Range> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        let range = project
            .queries()
            .renamable_range(LspHost.offset(&doc.content, &position))?;
        Some(LspHost.range(&doc.content, range))
    }

    /// Rename over the open document alone: gate on renamability, then rewrite every occurrence.
    async fn fallback_rename(
        doc: &Document,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        Self::fallback_prepare_rename(doc, position).await?;
        LspHost::workspace_edit(
            Self::fallback_references(doc, uri, position, true).await,
            new_name,
        )
    }

    /// Hover over the open document alone.
    async fn fallback_hover(doc: &Document, position: Position) -> Option<Hover> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        let markdown = project
            .queries()
            .hover_markdown(LspHost.offset(&doc.content, &position))
            .await?;
        Some(LspHost.hover(markdown))
    }

    /// Completions over the open document alone.
    async fn fallback_completions(doc: &Document, position: Position) -> Vec<CompletionItem> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        project
            .queries()
            .completions(LspHost.offset(&doc.content, &position))
            .await
            .into_iter()
            .map(|completion| LspHost.completion(completion))
            .collect()
    }

    /// Occurrence highlights over the open document alone.
    async fn fallback_highlights(doc: &Document, position: Position) -> Vec<DocumentHighlight> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        project
            .queries()
            .highlights(LspHost.offset(&doc.content, &position))
            .into_iter()
            .map(|highlight| LspHost.highlight(&doc.content, highlight))
            .collect()
    }

    /// Signature help over the open document alone.
    async fn fallback_signature_help(doc: &Document, position: Position) -> Option<SignatureHelp> {
        let project = SingleFileProject::new(&doc.content.parse).await;
        let help = project
            .queries()
            .signature_help(LspHost.offset(&doc.content, &position))
            .await?;
        Some(LspHost.signature_help(SignatureHelpUtf16::of(&help)))
    }

    // ---- Semantic tokens ------------------------------------------------------------------------

    /// The document's delta-encoded semantic tokens: cross-file-aware through the workspace that
    /// owns `uri` when there is one, otherwise a file-local classification over the open document
    /// alone. `None` if the document is not open.
    async fn compute_semantic_tokens(&self, uri: &Url) -> Option<Vec<SemanticToken>> {
        if let Some(workspace) = self.workspace_for(uri)
            && let Some(tokens) = workspace.semantic_tokens(uri).await
        {
            return Some(tokens.data);
        }
        let doc = self.store.get(uri)?;
        let classified =
            jals_editor::SemanticTokens::classify(&doc.content.parse.syntax(), None).await;
        Some(LspHost.semantic_tokens(&doc.content, classified).data)
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
    /// Assemble one project's full analysis + navigation inputs against a fresh aggregate:
    /// run its optional build script, snapshot the effective manifest's scopes, resolve the
    /// classpath (async HTTP through the native fetch adapter), and stage/materialize navigation
    /// sources. Runs on a spawned task, off the actor's queue; stderr is safe to log on (the LSP
    /// protocol owns stdout, not stderr).
    async fn assemble(manifest: &Manifest, root: &Path, exec: Exec) -> Result<Self, String> {
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
            jals_classpath::NativeProjectPlan::snapshot_scopes(manifest, root)
        };
        let mut storage = NativeStorage::for_project_scoped(root, scopes, exec)
            .await
            .map_err(|error| error.to_string())?;
        let mut effective_manifest = manifest.clone();
        let mut build_script_watch = configured_script.clone().map(|script| BuildWatchPolicy {
            script,
            rerun_files: BTreeSet::new(),
        });
        let mut project_sources = BTreeSet::new();
        let mut build_script_diagnostics =
            BuildScriptDiagnosticUpdate::new(configured_script.clone());
        if has_build_script {
            let mut environment = BuildScriptEnvironment::new();
            environment.insert("OUT_DIR", RHAI_OUTPUT_ROOT);
            environment.insert("JALS_MANIFEST_DIR", ".");
            if let Some(name) = &manifest.package.name {
                environment.insert("JALS_PACKAGE_NAME", name);
            }
            if let Some(version) = &manifest.package.version {
                environment.insert("JALS_PACKAGE_VERSION", version);
            }
            match execute_build_script(
                &mut storage,
                manifest,
                &environment,
                &BuildScriptLimits::default(),
                &mut BuildScriptSession::new(),
            )
            .await
            {
                Ok(Some(output)) => {
                    for diagnostic in &output.diagnostics {
                        Self::record_build_script_diagnostic(
                            root,
                            &mut build_script_diagnostics,
                            diagnostic,
                        );
                    }
                    build_script_watch
                        .as_mut()
                        .expect("a configured script has a watch policy")
                        .rerun_files
                        .clone_from(&output.rerun_files);
                    project_sources.clone_from(&output.generated_sources);
                    Self::augment_classpath(&mut effective_manifest, &output);
                }
                Ok(None) => {}
                Err(BuildScriptError::ReportedErrors(diagnostics)) => {
                    for diagnostic in &diagnostics {
                        Self::record_build_script_diagnostic(
                            root,
                            &mut build_script_diagnostics,
                            diagnostic,
                        );
                    }
                }
                Err(error) => {
                    let script = error.script_path().cloned();
                    let position = error.position();
                    let message = error.to_string();
                    eprintln!(
                        "jals-lsp: build script for {} failed; continuing with ordinary project \
                         analysis: {message}",
                        root.display()
                    );
                    if script.is_some() {
                        build_script_diagnostics.script = script;
                    }
                    build_script_diagnostics.push_failure(message, position);
                }
            }
        }
        let (inputs, source_roots) = jals_classpath::NativeProjectPlan::assemble_native(
            &effective_manifest,
            root,
            &mut storage,
            jals_classpath::ProjectInputOptions::Editor,
        )
        .await;
        for warning in &inputs.warnings {
            eprintln!("jals-lsp: {}", warning.message);
        }

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
            &effective_manifest,
            root,
            &source_roots,
            &project_sources,
            build_script_watch,
        );
        Ok(Self {
            storage,
            source_roots,
            project_sources: project_sources.into_iter().collect(),
            classpath_classes: inputs.classpath_classes,
            feature_set: inputs.feature_set,
            library_sources,
            source_dep_sources,
            materialized,
            watch_policy,
            build_script_diagnostics,
        })
    }

    fn record_build_script_diagnostic(
        root: &Path,
        update: &mut BuildScriptDiagnosticUpdate,
        diagnostic: &BuildScriptDiagnostic,
    ) {
        let severity = match diagnostic {
            BuildScriptDiagnostic::Warning(_) => "warning",
            BuildScriptDiagnostic::Error(_) => "error",
        };
        eprintln!(
            "jals-lsp: build script {severity} for {}: {}",
            root.display(),
            diagnostic.message()
        );
        update.push_reported(diagnostic);
    }

    /// Feed successful classpath directives to the existing native project plan without changing
    /// the parsed manifest retained by any other host. Generated sources stay exact identities and
    /// are passed separately through [`ProjectLayout`].
    fn augment_classpath(manifest: &mut Manifest, output: &BuildScriptOutput) {
        for classpath in &output.additional_classpath {
            let classpath = classpath.to_string();
            if !manifest.build.classpath.contains(&classpath) {
                manifest.build.classpath.push(classpath);
            }
        }
    }

    fn watch_policy(
        manifest: &Manifest,
        root: &Path,
        source_roots: &[DirKey],
        project_sources: &BTreeSet<FileKey>,
        build_script: Option<BuildWatchPolicy>,
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
            .materialize_source(&source.key, &source.path)
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
        FileChangeType, FileEvent, TextDocumentContentChangeEvent, TextDocumentItem,
        VersionedTextDocumentIdentifier,
    };
    use jals_exec::block_on_inline;

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
                "a file under no manifest builds no workspace"
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
            Actor::watched_project_action(
                root,
                Some(&ordinary),
                std::slice::from_ref(&source_path),
            ),
            WatchedProjectAction::Refresh,
            "authored Java under a source root refreshes in place"
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&ordinary), std::slice::from_ref(&manifest)),
            WatchedProjectAction::Reassemble
        );
        for changed in [classpath, source_dependency, external_dependency] {
            assert_eq!(
                Actor::watched_project_action(
                    root,
                    Some(&ordinary),
                    std::slice::from_ref(&changed),
                ),
                WatchedProjectAction::Reassemble,
                "classpath and source dependencies require reassembly: {changed:?}"
            );
        }
        assert_eq!(
            Actor::watched_project_action(
                root,
                Some(&ordinary),
                &[output_path.clone(), cache_path],
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
            Actor::watched_project_action(
                root,
                Some(&conservative),
                std::slice::from_ref(&source_path),
            ),
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
            Actor::watched_project_action(
                root,
                Some(&declared),
                std::slice::from_ref(&script_path),
            ),
            WatchedProjectAction::Reassemble
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&declared), std::slice::from_ref(&input_path),),
            WatchedProjectAction::Reassemble
        );
        assert_eq!(
            Actor::watched_project_action(
                root,
                Some(&declared),
                std::slice::from_ref(&source_path),
            ),
            WatchedProjectAction::Refresh,
            "unrelated files do not rerun a script with declared inputs"
        );
        assert_eq!(
            Actor::watched_project_action(root, Some(&declared), &[output_path]),
            WatchedProjectAction::Ignore,
            "generated outputs do nothing"
        );
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
        );

        for changed in [
            PathBuf::from("/workspace/shared/Shared.java"),
            root.join("deps/local/Local.java"),
            root.join("lib/api.jar"),
        ] {
            assert_eq!(
                Actor::watched_project_action(root, Some(&policy), std::slice::from_ref(&changed),),
                WatchedProjectAction::Reassemble,
                "manifest-derived input must reassemble: {changed:?}"
            );
        }
    }

    #[test]
    fn build_script_diagnostics_shape_messages_and_clear_previous_state() {
        let root = tempfile::tempdir().unwrap();
        let script = FileKey::parse("build.rhai").unwrap();
        let mut reported = BuildScriptDiagnosticUpdate::new(Some(script.clone()));
        reported.push_reported(&BuildScriptDiagnostic::Warning("generated fallback".into()));
        reported.push_reported(&BuildScriptDiagnostic::Error("generation failed".into()));

        let publications =
            Actor::build_script_diagnostic_publications(root.path(), None, &reported);
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
        assert_eq!(publications[0].diagnostics[1].message, "generation failed");
        assert_eq!(
            publications[0].diagnostics[0].range,
            Range::new(Position::new(0, 0), Position::new(0, 1))
        );

        let clean = BuildScriptDiagnosticUpdate::new(Some(script.clone()));
        let publications =
            Actor::build_script_diagnostic_publications(root.path(), Some(&script), &clean);
        assert_eq!(publications.len(), 1);
        assert!(publications[0].diagnostics.is_empty());

        let removed = BuildScriptDiagnosticUpdate::new(None);
        let publications =
            Actor::build_script_diagnostic_publications(root.path(), Some(&script), &removed);
        assert_eq!(publications.len(), 1);
        assert!(publications[0].diagnostics.is_empty());

        let mut failed = BuildScriptDiagnosticUpdate::new(Some(script));
        failed.push_failure("could not compile build script".into(), None);
        assert_eq!(
            failed.diagnostics[0].severity,
            Some(DiagnosticSeverity::ERROR)
        );
        assert_eq!(
            failed.diagnostics[0].message,
            "could not compile build script"
        );
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
    fn failed_reassembly_clears_script_watch_state() {
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
                .workspace_ready(dir.path().to_path_buf(), generation, Err("failed".into()))
                .await;

            assert!(
                actor.workspaces[0].watch_policy().is_none(),
                "a failed replacement cannot retain stale script/input watches"
            );
            let previous = FileKey::parse("build.rhai").unwrap();
            let clear = Actor::build_script_diagnostic_publications(
                dir.path(),
                Some(&previous),
                &BuildScriptDiagnosticUpdate::new(None),
            );
            assert_eq!(clear.len(), 1);
            assert!(clear[0].diagnostics.is_empty());
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
                    assembled.build_script_diagnostics.script,
                    Some(FileKey::parse("build.rhai").unwrap())
                );
                assert_eq!(assembled.build_script_diagnostics.diagnostics.len(), 1);
                assert_eq!(
                    assembled.build_script_diagnostics.diagnostics[0].range,
                    expected
                );
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
                .goto_definition(
                    &main_uri,
                    Position::new(0, main.find("Generated").unwrap() as u32),
                )
                .await
                .expect("the generated type resolves");
            assert_eq!(location.uri, generated_uri);
            assert!(
                actor
                    .workspace_for(&main_uri)
                    .unwrap()
                    .goto_definition(
                        &main_uri,
                        Position::new(0, main.find("Sibling").unwrap() as u32),
                    )
                    .await
                    .is_none(),
                "an unselected generated sibling is not a project source"
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
                .goto_definition(
                    &main_uri,
                    Position::new(0, main.find("Foo").unwrap() as u32),
                )
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
                    .goto_definition(
                        &main_uri,
                        Position::new(0, main.find("Second").unwrap() as u32),
                    )
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
                .goto_definition(
                    &main_uri,
                    Position::new(0, main.find("Second").unwrap() as u32),
                )
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
            let (mut actor, _receiver, _sender) = actor();
            // A path under no manifest, so no workspace is built and tokens come from the open
            // document.
            let uri = Url::parse("file:///no-manifest/A.java").unwrap();
            actor
                .store
                .upsert(uri.clone(), "class A {}".into(), 1)
                .await;

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
            actor
                .store
                .upsert(uri.clone(), "class A { int x; }".into(), 2)
                .await;
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
}
