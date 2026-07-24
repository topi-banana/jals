#![cfg_attr(not(test), no_std)]
//! **WIP — the formatter is being rewritten from scratch.**
//!
//! The entire previous implementation (CST lowering, the `Doc` IR, rendering, comment
//! attachment, and every configurable rule) has been removed. This crate is currently a
//! no-op skeleton: it keeps the public entry point so its consumers keep compiling, but it
//! performs **no formatting** at all.
//!
//! [`FormatOutput::format_source`] returns the input source **byte-for-byte unchanged**. It
//! still parses the source so that syntax errors continue to surface as [`Warning`]s, but no
//! layout, spacing, or normalization is applied. Configuration is accepted and ignored.
//!
//! The real implementation will be rebuilt here from the ground up.

extern crate alloc;

mod output;

use alloc::borrow::ToOwned;

use jals_config::fmt::Config;

pub use output::{FormatOutput, Warning};

impl FormatOutput {
    /// Format `src` according to `config`.
    ///
    /// **WIP no-op:** returns `src` unchanged, ignoring `config`. Only the parser's syntax
    /// errors are surfaced as [`Warning`]s.
    pub async fn format_source(src: &str, _config: &Config) -> Self {
        let parse = jals_syntax::Parse::parse(src).await;
        let warnings = parse
            .errors()
            .iter()
            .map(Warning::from_syntax_error)
            .collect();
        Self {
            formatted: src.to_owned(),
            warnings,
        }
    }
}
