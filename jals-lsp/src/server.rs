//! The LSP server: wires the document store and pure handlers to async-lsp's
//! `LanguageServer` trait, and runs the stdio event loop.

use std::ops::ControlFlow;

use async_lsp::client_monitor::ClientProcessMonitorLayer;
use async_lsp::concurrency::ConcurrencyLayer;
use async_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, DocumentSymbolParams, DocumentSymbolResponse, InitializeParams,
    InitializeResult, OneOf, PublishDiagnosticsParams, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url, notification,
};
use async_lsp::panic::CatchUnwindLayer;
use async_lsp::router::Router;
use async_lsp::server::LifecycleLayer;
use async_lsp::tracing::TracingLayer;
use async_lsp::{ClientSocket, LanguageServer, MainLoop, ResponseError};
use futures::future::BoxFuture;
use tower::ServiceBuilder;

use crate::handlers;
use crate::state::{Discovery, DocumentStore};

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
}

impl ServerState {
    fn new(client: ClientSocket) -> ServerState {
        ServerState {
            client,
            store: DocumentStore::default(),
            discovery: Discovery::default(),
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
        _params: InitializeParams,
    ) -> BoxFuture<'static, Result<InitializeResult, Self::Error>> {
        Box::pin(async move {
            Ok(InitializeResult {
                capabilities: server_capabilities(),
                server_info: None,
            })
        })
    }

    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Self::NotifyResult {
        let doc = params.text_document;
        let uri = doc.uri;
        self.store.upsert(uri.clone(), doc.text, doc.version);
        self.publish_diagnostics(&uri);
        ControlFlow::Continue(())
    }

    fn did_change(&mut self, mut params: DidChangeTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        // Full sync: the last change event carries the entire new document text.
        if let Some(change) = params.content_changes.pop() {
            self.store
                .upsert(uri.clone(), change.text, params.text_document.version);
        }
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
}

/// The capabilities advertised to the client during `initialize`.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_symbol_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..ServerCapabilities::default()
    }
}
