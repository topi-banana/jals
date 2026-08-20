//! R4.1 — re-wrap a string concatenation that overflows the column limit.
//!
//! google-java-format's `StringWrapper`. Unlike everything else in the crate this is a **second
//! pass over the formatted text**: the output is re-parsed, each over-long `+` chain of string
//! literals is flattened and re-split, and the result is adopted only if formatting it again
//! reproduces it exactly.
//!
//! # Why a fixed-point check, not a proof
//!
//! Re-chunking changes where the literals' boundaries fall, which changes the widths the engine
//! measures, which can change where it breaks — a feedback edge the single-pass engine has
//! nowhere to express. Rather than reason about it, the pass **checks**: format the candidate and
//! keep it only if it is already formatted. A candidate that fails is discarded and the original
//! output stands, so the pass can never make a file worse, and `fmt ∘ fmt = fmt` survives.
//!
//! # It does add tokens
//!
//! A lone literal too long for its line **is** split into a concatenation of its own, which adds
//! `+` operators the source never had ([`plan`](api::plan)'s `LITERAL` arm).
//! Re-chunking a chain can also return fewer pieces than it took, so the `STRING_LITERAL` count
//! moves in both directions too. This is therefore not a rearrangement, and the `+`/string-piece
//! multiset is *not* preserved — a claim this header used to make, and one `DESIGN.md` §10 recorded
//! as an open question that the code had already answered.
//!
//! What survives instead is what each site **spells**, which is what
//! [`token_license`](super::token_license) declares and [`TokenBudget`](super::TokenBudget) checks:
//! where the pieces are cut is layout, what they spell together is the program.
//!
//! # What it will not do
//!
//! Reach outside a site. [`sites`](api::sites) is the single definition of which node this
//! pass may touch, and the fail-safe licenses exactly those, so an arithmetic `+` — or a `+` in a
//! chain with a non-literal operand — is still held to exact equality.
//!
//! Reach into a formatter-disabled region either. Being the *last* stage means being the last one
//! able to break `@formatter:off`, and unlike every other stage this one does not run through
//! [`Ctx`](crate::visit::Ctx), so it needs [`OffOn`] of its own ([`plan`](api::plan)).

use crate::ir;
use crate::passes::off_on;
use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use text_size::TextRange;

use crate::style::Style;

pub(crate) use api::{candidate, sites, text_block_content};

/// Re-wraps over-long string concatenations.
pub(crate) mod api {
    use super::{String, Style, SyntaxElement, SyntaxKind, SyntaxNode, TextRange, Vec, ir, off_on};

    /// The re-wrapped text, or `None` when nothing applies.
    ///
    /// The caller **must** run the candidate back through the formatter and keep it only if it
    /// comes back unchanged; the pass is defined by that check, not by this function.
    pub(crate) async fn candidate(formatted: &str, style: &Style) -> Option<String> {
        if !style.cfg.wrapping.reflow_long_strings {
            return None;
        }
        let parse = jals_syntax::Parse::parse(formatted).await;
        let edits = plan(&parse.syntax(), formatted, style);
        if edits.is_empty() {
            return None;
        }
        Some(splice(formatted, &edits))
    }

    /// Re-indent every text block to the line it starts on — `indentTextBlocks`.
    ///
    /// A text block's *incidental* whitespace is decided by its own least-indented line, so
    /// moving the declaration that holds it changes what its content means. Stripping the
    /// incidental whitespace and re-adding the line's own indentation is what keeps the string
    /// the author wrote while letting the layout move.
    fn text_block_edits(root: &SyntaxNode, src: &str) -> Vec<(TextRange, String)> {
        let mut edits = Vec::new();
        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| tok.kind() == SyntaxKind::TEXT_BLOCK)
        {
            let end = usize::from(tok.text_range().end());
            let start = src[..usize::from(tok.text_range().start())]
                .rfind('\n')
                .map_or(0, |at| at + 1);
            let text = &src[start..end];
            let Some(indented) = reindent_text_block(text) else {
                continue;
            };
            if indented != text {
                let range = TextRange::new(
                    u32::try_from(start).unwrap_or(0).into(),
                    u32::try_from(end).unwrap_or(0).into(),
                );
                edits.push((range, indented));
            }
        }
        edits
    }

    /// The re-indented form of a text block, measured from the start of its opening line.
    fn reindent_text_block(text: &str) -> Option<String> {
        let leading = text.find(|ch: char| !ch.is_whitespace())?;
        let mut initial = text.split('\n');
        let first = initial.next()?;
        let body: Vec<&str> = initial.collect();
        if body.is_empty() {
            return None;
        }
        let body_owned = strip_indent(&body);
        let stripped = &body_owned;
        let last_initial = body.last()?.trim_end().chars().count();
        let last_stripped = stripped.last()?.trim_end().chars().count();
        // A block whose closing delimiter already sits at column zero of its own content stays
        // there: javac would warn that the extra indentation is trailing whitespace anyway.
        let deindent = last_initial == last_stripped;
        let prefix: String = if deindent {
            String::new()
        } else {
            " ".repeat(leading)
        };

        let mut out = prefix.clone();
        out.push_str(first.trim_start());
        for (nth, line) in stripped.iter().enumerate() {
            let trimmed = line.trim_end();
            out.push('\n');
            if !trimmed.is_empty() {
                out.push_str(&prefix);
            }
            if nth + 1 == stripped.len() {
                let without = trimmed
                    .strip_suffix("\"\"\"")
                    .map_or(trimmed, str::trim_end);
                if !without.trim_start().is_empty() {
                    out.push_str(without);
                    out.push('\\');
                    out.push('\n');
                    out.push_str(&prefix);
                }
                out.push_str("\"\"\"");
            } else {
                out.push_str(line);
            }
        }
        Some(out)
    }

    /// Java's `String.stripIndent` over the body lines of a text block.
    ///
    /// The common indentation is the least of every non-blank line's *and* of the last line,
    /// blank or not — that last line is the closing delimiter, and it is what an author moves to
    /// choose the block's margin.
    fn strip_indent(lines: &[&str]) -> Vec<String> {
        // Counted in *characters*, as `String.stripIndent` does. Bytes would let a line indented
        // with a multi-byte space and one indented with an ASCII space agree on a cut that falls
        // inside a character.
        let indent_of = |line: &str| line.chars().take_while(|ch| ch.is_whitespace()).count();
        let mut common = usize::MAX;
        for (nth, line) in lines.iter().enumerate() {
            let last = nth + 1 == lines.len();
            if line.trim().is_empty() && !last {
                continue;
            }
            common = common.min(indent_of(line));
        }
        let common = if common == usize::MAX { 0 } else { common };
        lines
            .iter()
            .map(|line| {
                let cut = common.min(indent_of(line));
                let rest: String = line.chars().skip(cut).collect();
                rest.trim_end().into()
            })
            .collect()
    }

    /// What a text block *says*, with its incidental whitespace removed.
    ///
    /// [`reindent_text_block`](reindent_text_block) rewrites that whitespace, so the token's
    /// text is not comparable across the pass — but what the block spells is, and
    /// [`TokenBudget`](super::TokenBudget) checks it the way it checks a reflowed concatenation.
    pub(crate) fn text_block_content(text: &str) -> String {
        let mut lines = text.split('\n');
        let Some(first) = lines.next() else {
            return String::new();
        };
        let body: Vec<&str> = lines.collect();
        if body.is_empty() {
            return first.into();
        }
        let mut stripped = strip_indent(&body);
        if let Some(last) = stripped.last_mut() {
            *last = last
                .strip_suffix("\"\"\"")
                .map_or_else(|| last.clone(), |rest| rest.trim_end().into());
        }
        // A `\` at end of line continues it, so `foo\` + newline spells what `foo` alone does —
        // which is the one rewrite re-indenting is allowed to make.
        stripped.join("\n").replace("\\\n", "")
    }

    /// Every node this pass may re-split, in source order, with the pieces it is built from.
    ///
    /// This is [`plan`](plan)'s own eligibility test, lifted so that
    /// [`TokenBudget`](super::TokenBudget) licenses exactly the nodes the pass can touch. The two
    /// cannot disagree about which `+` is a string `+`, because there is one predicate — and that
    /// disagreement is the shape of defect this seam exists to make impossible.
    pub(crate) fn sites(root: &SyntaxNode) -> Vec<(SyntaxNode, Vec<String>)> {
        root.descendants()
            // Outermost only: a nested chain is handled by its root, and editing both would
            // produce overlapping ranges. A lone literal inside one is likewise its root's.
            .filter(|node| {
                node.parent()
                    .is_none_or(|parent| parent.kind() != SyntaxKind::BINARY_EXPR)
            })
            .filter_map(|node| site_pieces(&node).map(|pieces| (node, pieces)))
            .collect()
    }

    /// The pieces `node` is built from, or `None` when it is not a site.
    fn site_pieces(node: &SyntaxNode) -> Option<Vec<String>> {
        match node.kind() {
            SyntaxKind::BINARY_EXPR => concatenation(node),
            // A single literal too long for its line is split into a concatenation of its own —
            // this is the case `reflow-long-strings` mostly exists for, and the one place in the
            // crate where `+` operators are added.
            SyntaxKind::LITERAL => literal_body(node).map(|body| alloc::vec![body]),
            _ => None,
        }
    }

    /// The replacements to make, in source order.
    ///
    /// **Not guaranteed non-overlapping.** A text-block edit's range reaches back to the start of the
    /// block's opening line, so it can cover a concatenation site that shares that line; the two
    /// families are otherwise disjoint, since a `TEXT_BLOCK` is never a site
    /// ([`literal_body`](literal_body) demands a `STRING_LITERAL`). Sorting decides which of
    /// such a pair comes first, and [`splice`](splice) drops the other rather than corrupt the
    /// file — so the cost is one re-indent or one rewrap not happening, on a shape the formatter has
    /// already put on separate lines by the time this pass runs.
    ///
    /// What the sort *does* fix is that the survivor used to be decided by collection order rather
    /// than position: a concatenation earlier in the file than any text block was dropped for no
    /// other reason.
    fn plan(root: &SyntaxNode, src: &str, style: &Style) -> Vec<(TextRange, String)> {
        let mut edits = text_block_edits(root, src);
        for (node, pieces) in sites(root) {
            let range = node.text_range();
            if !overflows(src, range, style) {
                continue;
            }
            // The node's range starts at its leading trivia; the column that matters is where
            // its first significant token lands.
            let start = node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .find(|tok| !tok.kind().is_trivia())
                .map_or_else(|| range.start(), |tok| tok.text_range().start());
            let column = column_of(src, usize::from(start));
            // What follows the concatenation on its line — the `);` of `foo("…");` — has to
            // fit after the last chunk, so the last line's budget answers for it.
            let end = usize::from(range.end());
            let trailing = src[end..].find('\n').map_or(src.len() - end, |at| at);
            if let Some(text) = rewrap(&pieces, column, trailing, style) {
                edits.push((range, text));
            }
        }
        // `splice` walks the source once and drops any edit that starts behind where it already
        // is, so the two families have to be merged rather than appended: text-block edits are
        // collected first, and a concatenation earlier in the file would otherwise be discarded
        // silently. Sorting is stable, so an edit's family no longer decides whether it survives.
        edits.sort_by_key(|(range, _)| range.start());
        // A formatter-disabled region has to survive byte-identical, and this is L4 — the last stage
        // that can still write into one. `OffOn` reaches the lowering walk through `Ctx`, which this
        // pass runs *after* and over re-parsed text, so the veto has to be applied here as well.
        let disabled = off_on::scan(root, style);
        edits.retain(|(range, _)| !self::disabled(&disabled, *range));
        edits
    }

    /// Whether `range` reaches into a formatter-disabled region.
    ///
    /// Any overlap at all disqualifies the edit, not just containment: an edit that starts outside a
    /// region and ends inside it would rewrite the region's opening just as surely.
    ///
    /// The regions are a *veto* over [`sites`](sites), deliberately not folded into it. `sites`
    /// is the predicate the fail-safe shares, and it answers over one tree; a disabled region is a
    /// property of the run's config, and the input's regions and the output's live in different
    /// coordinates. Keeping it here leaves the license **wider** than the pass, which is the safe
    /// direction — a pass that changes less than it is licensed to can never trip the fail-safe,
    /// whereas a license narrower than the pass is exactly the defect the table was written to fix.
    fn disabled(regions: &[TextRange], range: TextRange) -> bool {
        regions.iter().any(|region| {
            region
                .intersect(range)
                .is_some_and(|shared| !shared.is_empty())
        })
    }

    /// The string literals of a pure `+` concatenation, or `None` when the node is anything else.
    ///
    /// "Pure" means every leaf is a `STRING_LITERAL` and every operator is `+`. A chain with a
    /// non-literal operand cannot be re-split without changing evaluation, and a text block
    /// carries its own layout, so both are refused.
    fn concatenation(node: &SyntaxNode) -> Option<Vec<String>> {
        let mut pieces = Vec::new();
        if !collect(node, &mut pieces) || pieces.len() < 2 {
            return None;
        }
        Some(pieces)
    }

    /// Walk a `+` chain, pushing each literal's body. Returns whether the chain stayed pure.
    fn collect(node: &SyntaxNode, out: &mut Vec<String>) -> bool {
        for child in node.children_with_tokens() {
            match child {
                SyntaxElement::Token(tok) if tok.kind().is_trivia() => {}
                SyntaxElement::Token(tok) if tok.kind() == SyntaxKind::PLUS => {}
                SyntaxElement::Token(_) => return false,
                SyntaxElement::Node(child) => match child.kind() {
                    SyntaxKind::BINARY_EXPR => {
                        if !collect(&child, out) {
                            return false;
                        }
                    }
                    SyntaxKind::LITERAL => {
                        let Some(body) = literal_body(&child) else {
                            return false;
                        };
                        out.push(body);
                    }
                    _ => return false,
                },
            }
        }
        true
    }

    /// The text between a string literal's quotes, or `None` for anything that is not one.
    fn literal_body(node: &SyntaxNode) -> Option<String> {
        let tok = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|tok| !tok.kind().is_trivia())?;
        if tok.kind() != SyntaxKind::STRING_LITERAL {
            return None;
        }
        let text = tok.text();
        Some(text.strip_prefix('"')?.strip_suffix('"')?.into())
    }

    /// The column a byte offset sits at.
    fn column_of(src: &str, offset: usize) -> usize {
        let start = src[..offset].rfind('\n').map_or(0, |at| at + 1);
        ir::utf16(&src[start..offset])
    }

    /// Whether the concatenation is worth touching: one of the lines it occupies is over the
    /// limit.
    ///
    /// Spanning lines is *not* enough. A concatenation the author already broken into short
    /// pieces — a generated table of `"\u0000\u0000…"` rows, say — is under the limit on every
    /// line and google-java-format leaves it exactly as written: its `LongStringsAndTextBlockScanner`
    /// only collects a literal whose own line runs past the column limit.
    fn overflows(src: &str, range: TextRange, style: &Style) -> bool {
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let from = src[..start].rfind('\n').map_or(0, |at| at + 1);
        let to = src[end..].find('\n').map_or(src.len(), |at| end + at);
        src[from..to]
            .split('\n')
            .any(|line| ir::utf16(line) > style.max_width())
    }

    /// Re-chunk the pieces so each one fits a continuation line.
    ///
    /// The result is emitted **flat** — `"a" + "b" + "c"` on one logical line — and the caller
    /// re-formats it. Choosing the line breaks here as well would mean guessing what the engine
    /// is about to decide, and a guess that misses is exactly what the fixed-point check throws
    /// away. Re-splitting is the part the engine cannot do; placing breaks is the part only it
    /// should.
    ///
    /// Returns `None` when the budget is too small to make progress — a deeply indented
    /// concatenation with a narrow limit — rather than emitting one character per line.
    fn rewrap(pieces: &[String], column: usize, trailing: usize, style: &Style) -> Option<String> {
        // The first chunk starts where the literal already is and pays only for its quotes; a
        // continuation line pays for its indent and for `+ ` on top of that
        // (`width -= 6` in google-java-format's `reflow`, for its four-column indent).
        let first = style.max_width().checked_sub(column + 2)?;
        let budget = first.checked_sub(style.continuation_cols + 2)?;
        if budget < 16 {
            return None;
        }

        let joined: String = pieces.concat();
        let chunks = split(&joined, first, budget, trailing);
        // Nothing to gain when the re-split reproduces the pieces it started from.
        if chunks.len() < 2 || chunks == pieces {
            return None;
        }

        let mut out = String::new();
        for (nth, chunk) in chunks.iter().enumerate() {
            if nth > 0 {
                out.push_str(" + ");
            }
            out.push('"');
            out.push_str(chunk);
            out.push('"');
        }
        Some(out)
    }

    /// Split a literal body into chunks of at most `budget` columns, breaking after a space where
    /// possible and never inside an escape sequence.
    fn split(body: &str, first: usize, rest: usize, trailing: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut width = 0usize;
        let mut last_space: Option<usize> = None;
        let total = ir::utf16(body);
        let mut consumed = 0usize;

        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            let mut unit = String::new();
            unit.push(c);
            // An escape is one indivisible unit; `\uXXXX` and the octal forms are longer, and
            // splitting any of them would change what the literal means.
            if c == '\\'
                && let Some(next) = chars.next()
            {
                {
                    unit.push(next);
                    if next == 'u' {
                        for _ in 0..4 {
                            match chars.peek() {
                                Some(hex) if hex.is_ascii_hexdigit() => {
                                    unit.push(chars.next().unwrap_or('0'));
                                }
                                _ => break,
                            }
                        }
                    }
                }
            }

            // The break falls *before* a space, so the space itself is a break point: a chunk that
            // ends exactly at the budget is a chunk that fits. google-java-format reaches the same
            // answer by adding whole words — each already carrying its leading space.
            if c == ' ' {
                last_space = Some(current.len());
            }
            let unit_width = ir::utf16(&unit);
            let mut budget = if chunks.is_empty() { first } else { rest };
            // Once what is left fits, this is the last chunk and it shares its line with whatever
            // follows the concatenation.
            if total - consumed <= budget {
                budget = budget.saturating_sub(trailing);
            }
            if width + unit_width > budget && !current.is_empty() {
                match last_space {
                    Some(at) if at > 0 => {
                        let tail = current.split_off(at);
                        consumed += ir::utf16(&current);
                        chunks.push(core::mem::take(&mut current));
                        current = tail;
                        width = ir::utf16(&current);
                    }
                    _ => {
                        consumed += width;
                        chunks.push(core::mem::take(&mut current));
                        width = 0;
                    }
                }
                last_space = None;
            }

            current.push_str(&unit);
            width += unit_width;
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        chunks
    }

    /// Apply non-overlapping replacements to `src`.
    fn splice(src: &str, edits: &[(TextRange, String)]) -> String {
        let mut out = String::with_capacity(src.len());
        let mut at = 0usize;
        for (range, text) in edits {
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            // Overlapping or out-of-range edits are dropped rather than trusted: a bad splice
            // would corrupt the file, and skipping one only means a concatenation stays as it was.
            if start < at || end > src.len() {
                continue;
            }
            out.push_str(&src[at..start]);
            out.push_str(text);
            at = end;
        }
        out.push_str(&src[at..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use jals_config::fmt::Config;

    use super::api;
    use crate::style::Style;

    /// A literal past the column limit, and the `class` wrapper to hold it.
    const LONG: &str =
        "\"a single very long literal that runs well past the hundred column limit and then some\"";

    /// [`LONG`] inside an `@formatter:off` region — the same source for both `formatter-tags` values,
    /// so the only difference between the two tests is the rule.
    fn in_disabled_region() -> String {
        alloc::format!(
            "class X {{\n  // @formatter:off\n  String k = {LONG};\n  // @formatter:on\n}}\n"
        )
    }

    /// `candidate` under a config with `reflow-long-strings` on and `formatter-tags` as given.
    fn candidate(src: &str, tags: bool) -> Option<String> {
        let mut cfg = Config::default();
        cfg.wrapping.reflow_long_strings = true;
        cfg.layout.formatter_tags = tags;
        let (style, _) = Style::reify(&cfg, src, jals_config::FeatureSet::default());
        jals_exec::block_on_inline(api::candidate(src, &style))
    }

    #[test]
    fn a_literal_inside_a_disabled_region_is_never_re_split() {
        // L4 runs over re-parsed text and never sees `Ctx`, so before this veto existed it happily
        // rewrote `@formatter:off` — and did it badly, eating the space after `=`, because nothing
        // re-formats a disabled region afterwards to tidy up.
        assert_eq!(
            candidate(&in_disabled_region(), true),
            None,
            "a disabled region must survive byte-identical, so there is nothing to splice",
        );
    }

    #[test]
    fn the_same_literal_outside_one_is_still_re_split() {
        // The control: the veto has to cost the region and nothing else, or "nothing applies" would
        // be indistinguishable from "the pass stopped working".
        let src = alloc::format!("class X {{\n  String k = {LONG};\n}}\n");
        let rewrapped = candidate(&src, true).expect("an over-long literal is a site");
        assert!(
            rewrapped.contains(" + "),
            "the literal should have been split into a concatenation:\n{rewrapped}",
        );
    }

    #[test]
    fn the_region_is_only_spared_while_formatter_tags_is_on() {
        // `off_on::scan` returns nothing when the rule is off, so the marker is an ordinary comment
        // and the literal inside it is an ordinary site. Pins the veto to the rule that means it.
        assert!(
            candidate(&in_disabled_region(), false).is_some(),
            "with `formatter-tags` off there is no region to respect",
        );
    }
}
