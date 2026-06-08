# jals-fmt

A Wadler/Prettier-style Java pretty-printer driven by the `jals-syntax` CST.

The formatter lowers the lossless concrete syntax tree into a document IR (`Doc`) and renders
it, choosing for each group whether it fits on one line or must break:

```
CST ──▶ lower.rs ──▶ Doc IR ──▶ render.rs ──▶ formatted text
        (comments.rs attaches comments to significant tokens)
```

It upholds the workspace formatter invariants: the significant-token sequence is never
changed, comments are never dropped or reordered, and formatting is idempotent
(`format(format(x)) == format(x)`).

## What it does today

The current formatter is intentionally minimal. It performs:

- **Indentation** — spaces or a tab, configurable width.
- **Block layout** — class bodies, blocks, and switch blocks (`{ … }`) are laid out
  multi-line. The opening brace is **K&R (same-line) only** — there is no Allman/next-line
  option yet.
- **Delimited lists** — parameter lists, argument lists, record headers, annotation argument
  lists, and array initializers wrap **all-or-nothing** against `max-width`. There are no
  finer per-construct width heuristics.
- **Operator spacing** — binary and unary expressions get canonical spacing. Binary
  expressions are **not** wrapped across lines.
- **Token spacing** — normalized single-space spacing between tokens, with a fusion-safety
  net so operator fusion (`>>`, `->`, …) is never introduced or changed.
- **Comment placement** — leading / trailing / dangling comments are anchored and re-emitted.
- **Comment reflow** — with `wrap-comments` enabled, standalone line and block/Javadoc
  comments are rewrapped to `comment-width` at their indentation. Lines are wrapped
  independently (never merged), preformatted regions (`<pre>`, fenced code) are left intact,
  and same-line trailing comments are never wrapped. Off by default; see below.
- **Blank lines, final newline, trailing-whitespace trimming.**

Everything else falls back to inline emission with normalized spacing.

## Configuration

The formatter reads `jalsfmt.toml`. Every key is optional and falls back to its default; keys
are kebab-case.

| Key | Type | Default | Status |
| --- | --- | --- | --- |
| `indent-style` | `"space"` \| `"tab"` | `"space"` | ✅ wired |
| `indent-width` | integer | `4` | ✅ wired |
| `max-blank-lines` | integer | `1` | ✅ wired — runs of blank lines are clamped to this many (`0` removes them) |
| `line-ending` | `"lf"` \| `"crlf"` | `"lf"` | ✅ wired (no `auto`/`native` detection) |
| `insert-final-newline` | bool | `true` | ✅ wired |
| `max-width` | integer | `100` | ✅ wired |
| `wrap-comments` | bool | `false` | ✅ wired — when enabled, reflow comments/Javadoc to `comment-width` (mirrors rustfmt's `wrap_comments`) |
| `comment-width` | integer | `80` | ✅ wired — comment/Javadoc reflow target (columns); only consulted when `wrap-comments` is enabled |

---

# Roadmap: options to add

Goal: mirror **every rustfmt option that is not Rust-specific**, adapted to Java. The lists
below map each missing capability to the rustfmt option(s) it corresponds to. (Audited
against the rustfmt configuration reference.)

## 0. Existing options not fully wired up

These already exist in `Config` but do not affect output yet — closing these is the first
step.

| jals key | Gap | rustfmt equivalent |
| --- | --- | --- |
| `line-ending` | No `auto`/`native` (detect existing line endings) | `newline_style` (`Auto`/`Native`/`Unix`/`Windows`) |
| *(none)* | No lower bound for blank lines between items | `blank_lines_lower_bound` |

## 1. Brace & control-flow style (highest-demand for Java)

| Capability | rustfmt equivalent |
| --- | --- |
| Opening brace on same line (K&R) vs. next line (Allman) | `brace_style` |
| `} else {` / `} catch {` / `} finally {` same-line vs. broken | `control_brace_style` |
| Collapse empty method/class bodies to `{}` (currently fixed, not configurable) | `empty_item_single_line` |
| Keep single-statement methods on one line | `fn_single_line` |
| Force every block multi-line | `force_multiline_blocks` |
| Keep a `throws` clause / type bounds on one line | `where_single_line` (analogue) |

## 2. Width-based heuristics (jals only has the single `max-width`)

| Capability | rustfmt equivalent |
| --- | --- |
| Preset bundle for all width thresholds (Default/Off/Max) | `use_small_heuristics` |
| Method-call argument wrap width | `fn_call_width` |
| Array-initializer wrap width | `array_width` |
| **Method-chain** (`a.b().c().d()`) wrap width — chain formatting does not exist yet | `chain_width` |
| Keep a ternary / `if-else` on one line up to width | `single_line_if_else_max_width` |
| Annotation wrap widths | `attr_fn_like_width`, `inline_attribute_width` |
| Pack short array elements | `short_array_element_width_threshold` |

## 3. Wrapping shape (jals only does "all-or-nothing")

| Capability | rustfmt equivalent |
| --- | --- |
| Parameter/argument layout: Tall / Compressed / **Vertical (one per line)** | `fn_params_layout`, `fn_args_layout` |
| Wrap binary expressions; operator at line-start (Front) vs. line-end (Back) | `binop_separator` |
| Let the last argument (lambda/array) overflow the call parentheses | `overflow_delimited_expr` |
| Trailing comma: Always / Never / Vertical (currently only **preserved**) | `trailing_comma` |
| Combine a control expression with its argument | `combine_control_expr` |

## 4. Spacing

| Capability | rustfmt equivalent |
| --- | --- |
| Space after `:` (ternary, enhanced-`for`, labels, `case x:`) | `space_after_colon` |
| Space before `:` | `space_before_colon` |
| Density of type punctuation (`T extends A & B`) | `type_punctuation_density` |

## 5. Comments

Reflow comments/Javadoc to `comment-width` (`wrap_comments`) is **implemented** — see
[What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Normalize `/* */` ↔ `//` | `normalize_comments` |
| Format code blocks inside Javadoc | `format_code_in_doc_comments`, `doc_comment_code_block_width` |

## 6. Import organization (important for Java; currently nonexistent)

| Capability | rustfmt equivalent |
| --- | --- |
| Sort imports | `reorder_imports` |
| Group imports into blocks (e.g. java./javax./external) | `group_imports` |
| Granularity: collapse to `import a.b.*` vs. explicit single imports | `imports_granularity` |
| Wrapping layout/indent of import lists | `imports_indent`, `imports_layout` |

## 7. Alignment

| Capability | rustfmt equivalent |
| --- | --- |
| Align consecutive field declarations / assignments (`=`) | `struct_field_align_threshold` |
| Align enum constant initializers | `enum_discrim_align_threshold` |

## 8. Literal normalization

| Capability | rustfmt equivalent |
| --- | --- |
| Hex literal case (`0xFF` vs. `0xff`) | `hex_literal_case` |
| Float trailing zero (`1.0` vs. `1.`) | `float_literal_trailing_zero` |
| *(Java-specific extension)* underscore grouping; `L`/`F`/`D` suffix case | — |

## 9. File selection, errors & operational (language-agnostic)

| Capability | rustfmt equivalent |
| --- | --- |
| Exclude patterns | `ignore` |
| Error on line overflow / unformattable nodes | `error_on_line_overflow`, `error_on_unformatted` |
| Disable all formatting | `disable_all_formatting` |
| Skip `@generated` files | `format_generated_files`, `generated_marker_line_search_limit` |
| Require a tool version | `required_version` |
| CLI output color (jals-cli) | `color` |

---

## Out of scope (Rust-specific)

These rustfmt options have no meaningful Java analogue and are intentionally **not** planned:

`edition`, `style_edition`, `version`, `force_explicit_abi`, `merge_derives`,
`use_field_init_shorthand`, `use_try_shorthand` (`?`), `remove_nested_parens` (changes
meaning; conflicts with the lossless/significant-token invariant), `condense_wildcard_suffixes`,
`spaces_around_ranges` (`..`), `match_arm_leading_pipes`, `match_block_trailing_comma`,
`single_line_let_else_max_width`, `struct_lit_single_line`, `struct_lit_width`,
`struct_variant_width`, `format_macro_bodies`, `format_macro_matchers`,
`skip_macro_invocations`, `normalize_doc_attributes`, `reorder_modules`, `reorder_impl_items`,
`skip_children`, `trailing_semicolon` (expression-level), `unstable_features`.

Notes:
- `match_arm_blocks` is *not* listed above — it is reusable as switch-expression/statement arm
  formatting in Java and is a candidate.
- rustfmt's `indent_style` (`Block`/`Visual` = visual indentation) is a dated style and is not
  planned. (jals's own `indent-style` is the unrelated space-vs-tab choice.)

## Java-specific options worth adding (beyond rustfmt)

Mirroring rustfmt fully still leaves big Java-only knobs uncovered:

- **Annotation placement** — annotations on their own line vs. inline, per target
  (field/method/parameter). One of the most contested Java style points.
- **Modifier ordering** — canonical order of `public static final …`.
- **`switch` arm style** — legacy `case:` vs. arrow `case ->`; lambda block conversion.

## Suggested priority

By Java-user impact: **(1)** `brace_style` / `control_brace_style` → **(2)** import
organization (`reorder_imports` family) → **(3)** width heuristics (`chain_width`,
`fn_call_width`, `array_width`) → **(4)** `trailing_comma`. (Comment reflow — `comment-width`
via `wrap_comments` — is done.)
