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
    /// A blank line: one the author wrote, or one a tag asked for.
    ///
    /// `requested` is `JavadocWriter.requestBlankLine` — a gap a `<p>`, a heading, a blockquote,
    /// a list close, or a preformatted region asked for on its own account. The distinction
    /// survives into the footer, where a blank line *between* two tags is dropped
    /// (`writeFooterJavadocTagStart` asks only for a newline) while one a region requested is
    /// still written.
    Blank { requested: bool },
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
        own_line: bool,
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
            _ => Self::render_block(text, kind, indent, column, own_line, style),
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
    fn render_block(
        text: &str,
        kind: SyntaxKind,
        indent: usize,
        column: usize,
        own_line: bool,
        style: &Style,
    ) -> String {
        let cfg = style.comments();
        let doc = kind == SyntaxKind::DOC_COMMENT;
        let opener = if doc { "/**" } else { "/*" };

        let Some(body) = Self::body(text, doc) else {
            return text.into();
        };
        if doc && !cfg.reflow_unclosed_html && !Self::lexes_cleanly(&body) {
            return Self::shift(text, indent, column);
        }
        let mut blocks = Self::parse(&body, style);
        if blocks.is_empty() {
            return if doc { "/** */".into() } else { "/* */".into() };
        }

        // A comment that is one short paragraph collapses to a single line, which is what
        // google-java-format does and what most one-line Javadoc already looks like. Eclipse's
        // `new_lines_at_*_boundaries` asks for the delimiters to keep lines of their own instead.
        let collapses = if doc {
            !cfg.javadoc_boundaries_on_own_lines
        } else {
            !cfg.block_boundaries_on_own_lines || !own_line
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

        if !cfg.break_inside_inline_tags {
            for block in &mut blocks {
                match block {
                    Block::Prose { words, .. } | Block::Tag { words, .. } => {
                        Self::join_inline_tags(words, budget);
                    }
                    Block::Blank { .. } | Block::Verbatim { .. } => {}
                }
            }
        }

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
                Block::Blank { requested } => {
                    // The exception is the blank a preformatted region asks for *after* itself:
                    // it is the region's request, not a gap between two tags, so it survives the
                    // footer's solid run. Reading it off the preceding block rather than off the
                    // author's line is also what keeps the comment a fixed point — on the second
                    // run the same blank arrives as a `Blank` here instead of being inserted
                    // below, and dropping it then would shorten the comment by a line per run.
                    // Inside the footer `flushWhitespace` downgrades a requested blank line to a
                    // newline while `continuingFooterTag` holds — which is every position but one.
                    // `writeFooterJavadocTagStart` clears the flag *before* writing its token, so
                    // a blank the previous token requested survives exactly when the next thing is
                    // another tag: `</pre>` then `@throws` is separated, `<p>` in the middle of a
                    // tag's description is not.
                    let before_tag = matches!(blocks.get(at + 1), Some(Block::Tag { .. }));
                    let keep = if seen_tag {
                        cfg.blank_lines_between_tags || (*requested && before_tag)
                    } else {
                        cfg.preserve_blank_lines || *requested
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
                    //
                    // A tag that follows a *requested* blank is already separated: the region
                    // or the `<p>` before it asked for the gap, and that request outlives the
                    // token that made it. Only the first tag's own separator is added here.
                    if !seen_tag
                        && cfg.blank_line_before_tags
                        && previous.is_some()
                        && !matches!(previous, Some(Block::Blank { .. }))
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
        // A heading opens one too: `HeaderOpenTag`, `ListItemOpenTag` and `ParagraphOpenTag` are
        // the three `StartOfLineToken`s, and the writer withholds a space after one of those
        // however the author spaced it — `<h2> Thread safety` is written `<h2>Thread safety`.
        if lower.starts_with("<h") && lower[2..].starts_with(|c: char| ('1'..='6').contains(&c)) {
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
        // Whether a `<table>` is a preformatted region rather than ordinary HTML.
        let tables = cfg.tables_are_preformatted;

        // A worklist rather than an iterator over the lines. A line that glues two block-level
        // tags together — `<blockquote><pre>`, `</pre></blockquote>` — is two tokens to
        // `JavadocLexer` and two lines to its writer, and the tail has to re-enter the same
        // decisions its head just went through, fence state included.
        let mut queue: Vec<String> = body.split('\n').map(Into::into).collect();
        let mut at = 0usize;
        while at < queue.len() {
            let raw = queue[at].clone();
            let raw = raw.as_str();
            at += 1;
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
                let mut verbatim = raw.strip_prefix(' ').unwrap_or(raw).trim_end();
                let closed = if *snippet {
                    snippet_depth <= 0
                } else {
                    Self::closes_fence(line, tables)
                };
                // What follows the closing tag is outside the region: `</pre></blockquote>` ends
                // the preformatted text at `</pre>` and the `</blockquote>` is a token of its own.
                let tail = if closed && !*snippet {
                    Self::split_after_fence_close(verbatim, tables).map_or("", |(head, rest)| {
                        verbatim = head;
                        rest
                    })
                } else {
                    ""
                };
                lines.push(verbatim.into());
                if closed {
                    let mut region = core::mem::take(lines);
                    Self::normalize_pre_code_block(&mut region);
                    if cfg.format_source_in_comments {
                        Self::reindent_code_region(&mut region, style);
                    }
                    blocks.push(Block::Verbatim {
                        lines: region,
                        first: fence_indent,
                    });
                    // `writePreClose` and `writeSnippetEnd` ask for a blank line after the region
                    // they close, the mirror of the one `writePreOpen` asks for in front of it —
                    // and the same list rule silences it, since `flushWhitespace` writes no blank
                    // line while a list is open.
                    if depth == 0 {
                        blocks.push(Block::Blank { requested: true });
                    }
                    fence = None;
                    if !tail.is_empty() {
                        queue.insert(at, tail.into());
                    }
                }
                continue;
            }
            // Each of these tags asks for whitespace on both sides, so a line that glues one to
            // what follows it is two lines: `<blockquote><pre>` opens a quote *and* a
            // preformatted region. The tail re-enters the loop rather than being handled here —
            // it may open a fence, be a heading, or itself glue another tag on.
            if cfg.format_html
                && let Some((tag, tail)) = Self::split_block_tag(line)
            {
                queue.insert(at, tail.into());
                line = tag;
            }
            if Self::opens_fence(line, tables) {
                // A `<pre>…</pre>` written inside a sentence is an *element* of that sentence,
                // not a line of its own: `writePreOpen` asks for a blank line around the region
                // and leaves the prose on either side to reflow.
                let lower = line.to_ascii_lowercase();
                let mut trailing = "";
                let split = Self::self_closing_fence(line, tables)
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
                if depth == 0 && !matches!(blocks.last(), Some(Block::Blank { .. }) | None) {
                    blocks.push(Block::Blank { requested: true });
                }
                // The opening tag belongs to whatever paragraph it interrupts: a tag's
                // description continues at its continuation indent, a list item at the item's.
                fence_indent = if matches!(blocks.last(), Some(Block::Tag { .. })) {
                    style.continuation_cols
                } else {
                    rest
                };
                let lines = alloc::vec![String::from(line)];
                if Self::self_closing_fence(line, tables) {
                    blocks.push(Block::Verbatim {
                        lines,
                        first: fence_indent,
                    });
                    // `writePreClose` asks for a blank line after the region too, and whatever
                    // followed it on the line goes on reflowing after that.
                    if depth == 0 && split {
                        blocks.push(Block::Blank { requested: true });
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
            // `writeHtmlComment` asks for a newline on either side, so an HTML comment stands on
            // a line of its own however the author wove it into a sentence. It is asked after
            // the fence, because inside `<pre>` the lexer reads one as literal text.
            //
            // The line it takes is also what keeps `/** {@inheritDoc} <!--workaround--> */` a
            // three-line comment: `makeSingleLineIfPossible` collapses a Javadoc that renders as
            // *one* content line, and this one renders as two.
            let after_comment;
            if cfg.format_html
                && let Some((before, comment, after)) = Self::split_html_comment(line)
            {
                if !before.is_empty() {
                    Self::tokens_of(before, &mut prose);
                }
                Self::flush(&mut prose, &mut blocks, first, rest);
                blocks.push(Block::Prose {
                    // One token, not a refillable run: the comment's own spaces are inside it.
                    words: alloc::vec![Word {
                        text: comment.into(),
                        space: false,
                    }],
                    first,
                    rest,
                });
                after_comment = String::from(after);
                line = &after_comment;
                if line.is_empty() {
                    continue;
                }
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
                    && !matches!(blocks.last(), Some(Block::Blank { .. }))
                    && (!footer || cfg.blank_lines_between_tags)
                {
                    blocks.push(Block::Blank { requested: false });
                }
                continue;
            }
            if let Some(tag) = Self::block_tag(line) {
                // A `<p>` waiting for the word it introduces, with a footer tag arriving instead:
                // `writeParagraphOpen` already wrote the token, so it keeps a line of its own
                // rather than travelling into the tag's description.
                if let Some(glued) = pending.take() {
                    Self::tokenize(&glued, true, &mut prose);
                }
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
                if blocks
                    .iter()
                    .any(|block| !matches!(block, Block::Blank { .. }))
                {
                    // A blank line the author already wrote *is* the paragraph break — and
                    // inside a list there is none at all, because `flushWhitespace` downgrades
                    // the request to a newline while one is open. A `<p>` continuing an `<li>`
                    // therefore opens the next line, not the line after a gap.
                    if depth == 0 && !matches!(blocks.last(), Some(Block::Blank { .. })) {
                        blocks.push(Block::Blank { requested: true });
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
                if !matches!(blocks.last(), Some(Block::Blank { .. }) | None) {
                    blocks.push(Block::Blank { requested: true });
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
                blocks.push(Block::Blank { requested: true });
                continue;
            }
            // `writeListClose` asks for a newline before the tag and a blank line *after* it, so
            // the prose that follows a list opens a paragraph rather than continuing the line the
            // list closed on: `</OL> Note that …` is two paragraphs, not one sentence.
            if cfg.format_html && cfg.set_off_html_lists && Self::closes_list(line) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                let (list_first, list_rest) = Self::list_indents(depth, line);
                depth = Self::list_depth(depth, line);
                blocks.push(Block::Prose {
                    words: {
                        let mut tag = Vec::new();
                        Self::tokens_of(line, &mut tag);
                        tag
                    },
                    first: list_first + tag_indent,
                    rest: list_rest + tag_indent,
                });
                // Inside an outer list the blank is dropped again — `flushWhitespace` downgrades
                // one to a newline while a list is still open — so only the outermost close sets
                // the paragraph off.
                if depth == 0 {
                    blocks.push(Block::Blank { requested: true });
                }
                first = tag_indent;
                rest = tag_indent;
                continue;
            }
            if cfg.format_html && Self::is_html_block(line, tables) {
                Self::flush(&mut prose, &mut blocks, first, rest);
                // `writeListOpen` requests a blank line before a classic-Javadoc list — but a
                // list is a *block*, and `requestBlankLine` is ignored inside one, so a nested
                // list continues its item rather than starting a paragraph.
                if cfg.set_off_html_lists
                    && depth == 0
                    && Self::opens_list(line)
                    && !matches!(blocks.last(), Some(Block::Blank { .. }) | None)
                {
                    blocks.push(Block::Blank { requested: true });
                }
            }
            // A heading stands alone between blank lines, and what follows it starts a paragraph
            // of its own rather than continuing the heading's line.
            if cfg.format_html && Self::is_heading(line) {
                if !matches!(blocks.last(), Some(Block::Blank { .. }) | None) {
                    blocks.push(Block::Blank { requested: true });
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
                blocks.push(Block::Blank { requested: true });
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
        while matches!(blocks.last(), Some(Block::Blank { .. })) {
            blocks.pop();
        }
        while matches!(blocks.first(), Some(Block::Blank { .. })) {
            blocks.remove(0);
        }
        if cfg.format_html && cfg.paragraph_tags != ParagraphTags::Leading {
            Self::split_paragraph_tags(&mut blocks);
        }
        if cfg.format_html && cfg.paragraph_tags != ParagraphTags::Authored {
            Self::infer_paragraph_tags(&mut blocks, cfg.paragraph_tags == ParagraphTags::OwnLine);
        }
        blocks
    }

    /// Whether a block ends with a tag that is not a literal to google-java-format's lexer.
    ///
    /// `inferParagraphTags` inserts a `<p>` only *between two literals*. A heading, a blockquote,
    /// a list or a preformatted region is its own token at **either** end, so the paragraph after
    /// one opens without a `<p>` — `<blockquote>` followed by a blank line and a `{@code …}` is
    /// the quote's own first paragraph, not a new one.
    fn ends_block_tag(block: &Block) -> bool {
        const NAMES: [&str; 10] = [
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "blockquote",
            "pre",
            "table",
            "p",
        ];
        let Block::Prose { words, .. } = block else {
            return false;
        };
        words.last().is_some_and(|word| {
            let lower = word.text.to_ascii_lowercase();
            let Some(open) = lower.rfind('<') else {
                return false;
            };
            let tag = lower[open + 1..].trim_start_matches('/');
            (NAMES.iter().any(|name| tag.starts_with(name))
                || ["ul", "ol", "dl", "li", "dt", "dd"]
                    .iter()
                    .any(|name| tag.starts_with(name)))
                && lower.ends_with('>')
        })
    }

    /// Fuse each `{@… }` and everything up to its closing brace into one word, while it fits.
    ///
    /// The refill never breaks a word, so a fused tag stays on one line — Eclipse's shape. Two
    /// tags are left alone: one whose brace never closes inside this block, because swallowing
    /// the rest of the paragraph is a worse answer than the break it avoids, and one wider than
    /// `budget`, because Eclipse splits a tag that cannot fit a line either.
    fn join_inline_tags(words: &mut Vec<Word>, budget: usize) {
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
            if Width::utf16(&joined) > budget {
                at += 1;
                continue;
            }
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
            if !matches!(blocks[at - 1], Block::Blank { .. })
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
    fn opens_fence(line: &str, tables: bool) -> bool {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("```")
            || lower.contains("<pre>")
            || (tables && lower.contains("<table"))
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

    /// Rewrite a `<pre>{@code …}</pre>` region into the shape
    /// `JavadocLexer.deindentPreCodeBlocks` gives it.
    ///
    /// Three edits, and all three are on the *delimiter* lines rather than on the code:
    ///
    /// - the opening `{@code` is written against the `<pre>` that fences it, whatever the author
    ///   spaced them with (`Literal(value.trim())`, and the space between the two is a literal
    ///   inside `<pre>`, not a whitespace token);
    /// - the region's blank first and last lines go — they are the gap around the snippet, not
    ///   part of it;
    /// - the `}` that closes the `{@code` moves onto the `</pre>` line, and whatever shared the
    ///   line with it stays behind on one of its own.
    ///
    /// The last is the one that reads like a quirk: `deindentPreCodeBlock` pops the trailing `}`
    /// off the saved tokens and re-emits it *after* the region, so `}}</pre>` comes out as `}`
    /// then `}</pre>` — the inner brace closes the code, the outer one closes the tag.
    ///
    /// A region that says something else on its `<pre>` line — `<pre>{@code foo}` — is ordinary
    /// preformatted text: the lexer matches `[ \t]*[{]@code` against the whole joined literal, so
    /// one word after it is enough to leave every space where the author put it.
    fn normalize_pre_code_block(lines: &mut Vec<String>) {
        let Some(first) = lines.first() else {
            return;
        };
        let lower = first.to_ascii_lowercase();
        let Some(at) = lower.rfind("<pre>") else {
            return;
        };
        let open = at + "<pre>".len();
        if first[open..].trim() != "{@code" {
            return;
        }
        let mut head = String::from(&first[..open]);
        head.push_str("{@code");
        // The region has to close on its last line; a `<pre>` that never closed is left alone.
        let Some(close) = lines.last().and_then(|last| {
            last.to_ascii_lowercase()
                .find("</pre>")
                .map(|at| (at, last.clone()))
        }) else {
            return;
        };
        let (close_at, last) = close;
        let mut tail = String::from(&last[close_at..]);
        let mut body: Vec<String> = lines.drain(1..).collect();
        body.pop();
        let before = last[..close_at].trim_end();
        if !before.is_empty() {
            body.push(before.into());
        }
        while body.first().is_some_and(|line| line.trim().is_empty()) {
            body.remove(0);
        }
        while body.last().is_some_and(|line| line.trim().is_empty()) {
            body.pop();
        }
        if let Some(end) = body.last_mut()
            && end.ends_with('}')
        {
            end.pop();
            tail.insert(0, '}');
            if end.trim().is_empty() {
                body.pop();
            }
        }
        lines.clear();
        lines.push(head);
        lines.append(&mut body);
        lines.push(tail);
        Self::dedent_code_region(lines);
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
        if !lines.first().is_some_and(|line| line.contains("{@code")) {
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
    fn closes_fence(line: &str, tables: bool) -> bool {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("```")
            || lower.contains("</pre>")
            || (tables && lower.contains("</table>"))
    }

    /// Whether a line both opens and closes a region, so it is verbatim on its own.
    fn self_closing_fence(line: &str, tables: bool) -> bool {
        let lower = line.to_ascii_lowercase();
        (lower.contains("<pre>") && lower.contains("</pre>"))
            || (tables && lower.contains("<table") && lower.contains("</table>"))
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
    fn is_html_block(line: &str, tables: bool) -> bool {
        // `<br>` is deliberately absent. It ends a line — `writeBr` requests a newline after
        // it — but it does not start a *block*: the prose on either side of one is the same
        // paragraph, and flushing at it turned `… interfered with. <br>For example …` into two
        // paragraphs, which put the `<br>` at the head of the second instead of the tail of the
        // first. [`Self::is_break_tag`] is where a `<br>` ends its line.
        const BLOCK_TAGS: [&str; 12] = [
            "<p>", "<p ", "<ul", "<ol", "<li", "<dl", "<dt", "<dd", "<h", "</ul", "</ol", "</dl",
        ];
        // A table read as HTML rather than as a preformatted region: its rows and cells are the
        // block elements, and `<caption>` / `<thead>` / `<tbody>` are not — which is JDT's own
        // classification, not a simplification of it.
        const TABLE_TAGS: [&str; 8] = [
            "<table", "</table", "<tr", "</tr", "<td", "</td", "<th", "</th",
        ];
        let lower = line.trim_start().to_ascii_lowercase();
        BLOCK_TAGS.iter().any(|tag| lower.starts_with(tag))
            || (!tables && TABLE_TAGS.iter().any(|tag| lower.starts_with(tag)))
    }

    /// Whether google-java-format's Javadoc lexer would get through this body.
    ///
    /// `JavadocLexer.checkMatchingTags` throws when a nesting context is still open — at a footer
    /// tag, and again at the end of the comment — and `formatJavadoc` answers a `LexException` by
    /// returning the comment **exactly as written**. An unclosed `<pre>`, `<code>` or `<table>`,
    /// or an unbalanced `{@…}`, is therefore a Javadoc the reference does not reflow at all, and
    /// reflowing it here is a difference in every such comment rather than a better one.
    ///
    /// The contexts are the lexer's own: `<pre>`, `<code>` and `<table>` nest, a `{@…}` opens a
    /// brace context and a bare `{` nests only inside one, and inside a brace context there is no
    /// HTML to interpret. `popUntil` is a no-op when its context is not open, so a stray `</code>`
    /// closes nothing.
    fn lexes_cleanly(body: &str) -> bool {
        #[derive(Clone, Copy, PartialEq)]
        enum Ctx {
            Pre,
            Code,
            Table,
            Brace,
        }
        const TAGS: [(&str, Ctx); 3] = [
            ("pre", Ctx::Pre),
            ("code", Ctx::Code),
            ("table", Ctx::Table),
        ];

        let mut stack: Vec<Ctx> = Vec::new();
        for line in body.split('\n') {
            let line = line.trim_start();
            // `somethingSinceNewline` is still false after the `*` and the spaces behind it, so a
            // footer tag is one written at the head of a line.
            if !stack.is_empty() && Self::opens_footer_tag(line) {
                return false;
            }
            let lower = line.to_ascii_lowercase();
            let mut at = 0usize;
            while at < lower.len() {
                // Every tag this looks for is ASCII, so stepping a byte at a time is fine as long
                // as a multi-byte character is stepped over rather than into.
                if !lower.is_char_boundary(at) {
                    at += 1;
                    continue;
                }
                let rest = &lower[at..];
                if let Some(after) = rest.strip_prefix('{') {
                    // `{@…` opens a tag context; a bare `{` nests only inside one.
                    if after.starts_with('@') || stack.contains(&Ctx::Brace) {
                        stack.push(Ctx::Brace);
                    }
                    at += 1;
                    continue;
                }
                if rest.starts_with('}') {
                    if stack.last() == Some(&Ctx::Brace) {
                        stack.pop();
                    }
                    at += 1;
                    continue;
                }
                // Inside an inline tag the rest of the line is literal text.
                if stack.contains(&Ctx::Brace) {
                    at += 1;
                    continue;
                }
                let mut matched = 0usize;
                for (name, ctx) in TAGS {
                    if let Some(end) = Self::html_tag(rest, name, false) {
                        stack.push(ctx);
                        matched = end;
                        break;
                    }
                    if let Some(end) = Self::html_tag(rest, name, true) {
                        if let Some(open) = stack.iter().rposition(|held| *held == ctx) {
                            stack.truncate(open);
                        }
                        matched = end;
                        break;
                    }
                }
                at += matched.max(1);
            }
        }
        stack.is_empty()
    }

    /// The length of the `<name …>` (or `</name …>`) tag `rest` opens with, if it opens one.
    ///
    /// `openTagPattern` is `<(?:name)\b[^>]*>`, so the name has to end at a non-word character
    /// and the tag at the first `>`.
    fn html_tag(rest: &str, name: &str, closing: bool) -> Option<usize> {
        let after = rest
            .strip_prefix('<')?
            .strip_prefix(if closing { "/" } else { "" })?
            .strip_prefix(name)?;
        if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return None;
        }
        let end = after.find('>')? + 1;
        Some(rest.len() - after.len() + end)
    }

    /// Whether a line opens a footer tag — `FOOTER_TAG_PATTERN`, `@` and a lowercase word.
    fn opens_footer_tag(line: &str) -> bool {
        line.strip_prefix('@')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_lowercase()))
    }

    /// Split a line at the first HTML comment: what precedes it, the comment, and what follows.
    ///
    /// `None` when the line holds none, or holds one that does not close on it — a comment
    /// spanning lines is `HTML_COMMENT_PATTERN`'s `DOTALL` case, and giving its opening half a
    /// line of its own would split the comment rather than set it off.
    fn split_html_comment(line: &str) -> Option<(&str, &str, &str)> {
        let open = line.find("<!--")?;
        let close = line[open..].find("-->")? + open + "-->".len();
        Some((
            line[..open].trim_end(),
            &line[open..close],
            line[close..].trim_start(),
        ))
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

    /// Whether a line is nothing but an HTML list's closing tag.
    fn closes_list(line: &str) -> bool {
        let lower = line.trim().to_ascii_lowercase();
        ["</ul", "</ol", "</dl"]
            .iter()
            .any(|tag| lower.starts_with(tag))
            && lower.ends_with('>')
            && !lower[..lower.len() - 1].contains('>')
    }

    /// Split a line at the first block-level tag that has to stand on a line of its own.
    ///
    /// Returns the part before the split and the part from the split on, or `None` when the line
    /// is already one piece. A tag *opening* the line is split from what follows it; a tag
    /// further along is split from what precedes it — `</code></blockquote>` is `</code>` and then
    /// `</blockquote>`, and running the tail back through the loop separates any further tag on
    /// it.
    ///
    /// The tags are the ones the writer surrounds with a line break of its own: a list's open and
    /// close (`writeListOpen` / `writeListClose`) and a blockquote's
    /// (`writeBlockquoteOpenOrClose`).
    ///
    /// Three are deliberately absent. `<li>`'s text starts right after it; `<p>` is glued to the
    /// word it introduces by [`Self::paragraph_open`]; and a heading *contains* its text —
    /// `writeHeaderOpen` asks for a blank line before `<h2>` and none after it, so
    /// `<h2>1.0 Background</h2>` is one line and splitting it strands the heading's own words.
    fn split_block_tag(line: &str) -> Option<(&str, &str)> {
        const TAGS: [&str; 8] = [
            "<ul",
            "<ol",
            "<dl",
            "</ul",
            "</ol",
            "</dl",
            "<blockquote",
            "</blockquote",
        ];
        let lower = line.to_ascii_lowercase();
        // One pass, carrying the brace depth: inside an inline tag there is no HTML to interpret
        // — `{@code <ul>}` is one token to the lexer, and splitting it would break the tag rather
        // than set off a list. Re-counting the braces in front of each position instead would be
        // quadratic in the line, which is a cliff a line of encoded data would fall off.
        let mut depth = 0i32;
        let mut found = None;
        for (at, ch) in lower.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                '<' if depth <= 0 && TAGS.iter().any(|tag| lower[at..].starts_with(tag)) => {
                    found = Some(at);
                    break;
                }
                _ => {}
            }
        }
        let at = found?;
        if at > 0 {
            return Some((line[..at].trim_end(), line[at..].trim_start()));
        }
        let end = lower.find('>')? + 1;
        let tail = line[end..].trim_start();
        (!tail.is_empty()).then(|| (line[..end].trim_end(), tail))
    }

    /// Split a preformatted region's closing line at the end of the tag that closes it.
    ///
    /// `</pre></blockquote>` ends the region at `</pre>`; the `</blockquote>` after it is a token
    /// of the enclosing prose, and keeping it inside the region emits it verbatim — which is how
    /// two tags that each want a line of their own ended up sharing one.
    fn split_after_fence_close(line: &str, tables: bool) -> Option<(&str, &str)> {
        let lower = line.to_ascii_lowercase();
        let end = ["</pre>", "</table>"]
            .iter()
            .filter(|tag| tables || **tag != "</table>")
            .filter_map(|tag| lower.rfind(tag).map(|at| at + tag.len()))
            .max()?;
        let tail = line[end..].trim_start();
        (!tail.is_empty()).then(|| (line[..end].trim_end(), tail))
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
            if !current.is_empty() {
                let gap = usize::from(word.space);
                if Width::utf16(&current) + gap + width > budget && !word.text.starts_with('@') {
                    lines.push(core::mem::take(&mut current));
                } else if word.space {
                    current.push(' ');
                }
            }
            current.push_str(&word.text);
            // `writeBr` requests a newline after the tag, so `<br>` always ends its line — the
            // one it wrapped onto included. Asking only when it landed mid-line let a `<br>` that
            // opened a line swallow the word after it: `<br>For example` is one token to the next
            // run, and the space its own `requestWhitespace` stood for is gone.
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
