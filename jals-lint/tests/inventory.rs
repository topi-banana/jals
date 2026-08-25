//! The rustc / clippy ledger, as a ratchet rather than a claim.
//!
//! `jals-lint/MAPPING-rustc-clippy.md` says every lint the two tools ship has been placed in one of
//! six buckets, and that the `M` rows name rules this crate actually implements. Both are checkable,
//! so they are checked here against the committed inventories — the same move
//! `jals-fmt/src/import/*/inventory.tsv` makes for the formatter's vendor option sets.
//!
//! What this cannot check is whether a *judgement* is right: that `clippy::manual_strip` has no Java
//! spelling is an argument, not an arithmetic fact, and it lives in the `note` column and in the
//! MAPPING document. What it does check is that the arguments add up — a ledger whose buckets do not
//! sum to the source set is worthless as a completeness claim, which is exactly the property
//! `jals-fmt/MAPPING-rustfmt.md` §2 insists on.

use std::collections::{BTreeMap, BTreeSet};

use jals_lint::RuleInfo;

const RUSTC: &str = include_str!("../inventory-rustc.tsv");
const CLIPPY: &str = include_str!("../inventory-clippy.tsv");

/// One inventory row.
struct Row {
    lint: String,
    bucket: String,
    rule: String,
    note: String,
}

/// Parse an inventory, skipping the `#` header. `rule_column` is where the jals rule sits, which
/// differs between the two files because the clippy one carries a group column.
fn rows(tsv: &str, rule_column: usize) -> Vec<Row> {
    tsv.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(
                fields.len(),
                rule_column + 2,
                "malformed inventory row: {line:?}"
            );
            Row {
                lint: fields[0].to_owned(),
                bucket: fields[rule_column - 1].to_owned(),
                rule: fields[rule_column].to_owned(),
                note: fields[rule_column + 1].to_owned(),
            }
        })
        .collect()
}

fn rustc_rows() -> Vec<Row> {
    rows(RUSTC, 3)
}

fn clippy_rows() -> Vec<Row> {
    rows(CLIPPY, 4)
}

/// The rule names a jals diagnostic can carry: the configurable registry, plus the one fixed
/// diagnostic the engine emits outside it.
fn implemented() -> BTreeSet<&'static str> {
    let mut names: BTreeSet<&'static str> = RuleInfo::all().map(|rule| rule.name).collect();
    // Not a rule: a malformed `#[cfg(...)]` is a build-blocking error, so it is not configurable.
    // It is still what two rustc lints map onto, so the ledger has to be able to name it.
    names.insert("cfg");
    names
}

#[test]
fn every_lint_the_toolchain_ships_has_a_row() {
    // The two counts the MAPPING document's §0 fixes. A toolchain bump that adds a lint fails here,
    // which is the point: the ledger is only a completeness claim while it covers the whole set.
    assert_eq!(rustc_rows().len(), 244, "rustc lints");
    assert_eq!(clippy_rows().len(), 815, "clippy lints");
}

#[test]
fn the_buckets_sum_to_the_source_set() {
    // §2's table, as arithmetic. A bucket total that drifts from the document is the document going
    // stale, and it fails here before a reader believes it.
    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for row in rustc_rows().iter().chain(clippy_rows().iter()) {
        assert!(
            matches!(row.bucket.as_str(), "M" | "N" | "R" | "X" | "D" | "C"),
            "unknown bucket {:?} on {}",
            row.bucket,
            row.lint
        );
        *totals
            .entry(match row.bucket.as_str() {
                "M" => "M",
                "N" => "N",
                "R" => "R",
                "X" => "X",
                "D" => "D",
                _ => "C",
            })
            .or_default() += 1;
    }
    assert_eq!(
        totals,
        BTreeMap::from([
            ("M", 16),
            ("N", 376),
            ("R", 582),
            ("X", 32),
            ("D", 36),
            ("C", 17)
        ])
    );
    assert_eq!(totals.values().sum::<usize>(), 244 + 815);
}

#[test]
fn the_planned_rows_collapse_to_the_documented_rule_count() {
    // `jals-lint/README.md` says the 376 `N` rows are 286 jals rules, and lists all 286. That count
    // is a *derived* number — several source lints often answer one Java question — so pinning it
    // here is what keeps the README's roadmap from drifting when a row's target changes.
    let planned: BTreeSet<String> = rustc_rows()
        .iter()
        .chain(clippy_rows().iter())
        .filter(|row| row.bucket == "N")
        .map(|row| row.rule.clone())
        .collect();
    assert_eq!(planned.len(), 286, "distinct planned jals rules");
}

#[test]
fn every_mapped_row_names_a_rule_that_exists() {
    // The `M` bucket is the only bucket that is a claim about *this crate*, so it is the only one a
    // test can settle. A rule renamed here without the ledger following fails by name.
    let implemented = implemented();
    for row in rustc_rows().iter().chain(clippy_rows().iter()) {
        if row.bucket == "M" {
            assert!(
                implemented.contains(row.rule.as_str()),
                "`{}` maps to `{}`, which no rule implements",
                row.lint,
                row.rule
            );
        }
    }
}

#[test]
fn a_planned_row_names_a_rule_that_does_not_exist_yet() {
    // …unless it says why. An `N` row whose target is already implemented is an *extension* of that
    // rule rather than a new one, and it has to declare that in the note — otherwise a rule that
    // landed would silently leave rows claiming it is still to come.
    let implemented = implemented();
    for row in rustc_rows().iter().chain(clippy_rows().iter()) {
        if row.bucket == "N" && implemented.contains(row.rule.as_str()) {
            assert_eq!(
                row.note, "既存 rule の拡張",
                "`{}` maps to the implemented rule `{}` but is filed as planned",
                row.lint, row.rule
            );
        }
    }
}

#[test]
fn only_mapped_and_planned_rows_name_a_rule_and_only_rejected_rows_give_a_reason() {
    for row in rustc_rows().iter().chain(clippy_rows().iter()) {
        match row.bucket.as_str() {
            "M" | "N" => assert!(!row.rule.is_empty(), "`{}` names no jals rule", row.lint),
            _ => assert!(
                row.rule.is_empty(),
                "`{}` is bucket {} and still names `{}`",
                row.lint,
                row.bucket,
                row.rule
            ),
        }
        if row.bucket == "D" {
            assert!(
                !row.note.is_empty(),
                "`{}` is rejected with no reason given",
                row.lint
            );
        }
    }
}

#[test]
fn every_implemented_rule_that_ports_one_is_reachable_from_the_ledger() {
    // The other direction: a rule that claims a rustc/clippy ancestor must be findable in the
    // ledger. The rules with no ancestor are jals's own, and are listed here so that adding one
    // is a deliberate edit rather than an omission.
    const JALS_NATIVE: &[&str] = &[
        // The compile-frontend gates: no other tool has this dialect to gate.
        "attribute",
        "compact-source-file",
        "grouped-import",
        "module-import",
        // Java semantics rustc has no analogue for: resolution, assignability, checked exceptions.
        "cannot-resolve",
        "type-mismatch",
        "unreported-exception",
        // Java-shaped rules the two tools do not carry.
        "constant-condition",
        "empty-catch",
        "missing-braces",
        // Rust has no implicit receiver, so leaving one out is not a thing its lints can name.
        "implicit-this",
    ];
    let mut mapped: BTreeSet<String> = BTreeSet::new();
    for row in rustc_rows().iter().chain(clippy_rows().iter()) {
        if row.bucket == "M" {
            mapped.insert(row.rule.clone());
        }
    }
    for rule in RuleInfo::all() {
        assert!(
            mapped.contains(rule.name) || JALS_NATIVE.contains(&rule.name),
            "`{}` is neither a ledger `M` target nor listed as a jals-native rule",
            rule.name
        );
    }
}
