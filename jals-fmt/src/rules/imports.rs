//! Layout of the leading `import` run: `reorder-imports` and `group-imports`.
//!
//! [`lower_source_file`] lowers the compilation unit; when an [`ImportOrdering`] is
//! configured, the contiguous leading run of `IMPORT_DECL`s is reordered before emission.
//! The pure planning step ([`plan_run`]: pick the permutation and the group boundaries) is
//! kept separate from `Doc` emission so it is unit-testable without rendering. Emission
//! reuses the original import nodes, so every token keeps its byte offset, attached
//! comments travel with their import, and the significant-token *multiset* is preserved.

use jals_syntax::ast::{AstNode, ImportDecl};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode};

use crate::config::Config;
use crate::doc::{Doc, blank_line, concat, hardline, text};
use crate::lower::{
    Ctx, blank_lines_before, first_sig_token, item_separator, lower, lower_items, tok,
};
use crate::rules::StructuralRule;

/// The `reorder-imports` / `group-imports` rule: owns lowering of the `SOURCE_FILE` node.
pub(crate) struct ImportRule;

impl StructuralRule for ImportRule {
    fn lower(&self, node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
        lower_source_file(node, ctx)
    }
}

/// Which import reordering applies — resolved from the two config flags in exactly one
/// place: `group-imports` overrides `reorder-imports`
/// ([`from_config`](Self::from_config) returns [`Group`](Self::Group) whenever grouping is
/// on, regardless of the reorder flag).
#[derive(Debug, Clone, Copy)]
enum ImportOrdering<'a> {
    /// `reorder-imports`: one block — non-static first, then static, each alphabetical by
    /// qualified name.
    Sort,
    /// `group-imports`: partition into the prefix groups of the `import-groups` list, each
    /// group sorted alphabetically and separated from the next by a single blank line.
    Group(&'a [String]),
}

impl<'a> ImportOrdering<'a> {
    /// The ordering selected by `cfg`, or `None` when both flags are off (imports stay put).
    fn from_config(cfg: &'a Config) -> Option<Self> {
        if cfg.group_imports {
            Some(ImportOrdering::Group(&cfg.import_groups))
        } else if cfg.reorder_imports {
            Some(ImportOrdering::Sort)
        } else {
            None
        }
    }
}

/// Lower the compilation unit. With no [`ImportOrdering`] selected this is exactly
/// [`lower_items`]. Under [`ImportOrdering::Sort`] the leading run of `import` declarations
/// is sorted (non-static first, then static, each alphabetical by qualified name); under
/// [`ImportOrdering::Group`] the run is partitioned into the prefix groups of
/// [`Config::import_groups`], each group sorted alphabetically and separated from the next
/// by a single blank line. In both modes blank lines *between* the imports of one group are
/// dropped, while the gap before the block and the gap after it (to the first type decl)
/// are preserved.
///
/// Sorting reuses the original import nodes, so every token keeps its byte offset and its
/// attached comments follow it automatically; the significant-token *multiset* is preserved.
pub(crate) fn lower_source_file(node: &SyntaxNode, ctx: &Ctx<'_>) -> Doc {
    let Some(ordering) = ImportOrdering::from_config(ctx.cfg) else {
        return lower_items(node, ctx).0;
    };
    let els: Vec<SyntaxElement> = node.children_with_tokens().collect();
    let Some((start, end)) = import_run(&els) else {
        // Nothing to sort/group (zero or one import), or — defensively — a non-contiguous run.
        return lower_items(node, ctx).0;
    };
    // The blank lines before the whole block come from the import that was originally first
    // (its gap from the package decl). Capture it before sorting so the value is positional,
    // not tied to whichever import sorts to the front — this keeps formatting idempotent.
    let first = els[start]
        .as_node()
        .expect("import run index points at a node");
    let block_lead_blanks = import_block_lead_blanks(first, ctx);

    let run: Vec<SyntaxNode> = (start..=end)
        .map(|i| {
            els[i]
                .as_node()
                .expect("import run index points at a node")
                .clone()
        })
        .collect();
    let ImportPlan {
        imports: run,
        new_group,
    } = plan_run(run, ordering);

    let mut parts: Vec<Doc> = Vec::new();
    let mut saw = false;
    let mut emitted_import = false;
    for (i, el) in els.iter().enumerate() {
        if (start..=end).contains(&i) {
            // The reordered import for this positional slot.
            let k = i - start;
            let import = &run[k];
            if saw {
                parts.push(if !emitted_import {
                    // Before the first emitted import: the block's leading gap.
                    if block_lead_blanks > 0 {
                        blank_line(block_lead_blanks)
                    } else {
                        hardline()
                    }
                } else if new_group[k] {
                    // A group boundary: exactly one blank line between groups.
                    blank_line(1)
                } else {
                    // Within a group: never keep blank lines between sorted imports.
                    hardline()
                });
            }
            parts.push(lower(import, ctx));
            saw = true;
            emitted_import = true;
            continue;
        }
        // Outside the import run: the original `lower_items` logic. A following type decl's
        // separator reads its own (unmoved) leading trivia, so its gap stays stable.
        if let Some(child) = el.as_node() {
            if first_sig_token(child).is_none() {
                continue;
            }
            if saw {
                parts.push(item_separator(child, ctx));
            }
            parts.push(lower(child, ctx));
            saw = true;
        } else if let Some(t) = el.as_token() {
            let kind = t.kind();
            if kind == S::LBRACE || kind == S::RBRACE || kind.is_trivia() {
                continue;
            }
            if saw {
                parts.push(text(" "));
            }
            parts.push(tok(t, ctx));
            saw = true;
        }
    }
    concat(parts)
}

/// The planned layout of an import run.
struct ImportPlan {
    /// The run's import nodes in emission order — a permutation of the input.
    imports: Vec<SyntaxNode>,
    /// For each slot of `imports`, whether it begins a new group (and so earns a blank line
    /// before it). Never true for slot 0; all-false under [`ImportOrdering::Sort`].
    new_group: Vec<bool>,
}

/// Order `run` under `ordering` — the pure planning step, separate from `Doc` emission so
/// it is unit-testable without rendering. With [`ImportOrdering::Group`], sort by
/// (group rank, name) and break at every rank change. With [`ImportOrdering::Sort`], sort
/// by (is_static, name) as a single group with no boundaries, so the emitted Doc — and thus
/// the output — is byte-identical to the reorder-only behavior.
fn plan_run(mut run: Vec<SyntaxNode>, ordering: ImportOrdering<'_>) -> ImportPlan {
    match ordering {
        ImportOrdering::Group(groups) => {
            let mut keyed: Vec<(usize, String, SyntaxNode)> = run
                .into_iter()
                .map(|n| (import_group_rank(&n, groups), import_name(&n), n))
                .collect();
            keyed.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
            let new_group = (0..keyed.len())
                .map(|k| k > 0 && keyed[k].0 != keyed[k - 1].0)
                .collect();
            ImportPlan {
                imports: keyed.into_iter().map(|(_, _, n)| n).collect(),
                new_group,
            }
        }
        ImportOrdering::Sort => {
            run.sort_by_cached_key(import_sort_key);
            let new_group = vec![false; run.len()];
            ImportPlan {
                imports: run,
                new_group,
            }
        }
    }
}

/// The inclusive index range of the contiguous leading run of `import` declarations among the
/// source file's children, or `None` when there are fewer than two to sort (or — defensively —
/// the `IMPORT_DECL` children are not contiguous, which the grammar should never produce).
fn import_run(els: &[SyntaxElement]) -> Option<(usize, usize)> {
    let idx: Vec<usize> = els
        .iter()
        .enumerate()
        .filter(|(_, e)| e.as_node().is_some_and(|n| n.kind() == S::IMPORT_DECL))
        .map(|(i, _)| i)
        .collect();
    if idx.len() < 2 {
        return None;
    }
    if !idx.windows(2).all(|w| w[1] == w[0] + 1) {
        return None;
    }
    Some((idx[0], idx[idx.len() - 1]))
}

/// The blank lines preceding the import block — the value [`item_separator`] would compute for
/// the originally-first import (its gap from the package decl or file start).
fn import_block_lead_blanks(first_import: &SyntaxNode, ctx: &Ctx<'_>) -> usize {
    match first_sig_token(first_import) {
        Some(t) if ctx.comments.has_leading(&t) => ctx.comments.blank_lines_before_first(&t),
        Some(t) => blank_lines_before(&t),
        None => 0,
    }
}

/// The qualified name of an import as written (`a.b.C`, or `a.b.*`), or the empty string for a
/// malformed import with no name (so it sorts first within its group).
fn import_name(node: &SyntaxNode) -> String {
    ImportDecl::cast(node.clone())
        .and_then(|i| i.name())
        .map(|n| n.text())
        .unwrap_or_default()
}

/// The ordering tier of an import: module imports (`0`) lead, then ordinary type imports (`1`),
/// then static imports (`2`). `import module M;` (JEP 511) is a broad aggregate import, so it
/// forms its own leading tier — symmetric to static's trailing tier.
fn import_tier(node: &SyntaxNode) -> u8 {
    let Some(import) = ImportDecl::cast(node.clone()) else {
        return 1;
    };
    if import.is_module() {
        0
    } else if import.is_static() {
        2
    } else {
        1
    }
}

/// The total, deterministic sort key for an import under `reorder-imports`: by tier (module,
/// then ordinary, then static), then the dotted name text in lexicographic order. Duplicates are
/// kept (stable sort).
fn import_sort_key(node: &SyntaxNode) -> (u8, String) {
    (import_tier(node), import_name(node))
}

/// The group rank of an import under `groups` (the `import-groups` list); lower ranks emit first.
/// Module imports lead in their own group (rank `0`); every other import is the rank below shifted
/// up by one. A non-static import takes the index of its *longest* matching prefix (ties broken by
/// list order — the earliest such prefix wins); failing that, the catch-all `"*"`'s index, or
/// `groups.len()` when the list has no `"*"`. A static import takes `"static"`'s index, or
/// `groups.len() + 1` (after the catch-all) when the list has no `"static"`. The literals `"*"`,
/// `"static"` and `"module"` are never matched as textual prefixes (they are keywords, so they can
/// never be a real name segment).
fn import_group_rank(node: &SyntaxNode, groups: &[String]) -> usize {
    if import_tier(node) == 0 {
        return 0;
    }
    if ImportDecl::cast(node.clone()).is_some_and(|i| i.is_static()) {
        return 1 + groups
            .iter()
            .position(|g| g.as_str() == "static")
            .unwrap_or(groups.len() + 1);
    }
    let name = import_name(node);
    let mut best_len = 0usize;
    let mut best_idx: Option<usize> = None;
    for (i, g) in groups.iter().enumerate() {
        if matches!(g.as_str(), "*" | "static" | "module") {
            continue;
        }
        if g.len() > best_len && name.starts_with(g.as_str()) {
            best_len = g.len();
            best_idx = Some(i);
        }
    }
    1 + best_idx.unwrap_or_else(|| {
        groups
            .iter()
            .position(|g| g.as_str() == "*")
            .unwrap_or(groups.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The source file's child elements.
    fn els_of(src: &str) -> Vec<SyntaxElement> {
        jals_syntax::parse(src)
            .syntax()
            .children_with_tokens()
            .collect()
    }

    /// The leading import run of `src` (panics when it has none — test sources always do).
    fn run_of(src: &str) -> Vec<SyntaxNode> {
        let els = els_of(src);
        let (start, end) = import_run(&els).expect("test source has an import run");
        (start..=end)
            .map(|i| els[i].as_node().expect("run index is a node").clone())
            .collect()
    }

    /// The qualified names of `nodes`, in order.
    fn names(nodes: &[SyntaxNode]) -> Vec<String> {
        nodes.iter().map(import_name).collect()
    }

    /// Owned group list from literals.
    fn groups(g: &[&str]) -> Vec<String> {
        g.iter().map(ToString::to_string).collect()
    }

    /// A config with the two import flags set.
    fn cfg(reorder: bool, group: bool) -> Config {
        Config {
            reorder_imports: reorder,
            group_imports: group,
            ..Config::default()
        }
    }

    #[test]
    fn from_config_both_off_is_none() {
        assert!(ImportOrdering::from_config(&cfg(false, false)).is_none());
    }

    #[test]
    fn from_config_reorder_only_is_sort() {
        assert!(matches!(
            ImportOrdering::from_config(&cfg(true, false)),
            Some(ImportOrdering::Sort)
        ));
    }

    #[test]
    fn from_config_group_overrides_reorder() {
        for reorder in [false, true] {
            let c = cfg(reorder, true);
            match ImportOrdering::from_config(&c) {
                Some(ImportOrdering::Group(g)) => assert_eq!(g, c.import_groups.as_slice()),
                other => panic!("expected Group regardless of reorder flag, got {other:?}"),
            }
        }
    }

    #[test]
    fn sort_alphabetical_nonstatic_then_static() {
        let run = run_of("import static a.Z.z;import b.A;import static a.A.a;import b.B;class C{}");
        let plan = plan_run(run, ImportOrdering::Sort);
        assert_eq!(names(&plan.imports), ["b.A", "b.B", "a.A.a", "a.Z.z"]);
        assert_eq!(plan.new_group, [false; 4]);
    }

    #[test]
    fn sort_keeps_duplicates_and_multiset() {
        let run = run_of("import b.B;import a.A;import b.B;class C{}");
        let mut input_names = names(&run);
        let plan = plan_run(run, ImportOrdering::Sort);
        let output_names = names(&plan.imports);
        assert_eq!(output_names, ["a.A", "b.B", "b.B"]);
        // Permutation proof: same names as the input, duplicates included.
        input_names.sort();
        let mut sorted_output = output_names.clone();
        sorted_output.sort();
        assert_eq!(sorted_output, input_names);
    }

    #[test]
    fn sort_wildcard_before_named_sibling() {
        let run = run_of("import a.b.C;import a.b.*;class X{}");
        let plan = plan_run(run, ImportOrdering::Sort);
        assert_eq!(names(&plan.imports), ["a.b.*", "a.b.C"]);
    }

    #[test]
    fn group_marks_boundaries_at_rank_changes() {
        let run = run_of(
            "import com.foo.Bar;import javax.annotation.Nullable;\
             import static org.junit.Assert.assertEquals;import java.util.List;class C{}",
        );
        let c = Config::default();
        let plan = plan_run(run, ImportOrdering::Group(&c.import_groups));
        assert_eq!(
            names(&plan.imports),
            [
                "java.util.List",
                "javax.annotation.Nullable",
                "com.foo.Bar",
                "org.junit.Assert.assertEquals",
            ]
        );
        assert_eq!(plan.new_group, [false, true, true, true]);
    }

    #[test]
    fn group_empty_group_emits_single_boundary() {
        // No javax. import: the boundary marks the rank *change*, not each skipped rank,
        // so an empty group never produces a double blank line.
        let run = run_of("import com.a.B;import java.x.Y;class C{}");
        let c = Config::default();
        let plan = plan_run(run, ImportOrdering::Group(&c.import_groups));
        assert_eq!(names(&plan.imports), ["java.x.Y", "com.a.B"]);
        assert_eq!(plan.new_group, [false, true]);
    }

    #[test]
    fn group_rank_longest_prefix_wins() {
        // Ranks are the prefix index shifted up by one (rank 0 is reserved for module imports).
        let run = run_of("import java.util.List;import java.io.File;class C{}");
        let g = groups(&["java.", "java.util.", "*"]);
        assert_eq!(import_group_rank(&run[0], &g), 2);
        assert_eq!(import_group_rank(&run[1], &g), 1);
    }

    #[test]
    fn group_rank_missing_catch_all_and_static_implicit_trailing() {
        let run = run_of("import com.a.B;import static x.Y.z;class C{}");
        let g = groups(&["java."]);
        // No "*": non-matching non-static imports take groups.len() (shifted up by one)...
        assert_eq!(import_group_rank(&run[0], &g), 2);
        // ...and no "static": static imports take groups.len() + 1, after the catch-all (shifted).
        assert_eq!(import_group_rank(&run[1], &g), 3);
    }

    #[test]
    fn group_rank_static_slot_ignores_prefixes() {
        let run = run_of("import static a.B.c;import b.X;class C{}");
        let g = groups(&["a.", "static", "*"]);
        // A static import takes the "static" slot even though "a." matches its name (shifted).
        assert_eq!(import_group_rank(&run[0], &g), 2);
        // The "static" literal is never scanned as a textual prefix.
        assert_eq!(import_group_rank(&run[1], &g), 3);
    }

    #[test]
    fn group_rank_module_leads() {
        // Module imports always take the leading rank 0, before every prefix group and static.
        let run = run_of(
            "import module java.base;import java.util.List;\
             import static a.A.a;class C{}",
        );
        let c = Config::default();
        assert_eq!(import_group_rank(&run[0], &c.import_groups), 0);
        assert!(import_group_rank(&run[1], &c.import_groups) > 0);
        assert!(import_group_rank(&run[2], &c.import_groups) > 0);
    }

    #[test]
    fn sort_module_tier_leads() {
        let run = run_of(
            "import b.B;import module java.base;import static a.A.a;\
             import a.A;class C{}",
        );
        let plan = plan_run(run, ImportOrdering::Sort);
        assert_eq!(names(&plan.imports), ["java.base", "a.A", "b.B", "a.A.a"]);
    }

    #[test]
    fn import_run_none_for_zero_or_one_import() {
        assert!(import_run(&els_of("class C {}")).is_none());
        assert!(import_run(&els_of("import a.A;class C{}")).is_none());
    }

    #[test]
    fn import_run_contiguous_some_scrambled_none() {
        // Trivia is attached inside the following node, so import elements are adjacent.
        let els = els_of("package p;import a.A;import b.B;class C{}");
        assert_eq!(import_run(&els), Some((1, 2)));
        // A non-contiguous run is unreachable from `parse` (the grammar only builds imports
        // in one leading loop), so exercise the defensive branch on a synthetic element list.
        let els = els_of("import a.A;import b.B;class C{}");
        let (start, end) = import_run(&els).expect("two contiguous imports");
        let scrambled = vec![
            els[start].clone(),
            els[end + 1].clone(),
            els[start + 1].clone(),
        ];
        assert!(import_run(&scrambled).is_none());
    }
}
