use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::{
    BraceStyle, Config, ForceBraces, ImportOrder, IndentStyle, KeepOnOneLine, LineEnding,
    ParenPositions, WrapPolicy,
};

use super::super::ConfigImporter;
use super::{IntellijConfig, IntellijEditorConfig};

/// The machine-extracted option inventory this importer is measured against.
const INVENTORY: &str = include_str!("inventory.tsv");

/// One inventory row.
struct Row {
    /// The XML option name, or the editorconfig key when the setting has no XML form.
    setting: String,
    kind: String,
    /// The `.editorconfig` key, or `None` when the setting is XML-only.
    editorconfig: Option<String>,
}

/// The inventory reader and the scheme builder, grouped so they are not free functions.
struct Fixture;

impl Fixture {
    /// Every option row, comments and blank lines dropped.
    fn inventory() -> Vec<Row> {
        INVENTORY
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let columns: Vec<&str> = line.split('\t').collect();
                let (name, kind, key) = (columns[0], columns[2], columns[3]);
                Row {
                    setting: if name == "-" { key } else { name }.to_owned(),
                    kind: kind.to_owned(),
                    editorconfig: (key != "-").then(|| key.to_owned()),
                }
            })
            .collect()
    }

    /// A parseable, non-default value for a setting of `kind`.
    fn probe(kind: &str) -> &'static str {
        match kind {
            "bool" => "true",
            "int" | "wrap-on-typing" | "rearrange-mode" | "javadoc-names" => "7",
            "string" => "x",
            "wrap" => "normal",
            "brace" => "next_line",
            "force-braces" => "always",
            "package-table" => "java.**",
            "value-list" => "org.junit.Test",
            other => panic!("inventory row has an unknown value kind `{other}`"),
        }
    }

    /// Parse a scheme given as `(XML option name, value)` pairs.
    fn scheme(options: &[(&str, &str)]) -> IntellijConfig {
        let pairs = options
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        IntellijConfig::from_pairs(pairs).expect("scheme should parse")
    }
}

#[test]
fn every_inventoried_setting_is_modeled() {
    let baseline = IntellijConfig::default();
    let mut missing = Vec::new();

    for row in Fixture::inventory() {
        let mut pairs = BTreeMap::new();
        pairs.insert(row.setting.clone(), Fixture::probe(&row.kind).to_owned());
        let parsed = IntellijConfig::from_pairs(pairs).expect("single setting should parse");
        if parsed == baseline {
            missing.push(row.setting);
        }
    }

    assert!(
        missing.is_empty(),
        "{} IntelliJ setting(s) in inventory.tsv are not captured by the model: {missing:?}",
        missing.len()
    );
}

#[test]
fn every_editorconfig_key_resolves_to_its_setting() {
    // The generated key table and the inventory must not drift apart, or an `.editorconfig`
    // would silently lose settings the XML form keeps.
    let mut unresolved = Vec::new();
    for row in Fixture::inventory() {
        if let Some(key) = row.editorconfig
            && IntellijConfig::setting_name(&key) != Some(row.setting.as_str())
        {
            unresolved.push(key);
        }
    }
    assert!(
        unresolved.is_empty(),
        "editorconfig key(s) missing from the generated table: {unresolved:?}"
    );
}

#[test]
fn the_inventory_is_the_documented_size() {
    let rows = Fixture::inventory();
    assert_eq!(
        rows.len(),
        297,
        "14 indent + 182 common + 92 java + 6 general + 3 editorconfig"
    );
    // Eight `<indentOptions>` settings are reachable only through the XML scheme.
    assert_eq!(rows.iter().filter(|r| r.editorconfig.is_none()).count(), 8);
}

#[test]
fn editorconfig_and_xml_spellings_agree() {
    let from_editorconfig = IntellijEditorConfig::parse(
        "root = true\n\
         [*.java]\n\
         indent_style = space\n\
         indent_size = 2\n\
         ij_continuation_indent_size = 4\n\
         max_line_length = 120\n\
         ij_java_class_brace_style = next_line\n\
         ij_java_method_parameters_wrap = on_every_item\n\
         ij_java_if_brace_force = always\n",
    )
    .expect("editorconfig should parse");

    // The same settings in the scheme XML's raw-integer spelling.
    let from_scheme = Fixture::scheme(&[
        ("USE_TAB_CHARACTER", "false"),
        ("INDENT_SIZE", "2"),
        ("CONTINUATION_INDENT_SIZE", "4"),
        ("RIGHT_MARGIN", "120"),
        ("CLASS_BRACE_STYLE", "2"),
        ("METHOD_PARAMETERS_WRAP", "4"),
        ("IF_BRACE_FORCE", "3"),
    ]);

    assert_eq!(from_editorconfig, from_scheme);

    let config: Config = from_scheme.into();
    assert_eq!(config.layout.indent_style, IndentStyle::Space);
    assert_eq!(config.layout.indent_width, 2);
    assert_eq!(config.layout.continuation_indent, Some(4));
    assert_eq!(config.layout.max_width, 120);
    assert_eq!(config.braces.type_declaration, BraceStyle::NextLine);
    assert_eq!(
        config.wrapping.method_parameters,
        WrapPolicy::IfLongPerItem,
        "`on_every_item` / `4` is Chop Down If Long"
    );
    assert_eq!(config.braces.force_if, ForceBraces::Always);
}

#[test]
fn the_universal_section_cascades_into_the_java_one() {
    // The realistic file shape: the settings jals newly models — `end_of_line`,
    // `insert_final_newline`, `trim_trailing_whitespace` — are EditorConfig core properties that
    // real files put under `[*]`, not under `[*.java]`. The coverage test feeds the model
    // directly and would not notice a reader that only looked at `[*.java]`.
    let config: Config = IntellijEditorConfig::parse(
        "root = true\n\
         [*]\n\
         end_of_line = crlf\n\
         insert_final_newline = false\n\
         trim_trailing_whitespace = false\n\
         indent_size = 8\n\
         [*.java]\n\
         indent_size = 2\n",
    )
    .expect("editorconfig should parse")
    .into();

    assert_eq!(config.layout.line_ending, LineEnding::Crlf);
    assert!(!config.layout.insert_final_newline);
    assert!(!config.layout.trim_trailing_whitespace);
    // A key set in both sections resolves to the nearer, more specific one.
    assert_eq!(config.layout.indent_width, 2);
}

#[test]
fn an_inherit_sentinel_is_not_a_width() {
    // IntelliJ writes -1 for "inherit the general setting" on every width; taking it literally
    // would collapse the indent to zero columns.
    let config: Config = Fixture::scheme(&[
        ("INDENT_SIZE", "-1"),
        ("TAB_SIZE", "-1"),
        ("CONTINUATION_INDENT_SIZE", "-1"),
    ])
    .into();

    let defaults = Config::default();
    assert_eq!(config.layout.indent_width, defaults.layout.indent_width);
    assert_eq!(config.layout.tab_width, defaults.layout.tab_width);
    assert_eq!(config.layout.continuation_indent, None);
}

#[test]
fn the_three_integer_tables_are_not_interchangeable() {
    // `2` means Wrap Always, Allman braces, and nothing at all, depending on the property.
    let config: Config = Fixture::scheme(&[
        ("CALL_PARAMETERS_WRAP", "2"),
        ("CLASS_BRACE_STYLE", "2"),
        ("IF_BRACE_FORCE", "1"),
    ])
    .into();

    assert_eq!(config.wrapping.call_arguments, WrapPolicy::AlwaysPerItem);
    assert_eq!(config.braces.type_declaration, BraceStyle::NextLine);
    assert_eq!(config.braces.force_if, ForceBraces::IfMultiline);
}

#[test]
fn the_counter_intuitive_wrap_tokens_land_correctly() {
    let config: Config = Fixture::scheme(&[
        ("CALL_PARAMETERS_WRAP", "split_into_lines"),
        ("METHOD_PARAMETERS_WRAP", "on_every_item"),
        ("BINARY_OPERATION_WRAP", "normal"),
        ("TERNARY_OPERATION_WRAP", "off"),
    ])
    .into();

    // `split_into_lines` is *Wrap Always*, `on_every_item` is *Chop Down If Long*.
    assert_eq!(config.wrapping.call_arguments, WrapPolicy::AlwaysPerItem);
    assert_eq!(config.wrapping.method_parameters, WrapPolicy::IfLongPerItem);
    assert_eq!(config.wrapping.binary_operation, WrapPolicy::IfLong);
    assert_eq!(config.wrapping.ternary, WrapPolicy::Never);
}

#[test]
fn whitesmiths_and_gnu_stay_distinct() {
    let config: Config = Fixture::scheme(&[
        ("CLASS_BRACE_STYLE", "whitesmiths"),
        ("METHOD_BRACE_STYLE", "gnu"),
        ("BRACE_STYLE", "next_line_if_wrapped"),
    ])
    .into();

    assert_eq!(config.braces.type_declaration, BraceStyle::NextLineShifted);
    assert_eq!(
        config.braces.method_declaration,
        BraceStyle::NextLineShiftedBraces
    );
    assert_eq!(config.braces.block, BraceStyle::NextLineOnWrap);
}

#[test]
fn keep_simple_booleans_become_the_preserve_policy() {
    let config: Config = Fixture::scheme(&[
        ("KEEP_SIMPLE_METHODS_IN_ONE_LINE", "true"),
        ("KEEP_SIMPLE_CLASSES_IN_ONE_LINE", "false"),
    ])
    .into();

    // `true` is "leave it where the author put it" — the whitespace-retaining policy.
    assert_eq!(
        config.braces.keep_method_body_on_one_line,
        KeepOnOneLine::Preserve
    );
    assert_eq!(
        config.braces.keep_type_body_on_one_line,
        KeepOnOneLine::Never
    );
}

#[test]
fn the_import_layout_table_becomes_jals_groups() {
    let config: Config = IntellijEditorConfig::parse(
        "[*.java]\n\
         ij_java_imports_layout = $*, |, java.**, javax.**, |, *\n",
    )
    .expect("editorconfig should parse")
    .into();

    assert_eq!(config.imports.order, ImportOrder::Group);
    // Blank-line markers are dropped: jals separates every group by
    // `blank-lines.between-import-groups`.
    assert_eq!(config.imports.groups, ["static", "java.", "javax.", "*"]);
}

#[test]
fn paren_booleans_fold_onto_the_delimiter_vocabulary() {
    let config: Config = Fixture::scheme(&[
        ("METHOD_PARAMETERS_LPAREN_ON_NEXT_LINE", "true"),
        ("METHOD_PARAMETERS_RPAREN_ON_NEXT_LINE", "true"),
        ("CALL_PARAMETERS_LPAREN_ON_NEXT_LINE", "false"),
        ("CALL_PARAMETERS_RPAREN_ON_NEXT_LINE", "false"),
    ])
    .into();

    assert_eq!(
        config.wrapping.paren_method_declaration,
        ParenPositions::SeparateLines
    );
    assert_eq!(
        config.wrapping.paren_method_invocation,
        ParenPositions::CommonLines
    );
}

#[test]
fn keep_line_breaks_inverts_into_join_wrapped_lines() {
    let config: Config = Fixture::scheme(&[("KEEP_LINE_BREAKS", "true")]).into();
    assert!(!config.wrapping.join_wrapped_lines);

    let config: Config = Fixture::scheme(&[("KEEP_LINE_BREAKS", "false")]).into();
    assert!(config.wrapping.join_wrapped_lines);
}

#[test]
fn naming_and_codegen_settings_are_modeled_but_not_projected() {
    // They are part of the `ij_java_*` surface, so they must parse; they are not formatter
    // rules, so they must not move the config (MAPPING.md §7).
    let native = Fixture::scheme(&[
        ("FIELD_NAME_PREFIX", "m_"),
        ("VISIBILITY", "private"),
        ("INSERT_OVERRIDE_ANNOTATION", "false"),
        ("CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND", "99"),
    ]);
    assert_eq!(native.naming.field_name_prefix.as_deref(), Some("m_"));
    assert_eq!(native.naming.visibility.as_deref(), Some("private"));
    assert_eq!(native.codegen.insert_override_annotation, Some(false));
    assert_eq!(native.imports.class_count_to_use_import_on_demand, Some(99));

    let config: Config = native.into();
    assert_eq!(config, Config::default());
}

#[test]
fn an_unknown_editorconfig_key_is_dropped() {
    // Another language's settings, and keys from a newer IDE, must not fail the import.
    let config = IntellijEditorConfig::parse(
        "[*.java]\n\
         ij_kotlin_allow_trailing_comma = true\n\
         ij_java_from_the_future = 42\n\
         indent_size = 2\n",
    )
    .expect("editorconfig should parse");
    assert_eq!(config.indent.indent_size, Some(2));
}

#[test]
fn a_negative_right_margin_does_not_become_a_zero_width() {
    // IntelliJ writes -1 for "inherit the general setting".
    let config: Config = Fixture::scheme(&[("RIGHT_MARGIN", "-1")]).into();
    assert_eq!(config.layout.max_width, Config::default().layout.max_width);
}

#[cfg(feature = "std")]
#[test]
fn the_scheme_xml_reads_the_package_entry_table_and_skips_foreign_languages() {
    use super::IntellijXmlScheme;

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
    <KotlinCodeStyleSettings>
      <option name="RIGHT_MARGIN" value="200" />
    </KotlinCodeStyleSettings>
    <codeStyleSettings language="XML">
      <option name="CLASS_BRACE_STYLE" value="2" />
    </codeStyleSettings>
    <codeStyleSettings language="JAVA">
      <option name="CLASS_BRACE_STYLE" value="1" />
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
        <option name="CONTINUATION_INDENT_SIZE" value="4" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>"#;

    let native = IntellijXmlScheme::parse(xml).expect("scheme should parse");
    let config: Config = native.into();

    // The foreign blocks reuse the same option vocabulary and must not leak in.
    assert_eq!(config.layout.max_width, 120);
    assert_eq!(config.braces.type_declaration, BraceStyle::SameLine);
    assert_eq!(config.layout.indent_width, 2);
    assert_eq!(config.layout.continuation_indent, Some(4));
    assert_eq!(config.imports.order, ImportOrder::Group);
    assert_eq!(config.imports.groups, ["static", "java.", "*"]);
}

#[cfg(feature = "std")]
#[test]
fn the_module_row_is_carried_but_not_projected() {
    use super::IntellijXmlScheme;
    use super::values::{PackageEntry, PackageEntryTable};

    // IntelliJ's default layout leads with the "all module imports" row, which has an *empty*
    // name. Reading it as an ordinary package would emit a second catch-all group; ignoring the
    // `module` attribute outright would drop it from the model (MAPPING.md §7).
    let xml = r#"<code_scheme name="Project">
  <JavaCodeStyleSettings>
    <option name="IMPORT_LAYOUT_TABLE">
      <value>
        <package name="" withSubpackages="true" static="false" module="true" />
        <package name="java" withSubpackages="true" static="false" />
        <emptyLine />
        <package name="" withSubpackages="true" static="false" />
      </value>
    </option>
  </JavaCodeStyleSettings>
</code_scheme>"#;

    let native = IntellijXmlScheme::parse(xml).expect("scheme should parse");
    let PackageEntryTable(entries) = native
        .imports
        .import_layout_table
        .as_ref()
        .expect("the table should be read");
    assert!(
        matches!(entries.first(), Some(PackageEntry::Package { is_module: true, name, .. }) if name.is_empty()),
        "the module row must survive into the native model: {entries:?}"
    );

    let config: Config = native.into();
    assert_eq!(
        config.imports.groups,
        ["java.", "*"],
        "the module row must not become a second catch-all"
    );
}
