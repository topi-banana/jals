//! Internal re-exports of the formatter configuration, which now lives in the shared `jals-config`
//! crate ([`jals_config::fmt`]).
//!
//! The formatter's public entry points ([`crate::format_source`]) take a `jals_config::fmt::Config`
//! directly, and consumers import the config types from `jals-config`. This module is a private,
//! crate-internal alias so the formatter's own modules (`lower`, `render`, `rules`) keep referring to
//! `crate::config::*` unchanged. The rendering helpers the formatter needs (`indent_unit`,
//! `continuation_cols`, `newline`, …) are `pub` methods on `jals_config::fmt::Config`.

pub use jals_config::fmt::{
    AnnotationPlacement, BinopLayout, BinopSeparator, BraceStyle, ClosingParen, Config,
    ControlBraceStyle, FloatLiteralTrailingZero, FnParamsLayout, HexLiteralCase, IndentStyle,
    LiteralSuffixCase, SwitchCaseBody, TrailingComma, TypePunctuationDensity,
};
