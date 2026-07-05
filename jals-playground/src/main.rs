//! `jals-playground`: a browser playground for the `jals` formatter and workspace, built on the
//! Monaco Editor.
//!
//! A sidebar on the left holds the editable `jals.toml` / `jalsfmt.toml` configuration files over an
//! in-memory workspace of Java files; pick one to edit it in the center pane — a Monaco editor with
//! Java syntax highlighting. Diagnostics (syntax errors, lint findings, cross-file type mismatches
//! and unresolved types) are recomputed as you type and shown inline as Monaco markers. Editing
//! `jalsfmt.toml` updates the `jals-fmt` [`Config`] used by the top-right *Format* button (and
//! Monaco's *Format Document* action, which rewrite the buffer in place), and editing `jals.toml`
//! re-resolves its `[dependencies]` (downloaded through the header's CORS proxy). *Syntax tree*
//! dumps the lossless CST into the right pane. Everything runs in the browser via `wasm32`; there is
//! no server round-trip.
//!
//! The UI is split by responsibility into struct-based Yew components (see [`components`]), all
//! wired together by the stateful root [`app::App`]:
//! - [`app`] — the root component; owns the workspace, config buffers, and syntax dump.
//! - [`components`] — the presentational pieces (header, file tree, editor, syntax).
//! - [`monaco`] — the typed Rust bridge to the single imperative Monaco editor instance.
//! - [`workspace`] — the wasm-compatible in-memory multi-file workspace + analysis.
//! - [`providers`] — wires each Monaco language-feature provider to the workspace analysis.
//! - [`line_index`] — byte-offset → Monaco (UTF-16) position mapping for diagnostics.
//!
//! [`Config`]: jals_config::fmt::Config

mod app;
mod components;
mod fetcher;
mod line_index;
mod monaco;
mod providers;
mod workspace;

use app::App;

fn main() {
    yew::Renderer::<App>::new().render();
}
