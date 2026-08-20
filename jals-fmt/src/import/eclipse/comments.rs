//! Eclipse JDT — the 25 `comment.*` settings.
//!
//! Declared in `DefaultCodeFormatterConstants` as full literal ids rather than through the
//! `JavaCore.PLUGIN_ID + ".formatter."` concatenation the other 391 use, which is why they are
//! easy to miss when enumerating the surface.

use crate::import::serde_kv;
use serde::Deserialize;

use super::values::Insert;

/// The `comment.*` settings of a profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Comments {
    /// `comment.align_tags_descriptions_grouped`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.align_tags_descriptions_grouped",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_align_tags_descriptions_grouped: Option<bool>,
    /// `comment.align_tags_names_descriptions`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.align_tags_names_descriptions",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_align_tags_names_descriptions: Option<bool>,
    /// `comment.clear_blank_lines`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.clear_blank_lines",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_clear_blank_lines: Option<bool>,
    /// `comment.clear_blank_lines_in_block_comment`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.clear_blank_lines_in_block_comment",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_clear_blank_lines_in_block_comment: Option<bool>,
    /// `comment.clear_blank_lines_in_javadoc_comment`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.clear_blank_lines_in_javadoc_comment",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_clear_blank_lines_in_javadoc_comment: Option<bool>,
    /// `comment.count_line_length_from_starting_position`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.count_line_length_from_starting_position",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_count_line_length_from_starting_position: Option<bool>,
    /// `comment.format_block_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_block_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_block_comments: Option<bool>,
    /// `comment.format_comments`. Deprecated: JDT still reads it and fans it out into the finer settings above.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_comments: Option<bool>,
    /// `comment.format_header`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_header",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_header: Option<bool>,
    /// `comment.format_html`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_html",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_html: Option<bool>,
    /// `comment.format_javadoc_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_javadoc_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_javadoc_comments: Option<bool>,
    /// `comment.format_line_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_line_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_line_comments: Option<bool>,
    /// `comment.format_markdown_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_markdown_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_markdown_comments: Option<bool>,
    /// `comment.format_source_code`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.format_source_code",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_format_source_code: Option<bool>,
    /// `comment.indent_parameter_description`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.indent_parameter_description",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_indent_parameter_description: Option<bool>,
    /// `comment.indent_root_tags`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.indent_root_tags",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_indent_root_tags: Option<bool>,
    /// `comment.indent_tag_description`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.indent_tag_description",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_indent_tag_description: Option<bool>,
    /// `comment.insert_new_line_before_root_tags`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.insert_new_line_before_root_tags",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub comment_insert_new_line_before_root_tags: Option<Insert>,
    /// `comment.insert_new_line_between_different_tags`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.insert_new_line_between_different_tags",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub comment_insert_new_line_between_different_tags: Option<Insert>,
    /// `comment.insert_new_line_for_parameter`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.insert_new_line_for_parameter",
        deserialize_with = "serde_kv::opt_enum"
    )]
    pub comment_insert_new_line_for_parameter: Option<Insert>,
    /// `comment.javadoc_do_not_separate_block_tags`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.javadoc_do_not_separate_block_tags",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_javadoc_do_not_separate_block_tags: Option<bool>,
    /// `comment.line_length`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.line_length",
        deserialize_with = "serde_kv::opt_number"
    )]
    pub comment_line_length: Option<usize>,
    /// `comment.new_lines_at_block_boundaries`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.new_lines_at_block_boundaries",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_new_lines_at_block_boundaries: Option<bool>,
    /// `comment.new_lines_at_javadoc_boundaries`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.new_lines_at_javadoc_boundaries",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_new_lines_at_javadoc_boundaries: Option<bool>,
    /// `comment.preserve_white_space_between_code_and_line_comments`.
    #[serde(
        rename = "org.eclipse.jdt.core.formatter.comment.preserve_white_space_between_code_and_line_comments",
        deserialize_with = "serde_kv::opt_bool"
    )]
    pub comment_preserve_white_space_between_code_and_line_comments: Option<bool>,
}
