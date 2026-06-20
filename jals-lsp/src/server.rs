//! The LSP server: wires the document store and pure handlers to async-lsp's
//! `LanguageServer` trait, and runs the stdio event loop.

use std::ops::ControlFlow;

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{
    CreateFilesParams, DeleteFilesParams, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidChangeWorkspaceFoldersParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightParams, DocumentSymbolParams,
    DocumentSymbolResponse, FileSystemWatcher, FoldingRange, FoldingRangeParams,
    FoldingRangeProviderCapability, GlobPattern, InitializeParams, InitializeResult,
    InitializedParams, OneOf, PublishDiagnosticsParams, Registration, RegistrationParams,
    RenameFilesParams, SelectionRange, SelectionRangeParams, SelectionRangeProviderCapability,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WillSaveTextDocumentParams, WorkDoneProgressCancelParams,
    notification::{self, Notification},
    request,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, LanguageServer, MainLoop, ResponseError};
use futures::future::BoxFuture;
use tower::ServiceBuilder;

use crate::handlers;
use crate::state::{Discovery, DocumentStore, is_config_file, is_lint_config_file};

/// Build the server and run its stdio event loop until the client disconnects.
pub(crate) async fn run_server() -> anyhow::Result<()> {
    let (server, _client) = MainLoop::new_server(|client| {
        ServiceBuilder::new()
            .layer(TracingLayer::default())
            .layer(LifecycleLayer::default())
            .layer(CatchUnwindLayer::default())
            .layer(ConcurrencyLayer::default())
            .layer(ClientProcessMonitorLayer::new(client.clone()))
            .service(Router::from_language_server(ServerState::new(client)))
    });

    // Truly asynchronous piped stdin/stdout (unix). stdout is the LSP transport, so all
    // logging must go to stderr.
    let stdin = async_lsp::stdio::PipeStdin::lock_tokio()?;
    let stdout = async_lsp::stdio::PipeStdout::lock_tokio()?;
    server.run_buffered(stdin, stdout).await?;
    Ok(())
}

/// Server state: the client handle, open documents, and memoized config discovery (one cache
/// each for the formatter's `jalsfmt.toml` and the linter's `jalslint.toml`).
struct ServerState {
    client: ClientSocket,
    store: DocumentStore,
    discovery: Discovery<jals_fmt::Config>,
    lint_discovery: Discovery<jals_lint::Config>,
    /// Whether the client supports dynamic registration of `workspace/didChangeWatchedFiles`,
    /// taken from the `initialize` request. Gates the config watcher registration.
    config_watch_registration_supported: bool,
}

impl ServerState {
    fn new(client: ClientSocket) -> ServerState {
        ServerState {
            client,
            store: DocumentStore::default(),
            discovery: Discovery::default(),
            lint_discovery: Discovery::default(),
            config_watch_registration_supported: false,
        }
    }

    /// Compute and push diagnostics for `uri` (a no-op if the document is not open).
    ///
    /// Both the parser's syntax errors and the enabled `jals-lint` rules run over the same
    /// cached CST (no reparse), and their diagnostics are merged into one publish.
    fn publish_diagnostics(&mut self, uri: &Url) {
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        let mut diagnostics = handlers::compute_diagnostics(&doc.parse, &doc.text, &doc.line_index);
        let lint_config = self.lint_discovery.for_uri(uri);
        diagnostics.extend(handlers::compute_lint_diagnostics(
            &doc.parse,
            &doc.text,
            &doc.line_index,
            &lint_config,
        ));
        let _ = self
            .client
            .notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version: Some(doc.version),
            });
    }
}

impl LanguageServer for ServerState {
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(
        &mut self,
        params: InitializeParams,
    ) -> BoxFuture<'static, Result<InitializeResult, Self::Error>> {
        self.config_watch_registration_supported = params
            .capabilities
            .workspace
            .and_then(|workspace| workspace.did_change_watched_files)
            .and_then(|caps| caps.dynamic_registration)
            .unwrap_or(false);
        Box::pin(async move {
            Ok(InitializeResult {
                capabilities: server_capabilities(),
                server_info: None,
            })
        })
    }

    fn initialized(&mut self, _params: InitializedParams) -> Self::NotifyResult {
        if self.config_watch_registration_supported {
            let client = self.client.clone();
            // Notification handlers are sync and run on the main-loop task; send the
            // client request from a spawned task so the loop stays free to deliver
            // the response.
            tokio::spawn(async move {
                let _ = client
                    .request::<request::RegisterCapability>(config_watch_registration())
                    .await;
            });
        }
        ControlFlow::Continue(())
    }

    fn did_change_watched_files(
        &mut self,
        params: DidChangeWatchedFilesParams,
    ) -> Self::NotifyResult {
        // A created/changed/deleted config file can affect discovery for any directory at or
        // below it (including shadowing); drop the whole memo for the affected tool and
        // rediscover lazily on the next request that needs it.
        if params.changes.iter().any(|e| is_config_file(&e.uri)) {
            self.discovery.clear();
        }
        if params.changes.iter().any(|e| is_lint_config_file(&e.uri)) {
            self.lint_discovery.clear();
        }
        ControlFlow::Continue(())
    }

    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Self::NotifyResult {
        let doc = params.text_document;
        let uri = doc.uri;
        self.store.upsert(uri.clone(), doc.text, doc.version);
        self.publish_diagnostics(&uri);
        ControlFlow::Continue(())
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        self.store
            .apply_changes(&uri, &params.content_changes, params.text_document.version);
        self.publish_diagnostics(&uri);
        ControlFlow::Continue(())
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        self.store.remove(&uri);
        // Clear stale diagnostics for the now-closed document.
        let _ = self
            .client
            .notify::<notification::PublishDiagnostics>(PublishDiagnosticsParams {
                uri,
                diagnostics: Vec::new(),
                version: None,
            });
        ControlFlow::Continue(())
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
        _params: DidChangeConfigurationParams,
    ) -> Self::NotifyResult {
        ControlFlow::Continue(())
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
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| {
                DocumentSymbolResponse::Nested(handlers::document_symbols(
                    &doc.parse,
                    &doc.text,
                    &doc.line_index,
                ))
            }))
        })
    }

    fn document_highlight(
        &mut self,
        params: DocumentHighlightParams,
    ) -> BoxFuture<'static, Result<Option<Vec<DocumentHighlight>>, Self::Error>> {
        let pos = params.text_document_position_params;
        let doc = self.store.get(&pos.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| {
                handlers::document_highlight(&doc.parse, &doc.text, &doc.line_index, pos.position)
            }))
        })
    }

    fn formatting(
        &mut self,
        params: DocumentFormattingParams,
    ) -> BoxFuture<'static, Result<Option<Vec<TextEdit>>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        let config = self.discovery.for_uri(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| handlers::formatting_edits(&doc.text, &config, &doc.line_index)))
        })
    }

    fn semantic_tokens_full(
        &mut self,
        params: SemanticTokensParams,
    ) -> BoxFuture<'static, Result<Option<SemanticTokensResult>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| {
                SemanticTokensResult::Tokens(handlers::semantic_tokens(
                    &doc.parse,
                    &doc.text,
                    &doc.line_index,
                ))
            }))
        })
    }

    fn folding_range(
        &mut self,
        params: FoldingRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<FoldingRange>>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| handlers::folding_range(&doc.parse, &doc.text, &doc.line_index)))
        })
    }

    fn selection_range(
        &mut self,
        params: SelectionRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<SelectionRange>>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| {
                handlers::selection_ranges(
                    &doc.parse,
                    &doc.text,
                    &doc.line_index,
                    &params.positions,
                )
            }))
        })
    }
}

/// Ask the client to watch `jalsfmt.toml` and `jalslint.toml` files anywhere in the
/// workspace, so config edits invalidate the discovery caches without a server restart.
fn config_watch_registration() -> RegistrationParams {
    // `None` kind means create + change + delete.
    let watcher = |glob: &str| FileSystemWatcher {
        glob_pattern: GlobPattern::String(glob.into()),
        kind: None,
    };
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![watcher("**/jalsfmt.toml"), watcher("**/jalslint.toml")],
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
        document_formatting_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: handlers::semantic_tokens_legend(),
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..SemanticTokensOptions::default()
            },
        )),
        ..ServerCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A notification handler returning `ControlFlow::Break` stops `async-lsp`'s main loop,
    /// exiting the server process. The notifications we don't act on must therefore return
    /// `Continue`, not fall through to the omnitrait default (which `Break`s). Regression for
    /// a Helix crash: Helix sends `textDocument/didSave` on every save (we advertise
    /// `TextDocumentSyncCapability::Kind`), which otherwise killed the server.
    #[test]
    fn ignored_notifications_continue_rather_than_break() {
        let mut state = ServerState::new(ClientSocket::new_closed());
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

    #[test]
    fn config_watch_registration_targets_both_config_files() {
        let params = config_watch_registration();
        assert_eq!(params.registrations.len(), 1);
        let registration = &params.registrations[0];
        assert_eq!(registration.method, "workspace/didChangeWatchedFiles");
        let options = registration.register_options.as_ref().unwrap();
        assert_eq!(options["watchers"][0]["globPattern"], "**/jalsfmt.toml");
        assert_eq!(options["watchers"][1]["globPattern"], "**/jalslint.toml");
    }
}
