# jals-fmt

A Wadler/Prettier-style Java pretty-printer driven by the `jals-syntax` CST.

The formatter lowers the lossless concrete syntax tree into a document IR (`Doc`) and renders
it, choosing for each group whether it fits on one line or must break:

```
CST ──▶ lower.rs ──▶ Doc IR ──▶ render.rs ──▶ formatted text
        (comments.rs attaches comments to significant tokens)
```

It upholds the workspace formatter invariants: comments are never dropped and formatting is
idempotent (`format(format(x)) == format(x)`); by default the significant-token sequence is
preserved exactly. Three opt-in options relax this (see [Configuration](#configuration)):
`reorder-imports` and `group-imports` preserve the token *multiset* instead, and
`trailing-comma` may add or drop the single trailing comma of an array initializer.

## What it does today

The current formatter is intentionally minimal. It performs:

- **Indentation** — spaces or a tab, configurable width.
- **Block layout** — class bodies, blocks, and switch blocks (`{ … }`) are laid out
  multi-line. The opening brace of a **declaration body** (type, method, constructor, or
  initializer) follows `brace-style`, and the opening brace of a **control-flow / `switch` /
  lambda / bare block** plus the `} else` / `} catch` / `} finally` / `} while` continuations
  follow `control-brace-style`: each is K&R same-line (default) or Allman next-line. An empty
  body stays `{}` on the header's line either way.
- **Delimited lists** — parameter lists, argument lists, record headers, annotation argument
  lists, and array initializers wrap **all-or-nothing** against `max-width`. Call argument
  lists additionally honor `fn-call-width` and array initializers `array-width` (see below);
  the other lists have no finer per-construct width heuristics yet.
- **Method chains** — a `a.b().c().d()` chain with at least two calls is laid out one call per
  line when its flat width exceeds `chain-width` or the line would overflow `max-width`. The
  receiver and any leading field accesses stay on the first line and the first call hugs them
  (`source.stream()`, then `.filter(…)` / `.map(…)` on following lines); a lone call or a pure
  field path (`a.b.c`) is never broken.
- **Call arguments** — a function or method call (`f(a, b, c)`) whose argument list's flat
  width exceeds `fn-call-width` is laid out one argument per line, even when the line would
  otherwise fit `max-width`. Method-definition parameter lists are unaffected.
- **Array initializers** — an array initializer (`{a, b, c}`, including `new T[]{…}`) whose
  flat width exceeds `array-width` is laid out one element per line, even when the line would
  otherwise fit `max-width`. Argument and parameter lists are unaffected.
- **Trailing commas** — the trailing comma of an array initializer follows `trailing-comma`:
  `preserve` (default, keep the source's), `always`, `never`, or `vertical` (present only when
  the initializer breaks one element per line). Only array initializers are governed — Java
  permits a trailing comma only there and in enum constant lists — and a comma carrying a
  comment is never dropped. Off by default (`preserve`); see below.
- **Operator spacing** — binary and unary expressions get canonical spacing.
- **Binary-expression wrapping** — a binary expression that overflows `max-width` breaks at
  its operators: a same-precedence run wraps together, one operand per line, and
  lower-precedence operators break first (`a == b && c == d || e == f` breaks at `||`, then
  `&&`, while each `==` stays on its line). The operator sits at the start of the
  continuation line (`binop-separator = "front"`, default) or at the end of the broken line
  (`"back"`). Assignments (`=`) and ternaries are not wrapped yet.
- **Token spacing** — normalized single-space spacing between tokens, with a fusion-safety
  net so operator fusion (`>>`, `->`, …) is never introduced or changed.
- **Comment placement** — leading / trailing / dangling comments are anchored and re-emitted.
- **Comment reflow** — with `wrap-comments` enabled, standalone line and block/Javadoc
  comments are rewrapped to `comment-width` at their indentation. Lines are wrapped
  independently (never merged), preformatted regions (`<pre>`, fenced code) are left intact,
  and same-line trailing comments are never wrapped. Off by default; see below.
- **Line endings** — `lf` / `crlf`, or `auto` (match the source's first line break, falling
  back to the host terminator when the source has none) / `native` (the host's terminator).
  This governs the breaks the formatter emits; the interior of multi-line tokens (text blocks,
  string literals, verbatim comments) is preserved byte-for-byte to keep significant tokens
  unchanged, so such tokens may retain their original line breaks.
- **Import sorting** — with `reorder-imports` enabled, the leading `import` block is sorted
  (non-static first, then static, each alphabetical by qualified name); blank lines inside the
  block are collapsed and comments attached to an import move with it. Off by default; see below.
- **Import grouping** — with `group-imports` enabled, the leading `import` block is partitioned
  into the prefix groups of `import-groups` (e.g. `java.` / `javax.` / others / `static`), each
  group sorted alphabetically and separated by one blank line. Overrides `reorder-imports`. Off
  by default; see below.
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
| `line-ending` | `"lf"` \| `"crlf"` \| `"auto"` \| `"native"` | `"lf"` | ✅ wired — `auto` matches the source's first line break, `native` uses the host terminator |
| `insert-final-newline` | bool | `true` | ✅ wired |
| `max-width` | integer | `100` | ✅ wired |
| `chain-width` | integer | `60` | ✅ wired — a method chain (`a.b().c()`) with ≥2 calls wraps one call per line when its flat width exceeds this or the line overflows `max-width`; mirrors rustfmt's `chain_width` |
| `fn-call-width` | integer | `60` | ✅ wired — a function/method call whose argument list's flat width exceeds this wraps one argument per line, even when it fits `max-width`; mirrors rustfmt's `fn_call_width` |
| `array-width` | integer | `60` | ✅ wired — an array initializer (`{a, b, c}`) whose flat width exceeds this wraps one element per line, even when it fits `max-width`; mirrors rustfmt's `array_width` |
| `brace-style` | `"same-line"` \| `"next-line"` | `"same-line"` | ✅ wired — `next-line` (Allman) opens type/method/constructor/initializer bodies on their own line; control-flow & `switch` are governed by `control-brace-style` |
| `control-brace-style` | `"same-line"` \| `"next-line"` | `"same-line"` | ✅ wired — `next-line` (Allman) opens control-flow / `switch` / lambda / bare block braces on their own line and breaks `} else` / `} catch` / `} finally` / `} while`; mirrors rustfmt's `control_brace_style` |
| `wrap-comments` | bool | `false` | ✅ wired — when enabled, reflow comments/Javadoc to `comment-width` (mirrors rustfmt's `wrap_comments`) |
| `comment-width` | integer | `80` | ✅ wired — comment/Javadoc reflow target (columns); only consulted when `wrap-comments` is enabled |
| `reorder-imports` | bool | `false` | ✅ wired — sort the leading `import` block (non-static first, then static, each alphabetical by qualified name); blank lines inside the block collapse and comments attached to an import move with it. Off by default; when on, the significant-token *sequence* may change (the multiset is preserved). Mirrors rustfmt's `reorder_imports` |
| `trailing-comma` | `"preserve"` \| `"always"` \| `"never"` \| `"vertical"` | `"preserve"` | ✅ wired — trailing comma of an **array initializer** only (`{1, 2, 3,}`): `preserve` keeps the source's, `always`/`never` force it on/off, `vertical` adds it only when the initializer breaks one element per line. Non-`preserve` may add or drop that one comma (a comma carrying a comment is kept); the default `preserve` keeps the strict significant-token sequence. Mirrors rustfmt's `trailing_comma` |
| `group-imports` | bool | `false` | ✅ wired — partition the leading `import` block into the prefix groups of `import-groups`, each group sorted and separated by one blank line. Overrides `reorder-imports`; when on, the significant-token *sequence* may change (the multiset is preserved). Mirrors rustfmt's `group_imports` |
| `import-groups` | array of strings | `["java.", "javax.", "*", "static"]` | ✅ wired — ordered prefix groups for `group-imports`: a non-static import joins its *longest* matching prefix, `"*"` is the catch-all for the rest, and `"static"` groups all static imports. A missing `"*"` / `"static"` becomes an implicit trailing group. Only consulted when `group-imports` is enabled |
| `binop-separator` | `"front"` \| `"back"` | `"front"` | ✅ wired — placement of a binary operator when its expression wraps (driven by `max-width` alone): `front` starts the continuation line with the operator, `back` ends the broken line with it; mirrors rustfmt's `binop_separator` |

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
| *(none)* | No lower bound for blank lines between items | `blank_lines_lower_bound` |

## 1. Brace & control-flow style (highest-demand for Java)

Opening-brace placement is **implemented** for both halves — declaration bodies
(`brace_style`) and control-flow / `switch` / lambda braces plus `} else` / `} catch` /
`} finally` / `} while` continuations (`control_brace_style`) — see
[What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Collapse empty method/class bodies to `{}` (currently fixed, not configurable) | `empty_item_single_line` |
| Keep single-statement methods on one line | `fn_single_line` |
| Force every block multi-line | `force_multiline_blocks` |
| Keep a `throws` clause / type bounds on one line | `where_single_line` (analogue) |

## 2. Width-based heuristics (jals has `max-width`, `chain-width`, `fn-call-width`, and `array-width`)

**Method-chain** wrap width (`chain_width`), **method-call argument** wrap width
(`fn_call_width`), and **array-initializer** wrap width (`array_width`) are **implemented** —
see [What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Preset bundle for all width thresholds (Default/Off/Max) | `use_small_heuristics` |
| Keep a ternary / `if-else` on one line up to width | `single_line_if_else_max_width` |
| Annotation wrap widths | `attr_fn_like_width`, `inline_attribute_width` |
| Pack short array elements | `short_array_element_width_threshold` |

## 3. Wrapping shape (delimited lists wrap all-or-nothing)

| Capability | rustfmt equivalent |
| --- | --- |
| Parameter/argument layout: Tall / Compressed / **Vertical (one per line)** | `fn_params_layout`, `fn_args_layout` |
| Wrap binary expressions; operator at line-start (Front) vs. line-end (Back) | `binop_separator` ✅ |
| Let the last argument (lambda/array) overflow the call parentheses | `overflow_delimited_expr` |
| Trailing comma: Always / Never / Vertical (array initializers) | `trailing_comma` ✅ |
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
| Sort imports | `reorder_imports` ✅ |
| Group imports into blocks (e.g. java./javax./external) | `group_imports` ✅ |
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

By Java-user impact: the remaining import-organization option (`imports_granularity`).
(Brace styling — `brace_style` and `control_brace_style` — comment reflow — `comment-width`
via `wrap_comments` — method-chain wrapping — `chain_width` — call-argument wrapping —
`fn_call_width` — array-initializer wrapping — `array_width` — import sorting —
`reorder_imports` — import grouping — `group_imports` — trailing commas —
`trailing_comma` — and binary-expression wrapping — `binop_separator` — are done.)
