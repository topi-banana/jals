//! The single-owner language service actor.
//!
//! async-lsp's router requires every request handler to return a `Send` future, while all of
//! `jals-editor`'s analysis state is deliberately `!Send` (see `jals-exec`'s execution model).
//! The server therefore splits in two: the [`ServerState`](crate::server) frontend owns nothing
//! but a [`Cmd`] sender and per-request reply channels, and this actor — one local task spawned
//! next to the main loop — owns every document, workspace, and cache, and processes commands
//! strictly in arrival order. FIFO processing is what makes a `didChange` visible to every query
//! enqueued after it; no locks, no shared state.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use async_lsp::lsp_types::{
    CompletionItem, CompletionResponse, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentHighlight,
    DocumentSymbolResponse, FoldingRange, GotoDefinitionResponse, Hover, Location, Position,
    PrepareRenameResponse, PublishDiagnosticsParams, Range, SelectionRange, SemanticToken,
    SemanticTokens, SemanticTokensDelta, SemanticTokensFullDeltaResult, SemanticTokensResult,
    SignatureHelp, TextEdit, Url, WorkspaceEdit, notification,
};
use async_lsp::{ClientSocket, ErrorCode, ResponseError};
use futures::FutureExt;
use jals_build::ManifestExt;
use jals_config::{FeatureSet, Manifest};
use jals_editor::{
    EditorHost, FoldingHost, Folds, Ident, Outline, SelectionChains, SelectionHost,
    SemanticTokensHost, SignatureHelpUtf16, SingleFileProject,
};
use jals_exec::Exec;
use jals_storage::{DirKey, FileKey, NativeStorage};
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
        assembled: Result<Box<AssembledWorkspace>, String>,
    },
}

/// Everything a spawned assembly task produced for one project: the opened aggregate plus the
/// resolved analysis/navigation inputs, ready for [`ProjectWorkspace::load_storage`].
pub(crate) struct AssembledWorkspace {
    storage: NativeStorage,
    source_roots: Vec<DirKey>,
    classpath_classes: Vec<jals_classfile::ClassFile>,
    feature_set: FeatureSet,
    library_sources: Vec<FileKey>,
    source_dep_sources: Vec<FileKey>,
    materialized: BTreeMap<FileKey, PathBuf>,
}

/// One `jals.toml` project's slot: dependency assembly still in flight (queries on its files fall
/// back to the one-file path, exactly like a manifest-less file), or the loaded workspace.
enum WorkspaceSlot {
    Loading { root: PathBuf },
    Ready(Box<ProjectWorkspace>),
}

impl WorkspaceSlot {
    fn project_root(&self) -> &Path {
        match self {
            Self::Loading { root } => root,
            Self::Ready(workspace) => workspace.project_root(),
        }
    }

    fn ready(&self) -> Option<&ProjectWorkspace> {
        match self {
            Self::Loading { .. } => None,
            Self::Ready(workspace) => Some(workspace),
        }
    }

    fn ready_mut(&mut self) -> Option<&mut ProjectWorkspace> {
        match self {
            Self::Loading { .. } => None,
            Self::Ready(workspace) => Some(workspace),
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

    /// Apply one `didChange`, opportunistically coalescing a burst: everything already queued is
    /// drained into `pending`, the queued changes for this same document are applied in order
    /// (each event's splices are relative to the previous state), and the workspace overlay and
    /// diagnostics are refreshed once, for the newest text. The set-aside commands run afterwards
    /// in their original relative order — a query enqueued inside the burst still runs, it just
    /// observes the newest text, which is what a client that kept typing wants anyway.
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
        let mut index = 0;
        while index < pending.len() {
            let same_doc =
                matches!(&pending[index], Cmd::DidChange(next) if next.text_document.uri == uri);
            if !same_doc {
                index += 1;
                continue;
            }
            let Some(Cmd::DidChange(next)) = pending.remove(index) else {
                unreachable!("just matched a didChange at this index");
            };
            Self::guard(self.apply_change(next)).await;
        }
        Self::guard(self.after_change(&uri)).await;
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
                    self.after_change(&uri).await;
                })
                .await;
            }
            Cmd::DidClose(params) => Self::guard(async { self.did_close(params) }).await,
            Cmd::DidChangeWatchedFiles(params) => {
                Self::guard(self.watched_files_changed(params)).await;
            }
            Cmd::WorkspaceReady { root, assembled } => {
                Self::guard(self.workspace_ready(root, assembled)).await;
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
                // `self` is one mutable borrow: build the future in a temporary to end the
                // `discovery` borrow before `respond` awaits it. (No-op today; kept simple.)
                Self::respond(reply, self.formatting(&uri)).await;
            }
            Cmd::SemanticTokensFull { uri, reply } => {
                Self::respond(reply, self.semantic_tokens_full(&uri)).await;
            }
            Cmd::SemanticTokensFullDelta {
                uri,
                previous_result_id,
                reply,
            } => {
                Self::respond(
                    reply,
                    self.semantic_tokens_full_delta(&uri, &previous_result_id),
                )
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
        self.refresh_workspace_overlay(&uri).await;
        self.publish_diagnostics(&uri).await;
    }

    /// Splice one `didChange` into the stored document. The workspace overlay and diagnostics are
    /// refreshed separately ([`after_change`](Self::after_change)), once per coalesced burst.
    async fn apply_change(&mut self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        self.store
            .apply_changes(&uri, &params.content_changes, params.text_document.version)
            .await;
    }

    /// Reflect the (possibly coalesced) new text into the owning workspace's index and republish
    /// diagnostics.
    async fn after_change(&mut self, uri: &Url) {
        self.refresh_workspace_overlay(uri).await;
        self.publish_diagnostics(uri).await;
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.store.remove(&uri);
        // Drop the cached semantic-tokens baseline; a reopened document starts a fresh result id.
        self.semantic_tokens_cache.remove(&uri);
        // Clear stale diagnostics for the now-closed document.
        let _ = self
            .client
            .notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                uri,
                diagnostics: Vec::new(),
                version: None,
            });
    }

    async fn watched_files_changed(&mut self, params: DidChangeWatchedFilesParams) {
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
        // Only the workspaces that can see a changed file re-snapshot; an edit in one project
        // must not reload every other open project. A still-loading slot snapshots after the
        // change anyway.
        let changed: Vec<_> = params
            .changes
            .iter()
            .filter_map(|event| event.uri.to_file_path().ok())
            .collect();
        for workspace in self
            .workspaces
            .iter_mut()
            .filter_map(WorkspaceSlot::ready_mut)
        {
            if changed
                .iter()
                .any(|path| path.starts_with(workspace.project_root()))
            {
                workspace.refresh().await;
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
    /// the one-file path (same as manifest-less files). An unparsable manifest skips assembly and
    /// indexes the project root as a lone source root, no classpath.
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
        let Ok(manifest) = Manifest::from_file(&manifest_path).await else {
            let workspace = ProjectWorkspace::bare(&root, self.exec.clone()).await;
            self.workspaces
                .push(WorkspaceSlot::Ready(Box::new(workspace)));
            return;
        };
        self.workspaces
            .push(WorkspaceSlot::Loading { root: root.clone() });
        // The assembly borrows nothing from the actor: it owns the manifest, the root, an `Exec`
        // handle, and a sender clone, so the actor stays free to serve every other command while
        // dependencies resolve. Dropping the task handle detaches it; it always runs to
        // completion and reports back through the queue.
        let exec = self.exec.clone();
        let commands = self.commands.clone();
        drop(self.exec.spawn(async move {
            let assembled = AssembledWorkspace::assemble(&manifest, &root, exec).await;
            let _ = commands.send(Cmd::WorkspaceReady {
                root,
                assembled: assembled.map(Box::new),
            });
        }));
    }

    /// Finish a spawned assembly: build the workspace (falling back to a bare one when assembly
    /// failed), replace the project's `Loading` slot, then replay the open documents under that
    /// root into the fresh index and republish their diagnostics — they were answered by the
    /// one-file fallback until now.
    async fn workspace_ready(
        &mut self,
        root: PathBuf,
        assembled: Result<Box<AssembledWorkspace>, String>,
    ) {
        let workspace = match assembled {
            Ok(parts) => {
                let parts = *parts;
                ProjectWorkspace::load_storage(
                    root.clone(),
                    parts.storage,
                    parts.source_roots,
                    &parts.classpath_classes,
                    parts.library_sources,
                    parts.source_dep_sources,
                    parts.materialized,
                    parts.feature_set,
                )
                .await
            }
            Err(error) => {
                eprintln!(
                    "jals-lsp: assembling project inputs for {} failed: {error}",
                    root.display()
                );
                ProjectWorkspace::bare(&root, self.exec.clone()).await
            }
        };
        match self
            .workspaces
            .iter_mut()
            .find(|slot| slot.project_root() == root)
        {
            Some(slot) => *slot = WorkspaceSlot::Ready(Box::new(workspace)),
            None => self
                .workspaces
                .push(WorkspaceSlot::Ready(Box::new(workspace))),
        }
        let open: Vec<Url> = self.store.uris().cloned().collect();
        for uri in open {
            let owned_here = self
                .workspace_for(&uri)
                .is_some_and(|workspace| workspace.project_root() == root);
            if owned_here {
                self.refresh_workspace_overlay(&uri).await;
                self.publish_diagnostics(&uri).await;
            }
        }
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
        let location = match self.workspace_for(uri) {
            Some(workspace) => workspace.goto_definition(uri, position).await,
            None => None,
        };
        let location = match location {
            Some(location) => Some(location),
            None => match self.store.get(uri) {
                Some(doc) => Self::fallback_definition(&doc, uri, position).await,
                None => None,
            },
        };
        Ok(location.map(GotoDefinitionResponse::Scalar))
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
        let range = match self.workspace_for(uri) {
            Some(workspace) => workspace.prepare_rename(uri, position).await,
            None => None,
        };
        let range = match range {
            Some(range) => Some(range),
            None => match self.store.get(uri) {
                Some(doc) => Self::fallback_prepare_rename(&doc, position).await,
                None => None,
            },
        };
        Ok(range.map(PrepareRenameResponse::Range))
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

    /// Classifies cross-file type names by their declared kind through the owning workspace when
    /// there is one, else file-locally over the open document alone (see the response builder).
    async fn semantic_tokens_full(
        &mut self,
        uri: &Url,
    ) -> Result<Option<SemanticTokensResult>, ResponseError> {
        Ok(self.semantic_tokens_full_response(uri).await)
    }

    async fn semantic_tokens_full_delta(
        &mut self,
        uri: &Url,
        previous_result_id: &str,
    ) -> Result<Option<SemanticTokensFullDeltaResult>, ResponseError> {
        Ok(self
            .semantic_tokens_delta_response(uri, previous_result_id)
            .await)
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
        let workspace_tokens = match self.workspace_for(uri) {
            Some(workspace) => workspace.semantic_tokens(uri).await,
            None => None,
        };
        if let Some(tokens) = workspace_tokens {
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
    /// snapshot the manifest's scopes, resolve the classpath (async HTTP through the native
    /// fetch adapter), and stage/materialize the navigation sources. Runs on a spawned task, off
    /// the actor's queue; stderr is safe to log on (the LSP protocol owns stdout, not stderr).
    async fn assemble(manifest: &Manifest, root: &Path, exec: Exec) -> Result<Self, String> {
        let scopes = jals_classpath::NativeProjectPlan::snapshot_scopes(manifest, root);
        let mut storage = NativeStorage::for_project_scoped(root, scopes, exec)
            .await
            .map_err(|error| error.to_string())?;
        let (inputs, source_roots) = jals_classpath::NativeProjectPlan::assemble_native(
            manifest,
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
        Ok(Self {
            storage,
            source_roots,
            classpath_classes: inputs.classpath_classes,
            feature_set: inputs.feature_set,
            library_sources,
            source_dep_sources,
            materialized,
        })
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
        TextDocumentContentChangeEvent, TextDocumentItem, VersionedTextDocumentIdentifier,
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

    /// A burst of queued `didChange`s for one document coalesces: every splice applies in order,
    /// diagnostics run once for the newest text, and a request enqueued inside the burst is set
    /// aside (in order) rather than dropped — it answers over the newest text.
    #[test]
    fn didchange_bursts_coalesce_and_keep_interleaved_requests() {
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

            // The client typed twice more (and asked for a hover in between) before the actor
            // got to the first change.
            let (reply, response) = oneshot::channel();
            sender
                .send(Cmd::Hover {
                    uri: uri.clone(),
                    position: Position::new(0, 6),
                    reply,
                })
                .unwrap();
            sender.send(change(&uri, 3, "class C {}")).unwrap();

            let mut pending = VecDeque::new();
            let Cmd::DidChange(first) = change(&uri, 2, "class B {}") else {
                unreachable!()
            };
            actor.did_change(first, &mut receiver, &mut pending).await;

            // Both changes applied, newest text wins.
            let doc = actor.store.get(&uri).unwrap();
            assert_eq!(&*doc.content.text, "class C {}");
            assert_eq!(doc.version, 3);

            // The interleaved hover was set aside, not dropped; processing it answers over the
            // newest text.
            assert_eq!(pending.len(), 1, "only the hover remains pending");
            let cmd = pending.pop_front().unwrap();
            assert!(matches!(cmd, Cmd::Hover { .. }));
            actor.process(cmd).await;
            // The reply arrives (over the newest text); its payload is hover semantics,
            // pinned elsewhere.
            response
                .await
                .expect("the actor replied")
                .expect("hover is not an error");
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
