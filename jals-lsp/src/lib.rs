//! Language Server Protocol implementation for jals.
//!
//! Host-only crate: depends on `tokio`/`async-lsp` and uses stdio, so it is not built
//! for `wasm32` (same exemption as `jals-cli`). The parsing and formatting engines it
//! drives (`jals-syntax`, `jals-fmt`) remain wasm-compatible.

mod handlers;
mod line_index;
mod server;
mod state;

/// Run the language server over stdio. Blocks until the client disconnects.
pub fn run() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    runtime.block_on(server::run_server())
}
