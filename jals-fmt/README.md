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

## Config importers (`jals_fmt::import`)

Ahead of the formatter itself, the `import` module lowers a **native Java-formatter config** into
a jals [`jals_config::fmt::Config`]. Each importer is a serde-`Deserialize` model of the native
config plus an `impl From<Model> for Config`:

- [`GoogleJavaFormatConfig`] / [`PalantirJavaFormatConfig`] — minimal structs (these tools are
  deliberately non-configurable; the whole surface is a style flag, plus Palantir's `formatJavadoc`).
- [`EclipseConfig`] — the `.settings/org.eclipse.jdt.core.prefs` (Java properties) and exported
  XML profile forms, sharing one model over the `org.eclipse.jdt.core.formatter.*` id namespace.
- [`IntellijConfig`] — the `.editorconfig` (`ij_java_*`) and code-style XML scheme forms; the XML
  reader normalizes IntelliJ's raw integer enums to editorconfig tokens so both feed one model.
- [`SpotlessConfig`] — a thin orchestrator that resolves a delegate engine (GJF / Palantir /
  Eclipse) plus a few generic steps; it owns no layout of its own.

The projection is intentionally **lossy** — native surfaces range from *nothing* to ~400 (Eclipse)
/ ~270 (IntelliJ) options, while jals exposes one common-rule set — so only the subset with a jals
equivalent is modeled (a full bijection is impossible; see `DESIGN.md` §11 / §15). Native values
are kept **typed** (enums, a bitmask newtype for Eclipse's `alignment_for_*`, an ordered import
list) rather than stringly-typed, so the map onto jals options stays injective on that subset.

The `.prefs` / `.editorconfig` readers are pure `&str` parsers and portable (`no_std` / wasm). The
two XML readers use `quick-xml` and live behind the crate's **`std` feature**, which is not part of
the default (wasm) build.
