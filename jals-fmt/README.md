# jals-fmt

> **⚠️ WIP — under a from-scratch rewrite.**
>
> The entire previous formatter implementation (CST lowering, the `Doc` IR, rendering, comment
> attachment, and every configurable rule) has been **removed**. This crate is currently a
> **no-op skeleton**.

## Status

`jals-fmt` performs **no formatting** right now. Its single public entry point,
`FormatOutput::format_source`, is preserved so downstream crates (`jals-cli`, `jals-lsp`,
`jals-playground`, …) keep compiling, but it returns the input source **byte-for-byte
unchanged**.

- It still parses the source, so parser syntax errors continue to surface as `Warning`s.
- A formatter `Config` (from `jals-config`) is accepted and **ignored**.
- No layout, spacing, or literal normalization is applied.

The real implementation is being rebuilt here from the ground up. This README, and the design
notes / configuration reference that used to live here, will return alongside the new
implementation.
