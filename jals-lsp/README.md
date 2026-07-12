# jals-lsp

A Language Server Protocol (LSP) server for Java, built on the `jals-syntax` CST, the
`jals-fmt` formatter, the `jals-lint` linter, and `jals-hir`'s name resolution / type
inference.

`jals-lsp` is an [`async-lsp`](https://github.com/oxalica/async-lsp) server exposed through the
`jals lsp` subcommand. Syntax-only features (folding, selection range, document symbols,
formatting, the base semantic-token classification) are driven by the same lossless parse that
`jals fmt` uses. Everything that needs to understand code across files — hover, go-to-definition,
find-references, rename, completion, signature help, and cross-file diagnostics — is backed by a
per-project `jals-hir` `ProjectIndex` that also folds in the project's compiled classpath and
`[dependencies]`.

```
editor ◀── stdio (LSP) ──▶ jals lsp
                              │
      ┌─────────────┬────────┴────────┬──────────────┬────────────────┬─────────────┐
      ▼              ▼                 ▼              ▼                ▼             ▼
 diagnostics   documentSymbol    semanticTokens   foldingRange   documentHighlight formatting
(SyntaxErrors   (typed AST)      (CST, refined     (CST braces)   (CST lexical,    (jals-fmt)
 + jals-lint +                    by jals-hir                      sharpened by
 cross-file                       when resolvable)                 jals-hir when
 jals-hir checks)                                                  indexed)
      └─────────────┴─────────────────┴──────────────┴────────────────┴─────────────┘
                            byte offsets ──▶ UTF-16 positions (LineIndex)

              hover · definition · references · rename · completion · signatureHelp
                                          │
                     Workspace (state.rs): one jals-hir ProjectIndex per open jals.toml
                     project — its source files, [build] classpath, resolved
                     [dependencies] jars/sources, git/path source deps, and edition
```

## What it does today

Server capabilities advertised on `initialize`:

| LSP feature | Method | Source | Notes |
| --- | --- | --- | --- |
| Diagnostics | `textDocument/publishDiagnostics` | parser `SyntaxError`s + `jals-lint` + `jals-hir` | Pushed on open/change; cleared on close; `source: "jals"`. Merges parser errors (`ERROR` severity), the enabled `jals-lint` rules (severity from `jalslint.toml`; `unused-local` and a `constant-condition` dead branch fade via `DiagnosticTag::UNNECESSARY`, the latter as an extra HINT diagnostic over the dead range), and — for a file in an indexed `jals.toml` project — cross-file "cannot resolve symbol" and index-aware `type-mismatch` diagnostics (suppressing the file-local `type-mismatch` rule so the two never double-report). The project's `[package] edition` gates `compact-source-file`/`module-import`. |
| Document symbols | `textDocument/documentSymbol` | typed AST | Hierarchical: types → members (fields, methods, constructors, nested types, enum constants). |
| Semantic tokens | `textDocument/semanticTokens/full`, `full/delta` | CST + `jals-hir` | Whole-document, plus delta-encoded incremental updates against a cached `result_id` baseline (falls back to a full response when the baseline is stale/evicted). An identifier is classified by its resolved binding kind first — file-locally, and against the project index for a cross-file type when one is loaded — then falls back to a purely syntactic classification (keywords incl. contextual ones like `var`/`record`/`sealed`, literals, comments, annotations) for anything unresolved (external/JDK types, member-access right-hand names). The `range` variant is not implemented. |
| Code folding | `textDocument/foldingRange` | CST | Folds class/enum/module bodies, blocks (control-flow & lambdas included), switch blocks, array initializers, multi-line block/doc comments, and import groups. The closing brace stays visible; multi-line spans only. |
| Selection range | `textDocument/selectionRange` | CST | Expand/shrink: nests the token under each cursor up through its ancestor nodes to the file root. Syntax-only; multiple positions per request. |
| Occurrence highlight | `textDocument/documentHighlight` | CST + `jals-hir` | Semantic first: a cursor on a file-local binding highlights every occurrence of that binding only (respects shadowing/namespaces); a cross-file project type (when indexed) highlights every reference to it *in this file*. Falls back to a purely lexical same-spelled-`IDENT` match for anything else (external types, undeclared names). Declaration/binding names, simple-name assignment targets, and `++`/`--` operands are Write; everything else is Read. |
| Hover | `textDocument/hover` | `jals-hir` `TypeInference` | A Markdown code block with the inferred type of the expression under the cursor. Cross-file type resolution through the owning project's index; file-local inference otherwise. No hover for an un-inferable (`Unknown`) type. |
| Go to definition | `textDocument/definition` | `jals-hir` `Resolved` + `ProjectIndex` | A file-local binding, else the project type a reference names, else — for `receiver.field` / `receiver.method()` — the member resolved off the receiver's inferred type. For an indexed project, lands in a classpath type's real `-sources.jar` source (or a decompiled skeleton when the jar ships none) or a `git`/`path` source dependency's `.java`. File-local fallback for a document outside any indexed project. |
| Find references | `textDocument/references` | `jals-hir` `Resolved` + `ProjectIndex` | Project-wide for a project type (every reference across every workspace file, sorted by file then position); file-local for a local, parameter, field, method, or type parameter. `include_declaration` optional. File-local fallback outside an indexed project. |
| Rename | `textDocument/rename`, `textDocument/prepareRename` | `jals-hir` `Resolved` + `ProjectIndex` | Renames locals, parameters, type parameters, catch parameters, resources, pattern variables, and project types (class/interface/enum/record/annotation type) — project-wide for a type, file-wide for a file-scoped binding. Members (fields, methods, constructors, enum constants) and anything outside the project's own sources (a stdlib stub, a classpath `.class` type, a `git`/`path` source-dependency type) are withheld as not renamable. The new name is validated as a single legal Java identifier before any edit is produced. |
| Completion | `textDocument/completion` | `jals-hir` `ProjectIndex` | Triggered on `.`. A member access (`receiver.` or a partial `receiver.fo`) offers the receiver type's fields and methods; a bare identifier offers in-scope bindings, project type names, and the Java keywords. Cross-file through the project index; a single-file index otherwise. |
| Signature help | `textDocument/signatureHelp` | `jals-hir` `ProjectIndex` | Triggered on `(` and `,`. Shows the overloads of the call at the cursor with the active parameter's range. Cross-file through the project index; file-local otherwise. |
| Formatting | `textDocument/formatting` | `jals_fmt::format_source` | Whole-document: one full-range edit, or none if already formatted. |
| Config hot-reload | `workspace/didChangeWatchedFiles` | — | Dynamically registers `**/jalsfmt.toml` and `**/jalslint.toml` watchers via `client/registerCapability` (when the client supports dynamic registration); a change clears the affected config's discovery cache so the next request rediscovers. |
| Text sync | `didOpen` / `didChange` / `didClose` | — | Incremental sync (`TextDocumentSyncKind::INCREMENTAL`): change events are spliced in order (UTF-16 ranges → byte offsets); full-replacement events are still accepted. |
| Lifecycle | `initialize` / `shutdown` / `exit` | — | Managed by async-lsp's `LifecycleLayer`. |

Formatting and lint config are each discovered per document by searching upward for
`jalsfmt.toml` / `jalslint.toml` from the file's directory (memoized separately), matching the
`jals fmt` / `jals lint` CLIs. Non-`file:` URIs (e.g. `untitled:`) fall back to each config's
default. When the client supports file watching, edits to either file take effect without a
server restart.

Cross-file features (diagnostics beyond syntax + file-local lint, semantic tokens' cross-file
classification, hover, go-to-definition, find-references, rename, completion, and signature
help) run against a `Workspace`: one `jals-hir` `ProjectIndex` per `jals.toml` project, built
lazily the first time a file in that project is opened (walking up from the file to find its
manifest) and reused for every other file in the same project. It folds in the project's
`[build] classpath` `.class` files and resolved `[dependencies]` jars (via `jals-classpath`; the
`reqwest` download runs on a dedicated thread to stay off the Tokio runtime), each dependency's
extracted `sources` jar `.java` — or, when a jar ships none, a decompiled skeleton `.java` — as
read-only navigation targets, each `git`/`path` source dependency's `.java` as both an index
input (its types resolve for analysis) and a navigation target, and the project's `[package]
edition` (feeding the edition-gated lint rules). Library and source-dependency files are never
linted, and rename/find-references only ever rewrite the project's own sources. A file that
belongs to no `jals.toml` project falls back to file-local resolution for every one of these
features.

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
| `handlers/{diagnostics,symbols,semantic_tokens,folding_range,selection_range,document_highlight,hover,definition,references,rename,completion,signature_help,formatting}.rs` | Pure functions `(text [, config], &LineIndex) -> LSP payload` — the file-local core, used directly for a document outside any indexed project and as the shared LSP-payload mapping for the cross-file paths. No I/O, no async. |
| `line_index.rs` | Converts `jals-syntax` UTF-8 byte offsets to LSP UTF-16 `Position`s. |
| `file_id.rs` | `WorkspaceFileId`: the three disjoint `FileId` id-spaces (`Project`, `Library`, `SourceDep`) a `Workspace` addresses, and the one place that allocates/routes between them. |
| `state.rs` | `DocumentStore` (open documents, incremental text sync via the pure `apply_content_changes`) and memoized config `Discovery`; `Workspace` — one `jals-hir` `ProjectIndex` per `jals.toml` project — loads/rebuilds the index off the classpath and `[dependencies]`, and implements the cross-file hover/definition/references/rename/completion/signature-help/semantic-tokens/document-highlight logic. |
| `server.rs` | `LanguageServer` impl + advertised capabilities; glue that routes each request to the owning `Workspace` (falling back to the file-local handler) and manages the semantic-tokens delta cache. |
| `lib.rs` | `run()`: a current-thread tokio runtime driving the async-lsp `MainLoop` over stdio. |

The server runs single-threaded on a current-thread runtime, so document state needs no
locking (the one exception — resolving `[dependencies]` via `reqwest`'s blocking client — runs
on a dedicated `std::thread`, joined before the workspace is built). The `async-lsp` tower stack
supplies tracing, lifecycle, panic-catching, concurrency, and client-process-monitoring layers.

`jals-lsp` is **host-only** (it uses tokio and stdio); unlike `jals-syntax` / `jals-fmt` it is
not built for `wasm32`.

## Development

```sh
cargo test -p jals-lsp                                              # LineIndex + handler + Workspace tests
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

The core editor features (diagnostics, symbols, semantic tokens, folding, selection range,
occurrence highlight, formatting, hover, go-to-definition, find-references, rename, completion,
signature help) all ship today, cross-file-aware wherever a `jals.toml` project is open. What's
left groups into a few categories:

## Editor-feature gaps

| Capability | LSP method | Notes |
| --- | --- | --- |
| Range formatting | `textDocument/rangeFormatting` | Format a selection. Needs `jals-fmt` to format a sub-range (today it is whole-document only). |
| On-type formatting | `textDocument/onTypeFormatting` | Reformat on `}` / `;`. |
| Semantic tokens: range | `textDocument/semanticTokens/range` | The `full`/`full/delta` variants already ship — see above. |
| Document links (imports) | `textDocument/documentLink` | |
| Workspace symbols | `workspace/symbol` | |
| Code actions / quick fixes | `textDocument/codeAction` | e.g. a fix-it for a lint finding. |
| Go to declaration | `textDocument/declaration` | Distinct from `definition` (e.g. jump to an interface method vs. its implementation); `definition` covers the common case today. |
| Completion resolve | `completionItem/resolve` | Lazily fill in documentation/detail for a completion item. |

## Member rename / cross-file references

Renaming and find-references already reach across files for **project types**; a field, method,
constructor, or enum constant is still withheld from rename (see the table above) because there
is no cross-file *member* reference index yet — only the project's type-level index. Building
that index would let `rename`/`references`/`prepareRename` cover members too.

## Test coverage

| Item | Notes |
| --- | --- |
| Integration test | Drive async-lsp over an in-memory transport (initialize → didOpen → diagnostics → shutdown) for CI regression coverage beyond the current unit tests. |

## Operational / polish

| Capability | Notes |
| --- | --- |
| Windows transport | v1 uses unix `PipeStdin` / `PipeStdout`; add a `tokio-util` compat fallback for non-unix. |
| Server-side logging | structured tracing to **stderr** (stdout is the LSP transport — never log there). |
| Client configuration | accept `initializationOptions` / `workspace/didChangeConfiguration` (explicit config path, per-feature toggles). |
