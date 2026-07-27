//! L3 comment reflow — the nested mini-formatter for comment prose and Javadoc.
//!
//! This is the one part of the crate that is not a Java formatter. Javadoc has its own grammar
//! (block tags, inline tags, HTML, fenced code) and google-java-format gives it its own
//! `JavadocFormatter`; Eclipse gives it a 26-setting family of its own. It is not a per-node rule
//! and does not compose with the layout engine — it turns one comment's text into another
//! comment's text, and the result rides through the document as a single
//! [`Doc::Tok`](crate::ir::Doc::Tok).
//!
//! # What is never reflowed
//!
//! Content whose line structure carries meaning: a `<pre>` block, a fenced code region, a
//! multi-line `{@code …}`, and — unless `format-source-in-comments` is on — anything inside them.
//! Reflowing those would change what the comment *says*, not just how it looks.
//!
//! # Width
//!
//! The budget comes from [`Style::comment_width`], measured against the comment's **structural**
//! indent (nesting depth × indent width) rather than the column the engine will finally choose.
//! That keeps reflow a function of the tree: a comment never re-wraps because a sibling
//! expression happened to grow.
//!
//! [`Style::comment_width`]: crate::style::Style::comment_width

use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::SyntaxKind;

use crate::ir::Width;
use crate::style::Style;

/// One logical piece of a comment body.
enum Block {
    /// Prose to be refilled, with the extra indent its first and continuation lines take.
    ///
    /// Only an HTML list moves either off zero: `<li>` sits one list level in and the lines that
    /// continue it one item level further, which is `JavadocWriter`'s `continuingListStack` plus
    /// `continuingListItemStack`.
    Prose {
        /// The words to refill.
        words: Vec<String>,
        /// Columns of extra indent on the first line.
        first: usize,
        /// Columns of extra indent on every line after it.
        rest: usize,
    },
    /// Lines to be emitted exactly as they are — fenced code, `<pre>`, a table.
    Verbatim(Vec<String>),
    /// A block tag: its name (`@param`), an optional first argument, and its description words.
    Tag {
        /// The tag itself, `@`-prefixed.
        name: String,
        /// The parameter or exception name, for the tags that take one.
        argument: Option<String>,
        /// The description, as words to refill.
        words: Vec<String>,
    },
    /// A blank line the author wrote.
    Blank,
}

/// Reflows comment bodies.
pub(crate) struct CommentFormatter;

impl CommentFormatter {
    /// The text to emit for a comment, reflowed when the matching `[comments]` rule is on.
    ///
    /// `indent` is the comment's structural indent in columns. `is_header` marks the file's
    /// leading comment, which `format-header` gates separately.
    pub(crate) fn render(
        text: &str,
        kind: SyntaxKind,
        indent: usize,
        is_header: bool,
        style: &Style,
    ) -> String {
        let cfg = style.comments();
        let enabled = match kind {
            SyntaxKind::LINE_COMMENT => cfg.format_line,
            SyntaxKind::BLOCK_COMMENT => cfg.format_block,
            SyntaxKind::DOC_COMMENT => cfg.format_javadoc,
            _ => false,
        };
        if !enabled || (is_header && !cfg.format_header) {
            return text.into();
        }
        match kind {
            SyntaxKind::LINE_COMMENT => Self::render_line(text, indent, style),
            _ => Self::render_block(text, kind, indent, style),
        }
    }

    /// Wrap a `//` comment, continuing with further `//` lines.
    ///
    /// A line comment is **never refilled**. Its words are not redistributed and its internal
    /// spacing is left alone — `// a    b` keeps its run of spaces, because a `//` line is as
    /// often a hand-aligned table or a commented-out statement as it is prose. All that happens
    /// is the missing space after the slashes and a break when the line overruns the limit,
    /// which is exactly `JavaCommentsHelper.wrapLineComments`.
    fn render_line(text: &str, indent: usize, style: &Style) -> String {
        let line = Self::space_after_slashes(text.trim());
        // `// MOE:` marks a region another tool owns, and wrapping it would break that tool.
        if line.starts_with("// MOE:") {
            return line;
        }
        let limit = style.comment_width(indent);
        Self::wrap_line(&line, indent, limit).join("\n")
    }

    /// `//foo` → `// foo`.
    ///
    /// `//noinspection` and `//$NON-NLS-1$` are IDE directives that stop working with a space in
    /// them, so they are left alone — google-java-format's
    /// `LINE_COMMENT_MISSING_SPACE_PREFIX` lookahead.
    fn space_after_slashes(line: &str) -> String {
        let slashes = line.len() - line.trim_start_matches('/').len();
        if slashes < 2 {
            return line.into();
        }
        let rest = &line[slashes..];
        let directive = rest.starts_with("noinspection") || Self::is_non_nls(rest);
        // A leading space, an empty body, or another slash: nothing to separate.
        if directive || rest.starts_with(|c: char| c.is_whitespace()) || rest.is_empty() {
            return line.into();
        }
        let slashes = &line[..slashes];
        alloc::format!("{slashes} {rest}")
    }

    /// Whether `rest` opens with an Eclipse externalized-string marker, `$NON-NLS-<digits>$`.
    fn is_non_nls(rest: &str) -> bool {
        let Some(body) = rest.strip_prefix("$NON-NLS-") else {
            return false;
        };
        let digits: &str = body.split('$').next().unwrap_or("");
        !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
            && body.len() > digits.len()
    }

    /// Break `line` at whitespace until it fits, restarting each continuation with `//`.
    fn wrap_line(line: &str, column: usize, limit: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::from(line);
        while column + Width::utf16(&current) > limit {
            let Some(at) = Self::break_at(&current, limit.saturating_sub(column)) else {
                break;
            };
            let (head, tail) = current.split_at(at);
            let next = alloc::format!("//{tail}");
            lines.push(head.trim_end().into());
            current = next;
        }
        lines.push(current.trim_end().into());
        lines
    }

    /// The byte offset of the last whitespace at or before `budget` columns, past the `//`.
    ///
    /// `None` means there is nowhere to break — a single long word, or a URL — and the line stays
    /// over the limit rather than being cut mid-token.
    fn break_at(line: &str, budget: usize) -> Option<usize> {
        let mut best = None;
        let mut column = 0usize;
        for (at, ch) in line.char_indices() {
            if column > budget {
                break;
            }
            if column > 2 && ch.is_whitespace() {
                best = Some(at);
            }
            column += ch.len_utf16();
        }
        best
    }

    /// Reflow a `/* … */` or `/** … */` comment.
    fn render_block(text: &str, kind: SyntaxKind, indent: usize, style: &Style) -> String {
        let cfg = style.comments();
        let doc = kind == SyntaxKind::DOC_COMMENT;
        let opener = if doc { "/**" } else { "/*" };

        let Some(body) = Self::body(text, doc) else {
            return text.into();
        };
        let blocks = Self::parse(&body, style);
        if blocks.is_empty() {
            return if doc { "/** */".into() } else { "/* */".into() };
        }

        // A comment that is one short paragraph collapses to a single line, which is what
        // google-java-format does and what most one-line Javadoc already looks like.
        if let [
            Block::Prose {
                words, first: 0, ..
            },
        ] = blocks.as_slice()
        {
            let inline = words.join(" ");
            let width = indent + Width::utf16(opener) + Width::utf16(&inline) + 4;
            if width <= style.comment_width(indent) && !inline.is_empty() {
                return alloc::format!("{opener} {inline} */");
            }
        }

        let prefix_width = if cfg.leading_asterisks { 3 } else { 1 };
        let budget = style
            .comment_width(indent)
            .saturating_sub(indent + prefix_width)
            .max(16);

        // `align-tag-descriptions` lines every description up under one column, so the width is
        // a property of the whole comment rather than of each tag.
        let aligned = cfg.align_tag_descriptions.then(|| {
            blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Tag { name, argument, .. } => Some(
                        Width::utf16(name) + argument.as_deref().map_or(0, |a| Width::utf16(a) + 1),
                    ),
                    _ => None,
                })
                .max()
                .unwrap_or(0)
        });

        let mut out = String::from(opener);
        let mut seen_tag = false;
        let mut previous: Option<&Block> = None;
        for block in &blocks {
            match block {
                // Once the block tags start there are no more blank lines: only the *first* one
                // is separated from the description, and `JavadocWriter` requests a plain newline
                // between the rest.
                Block::Blank => {
                    if cfg.preserve_blank_lines && !seen_tag {
                        Self::push_line(&mut out, "", cfg.leading_asterisks);
                    }
                }
                Block::Prose { words, first, rest } => {
                    let lines = Self::fill_two(
                        words,
                        budget.saturating_sub(*first).max(16),
                        budget.saturating_sub(*rest).max(16),
                    );
                    for (nth, line) in lines.iter().enumerate() {
                        let pad = if nth == 0 { *first } else { *rest };
                        Self::push_indented(&mut out, line, pad, cfg.leading_asterisks);
                    }
                }
                Block::Verbatim(lines) => {
                    for line in lines {
                        Self::push_line(&mut out, line, cfg.leading_asterisks);
                    }
                }
                Block::Tag {
                    name,
                    argument,
                    words,
                } => {
                    // The blank line separates the tags from a *description*: a comment that is
                    // nothing but tags opens with none. And a blank line the author already
                    // wrote satisfies the rule, so asking for a second would open with two.
                    if !seen_tag
                        && cfg.blank_line_before_tags
                        && previous.is_some()
                        && !matches!(previous, Some(Block::Blank))
                    {
                        Self::push_line(&mut out, "", cfg.leading_asterisks);
                    }
                    seen_tag = true;
                    Self::push_tag(
                        &mut out,
                        name,
                        argument.as_deref(),
                        words,
                        budget,
                        aligned,
                        style,
                    );
                }
            }
            previous = Some(block);
        }
        out.push('\n');
        if cfg.leading_asterisks {
            out.push(' ');
        }
        out.push_str("*/");
        out
    }

    /// Emit one body line, with or without the leading `*`.
    ///
    /// Continuation lines start at column zero here; the writer re-aligns them under the opening
    /// delimiter when it knows the final column.
    fn push_line(out: &mut String, text: &str, asterisks: bool) {
        out.push('\n');
        if asterisks {
            out.push('*');
            if !text.is_empty() {
                out.push(' ');
            }
        }
        out.push_str(text);
    }

    /// Emit one body line with `pad` columns of extra indent.
    fn push_indented(out: &mut String, text: &str, pad: usize, asterisks: bool) {
        if pad == 0 {
            Self::push_line(out, text, asterisks);
            return;
        }
        let mut padded = String::with_capacity(pad + text.len());
        for _ in 0..pad {
            padded.push(' ');
        }
        padded.push_str(text);
        Self::push_line(out, &padded, asterisks);
    }

    /// Emit a block tag and its refilled description.
    fn push_tag(
        out: &mut String,
        name: &str,
        argument: Option<&str>,
        words: &[String],
        budget: usize,
        aligned: Option<usize>,
        style: &Style,
    ) {
        let cfg = style.comments();
        let mut head = String::from(name);
        if let Some(argument) = argument {
            head.push(' ');
            head.push_str(argument);
        }
        // Under alignment every head is padded to the widest, so the descriptions share a column.
        if let Some(column) = aligned {
            while Width::utf16(&head) < column {
                head.push(' ');
            }
        }

        // A continuation line is indented by one continuation step, not aligned under the
        // description: google-java-format's `innerIndent()` adds a flat `+4` while a footer tag
        // is being continued. Lining descriptions up under a shared column is
        // `align-tag-descriptions`, which is a separate rule.
        let continuation = if cfg.indent_tag_description {
            style.continuation_cols
        } else {
            0
        };
        let first_budget = budget.saturating_sub(Width::utf16(&head) + 1).max(8);
        let rest_budget = budget.saturating_sub(continuation).max(8);

        if words.is_empty() {
            Self::push_line(out, &head, cfg.leading_asterisks);
            return;
        }

        let lines = Self::fill_two(words, first_budget, rest_budget);
        for (nth, line) in lines.iter().enumerate() {
            if nth == 0 {
                let mut text = head.clone();
                text.push(' ');
                text.push_str(line);
                Self::push_line(out, &text, cfg.leading_asterisks);
            } else {
                let mut text = String::new();
                for _ in 0..continuation {
                    text.push(' ');
                }
                text.push_str(line);
                Self::push_line(out, &text, cfg.leading_asterisks);
            }
        }
    }

    /// The text between a comment's delimiters, with each line's leading `*` removed.
    fn body(text: &str, doc: bool) -> Option<String> {
        let inner = if doc {
            text.strip_prefix("/**")?
        } else {
            text.strip_prefix("/*")?
        };
        let inner = inner.strip_suffix("*/")?;
        let mut out = String::with_capacity(inner.len());
        for (nth, line) in inner.split('\n').enumerate() {
            if nth > 0 {
                out.push('\n');
            }
            let line = line.trim_end_matches('\r');
            let stripped = line.trim_start();
            if nth > 0 && stripped.starts_with('*') && !stripped.starts_with("*/") {
                out.push_str(stripped.trim_start_matches('*'));
            } else {
                out.push_str(line);
            }
        }
        Some(out)
    }

    /// Split a comment body into prose paragraphs, verbatim regions, and block tags.
    fn parse(body: &str, style: &Style) -> Vec<Block> {
        let cfg = style.comments();
        let mut blocks: Vec<Block> = Vec::new();
        let mut prose: Vec<String> = Vec::new();
        // `Some((lines, snippet))` — the region being collected verbatim, and whether it is a
        // `{@snippet …}` (which ends at a `}` rather than at a closing HTML tag).
        let mut fence: Option<(Vec<String>, bool)> = None;
        // A `<p>` waiting for the word it introduces, so the two are refilled as one unit.
        let mut pending: Option<String> = None;
        // The HTML list nesting depth, and the indents the prose being accumulated belongs at.
        // A list indents its contents by two columns per level and an item's continuation lines
        // by four more — `JavadocWriter`'s `continuingListStack` and `continuingListItemStack`.
        let mut depth = 0usize;
        let (mut first, mut rest) = (0usize, 0usize);

        for raw in body.split('\n') {
            let mut line = raw.trim();
            let stripped;
            if cfg.format_html && Self::has_paragraph_close(line) {
                // google-java-format drops `</p>` outright (`case ParagraphCloseTag -> {}`).
                stripped = Self::drop_paragraph_close(line);
                line = stripped.trim();
                if line.is_empty() {
                    continue;
                }
            }

            if let Some((lines, snippet)) = &mut fence {
                lines.push(line.into());
                let closed = if *snippet {
                    line.starts_with('}')
                } else {
                    Self::closes_fence(line)
                };
                if closed {
                    blocks.push(Block::Verbatim(core::mem::take(lines)));
                    fence = None;
                }
                continue;
            }
            if !cfg.format_source_in_comments && Self::opens_fence(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                let lines = alloc::vec![String::from(line)];
                if Self::self_closing_fence(line) {
                    blocks.push(Block::Verbatim(lines));
                } else {
                    fence = Some((lines, line.contains("{@snippet")));
                }
                continue;
            }
            if line.is_empty() {
                Self::flush(&mut prose, &mut blocks, first, rest);
                // A run of blank lines is one paragraph break: `JavadocWriter.requestBlankLine`
                // sets a flag, so asking twice still yields one. And the footer section has no
                // blank lines at all, so one written inside it is not a break in the text either
                // — dropping it here is what keeps a tag's description continuing across it.
                if !matches!(blocks.last(), Some(Block::Blank | Block::Tag { .. })) {
                    blocks.push(Block::Blank);
                }
                continue;
            }
            if let Some(tag) = Self::block_tag(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                blocks.push(tag);
                continue;
            }
            // `<p>` opens a paragraph: a blank line before it, and the tag glued to the word it
            // introduces (`<p>This method …`). An opening `<p>` before any prose is dropped, as
            // `JavadocWriter.writeParagraphOpen` does when nothing significant has been written.
            if cfg.format_html
                && let Some(after) = Self::paragraph_open(line)
            {
                Self::flush(&mut prose, &mut blocks, first, rest);
                // "Nothing significant written yet" is what makes an opening `<p>` disappear, and
                // a run of blank lines is not significant.
                if blocks.iter().any(|block| !matches!(block, Block::Blank)) {
                    // A blank line the author already wrote *is* the paragraph break.
                    if !matches!(blocks.last(), Some(Block::Blank)) {
                        blocks.push(Block::Blank);
                    }
                    pending = Some("<p>".into());
                }
                line = after;
                if line.is_empty() {
                    continue;
                }
            }
            // An HTML block element starts a new paragraph, so list items keep their own lines
            // instead of being refilled into the previous sentence.
            if cfg.format_html && Self::is_html_block(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
            }
            // A heading stands alone between blank lines, and what follows it starts a paragraph
            // of its own rather than continuing the heading's line.
            if cfg.format_html && Self::is_heading(line) {
                if !matches!(blocks.last(), Some(Block::Blank) | None) {
                    blocks.push(Block::Blank);
                }
                blocks.push(Block::Prose {
                    words: line.split_whitespace().map(Into::into).collect(),
                    first: 0,
                    rest: 0,
                });
                blocks.push(Block::Blank);
                continue;
            }
            if cfg.format_html {
                // The indents belong to the *block*, decided by the line that starts it: a line
                // continuing an item is at the item's continuation indent whatever it looks like
                // on its own, and re-deciding per line would move the item's first line too.
                // They are read *before* this line's own tags, so a line that opens a list is
                // still at the enclosing level.
                if prose.is_empty() {
                    (first, rest) = Self::list_indents(depth, line);
                }
                depth = Self::list_depth(depth, line);
            }
            // A tag's description continues onto the following lines.
            if let Some(Block::Tag { words, .. }) = blocks.last_mut()
                && prose.is_empty()
            {
                words.extend(line.split_whitespace().map(Into::into));
                continue;
            }
            for word in line.split_whitespace() {
                if let Some(mut glued) = pending.take() {
                    glued.push_str(word);
                    prose.push(glued);
                    continue;
                }
                prose.push(word.into());
            }
        }
        if let Some(glued) = pending.take() {
            prose.push(glued);
        }
        if let Some((mut lines, _)) = fence {
            // The region never closed — an unbalanced `{@code`, a `<pre>` with no `</pre>`. Its
            // trailing blank lines are the gap above `*/`, not content, and keeping them would
            // grow the comment by one line on every run.
            while lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.pop();
            }
            blocks.push(Block::Verbatim(lines));
        }
        Self::flush(&mut prose, &mut blocks, first, rest);

        // Trailing blank lines are layout, not content.
        while matches!(blocks.last(), Some(Block::Blank)) {
            blocks.pop();
        }
        while matches!(blocks.first(), Some(Block::Blank)) {
            blocks.remove(0);
        }
        if cfg.format_html {
            Self::infer_paragraph_tags(&mut blocks);
        }
        blocks
    }

    /// Whether a block is a section heading.
    ///
    /// `inferParagraphTags` only inserts a `<p>` between two *literals*; a heading's close tag is
    /// its own token, so the paragraph after a heading opens without one.
    fn ends_heading(block: &Block) -> bool {
        let Block::Prose { words, .. } = block else {
            return false;
        };
        words.last().is_some_and(|word| {
            word.to_ascii_lowercase().ends_with("</h1>")
                || word.to_ascii_lowercase().ends_with("</h2>")
                || word.to_ascii_lowercase().ends_with("</h3>")
                || word.to_ascii_lowercase().ends_with("</h4>")
                || word.to_ascii_lowercase().ends_with("</h5>")
                || word.to_ascii_lowercase().ends_with("</h6>")
        })
    }

    /// Insert a `<p>` wherever a blank line separates two runs of prose.
    ///
    /// A blank line between paragraphs is a paragraph break the author made in the *comment*; the
    /// rendered Javadoc only sees it if an HTML tag says so. google-java-format's
    /// `inferParagraphTags` does the same, and only between two literals — a blank line before a
    /// block tag or a `<pre>` region opens nothing.
    fn infer_paragraph_tags(blocks: &mut [Block]) {
        for at in 2..blocks.len() {
            if !matches!(blocks[at - 1], Block::Blank)
                || !matches!(blocks[at - 2], Block::Prose { .. })
                || Self::ends_heading(&blocks[at - 2])
            {
                continue;
            }
            let Block::Prose {
                words, first: 0, ..
            } = &mut blocks[at]
            else {
                continue;
            };
            let Some(first) = words.first_mut() else {
                continue;
            };
            if !first.starts_with('<') {
                first.insert_str(0, "<p>");
            }
        }
    }

    /// Move any accumulated prose into `blocks`, at `first` / `rest` columns of extra indent.
    fn flush(prose: &mut Vec<String>, blocks: &mut Vec<Block>, first: usize, rest: usize) {
        if !prose.is_empty() {
            blocks.push(Block::Prose {
                words: core::mem::take(prose),
                first,
                rest,
            });
        }
    }

    /// Whether a line opens a region whose layout must be preserved.
    /// A multi-line `{@code …}` region has to *start* a line to count. Refilling can leave a
    /// wrapped inline `{@code X}` with its opener at the end of a line, and treating that as a
    /// fence would freeze the rest of the comment on the next run.
    ///
    /// HTML tag names are case-insensitive, and hand-written Javadoc really does close a `<pre>`
    /// with `</PRE>`. Matching only the lower-case spelling leaves the fence open to the end of
    /// the comment, which grows a blank line on every run.
    fn opens_fence(line: &str) -> bool {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("```")
            || lower.contains("<pre>")
            || lower.contains("<table")
            || (line.contains("{@snippet") && !line.contains('}'))
            || (lower.starts_with("{@code") && !lower.contains('}'))
    }

    /// Whether a line closes such a region.
    fn closes_fence(line: &str) -> bool {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("```") || lower.contains("</pre>") || lower.contains("</table>")
    }

    /// Whether a line both opens and closes a region, so it is verbatim on its own.
    fn self_closing_fence(line: &str) -> bool {
        let lower = line.to_ascii_lowercase();
        (lower.contains("<pre>") && lower.contains("</pre>"))
            || (lower.contains("<table") && lower.contains("</table>"))
            || (lower.contains("{@code") && lower.contains('}'))
            || (line.contains("{@snippet") && line.contains('}'))
    }

    /// The list nesting depth after `line`.
    ///
    /// Counted over the whole line rather than its start: refilling can leave a `<ul>` at the end
    /// of a prose line, and a depth that only saw line-initial tags would then forget the list
    /// exists on the next run.
    fn list_depth(depth: usize, line: &str) -> usize {
        const OPEN: [&str; 3] = ["<ul", "<ol", "<dl"];
        const CLOSE: [&str; 3] = ["</ul", "</ol", "</dl"];
        let lower = line.to_ascii_lowercase();
        let count =
            |tags: [&str; 3]| -> usize { tags.iter().map(|tag| lower.matches(tag).count()).sum() };
        // `</ul` also matches `<ul`, so the opens have to have the closes taken back out of them.
        let closes = count(CLOSE);
        let opens = count(OPEN).saturating_sub(closes);
        depth.saturating_add(opens).saturating_sub(closes)
    }

    /// The `(first, rest)` indents a line at list `depth` takes.
    ///
    /// The list's own tags sit at the enclosing level; an item tag opens one level in and its
    /// continuations one item level further.
    fn list_indents(depth: usize, line: &str) -> (usize, usize) {
        /// Columns a list indents its contents by (`writeListOpen`).
        const LIST: usize = 2;
        /// Columns an item's continuation lines take on top of that (`writeListItemOpen`, which
        /// pushes the length of the `<li>` token).
        const ITEM: usize = 4;

        let lower = line.to_ascii_lowercase();
        // A closing tag belongs to the level it closes *out of*.
        let closes = ["</ul", "</ol", "</dl"]
            .iter()
            .any(|tag| lower.starts_with(tag));
        let level = if closes {
            depth.saturating_sub(1)
        } else {
            depth
        };
        if level == 0 {
            return (0, 0);
        }
        let inner = level * LIST;
        if ["<li", "<dt", "<dd"]
            .iter()
            .any(|tag| lower.starts_with(tag))
        {
            return (inner, inner + ITEM);
        }
        (inner + ITEM, inner + ITEM)
    }

    /// The rest of `line` after a leading `<p>`, or `None` when it does not open a paragraph.
    ///
    /// google-java-format standardizes any simple form — `<P>`, `<p/>`, `<p />` — to `<p>`
    /// (`standardizePToken`), so all of them are recognized here.
    fn paragraph_open(line: &str) -> Option<&str> {
        let rest = line.strip_prefix('<')?;
        let rest = rest.strip_prefix(['p', 'P'])?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        Some(rest.trim_start().strip_prefix('>')?.trim_start())
    }

    /// Whether `line` holds a `</p>` in any case.
    fn has_paragraph_close(line: &str) -> bool {
        line.contains("</p") || line.contains("</P")
    }

    /// `line` with every `</p>` removed.
    fn drop_paragraph_close(line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(at) = rest.find("</") {
            let (head, tail) = rest.split_at(at);
            out.push_str(head);
            let Some(close) = tail.find('>') else {
                rest = tail;
                break;
            };
            if !tail[2..close].trim().eq_ignore_ascii_case("p") {
                out.push_str(&tail[..=close]);
            }
            rest = &tail[close + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Whether a line starts an HTML block element, which begins its own paragraph.
    ///
    /// Matched case-insensitively: HTML tag names are, and hand-written Javadoc really does say
    /// `<UL>`.
    fn is_html_block(line: &str) -> bool {
        const BLOCK_TAGS: [&str; 13] = [
            "<p>", "<p ", "<br>", "<ul", "<ol", "<li", "<dl", "<dt", "<dd", "<h", "</ul", "</ol",
            "</dl",
        ];
        let lower = line.trim_start().to_ascii_lowercase();
        BLOCK_TAGS.iter().any(|tag| lower.starts_with(tag))
    }

    /// Whether a line is a section heading, which stands alone between blank lines.
    ///
    /// `JavadocWriter` requests one before `writeHeaderOpen` and one after `writeHeaderClose`.
    fn is_heading(line: &str) -> bool {
        let lower = line.trim_start().to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("<h") else {
            return false;
        };
        rest.starts_with(|c: char| ('1'..='6').contains(&c))
    }

    /// Parse a line that starts a block tag.
    ///
    /// `@param` / `@throws` / `@exception` take a name before their description; every other tag
    /// takes only a description. An inline `{@…}` tag is prose and is not matched here.
    fn block_tag(line: &str) -> Option<Block> {
        let rest = line.strip_prefix('@')?;
        let mut parts = rest.split_whitespace();
        let name = parts.next()?;
        // A block tag is `@` and a *lowercase* word — google-java-format's `FOOTER_TAG_PATTERN`.
        // What follows the word is description, even when no space separates it: `@xerces.internal`
        // is the `@xerces` tag, and keeping the whole run as the name renders it back unchanged.
        if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
            return None;
        }
        let takes_argument = matches!(name, "param" | "throws" | "exception");
        let argument = takes_argument
            .then(|| parts.next().map(Into::into))
            .flatten();
        Some(Block::Tag {
            name: alloc::format!("@{name}"),
            argument,
            words: parts.map(Into::into).collect(),
        })
    }

    /// Refill with a different budget for the first line than for the rest.
    ///
    /// One word never starts a line: a word beginning with `@`. At the start of a line that is a
    /// block tag, so refilling prose into that position would turn `… the @Override annotation`
    /// into a tag on the next run. Keeping it on the line it is already on costs a few columns of
    /// overflow and keeps the comment a fixed point.
    fn fill_two(words: &[String], first: usize, rest: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for word in words {
            let budget = if lines.is_empty() { first } else { rest };
            let width = Width::utf16(word);
            if current.is_empty() {
                current.push_str(word);
                continue;
            }
            if Width::utf16(&current) + 1 + width > budget && !word.starts_with('@') {
                lines.push(core::mem::take(&mut current));
            } else {
                current.push(' ');
            }
            current.push_str(word);
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
    }
}
