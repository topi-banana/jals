# jals — Zed extension

A [Zed](https://zed.dev) extension that wires the **jals** language server into the editor for
Java files. It runs `jals lsp` (the stdio server from [`jals-lsp`](../../jals-lsp)) and gets you
diagnostics, go-to-definition, find-references, hover, completion, rename, document symbols,
formatting, folding, selection ranges, signature help, semantic highlighting, and document
highlight — everything the server advertises.

The extension is intentionally thin: it ships **no grammar** and only registers the `jals`
language server, attaching it to Zed's `Java` language.

## Prerequisites

1. **The `jals` binary on your `$PATH`.** Build it from this repo:

   ```sh
   cargo install --path jals-cli    # installs the `jals` binary
   # or, once a release is published: cargo binstall jals-cli
   ```

   Verify it is reachable: `jals lsp --stdio` should start and wait on stdin (Ctrl-C to quit).

2. **A Java grammar.** jals attaches to Zed's `Java` language but does not define it. Install the
   **Java** extension from Zed's extensions view (`cmd-shift-x` / `ctrl-shift-x` → search
   "Java") so Zed knows what a `.java` file is.

## Install (as a dev extension)

Zed builds extensions with `cargo` and the `wasm32-wasip1` target, so a `rustup`-managed Rust
toolchain is required (a Homebrew Rust will not work).

1. Open Zed → extensions view (`cmd-shift-x` / `ctrl-shift-x`).
2. Click **Install Dev Extension** and choose this directory (`editors/zed`).
3. Open a `.java` file inside a project. jals starts automatically.

Run `zed --foreground` to see the extension's build output and language-server logs if something
goes wrong.

## Configuration

The language server is keyed as `jals` in Zed settings. Because Zed's Java extension also brings
its own server (`jdtls`), tell Zed which one(s) to run for Java:

```jsonc
{
  "languages": {
    "Java": {
      // Use jals only. Drop "!jdtls" to run both side by side.
      "language_servers": ["jals", "!jdtls", "..."]
    }
  }
}
```

Everything about how the binary is launched can be overridden under `lsp.jals`:

```jsonc
{
  "lsp": {
    "jals": {
      "binary": {
        // Absolute path to the binary. Omit to use whatever `jals` is on $PATH.
        "path": "/home/me/.cargo/bin/jals",
        // Replaces the default ["lsp"] wholesale.
        "arguments": ["lsp"],
        // Replaces the inherited shell environment when set.
        "env": {}
      }
    }
  }
}
```

`initialization_options` and `settings` under `lsp.jals` are forwarded to the server verbatim.
Note that jals reads its formatter/linter/project configuration from the on-disk `jals.toml`,
`jalsfmt.toml`, and `jalslint.toml` files (discovered upward from each source file), so most
setups need no `lsp.jals` settings at all.

## Build / check locally

This crate is its own Cargo workspace (detached from the outer `jals` workspace), so build it
from this directory:

```sh
cargo build --release --target wasm32-wasip1
```

Zed performs this build for you when you install or reload the dev extension.

## Prebuilt binary (CI artifact)

The `zed-extension` job in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) builds the
extension on every push and pull request and uploads a **`zed-jals-extension`** build artifact.

`cargo build` emits a wasm *core module*, but Zed loads a wasm *component*, so CI adapts the module
into a component (`wasm-tools component new`, using the same wasi-preview1 reactor adapter Zed's
wasmtime tracks). The artifact therefore contains the compiled, Zed-loadable extension:

```
extension.wasm     # the wasm component
extension.toml     # the extension manifest
```

Download it from the run's **Artifacts** section on the GitHub Actions page.
