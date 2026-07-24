//! Importer round-trips: native config text / model → jals [`Config`].

use alloc::collections::BTreeMap;

use jals_config::fmt::{
    AnnotationPlacement, BinopSeparator, BraceStyle, Config, FnParamsLayout, IndentStyle,
    LineEnding,
};

use super::ConfigImporter;
use super::eclipse::{EclipseConfig, EclipsePrefs};
use super::gjf::{GjfStyle, GoogleJavaFormatConfig};
use super::intellij::{IntellijConfig, IntellijEditorConfig};
use super::palantir::{PalantirJavaFormatConfig, PalantirStyle};
use super::spotless::{SpotlessConfig, SpotlessDelegate};

#[test]
fn gjf_google_and_aosp_indent() {
    let google: Config = GoogleJavaFormatConfig::default().into();
    assert_eq!(google.indent_width, 2);
    assert_eq!(google.continuation_indent, Some(4));
    assert_eq!(google.max_width, 100);
    // GJF binds every rustfmt-style sub-width to the single column driver.
    assert_eq!(google.fn_call_width, 100);
    assert_eq!(google.fn_params_layout, FnParamsLayout::Compressed);

    let aosp: Config = GoogleJavaFormatConfig {
        style: GjfStyle::Aosp,
    }
    .into();
    assert_eq!(aosp.indent_width, 4);
    assert_eq!(aosp.continuation_indent, Some(8));
    assert_eq!(aosp.max_width, 100);
}

#[test]
fn palantir_style_widths() {
    let palantir: Config = PalantirJavaFormatConfig::default().into();
    assert_eq!(palantir.indent_width, 4);
    assert_eq!(palantir.continuation_indent, Some(8));
    assert_eq!(palantir.max_width, 120);
    // formatJavadoc defaults off, so comment reflow is not enabled.
    assert!(!palantir.wrap_comments);

    let google_style: Config = PalantirJavaFormatConfig {
        style: PalantirStyle::Google,
        format_javadoc: true,
    }
    .into();
    assert_eq!(google_style.max_width, 100);
    assert!(google_style.wrap_comments);
}

#[test]
fn eclipse_prefs_common_rules() {
    let prefs = "\
eclipse.preferences.version=1
org.eclipse.jdt.core.compiler.compliance=21
org.eclipse.jdt.core.formatter.tabulation.char=space
org.eclipse.jdt.core.formatter.tabulation.size=4
org.eclipse.jdt.core.formatter.continuation_indentation=2
org.eclipse.jdt.core.formatter.lineSplit=120
org.eclipse.jdt.core.formatter.brace_position_for_type_declaration=next_line
org.eclipse.jdt.core.formatter.wrap_before_binary_operator=false
org.eclipse.jdt.core.formatter.alignment_for_parameters_in_method_declaration=48
org.eclipse.jdt.core.formatter.insert_new_line_at_end_of_file_if_missing=insert
";
    let config = EclipsePrefs::import(prefs).unwrap();
    assert_eq!(config.indent_style, IndentStyle::Space);
    assert_eq!(config.indent_width, 4);
    // continuation_indentation is in units; 2 units × 4-column tab = 8 columns.
    assert_eq!(config.continuation_indent, Some(8));
    assert_eq!(config.max_width, 120);
    assert_eq!(config.brace_style, BraceStyle::NextLine);
    // wrap_before_binary_operator=false ⇒ operator trails the broken line.
    assert_eq!(config.binop_separator, BinopSeparator::Back);
    // alignment 48 = M_ONE_PER_LINE_SPLIT ⇒ one parameter per line.
    assert_eq!(config.fn_params_layout, FnParamsLayout::Vertical);
    assert!(config.insert_final_newline);
    // A `compiler.*` key in the same file must not leak into the formatter model.
}

#[test]
fn eclipse_alignment_never_sentinel_stays_tall() {
    let prefs = "\
org.eclipse.jdt.core.formatter.alignment_for_parameters_in_method_declaration=2147483647
";
    let config = EclipsePrefs::import(prefs).unwrap();
    assert_eq!(config.fn_params_layout, FnParamsLayout::Tall);
}

#[test]
fn eclipse_insert_space_is_enum_not_bool() {
    // Eclipse spells the toggle `do not insert` (interior spaces); it must round-trip.
    let model: EclipseConfig = super::serde_kv::Kv::from_pairs(BTreeMap::from([(
        "org.eclipse.jdt.core.formatter.insert_space_after_colon_in_conditional".to_owned(),
        "do not insert".to_owned(),
    )]))
    .unwrap();
    let config: Config = model.into();
    assert!(!config.space_after_colon);
}

#[test]
fn intellij_editorconfig_common_rules() {
    let editorconfig = "\
root = true
[*]
end_of_line = crlf
insert_final_newline = true
[*.java]
indent_style = space
indent_size = 2
ij_continuation_indent_size = 4
max_line_length = 120
ij_java_keep_blank_lines_in_code = 3
ij_java_class_brace_style = whitesmiths
ij_java_method_parameters_wrap = split_into_lines
ij_java_class_annotation_wrap = on_every_item
ij_java_imports_layout = $*, |, java.**, |, *
";
    let config = IntellijEditorConfig::import(editorconfig).unwrap();
    assert_eq!(config.indent_style, IndentStyle::Space);
    assert_eq!(config.indent_width, 2);
    assert_eq!(config.continuation_indent, Some(4));
    assert_eq!(config.max_width, 120);
    assert_eq!(config.line_ending, LineEnding::Crlf);
    assert!(config.insert_final_newline);
    assert_eq!(config.max_blank_lines, 3);
    // whitesmiths is a next-line variant.
    assert_eq!(config.brace_style, BraceStyle::NextLine);
    // split_into_lines = Wrap Always ⇒ one parameter per line.
    assert_eq!(config.fn_params_layout, FnParamsLayout::Vertical);
    assert_eq!(config.annotation_placement, AnnotationPlacement::Expanded);
    assert!(config.group_imports);
    assert_eq!(config.import_groups, ["static", "java.", "*"]);
}

#[test]
fn intellij_ignores_non_java_sections() {
    let editorconfig = "\
[*.kt]
indent_size = 8
[*.javascript]
indent_size = 16
[*.java]
indent_size = 2
";
    let config = IntellijEditorConfig::import(editorconfig).unwrap();
    // `.javascript` must not be read as Java despite containing the substring "java".
    assert_eq!(config.indent_width, 2);
}

#[test]
fn editorconfig_values_are_case_insensitive() {
    // editorconfig property values are case-insensitive per spec, so a titlecased / uppercased
    // enum token must still apply rather than silently leaving the option at its default.
    let editorconfig = "\
[*.java]
indent_style = Tab
end_of_line = CRLF
insert_final_newline = TRUE
";
    let config = IntellijEditorConfig::import(editorconfig).unwrap();
    // Enum-coerced fields…
    assert_eq!(config.indent_style, IndentStyle::Tab);
    assert_eq!(config.line_ending, LineEnding::Crlf);
    // …and bool-coerced core properties both honor case-insensitive values.
    assert!(config.insert_final_newline);
}

#[test]
fn editorconfig_double_star_section_is_universal() {
    // `[**]` matches every file (a valid universal header), so its Java-applicable keys apply.
    let editorconfig = "\
[**]
indent_size = 8
";
    let config = IntellijEditorConfig::import(editorconfig).unwrap();
    assert_eq!(config.indent_width, 8);
}

#[test]
fn unknown_and_unset_enum_tokens_are_lenient() {
    // editorconfig's spec-valid `unset`, and any token outside the modeled variants, must leave
    // the option unset rather than failing the whole import.
    let editorconfig = "\
[*.java]
indent_style = unset
ij_java_class_brace_style = some_future_style
indent_size = 3
";
    let config = IntellijEditorConfig::import(editorconfig).unwrap();
    // The unparseable enum values fell back to defaults…
    assert_eq!(config.indent_style, IndentStyle::Space);
    assert_eq!(config.brace_style, BraceStyle::SameLine);
    // …while the well-formed numeric value still applied.
    assert_eq!(config.indent_width, 3);
}

#[test]
fn spotless_delegate_plus_generic_steps() {
    let spotless = SpotlessConfig {
        delegate: SpotlessDelegate::PalantirJavaFormat(PalantirJavaFormatConfig::default()),
        end_with_newline: Some(false),
        import_order: ["java".to_owned(), String::new(), "\\#".to_owned()].into(),
    };
    let config: Config = spotless.into();
    // Layout comes from the delegate…
    assert_eq!(config.max_width, 120);
    // …and the generic steps override on top.
    assert!(!config.insert_final_newline);
    assert!(config.group_imports);
    // Package prefixes are dotted so they match at a package boundary (`java` never `javax`) —
    // the same shape the IntelliJ importer produces.
    assert_eq!(config.import_groups, ["java.", "*", "static"]);
}

#[test]
fn spotless_default_delegate_is_gjf() {
    let config: Config = SpotlessConfig::default().into();
    assert_eq!(config.max_width, 100);
    assert_eq!(config.indent_width, 2);
}

#[test]
fn unmodeled_keys_are_ignored() {
    // A key outside the modeled subset must not fail deserialization.
    let model: IntellijConfig = super::serde_kv::Kv::from_pairs(
        [
            ("indent_size".to_owned(), "8".to_owned()),
            (
                "ij_java_some_future_option".to_owned(),
                "whatever".to_owned(),
            ),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    assert_eq!(model.indent_size, Some(8));
}

#[cfg(feature = "std")]
#[test]
fn eclipse_xml_profile_matches_prefs() {
    use super::eclipse::EclipseXmlProfile;

    let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<profiles version="23">
<profile kind="CodeFormatterProfile" name="Eclipse" version="23">
<setting id="org.eclipse.jdt.core.formatter.tabulation.size" value="4"/>
<setting id="org.eclipse.jdt.core.formatter.lineSplit" value="120"/>
<setting id="org.eclipse.jdt.core.formatter.brace_position_for_type_declaration" value="next_line"/>
</profile>
</profiles>"#;
    let config = EclipseXmlProfile::import(xml).unwrap();
    assert_eq!(config.indent_width, 4);
    assert_eq!(config.max_width, 120);
    assert_eq!(config.brace_style, BraceStyle::NextLine);
}

#[cfg(feature = "std")]
#[test]
fn intellij_xml_scheme_translates_ints_and_import_table() {
    use super::intellij::IntellijXmlScheme;

    let xml = r#"<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173">
    <option name="RIGHT_MARGIN" value="120" />
    <JavaCodeStyleSettings>
      <option name="IMPORT_LAYOUT_TABLE">
        <value>
          <package name="" withSubpackages="true" static="true" />
          <emptyLine />
          <package name="java" withSubpackages="true" static="false" />
          <emptyLine />
          <package name="" withSubpackages="true" static="false" />
        </value>
      </option>
    </JavaCodeStyleSettings>
    <codeStyleSettings language="JAVA">
      <option name="CLASS_BRACE_STYLE" value="2" />
      <option name="METHOD_PARAMETERS_WRAP" value="2" />
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
        <option name="CONTINUATION_INDENT_SIZE" value="4" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>"#;
    let config = IntellijXmlScheme::import(xml).unwrap();
    assert_eq!(config.max_width, 120);
    assert_eq!(config.indent_width, 2);
    assert_eq!(config.continuation_indent, Some(4));
    // CLASS_BRACE_STYLE=2 ⇒ next_line.
    assert_eq!(config.brace_style, BraceStyle::NextLine);
    // METHOD_PARAMETERS_WRAP=2 ⇒ split_into_lines ⇒ Vertical.
    assert_eq!(config.fn_params_layout, FnParamsLayout::Vertical);
    // The raw IMPORT_LAYOUT_TABLE (static, blank, java, blank, all) becomes jals groups.
    assert!(config.group_imports);
    assert_eq!(config.import_groups, ["static", "java.", "*"]);
}

#[cfg(feature = "std")]
#[test]
fn intellij_xml_scheme_scopes_options_to_java_language() {
    use super::intellij::IntellijXmlScheme;

    // A realistic multi-language scheme: the JAVA block must win, and a later non-Java block
    // (kotlin, sharing the same UPPER_SNAKE option names) must not clobber it.
    let xml = r#"<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173">
    <codeStyleSettings language="JAVA">
      <option name="METHOD_PARAMETERS_WRAP" value="2" />
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
        <option name="CONTINUATION_INDENT_SIZE" value="4" />
      </indentOptions>
    </codeStyleSettings>
    <codeStyleSettings language="kotlin">
      <option name="METHOD_PARAMETERS_WRAP" value="0" />
      <indentOptions>
        <option name="INDENT_SIZE" value="4" />
        <option name="CONTINUATION_INDENT_SIZE" value="8" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>"#;
    let config = IntellijXmlScheme::import(xml).unwrap();
    // Java's values, not kotlin's.
    assert_eq!(config.indent_width, 2);
    assert_eq!(config.continuation_indent, Some(4));
    assert_eq!(config.fn_params_layout, FnParamsLayout::Vertical);
}
