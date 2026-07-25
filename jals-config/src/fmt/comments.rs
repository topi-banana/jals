//! `[comments]` — comment and Javadoc reflow.
//!
//! Comments are their own pass in every native formatter (Eclipse's `CommentsPreparator` and its
//! 25 `comment.*` settings, IntelliJ's 20 `JD_*` settings plus `WRAP_COMMENTS`,
//! google-java-format's `JavadocFormatter`), which is why they get their own section rather than
//! the single `wrap-comments` / `comment-width` pair the old rule set had.
//! See `jals-fmt/MAPPING.md` §5.6.

use serde::Deserialize;

/// Comment and Javadoc formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
    /// Reflow `<pre>`-fenced source inside Javadoc. Eclipse `comment.format_source_code`.
    pub format_source_in_comments: bool,
    /// Target width for comment prose. Eclipse `comment.line_length`; IntelliJ reuses
    /// `RIGHT_MARGIN`, so its importer copies `layout.max-width` here.
    pub width: usize,
    /// Measure [`width`](Self::width) from the comment's start column rather than from column
    /// zero. Eclipse `comment.count_line_length_from_starting_position`.
    pub count_width_from_start: bool,
    /// Keep blank lines inside a comment instead of collapsing them. Eclipse
    /// `comment.clear_blank_lines_in_javadoc_comment` (inverted) / IntelliJ `JD_KEEP_EMPTY_LINES`.
    pub preserve_blank_lines: bool,
    /// Keep the source's line breaks inside comment prose instead of refilling. IntelliJ
    /// `JD_PRESERVE_LINE_FEEDS`. Reads input whitespace (`DESIGN.md` §17).
    pub preserve_line_breaks: bool,
    /// Emit a blank line between the Javadoc description and the first block tag. Eclipse
    /// `comment.insert_new_line_before_root_tags` / IntelliJ `JD_ADD_BLANK_AFTER_DESCRIPTION`.
    pub blank_line_before_tags: bool,
    /// Align the descriptions of `@param` / `@throws` tags into a column. Eclipse
    /// `comment.align_tags_names_descriptions` / IntelliJ `JD_ALIGN_PARAM_COMMENTS`.
    pub align_tag_descriptions: bool,
    /// Indent a tag description's continuation lines. Eclipse `comment.indent_tag_description` /
    /// IntelliJ `JD_INDENT_ON_CONTINUATION`.
    pub indent_tag_description: bool,
    /// Emit the leading `*` on every Javadoc line. IntelliJ `JD_LEADING_ASTERISKS_ARE_ENABLED`.
    pub leading_asterisks: bool,
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
            format_source_in_comments: false,
            width: 80,
            count_width_from_start: false,
            preserve_blank_lines: true,
            preserve_line_breaks: false,
            blank_line_before_tags: false,
            align_tag_descriptions: false,
            indent_tag_description: true,
            leading_asterisks: true,
            normalize_parameter_comments: false,
            inline_block_comments: false,
        }
    }
}
