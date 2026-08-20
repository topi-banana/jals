use super::*;
use crate::DiscoverableConfig;

/// The fragment parser, grouped so it is not a free function.
mod api {
    use super::Config;

    /// Parse a `jalsfmt.toml` fragment, panicking with the parse error on failure.
    pub(super) fn parse(src: &str) -> Config {
        toml::from_str(src).expect("fragment should parse")
    }
}

#[test]
fn defaults_align_mapped_keys_with_rustfmt() {
    let c = Config::default();

    // Layout: 4-space indent, a 100-column limit, rustfmt `newline_style = Auto`.
    assert_eq!(c.layout.indent_style, IndentStyle::Space);
    assert_eq!(c.layout.indent_width, 4);
    assert_eq!(c.layout.tab_width, 4);
    // The continuation indent is unset and falls back to `indent-width`.
    assert_eq!(c.layout.continuation_indent, None);
    assert_eq!(c.layout.continuation_cols(), 4);
    assert_eq!(c.layout.max_width, 100);
    assert_eq!(c.layout.line_ending, LineEnding::Auto);
    assert!(c.layout.insert_final_newline);
    assert!(c.layout.trim_trailing_whitespace);
    // Formatter on/off regions are opt-in, but the tags carry the vendors' common spelling.
    assert!(!c.layout.formatter_tags);
    assert_eq!(c.layout.formatter_off_tag, "@formatter:off");
    assert_eq!(c.layout.formatter_on_tag, "@formatter:on");

    // Blank lines: one preserved in code, the usual one-line separators around declarations.
    assert_eq!(c.blank_lines.max_in_code, 1);
    assert_eq!(c.blank_lines.after_package, 1);
    assert_eq!(c.blank_lines.around_method, 1);
    assert_eq!(c.blank_lines.at_type_body_start, 0);

    // Braces: K&R everywhere; an empty body collapses to `{}` and nothing else does.
    assert_eq!(c.braces.type_declaration, BraceStyle::SameLine);
    assert_eq!(c.braces.block, BraceStyle::SameLine);
    assert_eq!(
        c.braces.keep_method_body_on_one_line,
        KeepOnOneLine::IfEmpty
    );
    // The four IntelliJ `*_BRACE_FORCE` keys stay off; rustfmt `match_arm_blocks` is on.
    assert_eq!(c.braces.force_if, ForceBraces::Never);
    assert_eq!(c.braces.force_switch_arm, ForceBraces::Always);

    // Wrapping: wrap on overflow, break before the operator, hug the delimiters.
    // rustfmt `fn_params_layout = Tall` / `short_array_element_width_threshold = 10` /
    // `imports_layout = Mixed` / `remove_nested_parens = true`.
    assert_eq!(c.wrapping.call_arguments, WrapPolicy::IfLong);
    assert_eq!(c.wrapping.method_parameters, WrapPolicy::IfLongPerItem);
    assert_eq!(c.wrapping.fill_item_width, 10);
    assert_eq!(c.wrapping.import_group, WrapPolicy::IfLong);
    assert!(c.wrapping.remove_nested_parens);
    assert!(c.wrapping.before_binary_operator);
    assert_eq!(
        c.wrapping.paren_method_invocation,
        ParenPositions::CommonLines
    );
    // Declaration annotations take their own line; parameter annotations stay inline.
    assert_eq!(c.wrapping.method_annotations, WrapPolicy::AlwaysPerItem);
    assert_eq!(c.wrapping.parameter_annotations, WrapPolicy::Never);

    // Spacing: the idiomatic Java baseline.
    assert!(c.spacing.after_comma);
    assert!(!c.spacing.before_comma);
    assert!(c.spacing.before_keyword_parentheses);
    assert!(!c.spacing.before_method_call_parentheses);

    // Comment reflow is opt-in. rustfmt `doc_comment_code_block_width = 100`.
    assert!(!c.comments.format_javadoc);
    assert_eq!(c.comments.width, 80);
    assert_eq!(c.comments.code_block_width, 100);

    // rustfmt `reorder_imports = true`; the other token-reordering / token-removing import
    // passes stay off.
    assert_eq!(c.imports.order, ImportOrder::Sort);
    assert!(!c.imports.reorder_modifiers);
    assert!(!c.imports.remove_unused);
    assert_eq!(c.imports.groups, ["java.", "javax.", "*", "static"]);

    // Literal rewrites preserve the source, the behavior all four targets share.
    assert_eq!(c.literals.hex_case, HexLiteralCase::Preserve);
    assert_eq!(
        c.literals.float_trailing_zero,
        FloatLiteralTrailingZero::Preserve
    );
    assert_eq!(c.literals.suffix_case, LiteralSuffixCase::Preserve);
}

#[test]
fn empty_input_is_the_default_config() {
    assert_eq!(api::parse(""), Config::default());
}

#[test]
fn an_omitted_section_keeps_its_defaults() {
    let c = api::parse("[layout]\nindent-width = 2\n");
    assert_eq!(c.layout.indent_width, 2);
    // Untouched keys in the same section, and every other section, are unchanged.
    assert_eq!(c.layout.max_width, Layout::default().max_width);
    assert_eq!(c.spacing, Spacing::default());
    assert_eq!(c.wrapping, Wrapping::default());
}

#[test]
fn every_section_round_trips_from_toml() {
    let c = api::parse(
        r#"
        [layout]
        indent-style = "mixed"
        indent-width = 2
        tab-width = 8
        continuation-indent = 4
        max-width = 120
        line-ending = "crlf"
        formatter-tags = true
        formatter-off-tag = "// fmt:off"

        [blank-lines]
        max-in-code = 2
        around-method = 0

        [braces]
        type-declaration = "next-line"
        keep-method-body-on-one-line = "preserve"
        force-if = "always"

        [wrapping]
        call-arguments = "always-per-item"
        paren-method-invocation = "separate-lines"
        before-binary-operator = false

        [spacing]
        before-case-colon = true
        after-label-colon = false

        [comments]
        format-javadoc = true
        width = 100

        [imports]
        order = "group"
        groups = ["static", "*"]

        [literals]
        hex-case = "upper"
        "#,
    );

    assert_eq!(c.layout.indent_style, IndentStyle::Mixed);
    assert_eq!(c.layout.indent_width, 2);
    assert_eq!(c.layout.tab_width, 8);
    assert_eq!(c.layout.continuation_indent, Some(4));
    assert_eq!(c.layout.continuation_cols(), 4);
    assert_eq!(c.layout.max_width, 120);
    assert_eq!(c.layout.line_ending, LineEnding::Crlf);
    assert!(c.layout.formatter_tags);
    assert_eq!(c.layout.formatter_off_tag, "// fmt:off");
    // The `on` tag was not overridden and keeps its default.
    assert_eq!(c.layout.formatter_on_tag, "@formatter:on");

    assert_eq!(c.blank_lines.max_in_code, 2);
    assert_eq!(c.blank_lines.around_method, 0);

    assert_eq!(c.braces.type_declaration, BraceStyle::NextLine);
    assert_eq!(c.braces.method_declaration, BraceStyle::SameLine);
    assert_eq!(
        c.braces.keep_method_body_on_one_line,
        KeepOnOneLine::Preserve
    );
    assert_eq!(c.braces.force_if, ForceBraces::Always);

    assert_eq!(c.wrapping.call_arguments, WrapPolicy::AlwaysPerItem);
    assert_eq!(
        c.wrapping.paren_method_invocation,
        ParenPositions::SeparateLines
    );
    assert!(!c.wrapping.before_binary_operator);

    assert!(c.spacing.before_case_colon);
    assert!(!c.spacing.after_label_colon);

    assert!(c.comments.format_javadoc);
    assert_eq!(c.comments.width, 100);

    assert_eq!(c.imports.order, ImportOrder::Group);
    assert_eq!(c.imports.groups, ["static", "*"]);

    assert_eq!(c.literals.hex_case, HexLiteralCase::Upper);
}

#[test]
fn the_five_colon_contexts_are_independent() {
    // The old rule set folded these into `space-before-colon` plus an additive
    // `space-around-operator-colon`; each context now stands alone.
    let c = api::parse(
        r"
        [spacing]
        before-ternary-colon = false
        before-foreach-colon = true
        before-label-colon = true
        before-case-colon = false
        before-assert-colon = false
        ",
    );
    assert!(!c.spacing.before_ternary_colon);
    assert!(c.spacing.before_foreach_colon);
    assert!(c.spacing.before_label_colon);
    assert!(!c.spacing.before_case_colon);
    assert!(!c.spacing.before_assert_colon);
}

#[test]
fn an_unknown_key_is_ignored_rather_than_rejected() {
    // A config written for a newer jals must still load, so unknown keys are dropped.
    let c = api::parse("[layout]\nindent-width = 2\nnot-a-real-key = 7\n");
    assert_eq!(c.layout.indent_width, 2);
}

#[test]
fn a_rule_that_grew_states_still_reads_the_spelling_it_replaced() {
    // A key whose *value type* changed is the one migration `#[serde(alias)]` cannot perform, and
    // the failure is not that the key goes unread — TOML rejects the file, so every other rule in
    // it stops loading too. Both of these were emitted by `jals_fmt::generate` for every
    // google-java-format and palantir import, and one of them is what this repo's own
    // `jals-fmt.toml` said.
    let old = api::parse(concat!(
        "[wrapping]\ninline-argumentless-annotations = true\n",
        "[comments]\nalign-tag-descriptions = true\n",
    ));
    assert_eq!(
        old.wrapping.inline_argumentless_annotations,
        InlineAnnotations::Declarations,
    );
    assert_eq!(old.comments.tag_alignment, TagAlignment::All);

    let off = api::parse(concat!(
        "[wrapping]\ninline-argumentless-annotations = false\n",
        "[comments]\nalign-tag-descriptions = false\n",
    ));
    assert_eq!(
        off.wrapping.inline_argumentless_annotations,
        InlineAnnotations::Never,
    );
    assert_eq!(off.comments.tag_alignment, TagAlignment::None);

    // The new spelling is what the same keys are written back as, so a migrated file is stable.
    let new = api::parse(concat!(
        "[wrapping]\ninline-argumentless-annotations = \"locals\"\n",
        "[comments]\ntag-alignment = \"grouped\"\n",
    ));
    assert_eq!(
        new.wrapping.inline_argumentless_annotations,
        InlineAnnotations::Locals,
    );
    assert_eq!(new.comments.tag_alignment, TagAlignment::Grouped);
}

#[test]
fn a_documented_member_is_separated_more_less_or_by_its_kind() {
    // Three references, three answers, so the key is a keyword *or* a count — and `0` is not a
    // fourth: it is `AtLeast(0)`, which the kind rule wins over.
    assert_eq!(
        Config::default().blank_lines.around_documented_member,
        DocumentedMember::Inherit,
    );
    let raised = api::parse("[blank-lines]\naround-documented-member = 1\n");
    assert_eq!(
        raised.blank_lines.around_documented_member,
        DocumentedMember::AtLeast(1),
    );
    let kept = api::parse("[blank-lines]\naround-documented-member = \"preserve\"\n");
    assert_eq!(
        kept.blank_lines.around_documented_member,
        DocumentedMember::Preserve,
    );
    assert_eq!(DocumentedMember::Inherit.resolve(1), 1);
    assert_eq!(DocumentedMember::AtLeast(1).resolve(0), 1);
    assert_eq!(DocumentedMember::AtLeast(0).resolve(1), 1);
    assert_eq!(DocumentedMember::Preserve.resolve(1), 0);
}

#[test]
fn a_malformed_value_is_reported() {
    let err = toml::from_str::<Config>("[braces]\ntype-declaration = \"sideways\"\n")
        .expect_err("an unknown enum variant should fail");
    assert!(err.to_string().contains("sideways"), "{err}");
}

#[test]
fn line_ending_resolution() {
    assert_eq!(LineEnding::Lf.resolve("a\r\nb"), "\n");
    assert_eq!(LineEnding::Crlf.resolve("a\nb"), "\r\n");
    // `auto` reads the source's first break, falling back to the platform's for a source
    // with none.
    assert_eq!(LineEnding::Auto.resolve("a\r\nb"), "\r\n");
    assert_eq!(LineEnding::Auto.resolve("a\nb"), "\n");
    assert_eq!(
        LineEnding::Auto.resolve("no break"),
        LineEnding::Native.resolve("")
    );

    let c = Config::default();
    assert_eq!(c.layout.newline("a\r\nb"), "\r\n");
}

#[test]
fn indent_and_continuation_columns() {
    let mut c = Config::default();
    assert_eq!(c.layout.indent_cols(), 4);
    assert_eq!(c.layout.continuation_cols(), 4);

    c.layout.indent_width = 2;
    // Still unset, so it tracks `indent-width`.
    assert_eq!(c.layout.continuation_cols(), 2);
    c.layout.continuation_indent = Some(8);
    assert_eq!(c.layout.continuation_cols(), 8);

    // A zero width would collapse indentation entirely; both accessors clamp to one column.
    c.layout.indent_width = 0;
    c.layout.continuation_indent = Some(0);
    assert_eq!(c.layout.indent_cols(), 1);
    assert_eq!(c.layout.continuation_cols(), 1);
}

#[test]
fn discovery_file_name() {
    assert_eq!(Config::FILE_NAME, "jalsfmt.toml");
}
