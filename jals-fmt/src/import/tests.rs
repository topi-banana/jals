//! Cross-cutting importer tests.
//!
//! Per-vendor coverage and projection live next to each model (`eclipse::tests`,
//! `intellij::tests`). What is checked here is what spans them: the non-file importers, the
//! shared import-group encoding, and the reachability of jals's own rule set.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec;

use jals_config::fmt::{
    BlankLines, Comments, Config, DocumentedMember, ForceBraces, ImportOrder, IndentStyle,
    InlineAnnotations, KeepOnOneLine, Layout, LineEnding, WrapPolicy, Wrapping,
};

use super::eclipse::EclipsePrefs;
use super::gjf::{GjfStyle, GoogleJavaFormatConfig};
use super::intellij::IntellijEditorConfig;
use super::palantir::{PalantirJavaFormatConfig, PalantirStyle};
use super::spotless::{LeadingWhitespace, SpotlessConfig, SpotlessDelegate};
use super::{ConfigImporter, ImportError};

#[test]
fn google_java_format_defaults_to_google_style() {
    let config: Config = GoogleJavaFormatConfig::default().into();

    assert_eq!(config.layout.indent_style, IndentStyle::Space);
    assert_eq!(config.layout.indent_width, 2);
    assert_eq!(config.layout.continuation_indent, Some(4));
    assert_eq!(config.layout.max_width, 100);
    // One column limit drives everything, so the comment width tracks it.
    assert_eq!(config.comments.width, 100);
    assert!(config.comments.format_javadoc);
    assert!(config.comments.normalize_parameter_comments);
    assert!(config.comments.inline_block_comments);
    // A method chain that does not fit goes one call per line.
    assert_eq!(config.wrapping.method_chain, WrapPolicy::IfLongPerItem);
    assert!(config.wrapping.tabular_array_initializers);
    assert_eq!(config.imports.order, ImportOrder::Group);
    assert_eq!(config.imports.groups, ["static", "*"]);
    assert!(config.imports.static_first);
    assert!(config.imports.reorder_modifiers);
    // google-java-format never rewrites a literal.
    assert_eq!(config.literals, jals_config::fmt::Literals::default());
}

#[test]
fn the_aosp_variant_only_doubles_the_indents() {
    let google: Config = GoogleJavaFormatConfig::default().into();
    let aosp: Config = GoogleJavaFormatConfig {
        style: GjfStyle::Aosp,
        ..GoogleJavaFormatConfig::default()
    }
    .into();

    assert_eq!(aosp.layout.indent_width, 4);
    assert_eq!(aosp.layout.continuation_indent, Some(8));
    assert_eq!(aosp.layout.max_width, google.layout.max_width);
    // Nothing else moves.
    assert_eq!(aosp.wrapping, google.wrapping);
    assert_eq!(aosp.spacing, google.spacing);
    assert_eq!(aosp.imports, google.imports);
}

#[test]
fn skipping_a_google_java_format_pass_shows_up_in_the_config() {
    let config: Config = GoogleJavaFormatConfig {
        format_javadoc: false,
        sort_imports: false,
        ..GoogleJavaFormatConfig::default()
    }
    .into();

    assert!(!config.comments.format_javadoc);
    // `--skip-javadoc-formatting` reaches only Javadoc. Line comments are still rewrapped, and
    // block comments are still left alone — `JavaCommentsHelper` never refills one.
    assert!(config.comments.format_line);
    assert!(!config.comments.format_block);
    assert_eq!(config.imports.order, ImportOrder::Preserve);
    // `reorderModifiers` is independent of the import passes.
    assert!(config.imports.reorder_modifiers);
}

#[test]
fn palantir_defaults_to_its_own_style() {
    let config: Config = PalantirJavaFormatConfig::default().into();

    assert_eq!(config.layout.indent_width, 4);
    assert_eq!(config.layout.continuation_indent, Some(8));
    assert_eq!(config.layout.max_width, 120);
    // Unlike google-java-format, Javadoc reflow is off by default.
    assert!(!config.comments.format_javadoc);
}

#[test]
fn palantirs_borrowed_styles_take_google_java_formats_metrics() {
    let palantir_google: Config = PalantirJavaFormatConfig {
        style: PalantirStyle::Google,
        format_javadoc: true,
    }
    .into();
    let gjf: Config = GoogleJavaFormatConfig::default().into();
    // `--google` picks GJF's metrics, not GJF's formatter: the fork's own emission rules still
    // apply, so the two configs agree on everything *except* those.
    assert_eq!(palantir_google.layout, gjf.layout);
    assert_eq!(palantir_google.spacing, gjf.spacing);
    assert_eq!(palantir_google.braces, gjf.braces);
    assert_eq!(palantir_google.imports, gjf.imports);
    assert_eq!(palantir_google.comments, gjf.comments);
    assert_eq!(
        palantir_google.wrapping.inline_argumentless_annotations,
        InlineAnnotations::Locals,
    );
    assert_eq!(
        palantir_google.blank_lines.around_documented_member,
        DocumentedMember::Preserve,
    );
    assert_eq!(
        Config {
            wrapping: gjf.wrapping,
            blank_lines: gjf.blank_lines,
            ..palantir_google.clone()
        },
        Config {
            wrapping: Wrapping {
                inline_argumentless_annotations: InlineAnnotations::Declarations,
                ..palantir_google.wrapping
            },
            blank_lines: BlankLines {
                around_documented_member: DocumentedMember::AtLeast(1),
                ..palantir_google.blank_lines
            },
            ..gjf
        },
    );
}

#[test]
fn spotless_starts_from_its_delegate() {
    let config: Config = SpotlessConfig::default().into();
    let gjf: Config = GoogleJavaFormatConfig::default().into();
    assert_eq!(
        config, gjf,
        "an unconfigured pipeline is exactly its default delegate"
    );

    let palantir: Config = SpotlessConfig {
        delegate: SpotlessDelegate::PalantirJavaFormat(PalantirJavaFormatConfig::default()),
        ..SpotlessConfig::default()
    }
    .into();
    assert_eq!(palantir.layout.max_width, 120);
}

#[test]
fn spotless_generic_steps_layer_over_the_delegate() {
    let config: Config = SpotlessConfig {
        end_with_newline: Some(false),
        trim_trailing_whitespace: Some(false),
        leading_whitespace: Some(LeadingWhitespace::Tabs),
        leading_whitespace_size: Some(8),
        toggle_off_on: true,
        toggle_off_tag: Some("spotless:off".to_owned()),
        toggle_on_tag: Some("spotless:on".to_owned()),
        import_order: vec![
            "java".to_owned(),
            "javax".to_owned(),
            String::new(),
            "\\#".to_owned(),
        ],
        ..SpotlessConfig::default()
    }
    .into();

    assert!(!config.layout.insert_final_newline);
    assert!(!config.layout.trim_trailing_whitespace);
    assert_eq!(config.layout.indent_style, IndentStyle::Tab);
    // The step's `n` is the tab stop, so it fixes the tab's display width too.
    assert_eq!(config.layout.indent_width, 8);
    assert_eq!(config.layout.tab_width, 8);
    assert!(config.layout.formatter_tags);
    assert_eq!(config.layout.formatter_off_tag, "spotless:off");
    assert_eq!(config.layout.formatter_on_tag, "spotless:on");
    assert_eq!(config.imports.order, ImportOrder::Group);
    assert_eq!(config.imports.groups, ["java.", "javax.", "*", "static"]);
}

#[test]
fn spotless_delegates_to_an_eclipse_profile() {
    // `eclipse().configFile(...)` hands over the stringified profile, which is why the delegate
    // deserializes from a setting map rather than a typed table.
    let toml = r#"
        [delegate]
        engine = "eclipse"
        "org.eclipse.jdt.core.formatter.lineSplit" = "140"
        "org.eclipse.jdt.core.formatter.tabulation.size" = "8"
    "#;
    let native: SpotlessConfig = toml::from_str(toml).expect("pipeline should parse");
    let config: Config = native.into();

    assert_eq!(config.layout.max_width, 140);
    assert_eq!(config.layout.indent_width, 8);
}

#[test]
fn the_import_group_encoding_is_shared_across_importers() {
    // Both importers must spell "the package `java` and everything under it" the same way, or a
    // project migrating between them would silently regroup. The trailing dot is what stops
    // `java` from also capturing `javax`.
    let spotless: Config = SpotlessConfig {
        import_order: vec![
            "\\#".to_owned(),
            "java".to_owned(),
            "javax".to_owned(),
            String::new(),
        ],
        ..SpotlessConfig::default()
    }
    .into();
    let intellij: Config = IntellijEditorConfig::parse(
        "[*.java]\nij_java_imports_layout = $*, |, java.**, javax.**, |, *\n",
    )
    .expect("editorconfig should parse")
    .into();

    assert_eq!(spotless.imports.groups, ["static", "java.", "javax.", "*"]);
    assert_eq!(spotless.imports.groups, intellij.imports.groups);
}

#[test]
fn several_static_groups_collapse_into_one() {
    // Native configs may declare several static groups; jals models exactly one.
    let config: Config = SpotlessConfig {
        import_order: vec!["\\#com.acme".to_owned(), "\\#".to_owned(), String::new()],
        ..SpotlessConfig::default()
    }
    .into();
    assert_eq!(config.imports.groups, ["static", "*"]);
}

#[test]
fn a_malformed_xml_document_is_an_error_not_a_panic() {
    #[cfg(feature = "std")]
    {
        use super::eclipse::EclipseXmlProfile;
        let err = EclipseXmlProfile::parse("<profile><setting id=").expect_err("malformed");
        assert!(matches!(err, ImportError::Xml(_)), "{err:?}");
        assert!(err.to_string().contains("XML"));
    }
    // Nothing to assert without the XML readers; the portable readers never fail.
    let _ = ImportError::Deserialize("unused".to_owned());
}

/// Every section of jals's rule set must be reachable from at least one native config —
/// otherwise the rule is speculative and does not belong in the common vocabulary
/// (`jals-fmt/MAPPING.md` §2).
#[test]
fn every_config_section_is_reachable_from_some_vendor() {
    let defaults = Config::default();

    // Eclipse alone moves six of the eight sections.
    let eclipse: Config = EclipsePrefs::parse(
        "org.eclipse.jdt.core.formatter.lineSplit=120\n\
         org.eclipse.jdt.core.formatter.blank_lines_before_method=2\n\
         org.eclipse.jdt.core.formatter.brace_position_for_type_declaration=next_line\n\
         org.eclipse.jdt.core.formatter.alignment_for_enum_constants=2147483647\n\
         org.eclipse.jdt.core.formatter.insert_space_before_comma_in_method_invocation_arguments=insert\n\
         org.eclipse.jdt.core.formatter.comment.format_javadoc_comments=true\n",
    )
    .expect("profile should parse")
    .into();

    assert_ne!(eclipse.layout, defaults.layout);
    assert_ne!(eclipse.blank_lines, defaults.blank_lines);
    assert_ne!(eclipse.braces, defaults.braces);
    assert_ne!(eclipse.wrapping, defaults.wrapping);
    assert_ne!(eclipse.spacing, defaults.spacing);
    assert_ne!(eclipse.comments, defaults.comments);

    // `[imports]` has no Eclipse source at all — the JDT formatter deliberately does not touch
    // imports — so google-java-format is what makes it reachable.
    let gjf: Config = GoogleJavaFormatConfig::default().into();
    assert_ne!(gjf.imports, defaults.imports);

    // `[literals]` is the one jals-native section: all four targets agree on `preserve`, so no
    // importer can move it, and that is recorded rather than asserted away.
    assert_eq!(gjf.literals, defaults.literals);
    assert_eq!(eclipse.literals, defaults.literals);
}

#[test]
fn the_gjf_family_profile_is_the_google_preset() {
    // The golden harness and `jals-fmt.toml` both describe Google Java Style; this is the single
    // definition they should be derived from rather than restate.
    let config: Config = GoogleJavaFormatConfig::default().into();
    assert_eq!(
        config.layout,
        Layout {
            indent_width: 2,
            tab_width: 2,
            continuation_indent: Some(4),
            line_ending: LineEnding::Lf,
            ..Layout::default()
        }
    );
    assert_eq!(
        config.comments,
        Comments {
            format_line: true,
            format_javadoc: true,
            format_header: true,
            width: 100,
            blank_line_before_tags: true,
            reflow_unclosed_html: false,
            normalize_parameter_comments: true,
            inline_block_comments: true,
            code_block_width: 0,
            ..Comments::default()
        }
    );
    assert_eq!(
        config.wrapping,
        Wrapping {
            method_chain: WrapPolicy::IfLongPerItem,
            // `visitConditionalExpression` — `?` and `:` break together.
            ternary: WrapPolicy::IfLongPerItem,
            // `visitFormals` separates parameters with a UNIFIED break, so a parameter list that
            // does not fit goes one per line. An *argument* list is the fill.
            method_parameters: WrapPolicy::IfLongPerItem,
            case_labels: WrapPolicy::IfLongPerItem,
            // `visitEnumDeclaration` forces a break between constants, and `visitTry` between
            // resources.
            enum_constants: WrapPolicy::AlwaysPerItem,
            resource_list: WrapPolicy::AlwaysPerItem,
            // `classDeclarationTypeList`, `visitThrowsClause`, `visitParameterizedType`, and
            // `visitAnnotation` also break all-or-nothing.
            type_arguments: WrapPolicy::IfLongPerItem,
            type_parameters: WrapPolicy::IfLongPerItem,
            deconstruction_list: WrapPolicy::IfLongPerItem,
            multi_catch_types: WrapPolicy::IfLongPerItem,
            for_statement: WrapPolicy::IfLongPerItem,
            annotation_arguments: WrapPolicy::IfLongPerItem,
            extends_list: WrapPolicy::IfLongPerItem,
            throws_list: WrapPolicy::IfLongPerItem,
            tabular_array_initializers: true,
            // `hasOnlyShortItems` / `MAX_ITEM_LENGTH_FOR_FILLING` — an argument list fills only
            // while every argument is under 10 source columns.
            fill_item_width: 10,
            // `isFormatMethod` — a leading format string takes its own line.
            format_string_arguments: true,
            // `fieldAnnotationDirection` asks the same question of every variable.
            parameter_annotations: WrapPolicy::AlwaysPerItem,
            variable_annotations: WrapPolicy::AlwaysPerItem,
            // `visitLabeledStatement` — a forced break after the label's `:`.
            labeled_statement: WrapPolicy::AlwaysPerItem,
            // `fieldAnnotationDirection` — a variable's annotations stay on its line unless one
            // of them takes arguments.
            inline_argumentless_annotations: InlineAnnotations::Declarations,
            // `JavaFormatterOptions.reflowLongStrings` — google-java-format runs `StringWrapper`
            // unless `--skip-reflowing-long-strings`.
            reflow_long_strings: true,
            import_group: WrapPolicy::Never,
            remove_nested_parens: false,
            ..Wrapping::default()
        }
    );
    assert_eq!(
        config.braces,
        jals_config::fmt::Braces {
            keep_type_body_on_one_line: KeepOnOneLine::IfEmpty,
            keep_method_body_on_one_line: KeepOnOneLine::IfEmpty,
            keep_control_statement_on_one_line: true,
            force_switch_arm: ForceBraces::Never,
            ..jals_config::fmt::Braces::default()
        }
    );
    assert!(
        config.imports.remove_unused,
        "google-java-format runs RemoveUnusedImports by default",
    );
}
