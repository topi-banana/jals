//! R0.1 — import ordering, as a plan the compilation-unit visitor emits.
//!
//! # Plan, then emit
//!
//! The pass never rewrites the tree. It produces an ordering over the **original** `IMPORT_DECL`
//! nodes, and the visitor emits those nodes through the ordinary token path. Two things follow
//! for free: the token multiset is preserved by construction (the same nodes come out, in a
//! different order), and a comment attached to an import travels with it, because comments are
//! anchored to tokens and the tokens moved.
//!
//! # The three orders
//!
//! - `preserve` — no plan at all; the visitor walks source order.
//! - `sort` — one alphabetical block. `static-first` decides which side the static imports land
//!   on, matching what the two vendors that *have* an opinion do.
//! - `group` — the blocks named by `[imports] groups`, separated by
//!   `[blank-lines] between-import-groups`. A non-static import joins the group of its **longest**
//!   matching prefix, ties broken by list order; `"*"` is the catch-all and `"static"` collects
//!   every static import. A missing `"*"` or `"static"` becomes an implicit trailing group, so a
//!   partial list can never silently drop an import.
//!
//! google-java-format is `group` with `["static", "*"]` — static first, one blank line between,
//! ASCII order within — which is Google Java Style §3.3.3.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::ImportOrder;
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{AstNode, ImportDecl};

use crate::passes::unused_imports::UnusedImports;
use crate::style::Style;

/// One import, with the sort keys the plan needs.
struct Entry {
    /// The original declaration node.
    node: SyntaxNode,
    /// Whether it is a `static` import.
    is_static: bool,
    /// The dotted name, for alphabetical ordering.
    name: String,
    /// Which `[imports] groups` block it belongs to.
    group: usize,
}

/// The emission order of a compilation unit's imports.
pub(crate) struct ImportPlan {
    /// The declarations, in emission order.
    entries: Vec<SyntaxNode>,
    /// For each entry, how many blank lines precede it (a group separation).
    separators: Vec<usize>,
}

impl ImportPlan {
    /// Build the plan for `imports`, or `None` when the config asks for nothing.
    ///
    /// `used` is the used-name set when `imports.remove-unused` is on, and `None` when it is off.
    pub(crate) fn build(
        imports: &[ImportDecl],
        used: Option<&alloc::collections::BTreeSet<String>>,
        style: &Style,
    ) -> Option<Self> {
        let cfg = &style.cfg.imports;
        if cfg.order == ImportOrder::Preserve && used.is_none() {
            return None;
        }

        let kept: Vec<&ImportDecl> = imports
            .iter()
            .filter(|decl| used.is_none_or(|used| UnusedImports::is_used(decl, used)))
            .collect();

        let groups = Self::group_prefixes(style);
        let mut entries: Vec<Entry> = kept
            .iter()
            .map(|decl| {
                let name = Self::dotted_name(decl);
                let is_static = decl.is_static();
                Entry {
                    group: Self::group_of(&groups, &name, is_static),
                    node: decl.syntax().clone(),
                    is_static,
                    name,
                }
            })
            .collect();

        match cfg.order {
            ImportOrder::Preserve => {}
            ImportOrder::Sort => {
                let static_rank = usize::from(!cfg.static_first);
                entries.sort_by(|a, b| {
                    let rank = |e: &Entry| {
                        if e.is_static {
                            static_rank
                        } else {
                            1 - static_rank
                        }
                    };
                    rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
                });
                // One block: no group separation survives a flat sort.
                for entry in &mut entries {
                    entry.group = 0;
                }
            }
            ImportOrder::Group => {
                entries.sort_by(|a, b| a.group.cmp(&b.group).then_with(|| a.name.cmp(&b.name)));
            }
        }

        let between = style.cfg.blank_lines.between_import_groups;
        let mut separators = Vec::with_capacity(entries.len());
        let mut previous: Option<usize> = None;
        for entry in &entries {
            separators.push(match previous {
                Some(group) if group != entry.group => between,
                _ => 0,
            });
            previous = Some(entry.group);
        }

        Some(Self {
            entries: entries.into_iter().map(|entry| entry.node).collect(),
            separators,
        })
    }

    /// The declarations in emission order, each with the blank lines that precede it.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&SyntaxNode, usize)> {
        self.entries.iter().zip(self.separators.iter().copied())
    }

    /// The configured groups, with the implicit trailing `"*"` / `"static"` filled in.
    ///
    /// Leaving either out would send every unmatched import — or every static one — to no group
    /// at all. Appending them is what both vendors do with a partial list.
    fn group_prefixes(style: &Style) -> Vec<String> {
        let mut groups = style.cfg.imports.groups.clone();
        if !groups.iter().any(|g| g == "*") {
            groups.push("*".into());
        }
        if !groups.iter().any(|g| g == "static") {
            if style.cfg.imports.static_first {
                groups.insert(0, "static".into());
            } else {
                groups.push("static".into());
            }
        }
        groups
    }

    /// Which group an import belongs to: its longest matching prefix, or the catch-all.
    fn group_of(groups: &[String], name: &str, is_static: bool) -> usize {
        if is_static {
            if let Some(at) = groups.iter().position(|g| g == "static") {
                return at;
            }
        }
        let mut best: Option<(usize, usize)> = None;
        for (at, prefix) in groups.iter().enumerate() {
            if prefix == "*" || prefix == "static" {
                continue;
            }
            if name.starts_with(prefix.as_str()) && best.is_none_or(|(len, _)| prefix.len() > len) {
                best = Some((prefix.len(), at));
            }
        }
        best.map_or_else(
            || groups.iter().position(|g| g == "*").unwrap_or(0),
            |(_, at)| at,
        )
    }

    /// The import's dotted name, with no whitespace or comments — the alphabetical sort key.
    fn dotted_name(decl: &ImportDecl) -> String {
        decl.name().map_or_else(String::new, |name| {
            name.syntax()
                .descendants_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .filter(|tok| !tok.kind().is_trivia())
                .map(|tok| tok.text().to_owned())
                .collect()
        })
    }
}
