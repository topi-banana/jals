<!--
  Thanks for contributing to jals!

  Please write this PR description in English (matching the repository's
  "code comments and docs are written in English" convention in CLAUDE.md).

  This template mirrors the structure most PRs in this repo already use.
  Fill in each section, then DELETE any section that does not apply to your
  change. The HTML comments (like this one) are guidance only — they are not
  rendered on the PR page, so you may leave them in or strip them out.

  Keep the title in Conventional Commits form, scoped by crate, e.g.:
    feat(jals-fmt): add space-before-colon and space-after-colon
    fix(jals-syntax): parse primitive and array class literals
    refactor(jals-lsp): extract import layout into imports module
-->

## Summary

<!--
  What does this PR do, and why? One or two short paragraphs.
  If it advances a README roadmap item, say which one. Link related issues:
  e.g. "Related issue: #123" or "Related issue: N/A".
-->

## What changed

<!--
  The concrete changes, ideally grouped by file or module
  (e.g. `config.rs`, `lower.rs`, `doc.rs`, `render.rs`, `grammar.rs`).
  A small table or bullet list per area works well. Include a short
  before/after example or `java` snippet when it clarifies the behavior.
-->

## Invariants

<!--
  REQUIRED when touching the lexer, parser, or formatter; delete otherwise.

  State how this change relates to the invariants in CLAUDE.md (lossless lexer,
  never panics, always a tree, formatter fidelity / idempotency, wasm32 compat).
  If you relax the significant-token guarantee (reorder-imports, group-imports,
  trailing-comma), spell out exactly what is preserved (sequence vs. multiset)
  and confirm idempotency and comment preservation still hold.
-->

## Testing

<!--
  How did you verify this? List new/updated tests (snapshot, unit, proptest)
  and what they cover. Mention any manual / end-to-end checks (e.g. an stdio
  smoke test for jals-lsp, or a langtools-corpus run for jals-syntax).
-->

<!--
  If AI generated or substantially assisted with this PR, replace this comment
  with an attribution at the very end of the PR description. Name both the tool
  and the exact model used, for example:

  🤖 Generated with [Claude Code](https://claude.com/claude-code)
  Model: Fable 5

  or:

  🤖 Generated with [Codex](https://openai.com/codex)
  Model: GPT-5.6 sol

  Delete this comment if no AI tool was used.
-->
