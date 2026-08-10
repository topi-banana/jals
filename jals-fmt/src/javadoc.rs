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

use jals_config::fmt::{ParagraphTags, TagAlignment};
use jals_syntax::SyntaxKind;

use crate::ir::Width;
use crate::style::Style;

/// One token of comment prose, and whether whitespace preceded it.
///
/// google-java-format's Javadoc lexer emits HTML tags as tokens of their own, and its writer
/// starts a new line before *any* token that does not fit — not only before a space. That is what
/// lets `<code>` end a line and its content begin the next with no space between, so the tokens
/// have to carry the distinction rather than being joined into words.
struct Word {
    /// The token text.
    text: String,
    /// Whether a space separates it from the token before it.
    space: bool,
}

/// One logical piece of a comment body.
enum Block {
    /// Prose to be refilled, with the extra indent its first and continuation lines take.
    ///
    /// Only an HTML list moves either off zero: `<li>` sits one list level in and the lines that
    /// continue it one item level further, which is `JavadocWriter`'s `continuingListStack` plus
    /// `continuingListItemStack`.
    Prose {
        /// The tokens to refill.
        words: Vec<Word>,
        /// Columns of extra indent on the first line.
        first: usize,
        /// Columns of extra indent on every line after it.
        rest: usize,
    },
    /// Lines to be emitted exactly as they are — fenced code, `<pre>`, a table.
    Verbatim {
        /// The lines, already stripped of the `*` prefix.
        lines: Vec<String>,
        /// Columns of extra indent on the *opening* line. The region's content keeps its own
        /// indentation and takes none: only the tag that opens it belongs to the surrounding
        /// paragraph.
        first: usize,
    },
    /// A block tag: its name (`@param`), an optional first argument, and its description words.
    Tag {
        /// The tag itself, `@`-prefixed.
        name: String,
        /// The parameter or exception name, for the tags that take one.
        argument: Option<String>,
        /// The description, as tokens to refill.
        words: Vec<Word>,
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
        column: usize,
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
            return Self::shift(text, indent, column);
        }
        match kind {
            SyntaxKind::LINE_COMMENT => Self::render_line(text, indent, style),
            _ => Self::render_block(text, kind, indent, style),
        }
    }

    /// Move a comment the formatter does not reflow to its new column.
    ///
    /// A block comment's continuation lines are laid out against its opening `/*`, so a comment
    /// that moved has to take them with it. Only a *shift* is applied: the relative shape is the
    /// information such a comment carries, and ASCII art is exactly the case where reflowing it
    /// would be destruction.
    fn shift(text: &str, indent: usize, column: usize) -> String {
        if indent <= column || !text.contains('\n') {
            return text.into();
        }
        let extra = indent - column;
        let mut out = String::new();
        for (nth, line) in text.split('\n').enumerate() {
            if nth > 0 {
                out.push('\n');
                if !line.trim().is_empty() {
                    for _ in 0..extra {
                        out.push(' ');
                    }
                }
            }
            out.push_str(line);
        }
        out
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
        // google-java-format does and what most one-line Javadoc already looks like. Eclipse's
        // `new_lines_at_*_boundaries` asks for the delimiters to keep lines of their own instead.
        let collapses = if doc {
            !cfg.javadoc_boundaries_on_own_lines
        } else {
            !cfg.block_boundaries_on_own_lines
        };
        if collapses
            && let [
                Block::Prose {
                    words, first: 0, ..
                },
            ] = blocks.as_slice()
        {
            // `makeSingleLineIfPossible` collapses a comment that renders as *one* content line.
            // A `<br>` ends its line however wide the comment is, so a paragraph holding one is
            // several lines and stays that way.
            let lines = Self::fill_two(words, usize::MAX, usize::MAX);
            if let [inline] = lines.as_slice() {
                let width = indent + Width::utf16(opener) + Width::utf16(inline) + 4;
                if width <= style.comment_width(indent) && !inline.is_empty() {
                    return alloc::format!("{opener} {inline} */");
                }
            }
        }

        let prefix_width = if cfg.leading_asterisks { 3 } else { 1 };
        let budget = style
            .comment_width(indent)
            .saturating_sub(indent + prefix_width)
            .max(16);

        // `tag-alignment` lines descriptions up under a shared column, so the width belongs to a
        // run of tags rather than to any one of them: `all` measures the whole comment, `grouped`
        // each run of same-named tags. Resolved once per block so `push_tag` stays local.
        let aligned = Self::tag_columns(&blocks, cfg.tag_alignment);

        let mut out = String::from(opener);
        let mut seen_tag = false;
        let mut previous: Option<&Block> = None;
        // The column the description of the tag currently open is written at, so the blocks that
        // continue it line up under it.
        let mut tag_pad = 0usize;
        for (at, block) in blocks.iter().enumerate() {
            match block {
                // Once the block tags start there are no more blank lines: only the *first* one
                // is separated from the description, and `JavadocWriter` requests a plain newline
                // between the rest — unless `blank-lines-between-tags` keeps the author's.
                Block::Blank => {
                    let keep = if seen_tag {
                        cfg.blank_lines_between_tags
                    } else {
                        cfg.preserve_blank_lines
                    };
                    if keep {
                        Self::push_line(&mut out, "", cfg.leading_asterisks);
                    }
                }
                // A block after a tag continues that tag's description, so under alignment it
                // starts at the description's column rather than at the comment's margin.
                Block::Prose { words, first, rest } => {
                    let (first, rest) = (*first + tag_pad, *rest + tag_pad);
                    let lines = Self::fill_two(
                        words,
                        budget.saturating_sub(first).max(16),
                        budget.saturating_sub(rest).max(16),
                    );
                    for (nth, line) in lines.iter().enumerate() {
                        let pad = if nth == 0 { first } else { rest };
                        Self::push_indented(&mut out, line, pad, cfg.leading_asterisks);
                    }
                }
                Block::Verbatim { lines, first } => {
                    for (nth, line) in lines.iter().enumerate() {
                        let pad = if nth == 0 { *first + tag_pad } else { 0 };
                        Self::push_indented(&mut out, line, pad, cfg.leading_asterisks);
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
                    let column = aligned.get(at).copied().flatten();
                    tag_pad = column.map_or(0, |column| column + 1);
                    Self::push_tag(
                        &mut out,
                        name,
                        argument.as_deref(),
                        words,
                        budget,
                        column,
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

    /// Split a whitespace-delimited run into tokens at HTML tag boundaries.
    ///
    /// `space` says whether the run itself followed whitespace; the pieces after the first do not.
    /// An inline `{@…}` tag is left whole — it is one token to google-java-format's lexer too.
    fn tokenize(run: &str, space: bool, out: &mut Vec<Word>) {
        if run.starts_with("{@") {
            out.push(Word {
                text: run.into(),
                space,
            });
            return;
        }
        let mut rest = run;
        let mut space = space;
        while let Some(at) = rest.find('<') {
            let Some(close) = rest[at..].find('>') else {
                break;
            };
            let end = at + close + 1;
            // Only a tag google-java-format's lexer knows is a token of its own; a `<` that opens
            // nothing (`a < b`) is not one either.
            if !Self::is_known_tag(&rest[at + 1..end - 1]) {
                break;
            }
            if at > 0 {
                out.push(Word {
                    text: rest[..at].into(),
                    space,
                });
                space = false;
            }
            let tag = Self::standardize_tag(&rest[at..end]);
            // A list item's text starts right after its tag — `<dd>ISO 639`, never `<dd> ISO`.
            let item = Self::is_item_tag(&tag);
            out.push(Word { text: tag, space });
            space = false;
            rest = &rest[end..];
            if item {
                rest = rest.trim_start();
            }
        }
        if !rest.is_empty() {
            out.push(Word {
                text: rest.into(),
                space,
            });
        }
    }

    /// Whether `inner` — a tag's body, without its `<` and `>` — is a tag `JavadocLexer` lexes.
    ///
    /// Its patterns name exactly these: `pre`, `code`, `table`, `ul|ol|dl`, `li|dt|dd`, `h[1-6]`,
    /// `p`, `blockquote`, `br`, and `a`. Everything else — `<em>`, `<i>` — is part of the literal
    /// around it, which is what keeps `<em>locale-sensitive</em>` one unbreakable unit.
    fn is_known_tag(inner: &str) -> bool {
        const TAGS: [&str; 13] = [
            "pre",
            "code",
            "table",
            "ul",
            "ol",
            "dl",
            "li",
            "dt",
            "dd",
            "p",
            "blockquote",
            "br",
            "a",
        ];
        let inner = inner.trim().trim_start_matches('/').trim_start();
        let name: &str = inner
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or("");
        if name.len() == 2
            && name.starts_with(['h', 'H'])
            && name[1..].starts_with(|c: char| ('1'..='6').contains(&c))
        {
            return true;
        }
        TAGS.iter().any(|tag| name.eq_ignore_ascii_case(tag))
    }

    /// `<BR/>` and `<P />` in their canonical spelling.
    ///
    /// Only these two: google-java-format standardizes the `br` and `p` *tokens*
    /// (`standardizeBrToken`, `standardizePToken`) and leaves every other tag as the author wrote
    /// it, `<CODE>` included.
    fn standardize_tag(tag: &str) -> String {
        let inner = tag
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_end_matches('/')
            .trim();
        if inner.eq_ignore_ascii_case("br") || inner.eq_ignore_ascii_case("p") {
            return alloc::format!("<{}>", inner.to_ascii_lowercase());
        }
        tag.into()
    }

    /// Whether a token is a `<br>`, which ends its line.
    const fn is_break_tag(text: &str) -> bool {
        text.eq_ignore_ascii_case("<br>")
    }

    /// Every whitespace-delimited run of `line`, tokenized.
    fn tokens_of(line: &str, out: &mut Vec<Word>) {
        for run in line.split_whitespace() {
            // A list item's text starts right after its tag however the author spaced it.
            let space = !out.last().is_some_and(|last| Self::is_item_tag(&last.text));
            Self::tokenize(run, space, out);
        }
    }

    /// Whether the token after this one hugs it.
    ///
    /// A list item's text starts right after its tag, and `optionalizeSpacesAfterLinks` turns the
    /// whitespace after an `<a href=…>` into a break that renders as nothing — so a long link and
    /// the text it labels can share a line boundary without a stray space appearing. The link's
    /// closing `>` may arrive in a run of its own, since `<a` and its `href` are routinely written
    /// on separate lines.
    fn is_item_tag(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        // A tag the whitespace split cut in half — `<dt` of `<dt id="x">` — is not a tag yet, and
        // hugging the next token to it would delete the space inside the tag.
        if !lower.ends_with('>') {
            return false;
        }
        if lower.contains("href=") {
            return true;
        }
        ["<li", "<dt", "<dd", "<a "]
            .iter()
            .any(|tag| lower.starts_with(tag))
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

    /// The description column each block in `blocks` writes its description at, by index.
    ///
    /// `None` at an index means "no alignment here" — either the block is not a tag, or the mode
    /// asks for none. A tag's own head width is `@name` plus the argument it takes, so a run whose
    /// widest member is `@throws IllegalArgumentException` pads every sibling out to that.
    fn tag_columns(blocks: &[Block], mode: TagAlignment) -> Vec<Option<usize>> {
        let mut columns = alloc::vec![None; blocks.len()];
        if mode == TagAlignment::None {
            return columns;
        }
        let head_of = |block: &Block| match block {
            Block::Tag { name, argument, .. } => Some((
                name.clone(),
                Width::utf16(name) + argument.as_deref().map_or(0, |a| Width::utf16(a) + 1),
            )),
            _ => None,
        };
        if mode == TagAlignment::All {
            let widest = blocks.iter().filter_map(&head_of).map(|(_, w)| w).max();
            for (slot, block) in columns.iter_mut().zip(blocks) {
                if matches!(block, Block::Tag { .. }) {
                    *slot = widest;
                }
            }
            return columns;
        }
        // Grouped: a run is the consecutive tags sharing one name. A blank line does not end one —
        // JDT groups by the tag name, and `blank-lines-between-tags` may well be keeping blanks
        // inside the footer.
        let mut run: Vec<usize> = Vec::new();
        let mut run_name: Option<String> = None;
        let mut widest = 0usize;
        let close = |run: &mut Vec<usize>, widest: &mut usize, columns: &mut Vec<Option<usize>>| {
            for at in run.drain(..) {
                columns[at] = Some(*widest);
            }
            *widest = 0;
        };
        for (at, block) in blocks.iter().enumerate() {
            let Some((name, width)) = head_of(block) else {
                continue;
            };
            if run_name.as_deref() != Some(name.as_str()) {
                close(&mut run, &mut widest, &mut columns);
                run_name = Some(name);
            }
            widest = widest.max(width);
            run.push(at);
        }
        close(&mut run, &mut widest, &mut columns);
        columns
    }

    /// Emit a block tag and its refilled description.
    fn push_tag(
        out: &mut String,
        name: &str,
        argument: Option<&str>,
        words: &[Word],
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
        // is being continued. Under `tag-alignment` the description has a column of its own and
        // the continuation starts there instead — a column the description is *not* written at
        // would not be an alignment.
        let continuation = match aligned {
            Some(column) => column + 1,
            None if cfg.indent_tag_description => style.continuation_cols,
            None => 0,
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
        let mut prose: Vec<Word> = Vec::new();
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
        // The indent the region currently being collected opens at.
        let mut fence_indent = 0usize;
        // The brace depth of the `{@snippet …}` being collected.
        let mut snippet_depth = 0i32;
        // Everything after a block tag belongs to that tag's description, so it sits at the
        // description's continuation indent — headings, paragraphs and lists included.
        let mut tag_indent = 0usize;

        for raw in body.split('\n') {
            let mut line = raw.trim();
            let stripped;
            if cfg.format_html
                && cfg.paragraph_tags == ParagraphTags::Leading
                && Self::has_paragraph_close(line)
            {
                // google-java-format drops `</p>` outright (`case ParagraphCloseTag -> {}`).
                stripped = Self::drop_paragraph_close(line);
                line = stripped.trim();
                if line.is_empty() {
                    continue;
                }
            }

            if let Some((lines, snippet)) = &mut fence {
                // A `{@snippet …}` ends at the `}` that balances its `{`, wherever on the line
                // that falls — `SnippetEnd` is a token, not a line.
                if *snippet {
                    snippet_depth += Self::brace_delta(line);
                }
                // Preformatted content keeps its own indentation — that is what makes it
                // preformatted. Only the single space that conventionally follows the `*` is
                // dropped, and `JavadocWriter` writes these lines with no auto-indent at all.
                let verbatim = raw.strip_prefix(' ').unwrap_or(raw).trim_end();
                lines.push(verbatim.into());
                let closed = if *snippet {
                    snippet_depth <= 0
                } else {
                    Self::closes_fence(line)
                };
                if closed {
                    let mut region = core::mem::take(lines);
                    Self::dedent_code_region(&mut region);
                    if cfg.format_source_in_comments {
                        Self::reindent_code_region(&mut region, style);
                    }
                    blocks.push(Block::Verbatim {
                        lines: region,
                        first: fence_indent,
                    });
                    fence = None;
                }
                continue;
            }
            if Self::opens_fence(line) {
                // A `<pre>…</pre>` written inside a sentence is an *element* of that sentence,
                // not a line of its own: `writePreOpen` asks for a blank line around the region
                // and leaves the prose on either side to reflow.
                let lower = line.to_ascii_lowercase();
                let mut trailing = "";
                let split = Self::self_closing_fence(line)
                    && lower
                        .find("<pre>")
                        .zip(lower.rfind("</pre>"))
                        // A line may close a region it did not open (`</pre> more <pre>`), in
                        // which case there is no element on it to split out.
                        .is_some_and(|(open, close)| close > open);
                if split
                    && let Some(open) = lower.find("<pre>")
                    && let Some(close) = lower.rfind("</pre>")
                {
                    let before = line[..open].trim_end();
                    trailing = line[close + "</pre>".len()..].trim_start();
                    if !before.is_empty() {
                        Self::tokens_of(before, &mut prose);
                    }
                    line = line[open..close + "</pre>".len()].trim_end();
                }
                Self::flush(&mut prose, &mut blocks, first, rest);
                // `writeSnippetBegin` and `writePreOpen` each request a blank line before the
                // region they open — but not inside a list, which holds none.
                if depth == 0 && !matches!(blocks.last(), Some(Block::Blank) | None) {
                    blocks.push(Block::Blank);
                }
                // The opening tag belongs to whatever paragraph it interrupts: a tag's
                // description continues at its continuation indent, a list item at the item's.
                fence_indent = if matches!(blocks.last(), Some(Block::Tag { .. })) {
                    style.continuation_cols
                } else {
                    rest
                };
                let lines = alloc::vec![String::from(line)];
                if Self::self_closing_fence(line) {
                    blocks.push(Block::Verbatim {
                        lines,
                        first: fence_indent,
                    });
                    // `writePreClose` asks for a blank line after the region too, and whatever
                    // followed it on the line goes on reflowing after that.
                    if depth == 0 && split {
                        blocks.push(Block::Blank);
                    }
                    if !trailing.is_empty() {
                        Self::tokens_of(trailing, &mut prose);
                    }
                } else {
                    let snippet = line.contains("{@snippet");
                    if snippet {
                        snippet_depth = Self::brace_delta(line);
                    }
                    fence = Some((lines, snippet));
                }
                continue;
            }
            if line.is_empty() {
                Self::flush(&mut prose, &mut blocks, first, rest);
                // A run of blank lines is one paragraph break: `JavadocWriter.requestBlankLine`
                // sets a flag, so asking twice still yields one. A list has no blank lines in it
                // either: `writeListItemOpen` requests a newline, not a blank one.
                // Inside the footer the blank is kept only under `blank-lines-between-tags`;
                // google-java-format writes the footer as a solid run, which is also what keeps
                // a tag's description continuing across a blank line the author left in it.
                let footer = matches!(blocks.last(), Some(Block::Tag { .. }));
                if depth == 0
                    && !matches!(blocks.last(), Some(Block::Blank))
                    && (!footer || cfg.blank_lines_between_tags)
                {
                    blocks.push(Block::Blank);
                }
                continue;
            }
            if let Some(tag) = Self::block_tag(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                blocks.push(tag);
                if cfg.indent_tag_description {
                    tag_indent = style.continuation_cols;
                }
                first = tag_indent;
                rest = tag_indent;
                continue;
            }
            // `<p>` opens a paragraph: a blank line before it, and the tag glued to the word it
            // introduces (`<p>This method …`). An opening `<p>` before any prose is dropped, as
            // `JavadocWriter.writeParagraphOpen` does when nothing significant has been written.
            // The other two modes leave the tag in the prose for [`Self::split_paragraph_tags`],
            // which gives it a line of its own wherever on a line the author wrote it.
            if cfg.format_html
                && cfg.paragraph_tags == ParagraphTags::Leading
                && let Some(after) = Self::paragraph_open(line)
            {
                Self::flush(&mut prose, &mut blocks, first, rest);
                // "Nothing significant written yet" is what makes an opening `<p>` disappear,
                // and a run of blank lines is not significant.
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
            // A blockquote tag stands alone between blank lines
            // (`writeBlockquoteOpenOrClose`).
            if cfg.format_html && Self::is_blockquote(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                if !matches!(blocks.last(), Some(Block::Blank) | None) {
                    blocks.push(Block::Blank);
                }
                blocks.push(Block::Prose {
                    words: {
                        let mut tag = Vec::new();
                        Self::tokens_of(line, &mut tag);
                        tag
                    },
                    first: tag_indent,
                    rest: tag_indent,
                });
                blocks.push(Block::Blank);
                continue;
            }
            if cfg.format_html && Self::is_html_block(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                // `writeListOpen` requests a blank line before a classic-Javadoc list — but a
                // list is a *block*, and `requestBlankLine` is ignored inside one, so a nested
                // list continues its item rather than starting a paragraph.
                if cfg.set_off_html_lists
                    && depth == 0
                    && Self::opens_list(line)
                    && !matches!(blocks.last(), Some(Block::Blank) | None)
                {
                    blocks.push(Block::Blank);
                }
            }
            // A heading stands alone between blank lines, and what follows it starts a paragraph
            // of its own rather than continuing the heading's line.
            if cfg.format_html && Self::is_heading(line) {
                if !matches!(blocks.last(), Some(Block::Blank) | None) {
                    blocks.push(Block::Blank);
                }
                blocks.push(Block::Prose {
                    words: {
                        let mut heading = Vec::new();
                        Self::tokens_of(line, &mut heading);
                        heading
                    },
                    first: tag_indent,
                    rest: tag_indent,
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
                    let (list_first, list_rest) = if cfg.set_off_html_lists {
                        Self::list_indents(depth, line)
                    } else {
                        (0, 0)
                    };
                    first = list_first + tag_indent;
                    rest = list_rest + tag_indent;
                }
                depth = Self::list_depth(depth, line);
            }
            // A tag's description continues onto the following lines.
            if let Some(Block::Tag { words, .. }) = blocks.last_mut()
                && prose.is_empty()
            {
                Self::tokens_of(line, words);
                continue;
            }
            for run in line.split_whitespace() {
                // A list item's text starts right after its tag however the author spaced it.
                let space = !prose
                    .last()
                    .is_some_and(|last| Self::is_item_tag(&last.text));
                if let Some(mut glued) = pending.take() {
                    glued.push_str(run);
                    Self::tokenize(&glued, space, &mut prose);
                    continue;
                }
                Self::tokenize(run, space, &mut prose);
            }
        }
        if let Some(glued) = pending.take() {
            Self::tokenize(&glued, true, &mut prose);
        }
        if let Some((mut lines, _)) = fence {
            // The region never closed — an unbalanced `{@code`, a `<pre>` with no `</pre>`. Its
            // trailing blank lines are the gap above `*/`, not content, and keeping them would
            // grow the comment by one line on every run.
            while lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.pop();
            }
            blocks.push(Block::Verbatim {
                lines,
                first: fence_indent,
            });
        }
        Self::flush(&mut prose, &mut blocks, first, rest);

        // Trailing blank lines are layout, not content.
        while matches!(blocks.last(), Some(Block::Blank)) {
            blocks.pop();
        }
        while matches!(blocks.first(), Some(Block::Blank)) {
            blocks.remove(0);
        }
        if cfg.format_html && cfg.paragraph_tags != ParagraphTags::Leading {
            Self::split_paragraph_tags(&mut blocks);
        }
        if cfg.format_html && cfg.paragraph_tags != ParagraphTags::Authored {
            Self::infer_paragraph_tags(&mut blocks, cfg.paragraph_tags == ParagraphTags::OwnLine);
        }
        if !cfg.break_inside_inline_tags {
            for block in &mut blocks {
                match block {
                    Block::Prose { words, .. } | Block::Tag { words, .. } => {
                        Self::join_inline_tags(words);
                    }
                    Block::Blank | Block::Verbatim { .. } => {}
                }
            }
        }
        blocks
    }

    /// Whether a block ends with a tag that is not a literal to google-java-format's lexer.
    ///
    /// `inferParagraphTags` inserts a `<p>` only *between two literals*. A heading's, a
    /// blockquote's or a list's close tag is its own token, so the paragraph after one opens
    /// without a `<p>`.
    fn ends_block_tag(block: &Block) -> bool {
        const TAGS: [&str; 9] = [
            "</h1>",
            "</h2>",
            "</h3>",
            "</h4>",
            "</h5>",
            "</h6>",
            "</blockquote>",
            "</pre>",
            "</table>",
        ];
        let Block::Prose { words, .. } = block else {
            return false;
        };
        words.last().is_some_and(|word| {
            let lower = word.text.to_ascii_lowercase();
            TAGS.iter().any(|tag| lower.ends_with(tag))
                || ["</ul>", "</ol>", "</dl>"]
                    .iter()
                    .any(|tag| lower.ends_with(tag))
        })
    }

    /// Fuse each `{@… }` and everything up to its closing brace into one word.
    ///
    /// The refill never breaks a word, so a fused tag stays on one line — Eclipse's shape. A tag
    /// whose brace never closes inside this block is left alone: swallowing the rest of the
    /// paragraph would be a worse answer than the break it avoids.
    fn join_inline_tags(words: &mut Vec<Word>) {
        let mut at = 0usize;
        while at < words.len() {
            if !words[at].text.starts_with("{@") || Self::brace_delta(&words[at].text) <= 0 {
                at += 1;
                continue;
            }
            let mut depth = Self::brace_delta(&words[at].text);
            let mut end = at + 1;
            while end < words.len() && depth > 0 {
                depth += Self::brace_delta(&words[end].text);
                end += 1;
            }
            if depth > 0 {
                at += 1;
                continue;
            }
            let joined = words[at..end]
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            words[at].text = joined;
            words.drain(at + 1..end);
            at += 1;
        }
    }

    /// Give every `<p>` a content line of its own, wherever in a paragraph it was written.
    ///
    /// `<p>` is a block-level HTML element, and Eclipse's `CommentsPreparator` treats it as one:
    /// a `<p>` ending a sentence (`…implement this interface. <p>`) breaks the line before it and
    /// after it. Only google-java-format hoists it into the next paragraph's first word instead.
    fn split_paragraph_tags(blocks: &mut Vec<Block>) {
        let mut at = 0usize;
        while at < blocks.len() {
            let Block::Prose { words, rest, .. } = &mut blocks[at] else {
                at += 1;
                continue;
            };
            let Some(found) = words.iter().position(|word| word.text == "<p>") else {
                at += 1;
                continue;
            };
            // A `<p>` already at the head of the block splits *after* itself, so the tag keeps
            // its line and the paragraph it opens starts on the next one.
            let cut = if found == 0 { 1 } else { found };
            if cut >= words.len() {
                at += 1;
                continue;
            }
            let rest = *rest;
            let tail: Vec<Word> = words.drain(cut..).collect();
            // The tail is no longer the paragraph's opening line, so it takes the continuation
            // indent for both — inside a list item that is the item's, not the item tag's.
            blocks.insert(
                at + 1,
                Block::Prose {
                    words: tail,
                    first: rest,
                    rest,
                },
            );
            at += 1;
        }
    }

    /// A `<p>` occupying a content line of its own.
    fn paragraph_line(indent: usize) -> Block {
        Block::Prose {
            words: alloc::vec![Word {
                text: "<p>".into(),
                space: false,
            }],
            first: indent,
            rest: indent,
        }
    }

    /// Insert a `<p>` wherever a blank line separates two runs of prose.
    ///
    /// A blank line between paragraphs is a paragraph break the author made in the *comment*; the
    /// rendered Javadoc only sees it if an HTML tag says so. google-java-format's
    /// `inferParagraphTags` does the same, and only between two literals — a blank line before a
    /// block tag or a `<pre>` region opens nothing.
    ///
    /// `own_line` writes the inferred tag on its own line instead of gluing it to the first word,
    /// which is the shape IntelliJ's `JD_P_AT_EMPTY_LINES` produces.
    fn infer_paragraph_tags(blocks: &mut Vec<Block>, own_line: bool) {
        // Right to left: an insertion shifts every later index, and going backwards keeps the
        // ones still to be examined where they were.
        for at in (2..blocks.len()).rev() {
            if !matches!(blocks[at - 1], Block::Blank)
                || !matches!(blocks[at - 2], Block::Prose { .. })
                || Self::ends_block_tag(&blocks[at - 2])
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
            if first.text.starts_with('<') {
                continue;
            }
            if own_line {
                blocks.insert(at, Self::paragraph_line(0));
            } else {
                first.text.insert_str(0, "<p>");
            }
        }
    }

    /// Move any accumulated prose into `blocks`, at `first` / `rest` columns of extra indent.
    fn flush(prose: &mut Vec<Word>, blocks: &mut Vec<Block>, first: usize, rest: usize) {
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

    /// A line's net brace count, ignoring the braces inside a string or a character literal.
    fn brace_delta(line: &str) -> i32 {
        let mut delta = 0i32;
        for ch in line.chars() {
            match ch {
                '{' => delta += 1,
                '}' => delta -= 1,
                _ => {}
            }
        }
        delta
    }

    /// Strip the common indentation from the body of a `{@code …}` region.
    ///
    /// The braces make it one token, and google-java-format writes that token's content against
    /// the comment's own margin — the indentation that lined it up under `<pre>{@code` in the
    /// source is not part of what the snippet says. Relative indentation inside the snippet is,
    /// so only the common prefix goes. The opening and closing lines are excluded: they carry the
    /// delimiters, not the code.
    fn dedent_code_region(lines: &mut [String]) {
        let Some(first) = lines.first() else {
            return;
        };
        if !first.contains("{@code") {
            return;
        }
        let Some(body) = lines.get(1..lines.len().saturating_sub(1)) else {
            return;
        };
        let indent_of = |line: &str| line.chars().take_while(|ch| ch.is_whitespace()).count();
        let Some(common) = body
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| indent_of(line))
            .min()
        else {
            return;
        };
        if common == 0 {
            return;
        }
        let end = lines.len() - 1;
        for line in &mut lines[1..end] {
            *line = line.chars().skip(common).collect();
        }
    }

    /// Re-indent a fenced code region to the configured indentation.
    ///
    /// This is the reachable half of Eclipse's `comment.format_source_code`, which runs the Java
    /// formatter over the snippet: what a reader sees of that on already-formatted code is the
    /// indentation changing to the surrounding style — four spaces becoming a tab. jals does not
    /// re-run itself inside a comment (the region is a fragment, often not parseable on its own),
    /// so it re-indents and leaves the rest of the snippet as written; `DESIGN.md` §18.2's **D7**
    /// records the residue.
    ///
    /// The snippet's own unit is its smallest positive indent — nothing else in a fragment says
    /// what one level is. A region indented by nothing keeps its shape.
    fn reindent_code_region(lines: &mut [String], style: &Style) {
        // Only a region that says it holds *code*. A bare `<pre>` fences ASCII art and hand-laid
        // tables at least as often as it fences Java, and re-indenting one of those is the
        // destruction the fence exists to prevent.
        if !lines
            .first()
            .is_some_and(|line| line.contains("{@code") || line.contains("{@snippet"))
        {
            return;
        }
        let tab = style.tab_width();
        let columns = |line: &str| -> Option<usize> {
            let body = line.trim_start_matches([' ', '\t']);
            if body.is_empty() || body.len() == line.len() {
                return None;
            }
            let mut column = 0usize;
            for ch in line[..line.len() - body.len()].chars() {
                column = if ch == '\t' {
                    (column / tab + 1) * tab
                } else {
                    column + 1
                };
            }
            Some(column)
        };
        let Some(unit) = lines.iter().filter_map(|line| columns(line)).min() else {
            return;
        };
        for line in lines {
            let Some(cols) = columns(line) else {
                continue;
            };
            let body: String = line.trim_start_matches([' ', '\t']).into();
            let mut out = String::with_capacity(line.len());
            style.write_indent(cols / unit * style.indent_cols(), &mut out);
            out.push_str(&body);
            *line = out;
        }
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
        // Each level sits inside the previous level's *item continuation*, so nesting costs a
        // list indent on top of an item indent: `writeListOpen` inside `writeListItemOpen`.
        let inner = level * (LIST + ITEM) - ITEM;
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

    /// The close tags google-java-format throws away: `ParagraphCloseTag` and
    /// `ListItemCloseTag` are both `-> {}` in its `render`, because the writer decides where a
    /// paragraph or a list item ends and the tag adds nothing.
    const IGNORED_CLOSE: [&'static str; 4] = ["p", "li", "dt", "dd"];

    /// Whether `line` holds one of them.
    fn has_paragraph_close(line: &str) -> bool {
        line.contains("</") || line.contains("</")
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
            let inner = tail[2..close].trim();
            if !Self::IGNORED_CLOSE
                .iter()
                .any(|tag| inner.eq_ignore_ascii_case(tag))
            {
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

    /// Whether a line is a `<blockquote>` tag.
    fn is_blockquote(line: &str) -> bool {
        let lower = line.trim_start().to_ascii_lowercase();
        lower.starts_with("<blockquote") || lower.starts_with("</blockquote")
    }

    /// Whether a line opens an HTML list.
    fn opens_list(line: &str) -> bool {
        let lower = line.trim_start().to_ascii_lowercase();
        ["<ul", "<ol", "<dl"]
            .iter()
            .any(|tag| lower.starts_with(tag))
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
            words: {
                let mut description = Vec::new();
                for run in parts {
                    Self::tokenize(run, true, &mut description);
                }
                description
            },
        })
    }

    /// Refill with a different budget for the first line than for the rest.
    ///
    /// One word never starts a line: a word beginning with `@`. At the start of a line that is a
    /// block tag, so refilling prose into that position would turn `… the @Override annotation`
    /// into a tag on the next run. Keeping it on the line it is already on costs a few columns of
    /// overflow and keeps the comment a fixed point.
    fn fill_two(words: &[Word], first: usize, rest: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for word in words {
            let budget = if lines.is_empty() { first } else { rest };
            let width = Width::utf16(&word.text);
            if current.is_empty() {
                current.push_str(&word.text);
                continue;
            }
            let gap = usize::from(word.space);
            if Width::utf16(&current) + gap + width > budget && !word.text.starts_with('@') {
                lines.push(core::mem::take(&mut current));
            } else if word.space {
                current.push(' ');
            }
            current.push_str(&word.text);
            // `writeBr` requests a newline after the tag, so `<br>` always ends its line.
            if Self::is_break_tag(&word.text) {
                lines.push(core::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines
    }
}
