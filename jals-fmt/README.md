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
a jals [`jals_config::fmt::Config`]. Each importer is a `Deserialize` model of the native config
plus an `impl From<Model> for Config` — and the two have deliberately different completeness
criteria.

**The models are total.** Every option the vendor has is modeled, with none missing:

| importer | surface | forms |
|---|---|---|
| [`EclipseConfig`] | **416** ids (401 live + 15 deprecated-but-still-read) | `.settings/org.eclipse.jdt.core.prefs`, exported XML profile |
| [`IntellijConfig`] | **297** settings | `.editorconfig` (`ij_java_*`), `.idea/codeStyles/Project.xml`, exported scheme |
| [`GoogleJavaFormatConfig`] | 6 (`JavaFormatterOptions` + the two import-pass flags) | none — non-configurable by design |
| [`PalantirJavaFormatConfig`] | 2 (`Style`, `formatJavadoc`) | none — likewise |
| [`SpotlessConfig`] | delegate + every Java-applicable step | build DSL (modeled *resolved*, not parsed) |

The Eclipse and IntelliJ numbers are not estimates: each module ships an `inventory.tsv`
machine-extracted from the product's own sources, and a **coverage test that fails when a listed
option is not captured by the model**. Native values stay typed — an enum is a Rust enum,
`alignment_for_*` is a bitmask newtype, an import layout is an ordered list — so two distinct
native values can never collapse before the projection decides.

**The projection is partial**, because `jals_config::fmt::Config` is a curated common vocabulary
rather than the union of four surfaces (a full bijection is impossible; `DESIGN.md` §11 / §15).
An option with no jals equivalent — Eclipse's column-alignment settings, IntelliJ's naming
conventions, the classpath-dependent import-on-demand thresholds — is still modeled, named, and
typed; it simply is not carried across. `jals-fmt` has **one** layout engine and approximates the
other formatters by tuning rules, so an unprojected option is not an engine option in waiting: it
is the typed record of a divergence the design accepts and enumerates (`DESIGN.md` §18.2).

**[`MAPPING.md`](MAPPING.md) is the ledger**: the vendor inventories, the criterion for what
earns a jals rule, the per-rule correspondence table, and the explicit list of what is
deliberately not projected.

The `.prefs` / `.editorconfig` readers are pure `&str` parsers and portable (`no_std` / wasm). The
two XML readers use `quick-xml` and live behind the crate's **`std` feature**, which is not part of
the default (wasm) build — `jals-cli` enables it, so `jals` reaches all four.

## Config generation (`jals_fmt::generate`)

The other direction: [`Provenance::jalsfmt_toml`] renders a `jals_config::fmt::Config` back out
as the file jals discovers. Together with `import` it is `DESIGN.md` §15's *jalsfmt.toml 自動生成* —
`jals-cli` finds a native config, `import` projects it, `generate` writes it.

**Only the keys that differ from `Config::default()` are written** (§15 P-gen-6), under a header
recording the source file and the version it declared. Rather than 174 hand-written comparisons,
the config is diffed against its default through `serde_json::Value`: `Config` is exactly two
levels deep — eight section tables of leaves, no scalar at the root — which makes the diff short,
automatically correct when a section gains a key, and free of TOML's "scalars before sub-tables"
constraint. That shape is asserted by a test, not assumed.

[`MigrationWarning::rounding`] is the companion: the §17 rows whose value reads the input's line
breaks, which the single engine rounds to a canonical value. It is a pure function of the config —
§17 puts the rounding in the *engine*, so the projection stays lossless — and reports only rows
that differ from the default, which is exactly the set `jalsfmt_toml` writes.

**Finding the file is not this crate's job.** `jals-fmt` is portable and has no filesystem access;
the detection ladder (`DESIGN.md` A.1) lives in `jals-cli/src/migrate.rs`, which reads bytes
through a `jals-storage` `ProjectView` (`DESIGN.md` §19).
