use super::*;

#[test]
fn defaults() {
    let c = Config::default();
    assert_eq!(c.indent_width, 4);
    // Continuation indent is unset by default and falls back to `indent-width`.
    assert_eq!(c.continuation_indent, None);
    assert_eq!(c.continuation_cols(), 4);
    assert_eq!(c.max_width, 100);
    assert_eq!(c.chain_width, 60);
    assert_eq!(c.fn_call_width, 60);
    assert_eq!(c.comment_width, 80);
    assert_eq!(c.max_blank_lines, 1);
    // A leading blank line at the start of a braced body is dropped by default; opt-in keeps it.
    assert!(!c.blank_line_at_block_start);
    assert!(c.insert_final_newline);
    // Comment reflow is opt-in, mirroring rustfmt's `wrap_comments`.
    assert!(!c.wrap_comments);
    // Embedded comments are flushed to end of line by default; opt-in keeps them inline.
    assert!(!c.inline_block_comments);
    // K&R braces by default, for both declaration and control-flow braces.
    assert_eq!(c.brace_style, BraceStyle::SameLine);
    assert_eq!(c.control_brace_style, ControlBraceStyle::SameLine);
    // Empty declaration bodies collapse to `{}` by default (rustfmt's `empty_item_single_line`).
    assert!(c.empty_item_single_line);
    // Import sorting is opt-in; off by default to preserve the significant-token sequence.
    assert!(!c.reorder_imports);
    // Trailing-comma handling defaults to preserve, keeping the source comma exactly.
    assert_eq!(c.trailing_comma, TrailingComma::Preserve);
    // Import grouping is opt-in; off by default, with a JDK / others / static default order.
    assert!(!c.group_imports);
    assert_eq!(c.import_groups, ["java.", "javax.", "*", "static"]);
    // Wrapped binary operators lead their continuation line by default.
    assert_eq!(c.binop_separator, BinopSeparator::Front);
    // Binary runs default to the all-or-nothing Tall layout (the prior behavior).
    assert_eq!(c.binop_layout, BinopLayout::Tall);
    // Last-argument overflow is opt-in; off by default keeps the all-or-nothing layout.
    assert!(!c.overflow_delimited_expr);
    // A wrapped paren list's closing `)` dedents onto its own line by default (the prior behavior).
    assert_eq!(c.closing_paren, ClosingParen::OwnLine);
    // A switch expression stays on the `=` line by default.
    assert!(!c.switch_expression_on_new_line);
    // A legacy (colon-form) switch breaks and indents its case bodies by default (GJF layout).
    assert_eq!(c.switch_case_body, SwitchCaseBody::Always);
    // Colon spacing defaults to idiomatic `label:` / `case x:` style: no space before,
    // one space after.
    assert!(!c.space_before_colon);
    assert!(c.space_after_colon);
    // The operator-colon (enhanced-`for` / ternary / `assert`) space-before is opt-in; off by
    // default so colon spacing stays uniform.
    assert!(!c.space_around_operator_colon);
    // Parameter lists default to the all-or-nothing Tall layout (the prior behavior).
    assert_eq!(c.fn_params_layout, FnParamsLayout::Tall);
    // Modifier reordering is opt-in; off by default to preserve the significant-token sequence.
    assert!(!c.reorder_modifiers);
    // Single-statement bodies are not collapsed onto one line by default (rustfmt's
    // `fn_single_line` is also off by default).
    assert!(!c.fn_single_line);
    // Blocks are not forced multi-line by default (collapses stay available).
    assert!(!c.force_multiline_blocks);
    // Annotation placement defaults to Compact (inline, the prior behavior).
    assert_eq!(c.annotation_placement, AnnotationPlacement::Compact);
    // Hex-literal case defaults to preserve, keeping the source case exactly.
    assert_eq!(c.hex_literal_case, HexLiteralCase::Preserve);
}

#[test]
fn brace_style_parses_kebab_values() {
    let c: Config = toml::from_str("brace-style = \"next-line\"\n").unwrap();
    assert_eq!(c.brace_style, BraceStyle::NextLine);
    let c: Config = toml::from_str("brace-style = \"same-line\"\n").unwrap();
    assert_eq!(c.brace_style, BraceStyle::SameLine);
}

#[test]
fn closing_paren_parses_kebab_values() {
    let c: Config = toml::from_str("closing-paren = \"hug\"\n").unwrap();
    assert_eq!(c.closing_paren, ClosingParen::Hug);
    let c: Config = toml::from_str("closing-paren = \"own-line\"\n").unwrap();
    assert_eq!(c.closing_paren, ClosingParen::OwnLine);
}

#[test]
fn control_brace_style_parses_kebab_values() {
    let c: Config = toml::from_str("control-brace-style = \"next-line\"\n").unwrap();
    assert_eq!(c.control_brace_style, ControlBraceStyle::NextLine);
    let c: Config = toml::from_str("control-brace-style = \"same-line\"\n").unwrap();
    assert_eq!(c.control_brace_style, ControlBraceStyle::SameLine);
}

#[test]
fn empty_item_single_line_parses() {
    let c: Config = toml::from_str("empty-item-single-line = false\n").unwrap();
    assert!(!c.empty_item_single_line);
    let c: Config = toml::from_str("empty-item-single-line = true\n").unwrap();
    assert!(c.empty_item_single_line);
}

#[test]
fn fn_single_line_parses() {
    let c: Config = toml::from_str("fn-single-line = true\n").unwrap();
    assert!(c.fn_single_line);
    let c: Config = toml::from_str("fn-single-line = false\n").unwrap();
    assert!(!c.fn_single_line);
}

#[test]
fn force_multiline_blocks_parses() {
    let c: Config = toml::from_str("force-multiline-blocks = true\n").unwrap();
    assert!(c.force_multiline_blocks);
    let c: Config = toml::from_str("force-multiline-blocks = false\n").unwrap();
    assert!(!c.force_multiline_blocks);
}

#[test]
fn wrap_comments_parses() {
    let c: Config = toml::from_str("wrap-comments = true\ncomment-width = 60\n").unwrap();
    assert!(c.wrap_comments);
    assert_eq!(c.comment_width, 60);
}

#[test]
fn inline_block_comments_parses() {
    let c: Config = toml::from_str("inline-block-comments = true\n").unwrap();
    assert!(c.inline_block_comments);
}

#[test]
fn reorder_imports_parses() {
    let c: Config = toml::from_str("reorder-imports = true\n").unwrap();
    assert!(c.reorder_imports);
}

#[test]
fn reorder_modifiers_parses() {
    let c: Config = toml::from_str("reorder-modifiers = true\n").unwrap();
    assert!(c.reorder_modifiers);
}

#[test]
fn trailing_comma_parses_kebab_values() {
    let c: Config = toml::from_str("trailing-comma = \"always\"\n").unwrap();
    assert_eq!(c.trailing_comma, TrailingComma::Always);
    let c: Config = toml::from_str("trailing-comma = \"never\"\n").unwrap();
    assert_eq!(c.trailing_comma, TrailingComma::Never);
    let c: Config = toml::from_str("trailing-comma = \"vertical\"\n").unwrap();
    assert_eq!(c.trailing_comma, TrailingComma::Vertical);
    let c: Config = toml::from_str("trailing-comma = \"preserve\"\n").unwrap();
    assert_eq!(c.trailing_comma, TrailingComma::Preserve);
}

#[test]
fn binop_separator_parses_kebab_values() {
    let c: Config = toml::from_str("binop-separator = \"front\"\n").unwrap();
    assert_eq!(c.binop_separator, BinopSeparator::Front);
    let c: Config = toml::from_str("binop-separator = \"back\"\n").unwrap();
    assert_eq!(c.binop_separator, BinopSeparator::Back);
}

#[test]
fn binop_layout_parses_kebab_values() {
    let c: Config = toml::from_str("binop-layout = \"tall\"\n").unwrap();
    assert_eq!(c.binop_layout, BinopLayout::Tall);
    let c: Config = toml::from_str("binop-layout = \"compressed\"\n").unwrap();
    assert_eq!(c.binop_layout, BinopLayout::Compressed);
}

#[test]
fn overflow_delimited_expr_parses() {
    let c: Config = toml::from_str("overflow-delimited-expr = true\n").unwrap();
    assert!(c.overflow_delimited_expr);
}

#[test]
fn switch_expression_on_new_line_parses() {
    let c: Config = toml::from_str("switch-expression-on-new-line = true\n").unwrap();
    assert!(c.switch_expression_on_new_line);
}

#[test]
fn switch_case_body_parses_kebab_values() {
    let c: Config = toml::from_str("switch-case-body = \"always\"\n").unwrap();
    assert_eq!(c.switch_case_body, SwitchCaseBody::Always);
    let c: Config = toml::from_str("switch-case-body = \"single-line\"\n").unwrap();
    assert_eq!(c.switch_case_body, SwitchCaseBody::SingleLine);
    let c: Config = toml::from_str("switch-case-body = \"same-line\"\n").unwrap();
    assert_eq!(c.switch_case_body, SwitchCaseBody::SameLine);
}

#[test]
fn fn_params_layout_parses_kebab_values() {
    let c: Config = toml::from_str("fn-params-layout = \"tall\"\n").unwrap();
    assert_eq!(c.fn_params_layout, FnParamsLayout::Tall);
    let c: Config = toml::from_str("fn-params-layout = \"compressed\"\n").unwrap();
    assert_eq!(c.fn_params_layout, FnParamsLayout::Compressed);
    let c: Config = toml::from_str("fn-params-layout = \"vertical\"\n").unwrap();
    assert_eq!(c.fn_params_layout, FnParamsLayout::Vertical);
}

#[test]
fn fn_args_layout_is_a_deprecated_alias() {
    // The rustfmt-era `fn-args-layout` key maps to the same field as `fn-params-layout`.
    let c: Config = toml::from_str("fn-args-layout = \"vertical\"\n").unwrap();
    assert_eq!(c.fn_params_layout, FnParamsLayout::Vertical);
}

#[test]
fn colon_spacing_parses_kebab_keys() {
    let c: Config =
        toml::from_str("space-before-colon = true\nspace-after-colon = false\n").unwrap();
    assert!(c.space_before_colon);
    assert!(!c.space_after_colon);
}

#[test]
fn space_around_operator_colon_parses_kebab_key() {
    let c: Config = toml::from_str("space-around-operator-colon = true\n").unwrap();
    assert!(c.space_around_operator_colon);
}

#[test]
fn type_punctuation_density_parses_kebab_values() {
    let c: Config = toml::from_str("type-punctuation-density = \"wide\"\n").unwrap();
    assert_eq!(c.type_punctuation_density, TypePunctuationDensity::Wide);
    let c: Config = toml::from_str("type-punctuation-density = \"compressed\"\n").unwrap();
    assert_eq!(
        c.type_punctuation_density,
        TypePunctuationDensity::Compressed
    );
}

#[test]
fn annotation_placement_parses_kebab_values() {
    let c: Config = toml::from_str("annotation-placement = \"compact\"\n").unwrap();
    assert_eq!(c.annotation_placement, AnnotationPlacement::Compact);
    let c: Config = toml::from_str("annotation-placement = \"expanded\"\n").unwrap();
    assert_eq!(c.annotation_placement, AnnotationPlacement::Expanded);
}

#[test]
fn hex_literal_case_parses_kebab_values() {
    let c: Config = toml::from_str("hex-literal-case = \"preserve\"\n").unwrap();
    assert_eq!(c.hex_literal_case, HexLiteralCase::Preserve);
    let c: Config = toml::from_str("hex-literal-case = \"upper\"\n").unwrap();
    assert_eq!(c.hex_literal_case, HexLiteralCase::Upper);
    let c: Config = toml::from_str("hex-literal-case = \"lower\"\n").unwrap();
    assert_eq!(c.hex_literal_case, HexLiteralCase::Lower);
}

#[test]
fn float_literal_trailing_zero_parses_kebab_values() {
    let c: Config = toml::from_str("float-literal-trailing-zero = \"preserve\"\n").unwrap();
    assert_eq!(
        c.float_literal_trailing_zero,
        FloatLiteralTrailingZero::Preserve
    );
    let c: Config = toml::from_str("float-literal-trailing-zero = \"always\"\n").unwrap();
    assert_eq!(
        c.float_literal_trailing_zero,
        FloatLiteralTrailingZero::Always
    );
    let c: Config = toml::from_str("float-literal-trailing-zero = \"never\"\n").unwrap();
    assert_eq!(
        c.float_literal_trailing_zero,
        FloatLiteralTrailingZero::Never
    );
}

#[test]
fn literal_suffix_case_parses_kebab_values() {
    let c: Config = toml::from_str("literal-suffix-case = \"preserve\"\n").unwrap();
    assert_eq!(c.literal_suffix_case, LiteralSuffixCase::Preserve);
    let c: Config = toml::from_str("literal-suffix-case = \"upper\"\n").unwrap();
    assert_eq!(c.literal_suffix_case, LiteralSuffixCase::Upper);
    let c: Config = toml::from_str("literal-suffix-case = \"lower\"\n").unwrap();
    assert_eq!(c.literal_suffix_case, LiteralSuffixCase::Lower);
}

#[test]
fn group_imports_parses() {
    let c: Config = toml::from_str("group-imports = true\n").unwrap();
    assert!(c.group_imports);
}

#[test]
fn import_groups_parses() {
    // The Vec<String> key parses from a TOML array (no other Vec field exists yet).
    let c: Config = toml::from_str("import-groups = [\"java.\", \"*\"]\n").unwrap();
    assert_eq!(c.import_groups, ["java.", "*"]);
}

#[test]
fn chain_width_parses_kebab_key() {
    let c: Config = toml::from_str("chain-width = 40\n").unwrap();
    assert_eq!(c.chain_width, 40);
}

#[test]
fn continuation_indent_parses_kebab_key() {
    let c: Config = toml::from_str("continuation-indent = 8\n").unwrap();
    assert_eq!(c.continuation_indent, Some(8));
    assert_eq!(c.continuation_cols(), 8);
    // Omitted ⇒ None, falling back to `indent-width`.
    assert_eq!(Config::default().continuation_indent, None);
    // Tab style ignores `continuation-indent`: one tab per continuation level.
    let c: Config = toml::from_str("indent-style = \"tab\"\ncontinuation-indent = 8\n").unwrap();
    assert_eq!(c.continuation_cols(), c.indent_cols());
}

#[test]
fn fn_call_width_parses_kebab_key() {
    let c: Config = toml::from_str("fn-call-width = 40\n").unwrap();
    assert_eq!(c.fn_call_width, 40);
}

#[test]
fn array_width_parses_kebab_key() {
    let c: Config = toml::from_str("array-width = 40\n").unwrap();
    assert_eq!(c.array_width, 40);
}

#[test]
fn single_line_if_else_max_width_parses_kebab_key() {
    let c: Config = toml::from_str("single-line-if-else-max-width = 40\n").unwrap();
    assert_eq!(c.single_line_if_else_max_width, 40);
}

#[test]
fn max_blank_lines_parses_kebab_key() {
    let c: Config = toml::from_str("max-blank-lines = 2\n").unwrap();
    assert_eq!(c.max_blank_lines, 2);
}

#[test]
fn blank_line_at_block_start_parses_kebab_key() {
    let c: Config = toml::from_str("blank-line-at-block-start = true\n").unwrap();
    assert!(c.blank_line_at_block_start);
}

#[test]
fn partial_toml_falls_back_to_defaults() {
    let c: Config = toml::from_str("indent-width = 2\n").unwrap();
    assert_eq!(c.indent_width, 2);
    // untouched keys keep defaults
    assert_eq!(c.max_width, 100);
    assert_eq!(c.indent_style, IndentStyle::Space);
}

#[test]
fn enums_parse_kebab_values() {
    let c: Config = toml::from_str("indent-style = \"tab\"\nline-ending = \"crlf\"\n").unwrap();
    assert_eq!(c.indent_style, IndentStyle::Tab);
    assert_eq!(c.line_ending, LineEnding::Crlf);
    assert_eq!(c.indent_unit(), "\t");
    // A fixed line ending ignores the source text.
    assert_eq!(c.newline("a\nb"), "\r\n");
}

#[test]
fn auto_and_native_parse() {
    let c: Config = toml::from_str("line-ending = \"auto\"\n").unwrap();
    assert_eq!(c.line_ending, LineEnding::Auto);
    let c: Config = toml::from_str("line-ending = \"native\"\n").unwrap();
    assert_eq!(c.line_ending, LineEnding::Native);
}

#[test]
fn auto_detects_from_first_line_break() {
    let auto = Config {
        line_ending: LineEnding::Auto,
        ..Config::default()
    };
    // The first line break decides: CRLF stays CRLF, a bare LF stays LF.
    assert_eq!(auto.newline("a\r\nb\nc"), "\r\n");
    assert_eq!(auto.newline("a\nb\r\nc"), "\n");
    assert_eq!(auto.newline("only one\nbreak"), "\n");
    assert_eq!(auto.newline("\r\n"), "\r\n");
    assert_eq!(auto.newline("\n"), "\n");
}

#[test]
fn auto_without_line_break_falls_back_to_native() {
    let auto = Config {
        line_ending: LineEnding::Auto,
        ..Config::default()
    };
    let native = Config {
        line_ending: LineEnding::Native,
        ..Config::default()
    };
    // No `\n` anywhere ⇒ same answer as Native (platform-dependent, so compare the two).
    assert_eq!(auto.newline("no breaks here"), native.newline(""));
    assert_eq!(auto.newline(""), native.newline(""));
}

#[test]
fn native_matches_platform() {
    let native = Config {
        line_ending: LineEnding::Native,
        ..Config::default()
    };
    let expected = if cfg!(windows) { "\r\n" } else { "\n" };
    assert_eq!(native.newline(""), expected);
}

#[test]
fn space_indent_unit() {
    let c = Config {
        indent_width: 2,
        ..Config::default()
    };
    assert_eq!(c.indent_unit(), "  ");
}
