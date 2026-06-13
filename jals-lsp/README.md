# jals-lsp

A Language Server Protocol (LSP) server for Java, built on the `jals-syntax` CST and the
`jals-fmt` formatter.

`jals-lsp` is an [`async-lsp`](https://github.com/oxalica/async-lsp) server exposed through the
`jals lsp` subcommand. It reuses the lossless, error-resilient syntax tree and the formatter to
provide editor features — everything is driven by the same parse that `jals fmt` uses, with no
separate analysis pass.

```
editor ◀── stdio (LSP) ──▶ jals lsp
                              │
      ┌─────────────┬─────────┴────┬─────────────┬───────────────┬─────────────┐
      ▼             ▼              ▼             ▼               ▼             ▼
 diagnostics  documentSymbol semanticTokens foldingRange documentHighlight formatting
(SyntaxErrors) (typed AST)   (CST classify) (CST braces)   (CST lexical)   (jals-fmt)
      └─────────────┴──────────────┴─────────────┴───────────────┴─────────────┘
                    byte offsets ──▶ UTF-16 positions (LineIndex)
```

## What it does today

Server capabilities advertised on `initialize`:

| LSP feature | Method | Source | Notes |
| --- | --- | --- | --- |
| Diagnostics | `textDocument/publishDiagnostics` | parser `SyntaxError`s | Pushed on open/change; `ERROR` severity, `source: "jals"`. Cleared on close. |
| Document symbols | `textDocument/documentSymbol` | typed AST | Hierarchical: types → members (fields, methods, constructors, nested types, enum constants). |
| Semantic tokens | `textDocument/semanticTokens/full` | CST + parent context | Whole-document highlighting: keywords, types, declarations, name/field/call references, annotations, comments, literals. Best-effort without name resolution, but the lossless tree resolves contextual keywords (`var`, `record`, `sealed`, module directives) that TextMate grammars miss. |
| Code folding | `textDocument/foldingRange` | CST | Folds class/enum/module bodies, blocks (control-flow & lambdas included), switch blocks, array initializers, multi-line block/doc comments, and import groups. The closing brace stays visible; multi-line spans only. |
| Selection range | `textDocument/selectionRange` | CST | Expand/shrink: nests the token under each cursor up through its ancestor nodes to the file root. Syntax-only; multiple positions per request. |
| Occurrence highlight | `textDocument/documentHighlight` | CST (lexical) | Highlights every identifier with the same text as the one under the cursor — purely lexical, no name resolution. Declaration/binding names, simple-name assignment targets, and `++`/`--` operands are Write; everything else is Read. |
| Formatting | `textDocument/formatting` | `jals_fmt::format_source` | Whole-document: one full-range edit, or none if already formatted. |
| Config hot-reload | `workspace/didChangeWatchedFiles` | — | Dynamically registers a `**/jalsfmt.toml` watcher via `client/registerCapability` (when the client supports dynamic registration); changes clear the config cache so the next format request rediscovers. |
| Text sync | `didOpen` / `didChange` / `didClose` | — | Incremental sync (`TextDocumentSyncKind::INCREMENTAL`): change events are spliced in order (UTF-16 ranges → byte offsets); full-replacement events are still accepted. |
| Lifecycle | `initialize` / `shutdown` / `exit` | — | Managed by async-lsp's `LifecycleLayer`. |

Formatting config is discovered per document by searching upward for `jalsfmt.toml` from the
file's directory (memoized), matching the `jals fmt` CLI. Non-`file:` URIs (e.g. `untitled:`)
fall back to `Config::default()`. When the client supports file watching, `jalsfmt.toml` edits
take effect without a server restart.

## Usage

The server speaks LSP over stdio and is meant to be launched by an editor, not run
interactively:

```sh
jals lsp        # also accepts --stdio for editor compatibility; stdio is always used
```

### Neovim (0.11+, built-in client)

```lua
vim.lsp.config['jals'] = {
  cmd = { 'jals', 'lsp' },
  filetypes = { 'java' },
  root_markers = { 'jalsfmt.toml', '.git' },
}
vim.lsp.enable('jals')
```

### VS Code / other clients

Any generic LSP client works: launch `jals lsp` as the server command with stdio transport and
`java` as the document selector. A dedicated extension is not published yet.

## Architecture

The crate splits into a pure, unit-tested core and a thin async server shell:

| Module | Role |
| --- | --- |
| `handlers/{diagnostics,symbols,semantic_tokens,folding_range,selection_range,document_highlight,formatting}.rs` | Pure functions `(text [, config], &LineIndex) -> LSP payload`. No I/O, no async — the testable core. |
| `line_index.rs` | Converts `jals-syntax` UTF-8 byte offsets to LSP UTF-16 `Position`s. |
| `state.rs` | `DocumentStore` (open documents, incremental text sync via the pure `apply_content_changes`) and memoized config `Discovery`. |
| `server.rs` | `LanguageServer` impl + advertised capabilities; glue that calls the pure handlers. |
| `lib.rs` | `run()`: a current-thread tokio runtime driving the async-lsp `MainLoop` over stdio. |

The server runs single-threaded on a current-thread runtime, so document state needs no
locking. The `async-lsp` tower stack supplies tracing, lifecycle, panic-catching, concurrency,
and client-process-monitoring layers.

`jals-lsp` is **host-only** (it uses tokio and stdio); unlike `jals-syntax` / `jals-fmt` it is
not built for `wasm32`.

## Development

```sh
cargo test -p jals-lsp                                              # LineIndex + pure-handler tests
cargo clippy -p jals-lsp --all-targets --all-features -- -D warnings
```

Manual smoke test over stdio. async-lsp requires *pipe* stdin **and** stdout, so pipe stdout
(e.g. `| cat`) — a redirect to a regular file is rejected:

```sh
emit() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }
INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}'
INITED='{"jsonrpc":"2.0","method":"initialized","params":{}}'
SHUTDOWN='{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
EXIT='{"jsonrpc":"2.0","method":"exit"}'
{ emit "$INIT"; emit "$INITED"; emit "$SHUTDOWN"; emit "$EXIT"; } | jals lsp | cat
```

---

# Roadmap

v1 deliberately covers only what the **syntax layer** can support. The phases below group
future work by what each capability requires.

## 1. Near-term (syntax-only, low risk)

| Capability | LSP method | Notes |
| --- | --- | --- |
| Range formatting | `textDocument/rangeFormatting` | Format a selection. Needs `jals-fmt` to format a sub-range (today it is whole-document only). |
| On-type formatting | `textDocument/onTypeFormatting` | Reformat on `}` / `;`. |
| Integration test | — | Drive async-lsp over an in-memory transport (initialize → didOpen → diagnostics → shutdown) for CI regression coverage. |

## 2. Mid-term (syntax-based features — CST/AST only, no types)

| Capability | LSP method |
| --- | --- |
| Semantic tokens: incremental + range | `textDocument/semanticTokens/{full/delta,range}` (the `full` variant already ships — see above) |
| Document links (imports) | `textDocument/documentLink` |
| Workspace symbols | `workspace/symbol` |
| Lint diagnostics | merge a future `jals-lint`'s output into `publishDiagnostics` |

## 3. Long-term (requires a semantic layer)

These depend on name resolution / type checking, which `jals` does not have yet; they are
gated on a future analysis crate (`jals-hir` or similar):

| Capability | LSP method |
| --- | --- |
| Hover (types, Javadoc) | `textDocument/hover` |
| Completion | `textDocument/completion` |
| Go to definition / declaration | `textDocument/definition`, `textDocument/declaration` |
| Find references | `textDocument/references` |
| Rename | `textDocument/rename` + `prepareRename` |
| Signature help | `textDocument/signatureHelp` |
| Code actions / quick fixes | `textDocument/codeAction` |

## Operational / polish

| Capability | Notes |
| --- | --- |
| Windows transport | v1 uses unix `PipeStdin` / `PipeStdout`; add a `tokio-util` compat fallback for non-unix. |
| Server-side logging | structured tracing to **stderr** (stdout is the LSP transport — never log there). |
| Client configuration | accept `initializationOptions` / `workspace/didChangeConfiguration` (explicit config path, per-feature toggles). |

## Suggested priority

By editor-user impact: **(1)** range / on-type formatting → **(2)** the
semantic-analysis features, once an analysis layer lands. Incremental sync, code folding,
selection range, occurrence highlight, and semantic tokens (`full`) already ship; the
latter's `delta`/`range` variants are a later optimization.
