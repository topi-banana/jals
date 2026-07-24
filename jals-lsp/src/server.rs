//! The LSP server: the stdio event loop, plus the `Send` frontend that bridges async-lsp's
//! router to the single-owner language service actor ([`Actor`](crate::actor)).
//!
//! async-lsp's router requires request handlers to return `Send` futures, while the analysis
//! state is `!Send` by design. [`ServerState`] therefore owns nothing but the client handle and
//! a command sender: request handlers enqueue a [`Cmd`] carrying a oneshot reply channel and
//! return a future that only awaits the reply (channel endpoints are `Send`); notification
//! handlers enqueue and continue. The actor task processes the queue FIFO, which gives
//! didChange-before-query ordering for free.

use std::ops::ControlFlow;

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, CreateFilesParams, DeleteFilesParams,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightParams, DocumentSymbolParams,
    DocumentSymbolResponse, FileSystemWatcher, FoldingRange, FoldingRangeParams,
    FoldingRangeProviderCapability, GlobPattern, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverParams, HoverProviderCapability, InitializeParams, InitializeResult,
    InitializedParams, Location, OneOf, PrepareRenameResponse, ReferenceParams, Registration,
    RegistrationParams, RenameFilesParams, RenameOptions, RenameParams, SelectionRange,
    SelectionRangeParams, SelectionRangeProviderCapability, SemanticTokensDeltaParams,
    SemanticTokensFullDeltaResult, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities,
    ServerCapabilities, SignatureHelp, SignatureHelpOptions, SignatureHelpParams,
    TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    WillSaveTextDocumentParams, WorkDoneProgressCancelParams, WorkDoneProgressOptions,
    WorkspaceEdit,
    notification::{self, Notification},
    request,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, ErrorCode, LanguageServer, MainLoop, ResponseError};
use futures::future::BoxFuture;
use jals_exec::Exec;
use tokio::sync::{mpsc, oneshot};
use tower::ServiceBuilder;

use crate::actor::{Actor, Cmd, FeatureSelection, Reply};
use crate::host::LspHost;

/// The jals language server: builds the async-lsp main loop and runs the stdio event loop.
pub struct Server;

impl Server {
    /// Run the language server over stdio on a fresh current-thread runtime (the workspace-wide
    /// `jals-exec` bootstrap). Blocks until the client disconnects. The public entry point
    /// (`jals lsp`).
    pub fn run() -> anyhow::Result<()> {
        jals_exec::tokio_rt::run(Self::serve)?
    }

    /// Build the server and run its stdio event loop until the client disconnects. Exported so a
    /// host that already owns a `jals-exec` runtime (the CLI) can await it directly.
    // Runs on a current-thread runtime (see [`Server::run`]), so the future is deliberately
    // `!Send` — it holds the non-`Send` stdio locks across `.await`. Those guards are moved into
    // `run_buffered` and live for the whole loop by design, so neither can be dropped earlier.
    pub async fn serve(exec: Exec) -> anyhow::Result<()> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (server, _client) = MainLoop::new_server(|client| {
            // The actor task owns every piece of `!Send` analysis state (documents, workspaces,
            // caches); the router below only ever holds the command sender, so the futures its
            // handlers return stay `Send`. The actor keeps a sender clone of its own, for
            // spawned workspace assemblies to report back through the same queue.
            let actor = Actor::new(client.clone(), exec.clone(), commands.clone());
            drop(exec.spawn(actor.run(receiver)));
            ServiceBuilder::new()
                .layer(TracingLayer::default())
                .layer(LifecycleLayer::default())
                .layer(CatchUnwindLayer::default())
                .layer(ConcurrencyLayer::default())
                .layer(ClientProcessMonitorLayer::new(client.clone()))
                .service(Router::from_language_server(ServerState::new(
                    client, commands,
                )))
        });

        // stdout is the LSP transport, so all logging must go to stderr.
        //
        // On unix, use truly asynchronous piped stdin/stdout (no blocking tasks). async-lsp's
        // `stdio` module is unix-only — `PipeStdin`/`PipeStdout` set the fd non-blocking — so on
        // other platforms (Windows) fall back to tokio's stdin/stdout, which delegate reads/writes
        // to blocking threads, wrapped into futures' `AsyncRead`/`AsyncWrite` via tokio-util.
        #[cfg(unix)]
        let (stdin, stdout) = (
            async_lsp::stdio::PipeStdin::lock_tokio()?,
            async_lsp::stdio::PipeStdout::lock_tokio()?,
        );
        #[cfg(not(unix))]
        let (stdin, stdout) = {
            use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
            (
                tokio::io::stdin().compat(),
                tokio::io::stdout().compat_write(),
            )
        };
        server.run_buffered(stdin, stdout).await?;
        Ok(())
    }
}

/// The `Send` half of the server: the client handle, the actor's command sender, and the one
/// piece of handshake state the frontend itself needs (`initialize` is answered here — the
/// capabilities are static). Everything else is a one-way trip into the actor's queue.
struct ServerState {
    client: ClientSocket,
    commands: mpsc::UnboundedSender<Cmd>,
    /// Whether the client supports dynamic registration of `workspace/didChangeWatchedFiles`,
    /// taken from the `initialize` request. Gates project/config watcher registration.
    watch_registration_supported: bool,
}

impl ServerState {
    const fn new(client: ClientSocket, commands: mpsc::UnboundedSender<Cmd>) -> Self {
        Self {
            client,
            commands,
            watch_registration_supported: false,
        }
    }

    fn service_unavailable() -> ResponseError {
        ResponseError::new(ErrorCode::INTERNAL_ERROR, "language service unavailable")
    }

    /// Enqueue a request command and return the future that awaits its reply. The future holds
    /// only the oneshot receiver (`Send`), never any analysis state. A dead actor (send failed,
    /// or the reply sender dropped without answering) resolves to `INTERNAL_ERROR` instead of
    /// hanging the client.
    fn request<R: Send + 'static>(
        &self,
        build: impl FnOnce(Reply<R>) -> Cmd,
    ) -> BoxFuture<'static, Result<R, ResponseError>> {
        let (reply, response) = oneshot::channel();
        let sent = self.commands.send(build(reply)).is_ok();
        Box::pin(async move {
            if !sent {
                return Err(Self::service_unavailable());
            }
            response.await.map_err(|_| Self::service_unavailable())?
        })
    }

    /// Enqueue a notification command; delivery is fire-and-forget (a dead actor means the
    /// server is shutting down anyway) and the main loop always continues.
    fn forward(&self, cmd: Cmd) -> ControlFlow<async_lsp::Result<()>> {
        let _ = self.commands.send(cmd);
        ControlFlow::Continue(())
    }
}

impl LanguageServer for ServerState {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(
        &mut self,
        params: InitializeParams,
    ) -> BoxFuture<'static, Result<InitializeResult, Self::Error>> {
        self.watch_registration_supported = params
            .capabilities
            .workspace
            .and_then(|workspace| workspace.did_change_watched_files)
            .and_then(|caps| caps.dynamic_registration)
            .unwrap_or(false);
        // The client's build-feature selection (`jals.features` / `jals.allFeatures` /
        // `jals.noDefaultFeatures`), the LSP analogue of `--features` on `jals build`. Sent
        // before any workspace assembles, so the first assembly already resolves under it.
        if let Some(options) = &params.initialization_options {
            let _ = self
                .commands
                .send(Cmd::SetFeatureSelection(FeatureSelection::from_json(
                    options,
                )));
        }
        Box::pin(async move {
            Ok(InitializeResult {
                capabilities: Self::server_capabilities(),
                server_info: None,
            })
        })
    }

    fn initialized(&mut self, _params: InitializedParams) -> Self::NotifyResult {
        if self.watch_registration_supported {
            let client = self.client.clone();
            // Notification handlers are sync and run on the main-loop task; send the
            // client request from a spawned task so the loop stays free to deliver
            // the response. The registration future touches no `!Send` state.
            tokio::spawn(async move {
                let _ = client
                    .request::<request::RegisterCapability>(Self::watch_registration())
                    .await;
            });
        }
        // Project symbol indexes are built lazily, per `jals.toml` project, the first time a file
        // in that project is opened (see the actor's `did_open`) — so a client that opens a large
        // folder with no manifest never triggers a whole-tree walk here.
        ControlFlow::Continue(())
    }

    fn did_change_watched_files(
        &mut self,
        params: DidChangeWatchedFilesParams,
    ) -> Self::NotifyResult {
        self.forward(Cmd::DidChangeWatchedFiles(params))
    }

    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Self::NotifyResult {
        self.forward(Cmd::DidOpen(params))
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Self::NotifyResult {
        self.forward(Cmd::DidChange(params))
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Self::NotifyResult {
        self.forward(Cmd::DidClose(params))
    }

    // No-op notification handlers. `async-lsp`'s `from_language_server` wires *every*
    // standard LSP notification to the omnitrait default, which returns
    // `ControlFlow::Break(Err(..))` for any notification not starting with `$/` — and a
    // `Break` from a notification stops the main loop, exiting the server process. So a
    // client sending one of these (notably Helix sends `textDocument/didSave` on every
    // save, because we advertise `TextDocumentSyncCapability::Kind`) would crash the
    // server. We don't act on these, but we must consume them with `Continue` rather than
    // let them fall through to the loop-breaking default.

    fn did_save(&mut self, _params: DidSaveTextDocumentParams) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn will_save(&mut self, _params: WillSaveTextDocumentParams) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn did_change_configuration(
        &mut self,
        params: DidChangeConfigurationParams,
    ) -> Self::NotifyResult {
        // A changed feature selection reassembles every open workspace (the actor no-ops when
        // the selection is unchanged, so pushing settings unconditionally is safe).
        self.forward(Cmd::SetFeatureSelection(FeatureSelection::from_json(
            &params.settings,
        )))
    }

    fn did_change_workspace_folders(
        &mut self,
        _params: DidChangeWorkspaceFoldersParams,
    ) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn work_done_progress_cancel(
        &mut self,
        _params: WorkDoneProgressCancelParams,
    ) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn did_create_files(&mut self, _params: CreateFilesParams) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn did_rename_files(&mut self, _params: RenameFilesParams) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn did_delete_files(&mut self, _params: DeleteFilesParams) -> Self::NotifyResult {
        ControlFlow::Continue(())
    }

    fn document_symbol(
        &mut self,
        params: DocumentSymbolParams,
    ) -> BoxFuture<'static, Result<Option<DocumentSymbolResponse>, Self::Error>> {
        self.request(|reply| Cmd::DocumentSymbol {
            uri: params.text_document.uri,
            reply,
        })
    }

    fn document_highlight(
        &mut self,
        params: DocumentHighlightParams,
    ) -> BoxFuture<'static, Result<Option<Vec<DocumentHighlight>>, Self::Error>> {
        let pos = params.text_document_position_params;
        self.request(|reply| Cmd::DocumentHighlight {
            uri: pos.text_document.uri,
            position: pos.position,
            reply,
        })
    }

    fn definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> BoxFuture<'static, Result<Option<GotoDefinitionResponse>, Self::Error>> {
        let pos = params.text_document_position_params;
        self.request(|reply| Cmd::Definition {
            uri: pos.text_document.uri,
            position: pos.position,
            reply,
        })
    }

    fn references(
        &mut self,
        params: ReferenceParams,
    ) -> BoxFuture<'static, Result<Option<Vec<Location>>, Self::Error>> {
        let pos = params.text_document_position;
        let include_declaration = params.context.include_declaration;
        self.request(|reply| Cmd::References {
            uri: pos.text_document.uri,
            position: pos.position,
            include_declaration,
            reply,
        })
    }

    fn prepare_rename(
        &mut self,
        params: TextDocumentPositionParams,
    ) -> BoxFuture<'static, Result<Option<PrepareRenameResponse>, Self::Error>> {
        self.request(|reply| Cmd::PrepareRename {
            uri: params.text_document.uri,
            position: params.position,
            reply,
        })
    }

    fn rename(
        &mut self,
        params: RenameParams,
    ) -> BoxFuture<'static, Result<Option<WorkspaceEdit>, Self::Error>> {
        // The new-name validation (a single legal Java identifier, else `INVALID_PARAMS`) lives
        // in the actor: the identifier lexer is part of the `!Send` analysis stack.
        let pos = params.text_document_position;
        self.request(|reply| Cmd::Rename {
            uri: pos.text_document.uri,
            position: pos.position,
            new_name: params.new_name,
            reply,
        })
    }

    fn completion(
        &mut self,
        params: CompletionParams,
    ) -> BoxFuture<'static, Result<Option<CompletionResponse>, Self::Error>> {
        let pos = params.text_document_position;
        self.request(|reply| Cmd::Completion {
            uri: pos.text_document.uri,
            position: pos.position,
            reply,
        })
    }

    fn hover(
        &mut self,
        params: HoverParams,
    ) -> BoxFuture<'static, Result<Option<Hover>, Self::Error>> {
        let pos = params.text_document_position_params;
        self.request(|reply| Cmd::Hover {
            uri: pos.text_document.uri,
            position: pos.position,
            reply,
        })
    }

    fn signature_help(
        &mut self,
        params: SignatureHelpParams,
    ) -> BoxFuture<'static, Result<Option<SignatureHelp>, Self::Error>> {
        let pos = params.text_document_position_params;
        self.request(|reply| Cmd::SignatureHelp {
            uri: pos.text_document.uri,
            position: pos.position,
            reply,
        })
    }

    fn formatting(
        &mut self,
        params: DocumentFormattingParams,
    ) -> BoxFuture<'static, Result<Option<Vec<TextEdit>>, Self::Error>> {
        self.request(|reply| Cmd::Formatting {
            uri: params.text_document.uri,
            reply,
        })
    }

    fn semantic_tokens_full(
        &mut self,
        params: SemanticTokensParams,
    ) -> BoxFuture<'static, Result<Option<SemanticTokensResult>, Self::Error>> {
        self.request(|reply| Cmd::SemanticTokensFull {
            uri: params.text_document.uri,
            reply,
        })
    }

    fn semantic_tokens_full_delta(
        &mut self,
        params: SemanticTokensDeltaParams,
    ) -> BoxFuture<'static, Result<Option<SemanticTokensFullDeltaResult>, Self::Error>> {
        self.request(|reply| Cmd::SemanticTokensFullDelta {
            uri: params.text_document.uri,
            previous_result_id: params.previous_result_id,
            reply,
        })
    }

    fn folding_range(
        &mut self,
        params: FoldingRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<FoldingRange>>, Self::Error>> {
        self.request(|reply| Cmd::FoldingRange {
            uri: params.text_document.uri,
            reply,
        })
    }

    fn selection_range(
        &mut self,
        params: SelectionRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<SelectionRange>>, Self::Error>> {
        self.request(|reply| Cmd::SelectionRange {
            uri: params.text_document.uri,
            positions: params.positions,
            reply,
        })
    }
}

impl ServerState {
    /// Ask the client to watch workspace files. The actor applies focused config invalidation and
    /// per-project source/script/dependency policies; one broad glob avoids overlapping duplicate
    /// events while still covering arbitrary `rerun_if_changed` inputs.
    fn watch_registration() -> RegistrationParams {
        // `None` kind means create + change + delete.
        let watcher = |glob: &str| FileSystemWatcher {
            glob_pattern: GlobPattern::String(glob.into()),
            kind: None,
        };
        let options = DidChangeWatchedFilesRegistrationOptions {
            watchers: vec![watcher("**/*")],
        };
        RegistrationParams {
            registrations: vec![Registration {
                id: "jals-lsp-config-watch".into(),
                method: notification::DidChangeWatchedFiles::METHOD.into(),
                register_options: Some(
                    serde_json::to_value(options).expect("watcher options serialize to JSON"),
                ),
            }],
        }
    }

    /// The capabilities advertised to the client during `initialize`.
    fn server_capabilities() -> ServerCapabilities {
        ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(
                TextDocumentSyncKind::INCREMENTAL,
            )),
            document_symbol_provider: Some(OneOf::Left(true)),
            document_highlight_provider: Some(OneOf::Left(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            completion_provider: Some(CompletionOptions {
                trigger_characters: Some(vec![".".to_owned()]),
                ..CompletionOptions::default()
            }),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            signature_help_provider: Some(SignatureHelpOptions {
                trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
                retrigger_characters: None,
                work_done_progress_options: WorkDoneProgressOptions::default(),
            }),
            document_formatting_provider: Some(OneOf::Left(true)),
            folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
            selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
            semantic_tokens_provider: Some(
                SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                    legend: LspHost::legend(),
                    range: Some(false),
                    full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                    ..SemanticTokensOptions::default()
                }),
            ),
            ..ServerCapabilities::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::Url;
    use jals_exec::block_on_inline;

    use super::*;

    fn frontend() -> (ServerState, mpsc::UnboundedReceiver<Cmd>) {
        let (commands, receiver) = mpsc::unbounded_channel();
        (
            ServerState::new(ClientSocket::new_closed(), commands),
            receiver,
        )
    }

    /// A notification handler returning `ControlFlow::Break` stops `async-lsp`'s main loop,
    /// exiting the server process. The notifications we don't act on must therefore return
    /// `Continue`, not fall through to the omnitrait default (which `Break`s). Regression for
    /// a Helix crash: Helix sends `textDocument/didSave` on every save (we advertise
    /// `TextDocumentSyncCapability::Kind`), which otherwise killed the server.
    #[test]
    fn ignored_notifications_continue_rather_than_break() {
        let (mut state, _receiver) = frontend();
        assert!(matches!(
            state.did_save(DidSaveTextDocumentParams {
                text_document: async_lsp::lsp_types::TextDocumentIdentifier {
                    uri: Url::parse("file:///a/B.java").unwrap(),
                },
                text: None,
            }),
            ControlFlow::Continue(())
        ));
        assert!(matches!(
            state.did_change_configuration(DidChangeConfigurationParams {
                settings: serde_json::Value::Null,
            }),
            ControlFlow::Continue(())
        ));
    }

    /// Handled notifications become queue commands (they must reach the actor, in order), and
    /// still `Continue` the loop.
    #[test]
    fn handled_notifications_enqueue_commands_and_continue() {
        let (mut state, mut receiver) = frontend();
        let flow = state.did_open(DidOpenTextDocumentParams {
            text_document: async_lsp::lsp_types::TextDocumentItem {
                uri: Url::parse("file:///a/B.java").unwrap(),
                language_id: "java".into(),
                version: 1,
                text: "class B {}".into(),
            },
        });
        assert!(matches!(flow, ControlFlow::Continue(())));
        assert!(matches!(receiver.try_recv(), Ok(Cmd::DidOpen(_))));
    }

    /// A request future resolves to `INTERNAL_ERROR` — instead of hanging the client — when the
    /// language service is gone (the actor's receiver dropped).
    #[test]
    fn requests_fail_cleanly_when_the_service_is_gone() {
        let (mut state, receiver) = frontend();
        drop(receiver);
        let future = state.hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: async_lsp::lsp_types::TextDocumentIdentifier {
                    uri: Url::parse("file:///a/B.java").unwrap(),
                },
                position: async_lsp::lsp_types::Position::new(0, 0),
            },
            work_done_progress_params: async_lsp::lsp_types::WorkDoneProgressParams::default(),
        });
        let error = block_on_inline(future).expect_err("the service is unavailable");
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn advertises_rename_with_prepare_support() {
        let caps = ServerState::server_capabilities();
        let Some(OneOf::Right(rename)) = caps.rename_provider else {
            panic!("rename provider advertised with options");
        };
        assert_eq!(rename.prepare_provider, Some(true));
    }

    #[test]
    fn advertises_semantic_tokens_full_delta() {
        let Some(SemanticTokensServerCapabilities::SemanticTokensOptions(options)) =
            ServerState::server_capabilities().semantic_tokens_provider
        else {
            panic!("semantic tokens options advertised");
        };
        assert!(matches!(
            options.full,
            Some(SemanticTokensFullOptions::Delta { delta: Some(true) })
        ));
    }

    #[test]
    fn advertises_completion_triggered_on_dot() {
        let completion = ServerState::server_capabilities()
            .completion_provider
            .expect("completion provider advertised");
        assert_eq!(completion.trigger_characters, Some(vec![".".to_owned()]));
    }

    #[test]
    fn watch_registration_uses_one_non_overlapping_project_glob() {
        let params = ServerState::watch_registration();
        assert_eq!(params.registrations.len(), 1);
        let registration = &params.registrations[0];
        assert_eq!(registration.method, "workspace/didChangeWatchedFiles");
        let options = registration.register_options.as_ref().unwrap();
        assert_eq!(options["watchers"].as_array().unwrap().len(), 1);
        assert_eq!(options["watchers"][0]["globPattern"], "**/*");
    }
}
