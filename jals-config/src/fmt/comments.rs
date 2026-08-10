//! `[comments]` — comment and Javadoc reflow.
//!
//! Comments are their own pass in every native formatter (Eclipse's `CommentsPreparator` and its
//! 25 `comment.*` settings, IntelliJ's 20 `JD_*` settings plus `WRAP_COMMENTS`,
//! google-java-format's `JavadocFormatter`), which is why they get their own section rather than
//! the single `wrap-comments` / `comment-width` pair the old rule set had.
//! See `jals-fmt/MAPPING.md` §5.6.

use serde::{Deserialize, Serialize};

/// Where a Javadoc paragraph tag (`<p>`) is written.
///
/// `<p>` is the one piece of Javadoc markup the three reference formatters disagree about
/// outright, and the disagreement is not a width or an indent — it is whether the tag is a word
/// of the paragraph it opens or a line of its own, and whether the formatter may invent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ParagraphTags {
    /// Inferred at every blank line between two paragraphs and glued to the first word of the
    /// paragraph it opens, with the blank line kept in front of it. google-java-format's
    /// `inferParagraphTags` and `JavadocWriter.writeParagraphOpen`.
    #[default]
    Leading,
    /// Inferred at every blank line between two paragraphs, but written on a line of its own.
    /// IntelliJ `JD_P_AT_EMPTY_LINES`.
    OwnLine,
    /// Never inferred; one the author wrote keeps a line of its own. Eclipse JDT, which treats
    /// `<p>` as the block-level HTML element it is and adds none.
    Authored,
}

/// How the descriptions of Javadoc block tags line up.
///
/// Eclipse spells this as two independent booleans (`align_tags_names_descriptions` and
/// `align_tags_descriptions_grouped`) whose "both on" combination it resolves in favour of the
/// first, so the three states it really has are named here as three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TagAlignment {
    /// One space between a tag's name — and its argument, where it takes one — and its
    /// description, and a continuation line indented by
    /// [`indent_tag_description`](Comments::indent_tag_description) alone.
    #[default]
    None,
    /// Descriptions of a run of consecutive tags with the *same* name share a column, and a
    /// continuation line starts at that column. Eclipse `comment.align_tags_descriptions_grouped`.
    Grouped,
    /// Every block tag's description in the comment shares one column. Eclipse
    /// `comment.align_tags_names_descriptions` / IntelliJ `JD_ALIGN_PARAM_COMMENTS`.
    All,
}

/// Comment and Javadoc formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
#[allow(clippy::struct_excessive_bools)]
pub struct Comments {
    /// Reflow `//` line comments. Eclipse `comment.format_line_comments`.
    pub format_line: bool,
    /// Reflow `/* … */` block comments. Eclipse `comment.format_block_comments`.
    pub format_block: bool,
    /// Reflow `/** … */` Javadoc. Eclipse `comment.format_javadoc_comments` / IntelliJ
    /// `ENABLE_JAVADOC_FORMATTING` / google-java-format unless `--skip-javadoc-formatting`.
    pub format_javadoc: bool,
    /// Reflow the file's leading header comment too. Eclipse `comment.format_header`.
    pub format_header: bool,
    /// Reflow the HTML inside Javadoc. Eclipse `comment.format_html`.
    pub format_html: bool,
    /// Where a `<p>` goes, and whether one is invented at a paragraph break.
    pub paragraph_tags: ParagraphTags,
    /// Let a line break fall inside an inline `{@… }` tag.
    ///
    /// No native option turns this on or off — it is the difference between two lexers.
    /// google-java-format's `JavadocLexer` never sees `{@code true}` as a unit, so `{@code` and
    /// `true}` are two literals it will happily separate; Eclipse's `CommentsPreparator` builds
    /// one token out of the whole tag and splits it only when it does not fit a line.
    pub break_inside_inline_tags: bool,
    /// Reflow `<pre>`-fenced source inside Javadoc. Eclipse `comment.format_source_code`.
    pub format_source_in_comments: bool,
    /// Target width for comment prose. Eclipse `comment.line_length`; IntelliJ reuses
    /// `RIGHT_MARGIN`, so its importer copies `layout.max-width` here.
    pub width: usize,
    /// Measure [`width`](Self::width) from the comment's start column rather than from column
    /// zero. Eclipse `comment.count_line_length_from_starting_position`.
    pub count_width_from_start: bool,
    /// Keep blank lines inside a comment's **description** instead of collapsing them. Eclipse
    /// `comment.clear_blank_lines_in_javadoc_comment` (inverted) / IntelliJ `JD_KEEP_EMPTY_LINES`.
    pub preserve_blank_lines: bool,
    /// Keep a blank line the author wrote **between two block tags**.
    ///
    /// Separate from [`preserve_blank_lines`](Self::preserve_blank_lines) because the two
    /// references disagree only here: google-java-format keeps the blank lines of a description
    /// and writes the footer as a solid run whatever the source did (`JavadocWriter` requests a
    /// plain newline between tags), while Eclipse's one `clear_blank_lines_in_javadoc_comment`
    /// governs the whole comment.
    pub blank_lines_between_tags: bool,
    /// Keep the source's line breaks inside comment prose instead of refilling. IntelliJ
    /// `JD_PRESERVE_LINE_FEEDS`. Reads input whitespace, which the single engine does not do:
    /// it rounds this to `false` (always refill) and warns (`DESIGN.md` §17).
    pub preserve_line_breaks: bool,
    /// Emit a blank line between the Javadoc description and the first block tag. Eclipse
    /// `comment.insert_new_line_before_root_tags` / IntelliJ `JD_ADD_BLANK_AFTER_DESCRIPTION`.
    pub blank_line_before_tags: bool,
    /// Align the descriptions of block tags into a column.
    pub tag_alignment: TagAlignment,
    /// Indent a tag description's continuation lines. Eclipse `comment.indent_tag_description` /
    /// IntelliJ `JD_INDENT_ON_CONTINUATION`.
    pub indent_tag_description: bool,
    /// Emit the leading `*` on every Javadoc line. IntelliJ `JD_LEADING_ASTERISKS_ARE_ENABLED`.
    pub leading_asterisks: bool,
    /// Give `/**` and `*/` lines of their own instead of collapsing a Javadoc that would fit on
    /// one. Eclipse `comment.new_lines_at_javadoc_boundaries`.
    ///
    /// Off is google-java-format's `makeSingleLineIfPossible`. JDT's own reading of the option
    /// spares a comment the *source* wrote on one line; the single engine does not read the
    /// source's line breaks, so on means always — `DESIGN.md` §18.2's **D5** covers the residue.
    pub javadoc_boundaries_on_own_lines: bool,
    /// The same for a `/* … */` block comment. Eclipse `comment.new_lines_at_block_boundaries`.
    pub block_boundaries_on_own_lines: bool,
    /// Set an HTML list in Javadoc off from the prose around it: a blank line before it opens,
    /// two columns of indent per `<ul>` level, and four more for the lines continuing an `<li>`.
    ///
    /// google-java-format's `JavadocWriter` does all three — `writeListOpen` requests the blank
    /// line, `continuingListStack` and `continuingListItemStack` the two indents. Eclipse writes a
    /// list flush with the rest of the comment and asks for no gap. No native option either way.
    pub set_off_html_lists: bool,
    /// Reflow a Javadoc whose HTML or inline-tag nesting never closes.
    ///
    /// google-java-format's lexer refuses one: `checkMatchingTags` throws while a `<pre>`,
    /// `<code>`, `<table>` or `{@…}` context is still open — at a footer tag, and again at the
    /// end of the comment — and `formatJavadoc` answers by returning the comment exactly as
    /// written. Eclipse and IntelliJ reflow it anyway, which is the default here; no native
    /// option either way.
    pub reflow_unclosed_html: bool,
    /// Treat an HTML `<table>` in Javadoc as preformatted, emitting it byte-for-byte.
    ///
    /// google-java-format's `JavadocLexer` has a `TABLE_OPEN` / `TABLE_CLOSE` pair that switches
    /// it into preformatted mode; Eclipse reads a table as ordinary HTML and gives `<tr>`, `<td>`
    /// and `<th>` lines of their own while reflowing what is inside them. No native option either
    /// way.
    pub tables_are_preformatted: bool,
    /// Rewrite a parameter-name block comment into google-java-format's canonical spaced form
    /// (`/*a=*/` → `/* a= */`). Its `CommentsHelper.reformatParameterComment`; no Eclipse or
    /// IntelliJ equivalent.
    pub normalize_parameter_comments: bool,
    /// Keep a block comment written before a token on the same line hugging that token
    /// (`java.lang./* @A */ String`) instead of flushing it to end of line. What
    /// google-java-format does; no Eclipse or IntelliJ equivalent.
    pub inline_block_comments: bool,
}

impl Default for Comments {
    fn default() -> Self {
        Self {
            format_line: false,
            format_block: false,
            format_javadoc: false,
            format_header: false,
            format_html: true,
            paragraph_tags: ParagraphTags::Leading,
            break_inside_inline_tags: true,
            format_source_in_comments: false,
            width: 80,
            count_width_from_start: false,
            preserve_blank_lines: true,
            blank_lines_between_tags: false,
            preserve_line_breaks: false,
            blank_line_before_tags: false,
            tag_alignment: TagAlignment::None,
            indent_tag_description: true,
            leading_asterisks: true,
            javadoc_boundaries_on_own_lines: false,
            block_boundaries_on_own_lines: false,
            reflow_unclosed_html: true,
            set_off_html_lists: true,
            tables_are_preformatted: true,
            normalize_parameter_comments: false,
            inline_block_comments: false,
        }
    }
}
