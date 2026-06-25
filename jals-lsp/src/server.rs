//! The LSP server: wires the document store and pure handlers to async-lsp's
//! `LanguageServer` trait, and runs the stdio event loop.

use std::ops::ControlFlow;
use std::path::Path;

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
    InitializedParams, Location, OneOf, PrepareRenameResponse, PublishDiagnosticsParams,
    ReferenceParams, Registration, RegistrationParams, RenameFilesParams, RenameOptions,
    RenameParams, SelectionRange, SelectionRangeParams, SelectionRangeProviderCapability,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelp, SignatureHelpOptions,
    SignatureHelpParams, TextDocumentPositionParams, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WillSaveTextDocumentParams, WorkDoneProgressCancelParams,
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
use jals_build::Manifest;
use tower::ServiceBuilder;

use crate::handlers;
use crate::state::{Discovery, DocumentStore, Workspace, is_config_file, is_lint_config_file};

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

/// Server state: the client handle, open documents, memoized config discovery (one cache each for
/// the formatter's `jalsfmt.toml` and the linter's `jalslint.toml`), and one symbol index per
/// open `jals.toml` project.
struct ServerState {
    client: ClientSocket,
    store: DocumentStore,
    discovery: Discovery<jals_fmt::Config>,
    lint_discovery: Discovery<jals_lint::Config>,
    /// One [`Workspace`] per `jals.toml` project a client has a file open in. Populated lazily on
    /// `did_open` by walking up from the file to its manifest (see [`ServerState::ensure_workspace_for`]),
    /// so the server only ever indexes a real project's source roots, never a whole git checkout.
    /// Empty for files that belong to no manifest, which fall back to file-local resolution.
    workspaces: Vec<Workspace>,
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
            workspaces: Vec::new(),
            config_watch_registration_supported: false,
        }
    }

    /// Ensure a [`Workspace`] is loaded for the `jals.toml` project `uri` belongs to.
    ///
    /// Walks up from the file's directory to find its manifest. If there is one and no workspace
    /// for that project root is loaded yet, builds it from the manifest's source roots and adds it;
    /// if one already exists it is reused, and a file under no manifest is left for file-local
    /// resolution. The manifest is only parsed when a new workspace is actually built, so reopening
    /// files in an already-loaded project is cheap.
    fn ensure_workspace_for(&mut self, uri: &Url) {
        let Some(dir) = uri
            .to_file_path()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            return;
        };
        let Some(manifest_path) = Manifest::discover_path(&dir) else {
            return;
        };
        let root = manifest_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        if self.workspaces.iter().any(|ws| ws.project_root() == root) {
            return;
        }
        let source_roots = match Manifest::from_file(&manifest_path) {
            Ok(manifest) => manifest.source_roots(&root),
            Err(_) => vec![root.clone()],
        };
        self.workspaces.push(Workspace::load(root, source_roots));
    }

    /// The loaded workspace that owns `uri`, if any.
    fn workspace_for(&self, uri: &Url) -> Option<&Workspace> {
        self.workspaces.iter().find(|ws| ws.owns_uri(uri))
    }

    /// Reflect the open document at `uri` into the project index of the workspace that owns it, if
    /// any.
    fn refresh_workspace_overlay(&mut self, uri: &Url) {
        let Some(doc) = self.store.get(uri) else {
            return;
        };
        if let Some(workspace) = self.workspaces.iter_mut().find(|ws| ws.owns_uri(uri)) {
            workspace.set_overlay(uri, &doc);
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

        // The workspace (if any) that owns this file, paired with its id within that workspace's
        // index — looked up once and reused by both the lint-rule suppression and the cross-file
        // passes below.
        let workspace_file = self
            .workspace_for(uri)
            .and_then(|ws| Some((ws, ws.file_id(uri)?)));
        // Files in an indexed project get the index-aware `type-mismatch` check below; suppress the
        // file-local lint rule of the same name there so the two never double-report.
        let mut rule_config = lint_config.clone();
        if workspace_file.is_some() {
            rule_config.rules.insert(
                jals_lint::TYPE_MISMATCH_RULE.to_string(),
                jals_lint::Severity::Allow,
            );
        }
        diagnostics.extend(handlers::compute_lint_diagnostics(
            &doc.parse,
            &doc.text,
            &doc.line_index,
            &rule_config,
        ));
        // Cross-file diagnostics ("cannot resolve symbol" + index-aware type mismatches), only for
        // files in an indexed project. Both passes read the same file-local resolution, so resolve
        // the tree once here and share it rather than resolving twice per publish.
        if let Some((workspace, file)) = workspace_file {
            let resolved = jals_hir::resolve_node(&doc.parse.syntax());
            diagnostics.extend(handlers::compute_type_diagnostics(
                workspace.index(),
                file,
                &doc.parse,
                &resolved,
                &doc.text,
                &doc.line_index,
            ));
            diagnostics.extend(handlers::compute_type_mismatch_diagnostics(
                workspace.index(),
                file,
                &doc.parse,
                &resolved,
                &doc.text,
                &doc.line_index,
                &lint_config,
            ));
        }
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
        // Project symbol indexes are built lazily, per `jals.toml` project, the first time a file
        // in that project is opened (see `did_open`/`ensure_workspace_for`) — so a client that
        // opens a large folder with no manifest never triggers a whole-tree walk here.
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
        // Discover (and index, once) the `jals.toml` project this file belongs to, so cross-file
        // resolution works without ever walking a non-project folder.
        self.ensure_workspace_for(&uri);
        self.refresh_workspace_overlay(&uri);
        self.publish_diagnostics(&uri);
        ControlFlow::Continue(())
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        self.store
            .apply_changes(&uri, &params.content_changes, params.text_document.version);
        self.refresh_workspace_overlay(&uri);
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
        let uri = pos.text_document.uri;
        let position = pos.position;
        // A file in the project index highlights cross-file type names precisely through the
        // workspace; any other document falls back to file-local highlighting (a lexical match for
        // such a name) over the open document alone.
        let highlights = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.document_highlight(&uri, position))
            .or_else(|| {
                self.store.get(&uri).map(|doc| {
                    handlers::document_highlight(
                        &doc.parse,
                        &doc.text,
                        &doc.line_index,
                        position,
                        None,
                    )
                })
            });
        Box::pin(async move { Ok(highlights) })
    }

    fn definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> BoxFuture<'static, Result<Option<GotoDefinitionResponse>, Self::Error>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        // A file in the project index resolves cross-file (and file-locally) through the workspace.
        // `goto_definition` returns `None` for any other document, which then falls back to
        // file-local resolution against the open document alone.
        let location = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.goto_definition(&uri, position))
            .or_else(|| {
                self.store.get(&uri).and_then(|doc| {
                    handlers::goto_definition_local(
                        &doc.parse,
                        &doc.text,
                        &doc.line_index,
                        position,
                    )
                    .map(|range| Location {
                        uri: uri.clone(),
                        range,
                    })
                })
            });
        Box::pin(async move { Ok(location.map(GotoDefinitionResponse::Scalar)) })
    }

    fn references(
        &mut self,
        params: ReferenceParams,
    ) -> BoxFuture<'static, Result<Option<Vec<Location>>, Self::Error>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        // A file in an indexed project finds references project-wide through the workspace (a project
        // type used from any source file); any other document falls back to file-local references
        // over the open document alone.
        let locations = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.references(&uri, position, include_declaration))
            .or_else(|| {
                self.store.get(&uri).map(|doc| {
                    handlers::references(
                        &doc.parse,
                        &doc.text,
                        &doc.line_index,
                        &uri,
                        position,
                        include_declaration,
                    )
                })
            });
        Box::pin(async move { Ok(locations) })
    }

    fn prepare_rename(
        &mut self,
        params: TextDocumentPositionParams,
    ) -> BoxFuture<'static, Result<Option<PrepareRenameResponse>, Self::Error>> {
        let uri = params.text_document.uri;
        let position = params.position;
        // A file in an indexed project validates project types project-wide through the workspace;
        // any other document falls back to file-local renamability over the open document alone.
        let range = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.prepare_rename(&uri, position))
            .or_else(|| {
                self.store.get(&uri).and_then(|doc| {
                    handlers::prepare_rename_local(&doc.parse, &doc.text, &doc.line_index, position)
                })
            });
        Box::pin(async move { Ok(range.map(PrepareRenameResponse::Range)) })
    }

    fn rename(
        &mut self,
        params: RenameParams,
    ) -> BoxFuture<'static, Result<Option<WorkspaceEdit>, Self::Error>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let new_name = params.new_name;
        // Reject a new name that is not a single legal Java identifier before producing any edit, so
        // the editor surfaces the error instead of writing broken source.
        if !handlers::is_valid_identifier(&new_name) {
            return Box::pin(async move {
                Err(ResponseError::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("`{new_name}` is not a valid Java identifier"),
                ))
            });
        }
        // A file in an indexed project renames project types project-wide through the workspace;
        // any other document falls back to a file-local rename over the open document alone.
        let edit = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.rename(&uri, position, &new_name))
            .or_else(|| {
                self.store.get(&uri).and_then(|doc| {
                    handlers::rename_local(
                        &doc.parse,
                        &doc.text,
                        &doc.line_index,
                        &uri,
                        position,
                        &new_name,
                    )
                })
            });
        Box::pin(async move { Ok(edit) })
    }

    fn completion(
        &mut self,
        params: CompletionParams,
    ) -> BoxFuture<'static, Result<Option<CompletionResponse>, Self::Error>> {
        let pos = params.text_document_position;
        let uri = pos.text_document.uri;
        let position = pos.position;
        // A file in the project index completes members with cross-file type names through the
        // workspace; any other document falls back to a single-file index of the open document.
        let items = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.completions(&uri, position))
            .or_else(|| {
                self.store.get(&uri).map(|doc| {
                    handlers::completions_local(&doc.parse, &doc.text, &doc.line_index, position)
                })
            });
        Box::pin(async move { Ok(items.map(CompletionResponse::Array)) })
    }

    fn hover(
        &mut self,
        params: HoverParams,
    ) -> BoxFuture<'static, Result<Option<Hover>, Self::Error>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        // A file in the project index infers with cross-file type names through the workspace; any
        // other document falls back to file-local inference against the open document alone.
        let hover = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.hover(&uri, position))
            .or_else(|| {
                self.store.get(&uri).and_then(|doc| {
                    handlers::hover_local(&doc.parse, &doc.text, &doc.line_index, position)
                })
            });
        Box::pin(async move { Ok(hover) })
    }

    fn signature_help(
        &mut self,
        params: SignatureHelpParams,
    ) -> BoxFuture<'static, Result<Option<SignatureHelp>, Self::Error>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;
        let position = pos.position;
        // A file in the project index resolves overloads with cross-file type names through the
        // workspace; any other document falls back to a single-file index of the open document.
        let help = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.signature_help(&uri, position))
            .or_else(|| {
                self.store.get(&uri).and_then(|doc| {
                    handlers::signature_help_local(&doc.parse, &doc.text, &doc.line_index, position)
                })
            });
        Box::pin(async move { Ok(help) })
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
        let uri = params.text_document.uri;
        // A file in the project index classifies cross-file type names by their declared kind through
        // the workspace; any other document falls back to file-local classification (the generic
        // `type` for such a name) over the open document alone.
        let tokens = self
            .workspace_for(&uri)
            .and_then(|workspace| workspace.semantic_tokens(&uri))
            .or_else(|| {
                self.store.get(&uri).map(|doc| {
                    handlers::semantic_tokens(&doc.parse, &doc.text, &doc.line_index, None)
                })
            });
        Box::pin(async move { Ok(tokens.map(SemanticTokensResult::Tokens)) })
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
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        }),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
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

    /// `did_open` builds at most one workspace per `jals.toml` project, reuses it for later files in
    /// the same project, and builds none for a file under no manifest — so opening a file in a
    /// manifestless folder never triggers a whole-tree index walk (the Helix freeze regression).
    #[test]
    fn did_open_indexes_one_workspace_per_project() {
        use async_lsp::lsp_types::TextDocumentItem;

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

        fn open(state: &mut ServerState, path: std::path::PathBuf, text: &str) {
            std::fs::write(&path, text).unwrap();
            let _ = state.did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(path).unwrap(),
                    language_id: "java".into(),
                    version: 1,
                    text: text.into(),
                },
            });
        }

        let proj_a = project("a");
        let proj_b = project("b");
        let no_manifest = tempfile::tempdir().unwrap();

        let mut state = ServerState::new(ClientSocket::new_closed());

        open(&mut state, proj_a.path().join("src/A.java"), "class A {}");
        assert_eq!(state.workspaces.len(), 1, "first file builds one workspace");

        open(&mut state, proj_a.path().join("src/A2.java"), "class A2 {}");
        assert_eq!(
            state.workspaces.len(),
            1,
            "a second file in the same project reuses the workspace"
        );

        open(&mut state, proj_b.path().join("src/B.java"), "class B {}");
        assert_eq!(
            state.workspaces.len(),
            2,
            "a file in a different project adds a second workspace"
        );

        open(&mut state, no_manifest.path().join("C.java"), "class C {}");
        assert_eq!(
            state.workspaces.len(),
            2,
            "a file under no manifest builds no workspace"
        );
    }

    #[test]
    fn advertises_rename_with_prepare_support() {
        let caps = server_capabilities();
        let Some(OneOf::Right(rename)) = caps.rename_provider else {
            panic!("rename provider advertised with options");
        };
        assert_eq!(rename.prepare_provider, Some(true));
    }

    #[test]
    fn advertises_completion_triggered_on_dot() {
        let completion = server_capabilities()
            .completion_provider
            .expect("completion provider advertised");
        assert_eq!(completion.trigger_characters, Some(vec![".".to_string()]));
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
