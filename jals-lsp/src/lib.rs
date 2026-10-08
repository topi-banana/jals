//! Language Server Protocol implementation for jals.
//!
//! A thin protocol adapter over the `jals-editor` crate, which owns the editor workspace and
//! every semantic query in protocol-neutral shapes. This crate keeps only the LSP specifics:
//! the stdio server loop and the `Send` router frontend (`server`), the single-owner language
//! service actor that holds all `!Send` analysis state (`actor`), URI ↔ path mapping and the
//! open-document store (`state`), the `lsp_types` rendering of each neutral payload (`host`),
//! and formatting (`formatting`).
//!
//! Host-only crate: depends on `tokio`/`async-lsp` and uses stdio, so it is not built
//! for `wasm32` (same exemption as `jals-cli`). The analysis engines it drives
//! (`jals-editor` and everything beneath it) remain wasm-compatible.

// Every offset here lives in `jals-syntax`'s `u32` (`TextSize`) address space and every file index is
// a `jals-hir` `FileId` (`u32`) — a source document never approaches 4 GiB and a project never 2³²
// files — so the `usize`/`u32` conversions throughout the crate cannot truncate in practice.
#![allow(clippy::cast_possible_truncation)]

mod actor;
mod formatting;
mod host;
mod natives;
mod server;
mod state;

pub use server::Server;

pub(crate) use detail::toolchain_exclusion;

/// The toolchain-store exclusion, grouped per the repository's no-free-functions layout;
/// re-exported at the crate root.
mod detail {
    /// The project-local toolchain store, as a native-snapshot exclusion.
    ///
    /// `target/jdk` holds `[toolchain]` *inputs*, not project bytes: capturing it would read hundreds
    /// of megabytes per workspace assembly, and a `link`ed entry is a symlink out of the root that a
    /// root-wide scope would diagnose every time. The CLI applies the same exclusion to its
    /// root-scoped snapshots (`App::toolchain_exclusion`); both name the directory through
    /// [`jals_config::MANAGED_TOOLCHAIN_ROOT`].
    pub(crate) fn toolchain_exclusion() -> jals_storage::RelativePath {
        jals_storage::RelativePath::parse(jals_config::MANAGED_TOOLCHAIN_ROOT)
            .expect("the toolchain root is a portable path")
    }
}
