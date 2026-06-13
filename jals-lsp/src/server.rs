//! The LSP server: wires the document store and pure handlers to async-lsp's
//! `LanguageServer` trait, and runs the stdio event loop.

use std::ops::ControlFlow;

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentHighlight,
    DocumentHighlightParams, DocumentSymbolParams, DocumentSymbolResponse, FileSystemWatcher,
    FoldingRange, FoldingRangeParams, FoldingRangeProviderCapability, GlobPattern,
    InitializeParams, InitializeResult, InitializedParams, OneOf, PublishDiagnosticsParams,
    Registration, RegistrationParams, SelectionRange, SelectionRangeParams,
    SelectionRangeProviderCapability, SemanticTokensFullOptions, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url,
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
use crate::state::{Discovery, DocumentStore, is_config_file};

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

/// Server state: the client handle, open documents, and memoized config discovery.
struct ServerState {
    client: ClientSocket,
    store: DocumentStore,
    discovery: Discovery,
    /// Whether the client supports dynamic registration of `workspace/didChangeWatchedFiles`,
    /// taken from the `initialize` request. Gates the `jalsfmt.toml` watcher registration.
    config_watch_registration_supported: bool,
}

impl ServerState {
    fn new(client: ClientSocket) -> ServerState {
        ServerState {
            client,
            store: DocumentStore::default(),
            discovery: Discovery::default(),
            config_watch_registration_supported: false,
        }
    }

    /// Compute and push diagnostics for `uri` (a no-op if the document is not open).
    fn publish_diagnostics(&mut self, uri: &Url) {
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        let diagnostics = handlers::compute_diagnostics(&doc.text, &doc.line_index);
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
        // A created/changed/deleted jalsfmt.toml can affect discovery for any
        // directory at or below it (including shadowing); drop the whole memo and
        // rediscover lazily on the next formatting request.
        if params
            .changes
            .iter()
            .any(|event| is_config_file(&event.uri))
        {
            self.discovery.clear();
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

    fn document_symbol(
        &mut self,
        params: DocumentSymbolParams,
    ) -> BoxFuture<'static, Result<Option<DocumentSymbolResponse>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| {
                DocumentSymbolResponse::Nested(handlers::document_symbols(
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
            Ok(doc
                .map(|doc| handlers::document_highlight(&doc.text, &doc.line_index, pos.position)))
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
                SemanticTokensResult::Tokens(handlers::semantic_tokens(&doc.text, &doc.line_index))
            }))
        })
    }

    fn folding_range(
        &mut self,
        params: FoldingRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<FoldingRange>>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(
            async move { Ok(doc.map(|doc| handlers::folding_range(&doc.text, &doc.line_index))) },
        )
    }

    fn selection_range(
        &mut self,
        params: SelectionRangeParams,
    ) -> BoxFuture<'static, Result<Option<Vec<SelectionRange>>, Self::Error>> {
        let doc = self.store.get(&params.text_document.uri);
        Box::pin(async move {
            Ok(doc.map(|doc| {
                handlers::selection_ranges(&doc.text, &doc.line_index, &params.positions)
            }))
        })
    }
}

/// Ask the client to watch `jalsfmt.toml` files anywhere in the workspace, so config
/// edits invalidate the discovery cache without a server restart.
fn config_watch_registration() -> RegistrationParams {
    let options = DidChangeWatchedFilesRegistrationOptions {
        watchers: vec![FileSystemWatcher {
            glob_pattern: GlobPattern::String("**/jalsfmt.toml".into()),
            // `None` means create + change + delete.
            kind: None,
        }],
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

    #[test]
    fn config_watch_registration_targets_jalsfmt_toml() {
        let params = config_watch_registration();
        assert_eq!(params.registrations.len(), 1);
        let registration = &params.registrations[0];
        assert_eq!(registration.method, "workspace/didChangeWatchedFiles");
        let options = registration.register_options.as_ref().unwrap();
        assert_eq!(options["watchers"][0]["globPattern"], "**/jalsfmt.toml");
    }
}
