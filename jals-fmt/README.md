# jals-fmt

A formatter for JALS/Java source, driven by the `jals-syntax` CST. Its single public entry point is

```rust
FormatOutput::format_source(src: &str, config: &Config, features: FeatureSet) -> FormatOutput
```

`features` is the project's `[package] features`, or `FeatureSet::default()` for a source formatted
outside any project. Exactly one rule reads it — `[imports] granularity = "package"` writes jals
dialect syntax, which does not compile unless `grouped-imports` is on — and the formatter rounds
that rule away with a `Warning` rather than guessing.

## One engine

There is exactly **one layout engine**: a port of google-java-format's greedy, single-pass
`computeBreaks` over a GJF-shaped `Doc` IR. Every style target — google-java-format, Eclipse JDT,
IntelliJ IDEA, palantir-java-format — is reached by tuning `jals_config::fmt::Config` on top of
that engine, never by swapping engines. The four products really do have four mutually
incompatible resolution algorithms; porting all of them was considered and rejected, and
[`DESIGN.md`](DESIGN.md) §11 and §18 record both the decision and the differences it makes
permanent. **Do not add an engine trait, a second renderer, or a Wadler/prettier `fits`.**

The pipeline is five layers — L0 token passes, L2 CST→`Doc` lowering, L1 resolution, L3 comment
and Javadoc reflow, L4 text passes — sequenced in one place (`passes::Formatter`).

## The rule set

**196 rules in 8 sections** (`[layout]`, `[blank-lines]`, `[braces]`, `[wrapping]`, `[spacing]`,
`[comments]`, `[imports]`, `[literals]`), each documented in its own module under
`jals-config/src/fmt/`. `jals-fmt/jalsfmt.toml` lists the frequently-touched keys with their
defaults.

That the rules are *implemented*, and not merely declared, is a test rather than a claim:
`tests/coverage.rs` walks the serde schema, moves every leaf off its default in turn, and requires
the formatter to notice — so a key that reaches no emission site fails the build, by name.

## Invariants

- **Never panics, never loses input.** A node with no bespoke rule still emits all of its tokens;
  an `ERROR` node is emitted verbatim. If the output fails the fail-safe's check, the input is
  returned unchanged and `FormatOutput::fell_back` says so.
- **Idempotent.** `format(format(x)) == format(x)`.
- **Significant tokens are preserved as a multiset**, except where an operation declared in
  `passes::token_license::OPERATIONS` applies — `DESIGN.md` §20's table as data, which the
  fail-safe reads instead of reconstructing allowances from config fields.
- **Comments are never dropped.** Each is anchored to exactly one token and emitted with it.
- **Layout never reads input whitespace**, with one exception: whether two significant tokens had
  a blank line between them. A rule that would read more is rounded and the rounding is reported
  as a `Warning` (`DESIGN.md` §17).

These are checked against a corpus in `src/invariants.rs` rather than asserted in prose.

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

**[`MAPPING-rustfmt.md`](MAPPING-rustfmt.md)** is the same ledger for rustfmt: all 90 of its
options, which jals rule each becomes, and — for the ones that become none — whether they are
Rust-specific, deliberately declined, or not formatter rules at all. It is a separate file because
rustfmt is not a *reachable target* in MAPPING.md §2's sense: it does not format Java, so there is
no output to compare against, and a rustfmt column in that table would quietly deny the criterion
the table is built on.

The `.prefs` / `.editorconfig` readers are pure `&str` parsers and portable (`no_std` / wasm). The
two XML readers use `quick-xml` and live behind the crate's **`std` feature**, which is not part of
the default (wasm) build — `jals-cli` enables it, so `jals` reaches all four.

## Config generation (`jals_fmt::generate`)

The other direction: [`Provenance::jalsfmt_toml`] renders a `jals_config::fmt::Config` back out
as the file jals discovers. Together with `import` it is `DESIGN.md` §15's *jalsfmt.toml 自動生成* —
`jals-cli` finds a native config, `import` projects it, `generate` writes it.

**Only the keys that differ from `Config::default()` are written** (§15 P-gen-6), under a header
recording the source file and the version it declared. Rather than one hand-written comparison per rule,
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
