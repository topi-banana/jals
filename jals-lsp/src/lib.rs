//! Language Server Protocol implementation for jals.
//!
//! A thin protocol adapter over the `jals-editor` crate, which owns the editor workspace and
//! every semantic query in protocol-neutral shapes. This crate keeps only the LSP specifics:
//! the stdio server loop (`server`), URI ↔ path mapping and the open-document store (`state`),
//! the `lsp_types` rendering of each neutral payload (`host`), and formatting (`formatting`).
//!
//! Host-only crate: depends on `tokio`/`async-lsp` and uses stdio, so it is not built
//! for `wasm32` (same exemption as `jals-cli`). The analysis engines it drives
//! (`jals-editor` and everything beneath it) remain wasm-compatible.

// Every offset here lives in `jals-syntax`'s `u32` (`TextSize`) address space and every file index is
// a `jals-hir` `FileId` (`u32`) — a source document never approaches 4 GiB and a project never 2³²
// files — so the `usize`/`u32` conversions throughout the crate cannot truncate in practice.
#![allow(clippy::cast_possible_truncation)]

mod formatting;
mod host;
mod server;
mod state;

pub use server::Server;
