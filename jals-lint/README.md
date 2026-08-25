# jals-lint

A linter for JALS/Java source, driven by the `jals-syntax` CST and `jals-hir`'s semantic layer. Its
public entry points are

```rust
LintOutput::lint(request: LintRequest, config: &Config) -> LintOutput
LintOutput::lint_source(src: &str, config: &Config) -> LintOutput   // the file-local shorthand
RuleInfo::all() -> impl Iterator<Item = RuleInfo>                   // the rule registry, as data
```

**This crate produces every semantic diagnostic.** An unresolvable type name is the
`cannot-resolve` rule here, not a separate pass in a consumer — so every diagnostic a host shows has
a rule name, a `jalslint.toml` key, and a configurable level. The parser's own errors are the one
exception: they belong to the parse, and a caller reads them from `Parse::errors`.

## The rule set

**20 rules in 10 sections.** A section is a **defect class** — what kind of thing the rule found —
and it is the `jalslint.toml` table the rule is configured under. Every rule is in exactly one.

| rule | section | default | what it reports |
|---|---|---|---|
| `cannot-resolve` | `[correctness]` | `error` | a name the project index does not define |
| `type-mismatch` | `[correctness]` | `warn` | a value written into a slot its type cannot inhabit |
| `unreported-exception` | `[correctness]` | `warn` | a checked exception no `catch` or `throws` admits |
| `compact-source-file` | `[compatibility]` | `error` | a top-level `main` / member without the feature (JEP 512) |
| `module-import` | `[compatibility]` | `error` | `import module …;` without the feature (JEP 511) |
| `grouped-import` | `[compatibility]` | `error` | `import a.{B, C};` without the jals dialect feature |
| `attribute` | `[compatibility]` | `error` | `#[cfg(…)]` without the jals dialect feature |
| `constant-condition` | `[suspicious]` | `warn` | an `if` whose condition folds, so a branch is dead |
| `empty-catch` | `[suspicious]` | `warn` | a `catch` that swallows its exception with no stated reason |
| `unused-variables` | `[unused]` | `warn` | a binding the file scopes and never uses |
| `unused-imports` | `[unused]` | `warn` | an `import` no name resolves through |
| `dead-code` | `[unused]` | `warn` | a `private` member the declaring file never uses |
| `collapsible-if` | `[complexity]` | `warn` | an `if` whose whole body is another `if` |
| `boxed-primitive-constructor` | `[performance]` | `warn` | `new Integer(1)` where `Integer.valueOf(1)` caches |
| `wildcard-import` | `[style]` | `warn` | `import java.util.*;`, including a grouped member |
| `missing-braces` | `[style]` | `warn` | a control-flow body written as a bare statement |
| `naming-convention` | `[naming]` | `warn` | a declaration against the project's casing table |
| `empty-javadoc` | `[documentation]` | `warn` | a `/** … */` whose content is only whitespace |
| `print-to-console` | `[restriction]` | `allow` | a call on `System.out` / `System.err` |
| `implicit-this` | `[restriction]` | `warn` | an instance field named without the `this.` qualifier |

There is one diagnostic outside the table: `cfg`, a structurally malformed `#[cfg(…)]`. It is fixed
at `error` and is not configurable, because it is the same failure the compile frontend rejects a
build with, not a judgement about the code.

That the rules are *implemented*, and not merely declared, is a test rather than a claim:
`tests/registry.rs` joins the registry against the serialized schema in both directions, pins the
default level set, and — the sweep — moves every **option** in the schema off its default in turn and
requires the linter to notice. An option that reaches no rule fails the build, by name. It is
`jals-fmt/tests/coverage.rs`'s property, for the linter.

## Configuration

`jalslint.toml`, discovered upward from each reported file (so one run can span directories with
different configs). The schema is `jals_config::lint`, one module per section;
[`jalslint.toml`](jalslint.toml) here lists every key with its built-in value.

A rule's value is its **level** — `"allow"` / `"warn"` / `"error"` — or a **table** of that level and
the rule's own options:

```toml
[style]
wildcard-import = "error"                                  # level only
missing-braces = { level = "warn", policy = "multi-line" } # level + options

[naming.naming-convention]                                 # the same, spelled long
fields = "any"                                             # …and `level` may be omitted
```

Three properties are worth stating, because each replaced something worse:

- **A rule's built-in level lives in exactly one place** — the `Default` impl of the section that
  declares it. `RuleMeta` carries an accessor into the schema rather than a copy of the level, so
  the two cannot drift.
- **A table may omit `level`.** Deserialization is a *patch* applied onto the default, not a fresh
  value, so a config that sets one option keeps a built-in it did not choose. Making `level`
  mandatory would have copied every default into every config file, where it would go stale.
- **An unknown key is kept and named.** Rejecting the file would let one stale name silence every
  *other* rule it configures; ignoring it silently would make a key the file plainly writes do
  nothing. So the run loads, the good keys apply, and `jals lint` prints
  `warning: <file>: unknown lint key <key>` for the rest. *Keeping* is a property of the schema and
  holds for every host; **reporting is `jals lint`'s only** so far — the language server and the
  playground read `Config::unknown_keys` nowhere yet, so a stale key is still silent there. Wiring
  those two is follow-up work, not a property this schema fails to provide.

### In-source suppression: `@SuppressWarnings`

A `jalslint.toml` levels a rule for a whole directory tree. `@SuppressWarnings` levels it for one
declaration, which is the granularity the one wrong call site needs.

```java
@SuppressWarnings("unused-variables")          // one rule
@SuppressWarnings("unused")                    // a whole section
@SuppressWarnings({"unused", "complexity"})    // several
@SuppressWarnings(value = "all")               // every rule; javac's spelling
```

A name is a **rule name**, a **section name**, or `all` — and all three are read off the registry
rather than a list kept beside it, so a rule or a section added later is suppressible the day it
lands. That is also where this gets its Java compatibility for free: javac's
`@SuppressWarnings("unused")` and jals's `[unused]` section are the same word, so the annotation a
codebase already carries silences all three `[unused]` rules without being rewritten.

A suppression covers **what its declaration contains** — the whole significant span, the annotation
included — so one on a type reaches a finding several levels down. Nesting needs no innermost-wins
rule: `@SuppressWarnings` has no negative form, so there is nothing an inner one could take back.

A name none of the three vocabularies knows is **ignored silently**. `"unchecked"`, `"rawtypes"`,
`"serial"`, an IntelliJ inspection id — a real Java corpus is full of names addressed to other tools,
and JLS §9.6.4.5 leaves an unrecognized one to the compiler's discretion.

Three things it deliberately does not do:

- **`unused-imports` cannot be suppressed in source.** Java does not allow `@SuppressWarnings` on an
  import declaration, and imports sit outside the type declaration that could carry one. The
  `jalslint.toml` key is the answer; a file-level jals syntax would be a second suppression language.
- **The match is syntactic**, on the annotation's last segment, so a user-defined
  `com.acme.SuppressWarnings` matches too. Resolving the annotation type would make the suppression
  map depend on the file's analysis — which the engine computes lazily, *after* rules start running.
- **The `cfg` diagnostic is out of reach.** Not by a rule-name test but by construction: it is added
  after suppression has already been applied, because it is the failure the compile frontend rejects
  a build with rather than a judgement about the code.

### Options are values, never absent keys

Where a rule has a choice, it is an enum with every reachable state named, and no unreachable one.
`Case::Any` is how a naming kind is exempted — not an absent key, which would have had two states a
reader cannot tell apart. `ConsoleStreams` is one key with three values where clippy has two lints a
config can enable in any combination, including the combination neither of them names.

### There is no level ladder below `allow` or above `error`

rustc has four levels; jals has three. `deny` and `forbid` differ from each other only in whether an
**in-source** suppression may override them — which jals now has, so the distinction is at last
*expressible*. It still does not belong on this ladder: whether a diagnostic can be suppressed is a
different axis from how loudly it speaks, and folding the two into one would tie the only value
carrying the first to the loudest value of the second, leaving no way to write an unsuppressible
warning or a suppressible error.

Exactly one diagnostic must not be suppressible today — the `cfg` one above — and it is out of reach
structurally, by living outside the rule table, not by sitting at a level. `forbid` is the open
question a second such diagnostic would reopen.

## Roadmap: the rustc / clippy port

The goal is to implement every rustc and clippy lint that is not Rust-specific. The complete
ledger — all **1,059** lints of `rustc 1.97.1` / `clippy 0.1.97`, each placed in one bucket — is
[`MAPPING-rustc-clippy.md`](MAPPING-rustc-clippy.md), with the raw tables in
[`inventory-rustc.tsv`](inventory-rustc.tsv) and [`inventory-clippy.tsv`](inventory-clippy.tsv).
`tests/inventory.rs` keeps them and this crate's registry in step.

| bucket | meaning | count |
|---|---|---|
| **M** | maps onto a rule implemented today | 16 |
| **N** | portable to Java, **not yet implemented** | 376 |
| **R** | Rust-specific by naming a construct Java has no analogue of | 582 |
| **X** | Rust-specific at the *mechanism* level (editions, ABI, const-eval, toolchain) | 32 |
| **D** | portable in principle, deliberately not adopted (each row carries its reason) | 36 |
| **C** | not a source lint (lint machinery, driver output, `Cargo.toml`) | 17 |
| | **total** | **1,059** |

The 376 **N** rows collapse to **286 jals rules**, because several source lints often answer one
Java question — clippy's `shadow_same` / `shadow_reuse` / `shadow_unrelated` are one `shadowed-name`
with a `kinds` key, and its `print_stdout` / `print_stderr` are the one `print-to-console` that is
already implemented.

Eleven of today's twenty rules have **no** rustc or clippy ancestor and are jals's own: the four
`[compatibility]` feature gates (no other tool has this dialect to gate), the three
`[correctness]` rules that are Java semantics rustc has no analogue for, and `constant-condition`,
`empty-catch`, `missing-braces` and `implicit-this` — the last because Rust has no implicit
receiver to leave out.

### Prerequisite: in-source suppression — **done**

This was the first item on the roadmap, ahead of any individual rule, because a large part of the
`N` bucket is only usable with it: every rule whose false positives a project must silence one site
at a time, plus the two rows that are *about* suppression (`allow-attributes-without-reason`,
`unfulfilled-lint-expectations`). Java spells it `@SuppressWarnings`, and jals reads it — see
[In-source suppression](#in-source-suppression-suppresswarnings) above for the vocabulary and the
two limits it keeps.

The two rows that are about suppression stay in `N`, as rules rather than as parts of the mechanism.
`unfulfilled-suppression` in particular needs a decision this mechanism has not made: rustc's
`unfulfilled_lint_expectations` fires for `#[expect]` and never for `#[allow]`, Java has no `expect`,
and reporting a `@SuppressWarnings` that suppressed nothing would strengthen allow-semantics into
expect-semantics.

### The 286 planned rules, by section

### `[correctness]` — 28 rules

| jals rule | ported from |
|---|---|
| `absurd-comparison` | `clippy::absurd-extreme-comparisons`, `clippy::invalid-upcast-comparisons` |
| `almost-swapped` | `clippy::almost-swapped` |
| `bad-bit-mask` | `clippy::bad-bit-mask`, `clippy::ineffective-bit-mask` |
| `bidirectional-text` | `rustc::text-direction-codepoint-in-comment`, `rustc::text-direction-codepoint-in-literal` |
| `case-mismatch-switch` | `clippy::match-str-case-mismatch` |
| `code-point-index-confusion` | `clippy::char-indices-as-byte-indices` |
| `constant-clamp` | `clippy::min-max` |
| `constant-overflow` | `rustc::arithmetic-overflow` |
| `duplicate-if-condition` | `clippy::ifs-same-cond`, `clippy::same-functions-in-if-condition` |
| `equals-hashcode-pair` | `clippy::derived-hash-with-manual-eq` |
| `erasing-operation` | `clippy::erasing-op` |
| `identical-operands` | `clippy::eq-op` |
| `impossible-comparison` | `clippy::impossible-comparisons` |
| `infinite-while` | `clippy::while-immutable-condition` |
| `invalid-regex` | `clippy::invalid-regex` |
| `manual-midpoint` | `clippy::manual-midpoint` |
| `modulo-one` | `clippy::modulo-one` |
| `nan-comparison` | `rustc::invalid-nan-comparisons` |
| `never-loop` | `clippy::never-loop` |
| `null-argument` | `rustc::invalid-null-arguments` |
| `possible-missing-comma` | `clippy::possible-missing-comma` |
| `read-into-empty-array` | `clippy::read-zero-byte-vec` |
| `reversed-range` | `clippy::reversed-empty-ranges` |
| `self-assignment` | `clippy::self-assignment` |
| `suspicious-unicode-literal` | `clippy::invisible-characters`, `clippy::unicode-not-nfc` |
| `unconditional-exception` | `clippy::out-of-bounds-indexing`, `rustc::unconditional-panic` |
| `unconditional-recursion` | `clippy::main-recursion`, `clippy::recursive-format-impl`, `clippy::unconditional-recursion`, `rustc::unconditional-recursion` |
| `unused-io-amount` | `clippy::unused-io-amount` |

### `[compatibility]` — 5 rules

| jals rule | ported from |
|---|---|
| `deprecated-api` | `rustc::deprecated` |
| `deprecated-for-removal` | `rustc::deprecated-in-future` |
| `legacy-api` | `clippy::legacy-numeric-constants` |
| `restricted-identifier` | `rustc::keyword-idents-2018`, `rustc::keyword-idents-2024` |
| `unavailable-api` | `clippy::incompatible-msrv` |

### `[suspicious]` — 56 rules

| jals rule | ported from |
|---|---|
| `absolute-path-resolve` | `clippy::join-absolute-paths`, `clippy::path-buf-push-overwrite` |
| `ambiguous-wildcard-import` | `rustc::ambiguous-glob-imported-traits`, `rustc::ambiguous-glob-imports` |
| `approximate-constant` | `clippy::approx-constant` |
| `assert-with-side-effect` | `clippy::debug-assert-with-mut-call` |
| `builtin-type-shadow` | `clippy::builtin-type-shadow` |
| `case-sensitive-extension-check` | `clippy::case-sensitive-file-extension-comparisons` |
| `char-cast-truncation` | `clippy::char-lit-as-u8` |
| `command-arg-space` | `clippy::suspicious-command-arg-space` |
| `confusable-identifier` | `rustc::confusable-idents`, `rustc::mixed-script-confusables`, `rustc::uncommon-codepoints` |
| `constant-assertion` | `clippy::assertions-on-constants` |
| `constant-condition` **(implemented; this row is an extension)** | `clippy::const-is-empty` |
| `duplicate-annotation` | `clippy::duplicated-attributes` |
| `empty-enum` | `clippy::empty-enums` |
| `empty-loop` | `clippy::empty-loop` |
| `excessive-float-precision` | `clippy::excessive-precision`, `clippy::lossy-float-literal` |
| `float-equality` | `clippy::float-cmp`, `clippy::float-cmp-const`, `clippy::float-equality-without-abs` |
| `identical-branches` | `clippy::if-same-then-else` |
| `identical-switch-arms` | `clippy::match-same-arms` |
| `inaccessible-type-in-signature` | `rustc::exported-private-dependencies`, `rustc::private-bounds`, `rustc::private-interfaces`, `rustc::unnameable-types` |
| `inconsistent-compare-to` | `clippy::derive-ord-xor-partial-ord`, `clippy::non-canonical-partial-ord-impl` |
| `irrefutable-pattern` | `rustc::irrefutable-let-patterns` |
| `iterator-method-not-returning-iterator` | `clippy::iter-not-returning-iterator` |
| `literal-with-format-args` | `clippy::literal-string-with-formatting-args` |
| `misnamed-getter` | `clippy::misnamed-getters` |
| `misrefactored-assign-op` | `clippy::misrefactored-assign-op` |
| `mistyped-literal-suffix` | `clippy::mistyped-literal-suffixes` |
| `mixed-read-write` | `clippy::mixed-read-write-in-expression` |
| `mutable-constant` | `clippy::borrow-interior-mutable-const`, `clippy::declare-interior-mutable-const`, `rustc::const-item-interior-mutations` |
| `mutable-map-key` | `clippy::mutable-key-type` |
| `nan-cast` | `clippy::cast-nan-to-int` |
| `narrowing-cast` | `clippy::cast-possible-truncation`, `clippy::cast-precision-loss` |
| `negated-float-comparison` | `clippy::neg-cmp-op-on-partial-ord` |
| `no-effect-statement` | `clippy::no-effect`, `clippy::no-effect-underscore-binding`, `clippy::unnecessary-operation` |
| `non-exhaustive-switch` | `rustc::non-exhaustive-omitted-patterns` |
| `octal-escape` | `clippy::octal-escapes` |
| `octal-literal` | `clippy::zero-prefixed-literal` |
| `path-extension-check` | `clippy::path-ends-with-ext` |
| `possible-missing-else` | `clippy::possible-missing-else` |
| `print-in-to-string` | `clippy::print-in-format-impl` |
| `reference-equality` | `clippy::ptr-eq` |
| `same-item-added-in-loop` | `clippy::same-item-push` |
| `same-name-method` | `clippy::same-name-method` |
| `suspicious-formatting` | `clippy::suspicious-assignment-formatting`, `clippy::suspicious-else-formatting`, `clippy::suspicious-unary-op-formatting` |
| `suspicious-operand` | `clippy::suspicious-operation-groupings` |
| `type-range-comparison` | `rustc::unused-comparisons` |
| `unclear-precedence` | `clippy::precedence`, `clippy::precedence-bits` |
| `unreachable-case` | `clippy::match-overlapping-arm`, `rustc::unreachable-patterns` |
| `unreachable-code` | `rustc::unreachable-code` |
| `unused-format-specifier` | `clippy::unused-format-specs` |
| `unused-return-value` | `rustc::unused-must-use`, `rustc::unused-results` |
| `unused-rounding` | `clippy::unused-rounding` |
| `unwaited-process` | `clippy::zombie-processes` |
| `used-underscore-binding` | `clippy::used-underscore-binding`, `clippy::used-underscore-items` |
| `while-float` | `clippy::while-float` |
| `xor-used-as-power` | `clippy::suspicious-xor-used-as-pow` |
| `zero-divided-by-zero` | `clippy::zero-divided-by-zero` |

### `[unused]` — 9 rules

| jals rule | ported from |
|---|---|
| `collection-never-read` | `clippy::collection-is-never-read` |
| `only-used-in-recursion` | `clippy::only-used-in-recursion`, `clippy::self-only-used-in-recursion` |
| `redundant-import` | `clippy::single-component-path-imports`, `rustc::redundant-imports` |
| `unfulfilled-suppression` | `rustc::unfulfilled-lint-expectations` |
| `unused-assignment` | `rustc::unused-assignments` |
| `unused-attribute` | `rustc::unused-attributes` |
| `unused-dependency` | `rustc::unused-crate-dependencies` |
| `unused-label` | `rustc::unused-labels` |
| `unused-public-member` | `rustc::dead-code-pub-in-binary` |

### `[complexity]` — 66 rules

| jals rule | ported from |
|---|---|
| `bool-to-int-with-if` | `clippy::bool-to-int-with-if` |
| `boolean-comparison` | `clippy::bool-comparison` |
| `branches-sharing-code` | `clippy::branches-sharing-code` |
| `cognitive-complexity` | `clippy::cognitive-complexity` |
| `collapsible-if` **(implemented; this row is an extension)** | `clippy::collapsible-match` |
| `double-negation` | `rustc::double-negations` |
| `double-parens` | `clippy::double-parens` |
| `duplicate-type-bound` | `clippy::trait-duplication-in-bounds`, `clippy::type-repetition-in-bounds` |
| `empty-else` | `clippy::needless-else` |
| `empty-if` | `clippy::needless-ifs` |
| `excessive-booleans` | `clippy::fn-params-excessive-bools`, `clippy::struct-excessive-bools` |
| `excessive-nesting` | `clippy::excessive-nesting` |
| `identity-operation` | `clippy::identity-op` |
| `immediately-invoked-lambda` | `clippy::redundant-closure-call` |
| `int-plus-one` | `clippy::int-plus-one` |
| `let-and-return` | `clippy::let-and-return` |
| `manual-abs-diff` | `clippy::manual-abs-diff` |
| `manual-affix-check` | `clippy::chars-last-cmp`, `clippy::chars-next-cmp` |
| `manual-bit-count` | `clippy::manual-bits` |
| `manual-clamp` | `clippy::manual-clamp` |
| `manual-collection-literal` | `clippy::vec-init-then-push` |
| `manual-collection-swap` | `clippy::manual-swap` |
| `manual-div-ceil` | `clippy::manual-div-ceil` |
| `manual-duration-part` | `clippy::duration-subsec` |
| `manual-empty-string` | `clippy::manual-string-new` |
| `manual-exact-conversion` | `clippy::checked-conversions` |
| `manual-floor-mod` | `clippy::manual-rem-euclid` |
| `manual-get-last` | `clippy::get-last-with-len` |
| `manual-ilog2` | `clippy::manual-ilog2` |
| `manual-is-empty` | `clippy::comparison-to-empty`, `clippy::len-zero`, `clippy::unnecessary-first-then-check` |
| `manual-is-power-of-two` | `clippy::manual-is-power-of-two` |
| `manual-length` | `clippy::bytes-count-to-len`, `clippy::iter-count` |
| `manual-list-index` | `clippy::iter-nth`, `clippy::iter-nth-zero`, `clippy::iter-skip-next` |
| `needless-boolean-branch` | `clippy::needless-bool`, `clippy::needless-bool-assign` |
| `needless-continue` | `clippy::needless-continue` |
| `non-minimal-cfg` | `clippy::non-minimal-cfg` |
| `nonminimal-boolean` | `clippy::nonminimal-bool`, `clippy::overly-complex-bool-expr` |
| `redundant-case-guard` | `clippy::redundant-guards` |
| `redundant-cast` | `clippy::cast-lossless`, `clippy::needless-type-cast`, `clippy::unnecessary-cast`, `rustc::trivial-casts`, `rustc::trivial-numeric-casts` |
| `redundant-comparison` | `clippy::double-comparisons`, `clippy::redundant-comparisons` |
| `redundant-else` | `clippy::redundant-else` |
| `redundant-instanceof` | `clippy::redundant-pattern-matching` |
| `redundant-lambda` | `clippy::redundant-closure`, `clippy::redundant-closure-for-method-calls` |
| `redundant-lookup` | `clippy::unnecessary-get-then-check` |
| `redundant-radix` | `clippy::from-str-radix-10`, `clippy::is-digit-ascii-radix` |
| `redundant-semicolon` | `clippy::unnecessary-semicolon`, `rustc::redundant-semicolons` |
| `redundant-string-bytes-roundtrip` | `clippy::string-from-utf8-as-bytes` |
| `redundant-substring` | `clippy::redundant-slicing` |
| `redundant-trim` | `clippy::trim-split-whitespace` |
| `redundant-type-annotation` | `clippy::redundant-type-annotations` |
| `repeat-once` | `clippy::repeat-once` |
| `single-element-loop` | `clippy::single-element-loop` |
| `too-many-arguments` | `clippy::too-many-arguments` |
| `too-many-lines` | `clippy::too-many-lines` |
| `type-complexity` | `clippy::type-complexity` |
| `unnecessary-block` | `rustc::unused-braces` |
| `unnecessary-fold` | `clippy::unnecessary-fold` |
| `unnecessary-import-group` | `rustc::unused-import-braces` |
| `unnecessary-join` | `clippy::unnecessary-join` |
| `unnecessary-min-max` | `clippy::unnecessary-min-or-max` |
| `unnecessary-parens` | `rustc::unused-parens` |
| `unnecessary-qualification` | `rustc::unused-qualifications` |
| `unnecessary-sort-comparator` | `clippy::unnecessary-sort-by` |
| `unnecessary-text-block` | `clippy::needless-raw-string-hashes`, `clippy::needless-raw-strings` |
| `useless-format` | `clippy::useless-format` |
| `verbose-bit-mask` | `clippy::verbose-bit-mask` |

### `[performance]` — 29 rules

| jals rule | ported from |
|---|---|
| `busy-wait` | `clippy::missing-spin-loop` |
| `collapsible-replace` | `clippy::collapsible-str-replace` |
| `discouraged-collection` | `clippy::linkedlist` |
| `eager-argument-evaluation` | `clippy::expect-fun-call`, `clippy::or-fun-call` |
| `format-then-append` | `clippy::format-push-string` |
| `imprecise-float-op` | `clippy::imprecise-flops`, `clippy::suboptimal-flops` |
| `manual-array-copy` | `clippy::manual-memcpy` |
| `manual-array-fill` | `clippy::manual-slice-fill` |
| `manual-contains` | `clippy::manual-contains`, `clippy::search-is-some` |
| `manual-ignore-case-compare` | `clippy::manual-ignore-case-cmp` |
| `manual-retain` | `clippy::manual-retain` |
| `manual-string-repeat` | `clippy::manual-str-repeat` |
| `map-contains-then-put` | `clippy::map-entry`, `clippy::set-contains-or-insert` |
| `map-key-set-lookup` | `clippy::for-kv-map`, `clippy::iter-kv-map` |
| `missing-collection-capacity` | `clippy::reserve-after-initialization`, `clippy::slow-vector-initialization` |
| `mutex-for-atomic` | `clippy::mutex-atomic`, `clippy::mutex-integer` |
| `needless-collect` | `clippy::needless-collect` |
| `needless-string-allocation` | `clippy::cmp-owned`, `clippy::unnecessary-owned-empty-strings`, `clippy::unnecessary-to-owned` |
| `nested-format` | `clippy::format-in-format-args` |
| `no-op-method-call` | `clippy::no-effect-replace`, `clippy::useless-conversion`, `rustc::noop-method-call` |
| `pattern-compile-in-loop` | `clippy::regex-creation-in-loops` |
| `readonly-write-lock` | `clippy::readonly-write-lock` |
| `redundant-to-string` | `clippy::inefficient-to-string`, `clippy::to-string-in-format-args` |
| `single-char-append` | `clippy::single-char-add-str` |
| `single-char-string-argument` | `clippy::single-char-pattern` |
| `string-concat-in-loop` | `clippy::string-add-assign` |
| `trivial-regex` | `clippy::trivial-regex` |
| `unbuffered-stream-read` | `clippy::unbuffered-bytes` |
| `unnecessary-lazy-evaluation` | `clippy::unnecessary-lazy-evaluations` |

### `[style]` — 34 rules

| jals rule | ported from |
|---|---|
| `bitwise-boolean-operator` | `clippy::needless-bitwise-bool` |
| `boolean-assert-comparison` | `clippy::bool-assert-comparison`, `clippy::manual-assert-eq` |
| `chained-assignment` | `clippy::multi-assignments` |
| `collapsible-else-if` | `clippy::collapsible-else-if` |
| `comparison-chain` | `clippy::comparison-chain` |
| `decimal-bitwise-operand` | `clippy::decimal-bitwise-operands`, `clippy::decimal-literal-representation` |
| `duration-unit` | `clippy::duration-suboptimal-units` |
| `empty-println` | `clippy::println-empty-string`, `clippy::writeln-empty-string` |
| `hardcoded-loopback-address` | `clippy::ip-constant` |
| `inconsistent-digit-grouping` | `clippy::inconsistent-digit-grouping`, `clippy::large-digit-groups`, `clippy::unusual-byte-groupings` |
| `manual-ascii-check` | `clippy::manual-is-ascii-check`, `clippy::to-digit-is-some` |
| `manual-assert` | `clippy::manual-assert` |
| `manual-assign-op` | `clippy::assign-op-pattern` |
| `manual-elapsed` | `clippy::manual-instant-elapsed` |
| `manual-for-each` | `clippy::explicit-counter-loop`, `clippy::needless-range-loop`, `clippy::unused-enumerate-index` |
| `manual-get-first` | `clippy::get-first` |
| `manual-is-finite` | `clippy::manual-is-finite`, `clippy::manual-is-infinite` |
| `manual-iterator-loop` | `clippy::while-let-on-iterator` |
| `manual-rotate` | `clippy::manual-rotate` |
| `mismatched-type-parameter-order` | `clippy::mismatching-type-param-order` |
| `needless-for-each` | `clippy::needless-for-each` |
| `needless-late-init` | `clippy::needless-late-init`, `clippy::useless-let-if-seq` |
| `needless-return` | `clippy::needless-return` |
| `negated-if-condition` | `clippy::if-not-else` |
| `negation-by-multiply` | `clippy::neg-multiply` |
| `print-with-newline` | `clippy::print-with-newline`, `clippy::write-with-newline` |
| `renamed-override-parameter` | `clippy::renamed-function-params` |
| `should-implement-interface` | `clippy::iter-without-into-iter`, `clippy::should-implement-trait` |
| `size-without-is-empty` | `clippy::len-without-is-empty` |
| `split-at-newline` | `clippy::str-split-at-newline` |
| `unreachable-visibility` | `rustc::unreachable-pub` |
| `unreadable-literal` | `clippy::unreadable-literal` |
| `unused-this` | `clippy::unused-self` |
| `wildcard-for-single-case` | `clippy::match-wildcard-for-single-variants` |

### `[naming]` — 12 rules

| jals rule | ported from |
|---|---|
| `confusable-parameter-names` | `clippy::duplicate-underscore-argument` |
| `disallowed-name` | `clippy::disallowed-names` |
| `enum-constant-names` | `clippy::enum-variant-names` |
| `exception-naming` | `clippy::error-impl-error` |
| `field-name-repetition` | `clippy::struct-field-names` |
| `many-single-char-names` | `clippy::many-single-char-names` |
| `min-identifier-length` | `clippy::min-ident-chars` |
| `public-underscore-field` | `clippy::pub-underscore-fields` |
| `redundant-test-prefix` | `clippy::redundant-test-prefix` |
| `similar-names` | `clippy::similar-names` |
| `uninformative-name` | `clippy::just-underscores-and-digits` |
| `upper-case-acronym` | `clippy::upper-case-acronyms` |

### `[documentation]` — 11 rules

| jals rule | ported from |
|---|---|
| `broken-javadoc-link` | `clippy::doc-broken-link` |
| `detached-attribute` | `clippy::empty-line-after-outer-attr` |
| `detached-javadoc` | `clippy::empty-line-after-doc-comments` |
| `disabled-test-without-reason` | `clippy::ignore-without-reason` |
| `javadoc-punctuation` | `clippy::doc-paragraphs-missing-punctuation` |
| `long-javadoc-summary` | `clippy::too-long-first-doc-paragraph` |
| `malformed-javadoc` | `rustc::invalid-doc-attributes` |
| `misplaced-javadoc` | `rustc::unused-doc-comments` |
| `missing-javadoc` | `clippy::missing-docs-in-private-items`, `rustc::missing-docs` |
| `missing-javadoc-tag` | `clippy::missing-errors-doc`, `clippy::missing-panics-doc`, `clippy::missing-safety-doc` |
| `tab-in-javadoc` | `clippy::tabs-in-doc-comments` |

### `[restriction]` — 36 rules

| jals rule | ported from |
|---|---|
| `absolute-type-name` | `clippy::absolute-paths` |
| `arithmetic-overflow-risk` | `clippy::arithmetic-side-effects` |
| `assert-without-message` | `clippy::missing-assert-message` |
| `bare-runtime-exception` | `clippy::panic` |
| `byte-order` | `clippy::big-endian-bytes`, `clippy::host-endian-bytes`, `clippy::little-endian-bytes` |
| `cast-conversion` | `clippy::as-conversions` |
| `cfg-not-test` | `clippy::cfg-not-test` |
| `create-dir-not-recursive` | `clippy::create-dir` |
| `debug-print` | `clippy::dbg-macro` |
| `disallowed-field` | `clippy::disallowed-fields` |
| `disallowed-method` | `clippy::disallowed-methods` |
| `disallowed-type` | `clippy::disallowed-types` |
| `else-if-without-else` | `clippy::else-if-without-else` |
| `empty-parens-on-enum-constant` | `clippy::empty-enum-variants-with-brackets` |
| `exception-cause-dropped` | `clippy::map-err-ignore` |
| `filetype-is-file` | `clippy::filetype-is-file` |
| `float-arithmetic` | `clippy::float-arithmetic` |
| `indexing` | `clippy::indexing-slicing`, `clippy::missing-asserts-for-indexing` |
| `infinite-loop` | `clippy::infinite-loop` |
| `integer-division` | `clippy::integer-division`, `clippy::integer-division-remainder-used` |
| `iterate-unordered-collection` | `clippy::iter-over-hash-type` |
| `member-ordering` | `clippy::arbitrary-source-item-ordering` |
| `modulo-arithmetic` | `clippy::modulo-arithmetic` |
| `non-ascii-identifier` | `clippy::disallowed-script-idents`, `rustc::non-ascii-idents` |
| `non-ascii-literal` | `clippy::non-ascii-literal` |
| `non-private-field` | `clippy::field-scoped-visibility-modifiers`, `clippy::partial-pub-fields` |
| `shadowed-name` | `clippy::shadow-reuse`, `clippy::shadow-same`, `clippy::shadow-unrelated` |
| `single-call-method` | `clippy::single-call-fn` |
| `string-concatenation` | `clippy::string-add` |
| `string-substring` | `clippy::string-slice` |
| `suppression-without-reason` | `clippy::allow-attributes-without-reason` |
| `system-exit` | `clippy::exit` |
| `unimplemented-default-method` | `clippy::missing-trait-methods` |
| `unimplemented-stub` | `clippy::todo`, `clippy::unimplemented`, `clippy::unreachable` |
| `verbose-file-read` | `clippy::verbose-file-reads` |
| `wildcard-switch-arm` | `clippy::wildcard-enum-match-arm` |

## Invariants

- **Never panics.** A source with syntax errors is still linted over the lossless CST
  (`tests/invariants.rs` holds it against arbitrary input).
- **Read-only.** No rule mutates the tree.
- **Deterministic.** Diagnostics come back sorted by start offset, in one order, whichever entry
  point produced them.
- **A broken parse suppresses inference, and the engine decides that** — not a caller editing the
  config it passes down, and identically whichever entry point is used.
- **Nothing inside a `cfg`-disabled region is reported.** The code will not be compiled, so findings
  there are noise; one geometric pass covers every rule at once.
- **A `@SuppressWarnings` covers what its declaration contains, and the engine decides that** — one
  place, before a finding becomes a diagnostic, so it reaches every rule and every host identically
  and the fixed `cfg` diagnostic is outside it by construction.
