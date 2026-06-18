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
preserved exactly. Seven opt-in options relax this (see [Configuration](#configuration)):
`reorder-imports`, `group-imports`, and `reorder-modifiers` preserve the token *multiset*
instead, `trailing-comma` may add or drop the single trailing comma of an array initializer,
`hex-literal-case` may rewrite the case of a hex literal's digits,
`float-literal-trailing-zero` may add or strip the trailing zero of a decimal float literal, and
`literal-suffix-case` may rewrite the case of a literal's `l` / `f` / `d` type suffix
(the token *kind* sequence is preserved exactly).

## What it does today

The current formatter is intentionally minimal. It performs:

- **Indentation** — spaces or a tab, configurable width. A separate `continuation-indent` governs
  the indent of **continuation lines** — the wrapped lines of an expression / statement (a method
  chain, wrapped binary / ternary operators, or a wrapped delimited list) — distinct from the
  block-body indent (`indent-width`); it defaults to `indent-width`, so output is unchanged until
  set. Block bodies always use `indent-width`. Tab style ignores it (one tab per continuation),
  keeping the output a whole number of tabs. Layout-only (the significant-token sequence is
  preserved exactly). See below.
- **Block layout** — class bodies, blocks, and switch blocks (`{ … }`) are laid out
  multi-line. The opening brace of a **declaration body** (type, method, constructor, or
  initializer) follows `brace-style`, and the opening brace of a **control-flow / `switch` /
  lambda / bare block** plus the `} else` / `} catch` / `} finally` / `} while` continuations
  follow `control-brace-style`: each is K&R same-line (default) or Allman next-line. An empty
  body stays `{}` on the header's line either way, unless `empty-item-single-line` is turned
  off (see below).
- **Empty-body collapse** — an empty **declaration** body (a `class` / `interface` /
  `@interface` / record body, or a method / constructor / initializer block) collapses to `{}`
  on the header's line when `empty-item-single-line` is on (the default). Turning it off
  expands such a body to a two-line `{` … `}` (with the opening brace on its own line under
  `brace-style = next-line`). Control-flow / `switch` / lambda / bare blocks always keep `{}`,
  and `enum` bodies (not block-formatted yet) are unaffected. Layout-only (the
  significant-token sequence is preserved exactly).
- **Single-statement-body collapse** — with `fn-single-line` enabled, a **declaration** body (a
  method / constructor / initializer block) holding exactly one statement and no comments
  collapses onto the header's line (`int foo() { return 1; }`) when it fits `max-width`. A body
  with two or more statements, a comment, a nested block, or one that would overflow `max-width`
  stays multi-line. The one-liner is emitted regardless of `brace-style` (like the empty-body
  collapse); only when it does not fit does the brace open on its own line under
  `brace-style = next-line`. Off by default; layout-only (the significant-token sequence is
  preserved exactly).
- **Force multiline blocks** — with `force-multiline-blocks` enabled, every block is laid out
  multi-line: an **empty** block of any kind (a type body, a method / constructor / initializer
  block, or a control-flow / `switch` / lambda / bare block) expands to a two-line `{` … `}`
  instead of collapsing to `{}` (overriding `empty-item-single-line` and extending past its
  declaration-only scope), and a single-statement declaration body is never collapsed onto the
  header's line (overriding `fn-single-line`). The opening brace still follows `brace-style` /
  `control-brace-style`. Off by default; layout-only (the significant-token sequence is preserved
  exactly).
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
- **Parameter layout** — a method or constructor parameter list follows `fn-params-layout`:
  `tall` (the default, all-or-nothing — one line when it fits, else one parameter per line),
  `vertical` (always one parameter per line, even when the list would fit), or `compressed`
  (pack as many parameters per line as fit `max-width`, wrapping at the width). It governs only
  **declaration parameter lists**, never call argument lists; the deprecated rustfmt key
  `fn-args-layout` is accepted as an alias. Layout-only (the significant-token sequence is
  preserved exactly). See below.
- **Last-argument overflow** — with `overflow-delimited-expr` enabled, a call or annotation
  argument list whose last item is a delimited expression — a block-bodied lambda, an
  anonymous-class `new X() {…}`, or an array initializer (including `new T[]{…}` and
  `name = {…}` annotation pairs) — hangs that item past the call line (`f(a, () -> {` …
  `});`) instead of breaking one argument per line, when the first line fits
  `fn-call-width` and `max-width`. An earlier multi-line argument or a comment among the
  arguments keeps the all-or-nothing layout. Off by default; see below.
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
  (`"back"`). Assignments (`=`) are not wrapped yet.
- **Ternary wrapping** — a ternary conditional (`a ? b : c`) is kept on one line while its flat
  width fits `single-line-if-else-max-width` (default `50`); a wider ternary, or one that would
  overflow `max-width`, wraps with `?` and `:` each leading a continuation line
  (`binop-separator = "front"`, default) or trailing the broken line (`"back"`). The flat form is
  byte-identical to the inline emission (the `:` spacing follows the colon options), and a nested
  ternary wraps independently. Set the width to `0` to wrap every ternary. Layout-only (the
  significant-token sequence is preserved exactly). See below.
- **Token spacing** — normalized single-space spacing between tokens, with a fusion-safety
  net so operator fusion (`>>`, `->`, …) is never introduced or changed.
- **Colon spacing** — the spacing around a `:` follows `space-before-colon` (default off) and
  `space-after-colon` (default on), applied uniformly to every Java colon context: a ternary
  (`a ? b : c`), an enhanced `for` (`for (T x : xs)`), a labeled statement (`label:`), an
  `assert` message (`assert c : m`), and a `switch` `case` / `default` label (`case x:`). The
  defaults give idiomatic `label:` / `case x:` style. The `::` method-reference token is a
  distinct token and is never affected.
- **Type-punctuation density** — the spacing around the `&` of a Java intersection type follows
  `type-punctuation-density`: `wide` (default, `A & B`) or `compressed` (`A&B`). It governs both
  intersection contexts — a type-parameter bound (`<T extends A & B>`) and a cast intersection
  (`(A & B) x`) — uniformly. The bitwise-AND operator `&` (an expression, `a & b`) is never
  affected. Layout-only (the significant-token sequence is preserved exactly).
- **Comment placement** — leading / trailing / dangling comments are anchored and re-emitted.
- **Comment reflow** — with `wrap-comments` enabled, standalone line and block/Javadoc
  comments are rewrapped to `comment-width` at their indentation. Lines are wrapped
  independently (never merged), preformatted regions (`<pre>`, fenced code) are left intact,
  and same-line trailing comments are never wrapped. Off by default; see below.
- **Parameter-comment normalization** — with `normalize-parameter-comments` enabled, a block
  comment that is a parameter-name label before an argument (`/*a=*/`, `/*xs...=*/`) is
  rewritten to google-java-format's canonical `/* name= */` form (collapsing interior
  whitespace) and hugged to the following token on the same line (`/* a= */ 1`). Javadoc and
  any non-matching block comment are left exactly as written. Off by default; see below.
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
- **Modifier ordering** — with `reorder-modifiers` enabled, every declaration's keyword
  modifiers (`public`, `static`, `final`, …) are sorted into the canonical JLS / Checkstyle
  order (public, protected, private, abstract, default, static, sealed, non-sealed, final,
  transient, volatile, synchronized, native, strictfp) and all annotations are hoisted to the
  front (keeping their relative order). The significant-token *multiset* is preserved (none
  added, dropped, or altered) and each comment stays glued to its modifier. Off by default; see
  below.
- **Annotation placement** — `annotation-placement` controls a declaration's leading
  annotations: `compact` (the default) keeps them inline (`@Override public void m()`);
  `expanded` breaks each annotation in the leading run onto its own line above the declaration.
  It governs only declaration-level targets (a type / method / constructor / field /
  initializer / local-variable declaration); a parameter's annotations and type-use /
  enum-constant / type-parameter annotations always stay inline. Layout-only (the
  significant-token sequence is preserved exactly). Off by default; see below.
- **Hex literal case** — with `hex-literal-case` set to `upper` or `lower`, the hexadecimal
  digit letters (`a`–`f` / `A`–`F`) of an integer or floating-point literal are normalized to
  that case (`0xCafe` → `0xCAFE` / `0xcafe`). Only the hex *mantissa* digits change: the `0x` /
  `0X` radix prefix, the `p` / `P` binary exponent of a hex float (and its decimal digits), and
  any `l` / `L` / `f` / `F` / `d` / `D` suffix are left exactly as written, and decimal / octal /
  binary literals are never touched. Off by default (`preserve`); when on, a literal token's
  *text* may change (but never its kind), so the significant-token sequence is no longer
  byte-for-byte preserved. See below.
- **Float literal trailing zero** — with `float-literal-trailing-zero` set to `always` or `never`,
  a **decimal** floating-point literal's trailing zero is normalized between the two legal forms
  `1.0` and `1.`: `always` gives every empty-fraction float a trailing zero (`1.` → `1.0`,
  `1.f` → `1.0f`, `1.e10` → `1.0e10`) and `never` strips an all-zero fraction (`1.0` / `1.00` →
  `1.`, `1.0f` → `1.f`). Only in-scope decimal floats change: a fraction with a non-zero digit
  (`1.50`), a leading-dot float (`.5`, `.0`, which `never` leaves alone since stripping would yield
  the illegal bare `.`), a dotless float (`1e10`, `100f`), a hex float (`0x1.0p3`), and every
  integer literal are left exactly as written, as are the numeric value, the `f` / `F` / `d` / `D`
  suffix, and any exponent. Off by default (`preserve`); when on, a literal token's *text* may
  change (but never its kind), so the significant-token sequence is no longer byte-for-byte
  preserved. See below.
- **Literal suffix case** — with `literal-suffix-case` set to `upper` or `lower`, the trailing
  type-suffix letter of a numeric literal is normalized to that case: the `l` / `L` `long` suffix
  of an integer (`123l` → `123L` / `123l`) and the `f` / `F` / `d` / `D` `float` / `double` suffix
  of a floating-point literal (`1.5f` → `1.5F` / `1.5f`). Only that one trailing letter changes;
  the token *kind* is unaffected, and the numeric value, radix prefix, mantissa, and exponent are
  left exactly as written. The literal's kind tells the ambiguous letters apart: an integer's
  trailing `f` / `d` is a hex *digit* (`0xabcdef`), never a suffix, and a float never ends in
  `l` / `L`. A Java-specific extension with no rustfmt equivalent. Off by default (`preserve`);
  when on, a literal token's *text* may change (but never its kind), so the significant-token
  sequence is no longer byte-for-byte preserved. See below.
- **Blank lines, final newline, trailing-whitespace trimming.**

Everything else falls back to inline emission with normalized spacing.

## Configuration

The formatter reads `jalsfmt.toml`. Every key is optional and falls back to its default; keys
are kebab-case.

| Key | Type | Default | Status |
| --- | --- | --- | --- |
| `indent-style` | `"space"` \| `"tab"` | `"space"` | ✅ wired |
| `indent-width` | integer | `4` | ✅ wired |
| `continuation-indent` | integer (optional) | falls back to `indent-width` | ✅ wired — columns to indent a *continuation* line (the wrapped lines of an expression / statement): a method chain, wrapped binary / ternary operators, or a wrapped delimited list (parameter / argument / array-initializer / annotation-arg / record-header). Block bodies (`{ … }`) keep `indent-width`. Unset by default (output unchanged). Ignored in tab style (one tab per continuation, keeping the output a whole number of tabs). Layout-only (the significant-token sequence is preserved exactly). A Java-specific option (cf. IntelliJ / Checkstyle continuation indent) with no rustfmt equivalent |
| `max-blank-lines` | integer | `1` | ✅ wired — runs of blank lines are clamped to this many (`0` removes them) |
| `line-ending` | `"lf"` \| `"crlf"` \| `"auto"` \| `"native"` | `"lf"` | ✅ wired — `auto` matches the source's first line break, `native` uses the host terminator |
| `insert-final-newline` | bool | `true` | ✅ wired |
| `max-width` | integer | `100` | ✅ wired |
| `chain-width` | integer | `60` | ✅ wired — a method chain (`a.b().c()`) with ≥2 calls wraps one call per line when its flat width exceeds this or the line overflows `max-width`; mirrors rustfmt's `chain_width` |
| `fn-call-width` | integer | `60` | ✅ wired — a function/method call whose argument list's flat width exceeds this wraps one argument per line, even when it fits `max-width`; mirrors rustfmt's `fn_call_width` |
| `array-width` | integer | `60` | ✅ wired — an array initializer (`{a, b, c}`) whose flat width exceeds this wraps one element per line, even when it fits `max-width`; mirrors rustfmt's `array_width` |
| `single-line-if-else-max-width` | integer | `50` | ✅ wired — a ternary conditional (`a ? b : c`) whose flat width exceeds this — or that would overflow `max-width` — wraps, the `?` and `:` placed per `binop-separator` (leading the continuation line under `front`, trailing the broken line under `back`); `0` wraps every ternary. The flat form is byte-identical to inline emission. Layout-only (the significant-token sequence is preserved exactly). Mirrors rustfmt's `single_line_if_else_max_width` (whose Rust if-else expression maps to Java's ternary) |
| `brace-style` | `"same-line"` \| `"next-line"` | `"same-line"` | ✅ wired — `next-line` (Allman) opens type/method/constructor/initializer bodies on their own line; control-flow & `switch` are governed by `control-brace-style` |
| `control-brace-style` | `"same-line"` \| `"next-line"` | `"same-line"` | ✅ wired — `next-line` (Allman) opens control-flow / `switch` / lambda / bare block braces on their own line and breaks `} else` / `} catch` / `} finally` / `} while`; mirrors rustfmt's `control_brace_style` |
| `empty-item-single-line` | bool | `true` | ✅ wired — collapse an empty declaration body (a `class` / `interface` / `@interface` / record body, or a method / constructor / initializer block) to `{}` on the header's line; when off it expands to a two-line `{` … `}` (opening on its own line under `brace-style = next-line`). Control-flow / `switch` / lambda / bare blocks always keep `{}`, and `enum` bodies are unaffected. Layout-only (the significant-token sequence is preserved exactly). Mirrors rustfmt's `empty_item_single_line` |
| `fn-single-line` | bool | `false` | ✅ wired — keep a declaration body (a method / constructor / initializer block) holding exactly one statement and no comments on the header's line (`int foo() { return 1; }`) when it fits `max-width`; a body with ≥2 statements, a comment, a nested block, or one that overflows stays multi-line. The one-liner is emitted regardless of `brace-style`; only when it does not fit does the brace open on its own line under `next-line`. Off by default; layout-only (the significant-token sequence is preserved exactly). Mirrors rustfmt's `fn_single_line` |
| `force-multiline-blocks` | bool | `false` | ✅ wired — force every block multi-line: an **empty** block of any kind (type body, method / constructor / initializer block, control-flow / `switch` / lambda / bare block) expands to a two-line `{` … `}` instead of collapsing to `{}` (overrides `empty-item-single-line` and extends past its declaration-only scope), and a single-statement declaration body is never collapsed onto one line (overrides `fn-single-line`). The opening brace still follows `brace-style` / `control-brace-style`. Off by default; layout-only (the significant-token sequence is preserved exactly). Reinterprets rustfmt's `force_multiline_blocks` (its literal closure / match-arm brace-wrapping would add tokens, which jals's invariants forbid) |
| `wrap-comments` | bool | `false` | ✅ wired — when enabled, reflow comments/Javadoc to `comment-width` (mirrors rustfmt's `wrap_comments`) |
| `comment-width` | integer | `80` | ✅ wired — comment/Javadoc reflow target (columns); only consulted when `wrap-comments` is enabled |
| `normalize-parameter-comments` | bool | `false` | ✅ wired — rewrite a parameter-name block comment (`/*a=*/`, `/*  a  =  */`, `/*xs...=*/`) to the canonical `/* name= */`, where `name` is a Java identifier optionally suffixed `...`, and hug it to the following token (`/* a= */ 1`). The whole comment must match; Javadoc (`/** … */`), line comments, and any other block comment are left exactly as written. Operates only on comment trivia (the significant-token sequence is preserved exactly). Mirrors google-java-format's `CommentsHelper.reformatParameterComment` |
| `reorder-imports` | bool | `false` | ✅ wired — sort the leading `import` block (non-static first, then static, each alphabetical by qualified name); blank lines inside the block collapse and comments attached to an import move with it. Off by default; when on, the significant-token *sequence* may change (the multiset is preserved). Mirrors rustfmt's `reorder_imports` |
| `trailing-comma` | `"preserve"` \| `"always"` \| `"never"` \| `"vertical"` | `"preserve"` | ✅ wired — trailing comma of an **array initializer** only (`{1, 2, 3,}`): `preserve` keeps the source's, `always`/`never` force it on/off, `vertical` adds it only when the initializer breaks one element per line. Non-`preserve` may add or drop that one comma (a comma carrying a comment is kept); the default `preserve` keeps the strict significant-token sequence. Mirrors rustfmt's `trailing_comma` |
| `group-imports` | bool | `false` | ✅ wired — partition the leading `import` block into the prefix groups of `import-groups`, each group sorted and separated by one blank line. Overrides `reorder-imports`; when on, the significant-token *sequence* may change (the multiset is preserved). Mirrors rustfmt's `group_imports` |
| `import-groups` | array of strings | `["java.", "javax.", "*", "static"]` | ✅ wired — ordered prefix groups for `group-imports`: a non-static import joins its *longest* matching prefix, `"*"` is the catch-all for the rest, and `"static"` groups all static imports. A missing `"*"` / `"static"` becomes an implicit trailing group. Only consulted when `group-imports` is enabled |
| `binop-separator` | `"front"` \| `"back"` | `"front"` | ✅ wired — placement of a binary operator when its expression wraps (driven by `max-width` alone): `front` starts the continuation line with the operator, `back` ends the broken line with it; mirrors rustfmt's `binop_separator` |
| `overflow-delimited-expr` | bool | `false` | ✅ wired — let the last item of a call / annotation argument list hang past the call line when it is a block-bodied lambda, anonymous-class `new`, or array initializer (`f(a, () -> {` … `});`); falls back to the all-or-nothing layout when an earlier item is multi-line or the first line overflows `fn-call-width`/`max-width`. Layout-only (the significant-token sequence is preserved exactly); mirrors rustfmt's `overflow_delimited_expr` |
| `space-before-colon` | bool | `false` | ✅ wired — emit a space before a `:`, applied uniformly to every Java colon context (ternary, enhanced-`for`, labels, `assert`, `case`/`default`). Off by default (idiomatic `label:` / `case x:`). `::` is a distinct token and is never affected. Layout-only; mirrors rustfmt's `space_before_colon` |
| `space-after-colon` | bool | `true` | ✅ wired — emit a space after a `:`, in the same contexts as `space-before-colon`. On by default. `::` is never affected. Layout-only; mirrors rustfmt's `space_after_colon` |
| `switch-case-body` | `"always"` \| `"single-line"` \| `"same-line"` | `"always"` | ✅ wired — layout of a **legacy** (colon-form) `switch` group's body relative to its `case x:` / `default:` colon: `always` puts each label on its own line and breaks every body statement onto its own line indented one level (google-java-format's layout), `single-line` keeps a lone label with a single, comment-free statement inline (`case x: stmt;`) and breaks the rest, `same-line` keeps the whole group inline (`case x: stmt; stmt;`). The arrow form (`case x -> …`) is never affected. Layout-only (the significant-token sequence is preserved exactly). A Java-specific option with no rustfmt equivalent |
| `fn-params-layout` | `"tall"` \| `"compressed"` \| `"vertical"` | `"tall"` | ✅ wired — layout of a method / constructor parameter list: `tall` (all-or-nothing), `compressed` (pack as many parameters per line as fit `max-width`), or `vertical` (always one per line, even when it fits). Governs only declaration parameter lists, never call argument lists. Layout-only (the significant-token sequence is preserved exactly). The deprecated key `fn-args-layout` is accepted as an alias. Mirrors rustfmt's `fn_params_layout` |
| `type-punctuation-density` | `"wide"` \| `"compressed"` | `"wide"` | ✅ wired — spacing around the `&` of a Java intersection type: `wide` (`A & B`) or `compressed` (`A&B`). Governs both a type-parameter bound (`<T extends A & B>`) and a cast intersection (`(A & B) x`); the bitwise-AND operator `&` (`a & b`) is never affected. Layout-only (the significant-token sequence is preserved exactly). Mirrors rustfmt's `type_punctuation_density` |
| `reorder-modifiers` | bool | `false` | ✅ wired — sort each declaration's keyword modifiers into the canonical JLS / Checkstyle order (public, protected, private, abstract, default, static, sealed, non-sealed, final, transient, volatile, synchronized, native, strictfp) and hoist all annotations to the front (relative order kept). Off by default; when on, the significant-token *sequence* may change (the multiset is preserved, comments stay glued to their modifier). A Java-specific option with no rustfmt equivalent |
| `annotation-placement` | `"compact"` \| `"expanded"` | `"compact"` | ✅ wired — placement of a declaration's leading annotations (a type / method / constructor / field / initializer / local-variable declaration): `compact` keeps them inline (`@Override public void m()`), `expanded` breaks each annotation in the leading run onto its own line above the declaration. A parameter's annotations and type-use / enum-constant / type-parameter annotations are never affected (always inline). Layout-only (the significant-token sequence is preserved exactly). A Java-specific option with no rustfmt equivalent |
| `hex-literal-case` | `"preserve"` \| `"upper"` \| `"lower"` | `"preserve"` | ✅ wired — case of the hex digit letters of an integer / float literal (`0xCafe`): `preserve` keeps the source's, `upper` / `lower` force it. Only the hex mantissa digits change; the `0x` prefix, the `p` exponent, and any `l` / `f` / `d` suffix are untouched, and non-hex literals are never affected. Non-`preserve` may rewrite a literal token's text (never its kind); the default `preserve` keeps the strict significant-token sequence. Mirrors rustfmt's `hex_literal_case` |
| `float-literal-trailing-zero` | `"preserve"` \| `"always"` \| `"never"` | `"preserve"` | ✅ wired — trailing zero of a **decimal** float literal (`1.0` vs. `1.`): `preserve` keeps the source's, `always` adds it (`1.` → `1.0`), `never` strips an all-zero fraction (`1.0` / `1.00` → `1.`). Only in-scope decimal floats change; a non-zero fraction (`1.50`), a leading-dot float (`.5`), a dotless float (`1e10`), a hex float (`0x1.0p3`), and integers are untouched, as are the value, suffix, and exponent. Non-`preserve` may rewrite a literal token's text (never its kind); the default `preserve` keeps the strict significant-token sequence. Mirrors rustfmt's `float_literal_trailing_zero` (its Rust-only `IfNoPostfix` mode is omitted — both `1.f` and `1.0f` are legal Java) |
| `literal-suffix-case` | `"preserve"` \| `"upper"` \| `"lower"` | `"preserve"` | ✅ wired — case of a numeric literal's trailing type suffix: the integer `l` / `L` (`123l` vs. `123L`) and the float `f` / `F` / `d` / `D` (`1.5f` vs. `1.5F`). `preserve` keeps the source's, `upper` / `lower` force it. Only the single trailing suffix letter changes; the value, radix prefix, mantissa, and exponent are untouched, and an integer's trailing `f` / `d` hex *digit* (`0xabcdef`) is never a suffix. Non-`preserve` may rewrite a literal token's text (never its kind); the default `preserve` keeps the strict significant-token sequence. A Java-specific option with no rustfmt equivalent |

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
`} finally` / `} while` continuations (`control_brace_style`) — empty-declaration-body
collapse is configurable (`empty_item_single_line`), single-statement-body collapse is
configurable (`fn_single_line`), and forcing every block multi-line is configurable
(`force_multiline_blocks`) — see [What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Keep single-statement methods on one line | `fn_single_line` ✅ |
| Force every block multi-line | `force_multiline_blocks` ✅ |
| Keep a `throws` clause / type bounds on one line | `where_single_line` (analogue) |

## 2. Width-based heuristics (jals has `max-width`, `chain-width`, `fn-call-width`, and `array-width`)

**Method-chain** wrap width (`chain_width`), **method-call argument** wrap width
(`fn_call_width`), **array-initializer** wrap width (`array_width`), and **ternary** single-line
max width (`single_line_if_else_max_width`) are **implemented** — see
[What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Preset bundle for all width thresholds (Default/Off/Max) | `use_small_heuristics` |
| Keep a ternary / `if-else` on one line up to width | `single_line_if_else_max_width` ✅ |
| Annotation wrap widths | `attr_fn_like_width`, `inline_attribute_width` |
| Pack short array elements | `short_array_element_width_threshold` |

## 3. Wrapping shape (delimited lists wrap all-or-nothing)

Parameter-list layout (`fn_params_layout` — Tall / Compressed / Vertical) is **implemented** for
declaration parameter lists — see [What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Parameter layout: Tall / Compressed / Vertical (one per line) | `fn_params_layout`, `fn_args_layout` ✅ |
| Wrap binary expressions; operator at line-start (Front) vs. line-end (Back) | `binop_separator` ✅ |
| Let the last argument (lambda/array) overflow the call parentheses | `overflow_delimited_expr` ✅ |
| Trailing comma: Always / Never / Vertical (array initializers) | `trailing_comma` ✅ |
| Combine a control expression with its argument | `combine_control_expr` |

## 4. Spacing

Colon spacing (`space_after_colon`, `space_before_colon`), applied uniformly to every Java
colon context (ternary, enhanced-`for`, labels, `assert`, `case`/`default`), and
type-punctuation density (`type_punctuation_density`), governing the `&` of an intersection
type (`T extends A & B` and `(A & B) x`), are both **implemented** — see
[What it does today](#what-it-does-today). Nothing remains in this section:

| Capability | rustfmt equivalent |
| --- | --- |
| Space after `:` (ternary, enhanced-`for`, labels, `case x:`) | `space_after_colon` ✅ |
| Space before `:` | `space_before_colon` ✅ |
| Density of type punctuation (`T extends A & B`) | `type_punctuation_density` ✅ |

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

Hex literal case (`hex_literal_case`), float trailing zero (`float_literal_trailing_zero`), and
the Java-specific `L`/`F`/`D` suffix case (`literal-suffix-case`) are all **implemented** — see
[What it does today](#what-it-does-today). Remaining:

| Capability | rustfmt equivalent |
| --- | --- |
| Hex literal case (`0xFF` vs. `0xff`) | `hex_literal_case` ✅ |
| Float trailing zero (`1.0` vs. `1.`) | `float_literal_trailing_zero` ✅ |
| *(Java-specific extension)* `L`/`F`/`D` suffix case (`123l` vs. `123L`) | `literal-suffix-case` ✅ |
| *(Java-specific extension)* underscore grouping (`1000000` vs. `1_000_000`) | — |

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

- **Annotation placement** — annotations on their own line vs. inline. **Implemented**
  (`annotation-placement`: `compact` / `expanded`) for declaration-level targets — see
  [What it does today](#what-it-does-today). Remaining: finer per-target control and a
  single-marker-stays-inline exception (Checkstyle's `allowSamelineSingleParameterlessAnnotation`).
- **Modifier ordering** — canonical order of `public static final …`. **Implemented**
  (`reorder-modifiers`) — see [What it does today](#what-it-does-today).
- **`switch` arm style** — legacy `case:` vs. arrow `case ->`; lambda block conversion.

## Suggested priority

By Java-user impact: the remaining import-organization option (`imports_granularity`).
(Brace styling — `brace_style` and `control_brace_style` — empty-body collapse —
`empty_item_single_line` — single-statement-body collapse — `fn_single_line` — comment reflow —
`comment-width`
via `wrap_comments` — method-chain wrapping — `chain_width` — call-argument wrapping —
`fn_call_width` — array-initializer wrapping — `array_width` — import sorting —
`reorder_imports` — import grouping — `group_imports` — trailing commas —
`trailing_comma` — binary-expression wrapping — `binop_separator` — last-argument
overflow — `overflow_delimited_expr` — colon spacing — `space_before_colon` /
`space_after_colon` — parameter-list layout — `fn_params_layout` — type-punctuation
density — `type_punctuation_density` — modifier ordering — `reorder_modifiers` —
annotation placement — `annotation-placement` — hex-literal case —
`hex_literal_case` — float trailing zero — `float_literal_trailing_zero` — ternary
wrapping — `single_line_if_else_max_width` — and forcing every block multi-line —
`force_multiline_blocks` — are done.)
