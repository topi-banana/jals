//! What token changes one format run is authorized to make.
//!
//! `DESIGN.md` §20 lists, in prose, every operation in this crate that can make the output's
//! significant-token multiset differ from the input's. This module is that list as data, so
//! [`TokenBudget`](super::TokenBudget) can ask *"was this change authorized?"* instead of
//! reconstructing an answer from whichever config fields it happens to remember to read.
//!
//! # Why the table, and not four booleans
//!
//! The fail-safe used to read `[literals]`, `[braces] force-*`, `wrapping.reflow-long-strings`, and
//! `imports.remove-unused` directly, and derive its allowances from them. That works only for
//! operations that *have* a config key. The dialect's grouped-import trailing-comma drop has none,
//! so there was nothing for the check to read — and it survived only when
//! `imports.remove-unused` happened to be on, because that key waives the whole import block. With
//! the default config the drop was rejected and **the entire file came back unformatted, silently**.
//!
//! A row with `enabled: |_| true` says what four booleans cannot.
//!
//! # One predicate, two callers
//!
//! Declaring the operations is half of it. The other half is that the pass which makes a change and
//! the check which authorizes it call the **same predicate**:
//!
//! - [`License::is_group_trailing_comma`] — used by `visit::Ctx::visit_import_group` to decide which
//!   comma to drop, and by [`License::lane`] to decide which comma may be missing.
//! - [`StringWrapper::sites`](super::StringWrapper::sites) — used by `StringWrapper::plan` to decide
//!   which node to re-split, and by [`Sites`] to decide which `PLUS` is a string `+`.
//! - [`LiteralRewrite::KINDS`] — used by `LiteralRewrite::apply` to decide which token it may
//!   respell, and by that operation's row to say which kinds it claims.
//!
//! Two implementations of "which comma" is exactly how the defect above arose. With one, the check
//! cannot drift from the pass without failing to compile.
//!
//! Three rows have no such predicate to share, because their pass decides by token kind alone and
//! the check can ask the same question of the tree directly: `[braces] force-*`, `[imports] order`,
//! and `[imports] reorder-modifiers`. The fourth — `[imports] remove-unused` — *could* share one;
//! see [`Effect::RemovesSubtrees`].

use alloc::string::String;
use alloc::vec::Vec;

use jals_config::fmt::{Config, ForceBraces, ImportOrder};
use jals_syntax::ast::{AstNode, ImportGroup};
use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use text_size::{TextRange, TextSize};

use super::StringWrapper;
use super::literals::LiteralRewrite;

/// Where a token has to sit for an effect to reach it.
///
/// Only the sites a row can *spell*. [`Effect::RemovesSubtrees`] scopes by node kind instead, which
/// is [`within`](Self::within) rather than a variant here — a `Site` a row never names would be a
/// third of this vocabulary that no reader of [`OPERATIONS`] can find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    /// A `COMMA` in an `IMPORT_GROUP` that separates nothing.
    TrailingGroupComma,
    /// Inside a node [`StringWrapper::sites`](super::StringWrapper::sites) reports.
    ///
    /// **How wide this really is**: `sites` is the pass's *eligibility* test, and it applies no
    /// `overflows` filter — that is `plan`'s, one step later. So nearly every string literal in the
    /// file is a site, and under a reflow the licensed set is "the string literals" rather than "the
    /// over-long ones". What the site scope buys over the file-wide `STRING_LITERAL` / `PLUS`
    /// exclusion it replaced is narrower than it sounds: an arithmetic `+`, and a literal in a chain
    /// with a non-literal operand, are held to exact equality again. Both were exempt before and are
    /// not now, which is a real tightening — but it is that, not per-token precision.
    ///
    /// Scoping to the over-long sites instead would be tighter still and is *not* done, because
    /// `overflows` measures columns in the tree it is given: the input's and the output's answers
    /// differ by exactly the layout the run is deciding, so the two ledgers would be built over
    /// different site sets and disagree for reasons that are not losses.
    Reflow,
}

impl Site {
    /// Whether an effect at this site reaches `tok`.
    fn holds(self, tok: &SyntaxToken, sites: &Sites) -> bool {
        match self {
            Self::TrailingGroupComma => License::is_group_trailing_comma(tok),
            Self::Reflow => sites.holds(tok.text_range().start()),
        }
    }

    /// Whether `tok` sits anywhere inside a node of kind `kind`.
    fn within(tok: &SyntaxToken, kind: SyntaxKind) -> bool {
        tok.parent_ancestors().any(|node| node.kind() == kind)
    }
}

/// The text a scoped allowance must nonetheless preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Content {
    /// What each reflowable concatenation spells.
    Concatenations,
    /// What each text block spells once its incidental whitespace is stripped.
    TextBlocks,
}

/// What one operation may do to the significant-token multiset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Only the order moves; the multiset is unchanged, so nothing is exempted.
    Reorders,
    /// Whole `kind` subtrees may vanish.
    ///
    /// **The granted allowance is looser than this effect**: any token inside a `kind` subtree may
    /// be missing, not only a whole one. So a bug that dropped a *used* import's type name, or half
    /// an import, is invisible to the fail-safe. Declared here so the gap is chosen rather than
    /// inherited.
    ///
    /// What is missing is **evidence, not mechanism**. The narrowing has the same shape as
    /// [`Site::Reflow`]: a per-tree payload threaded into [`License::lane`], here backed by
    /// `UnusedImports::used_names`, which would make this the fourth row whose pass and check share
    /// one predicate. What blocks it is that tightening can turn an output the formatter accepts
    /// today into a silent fallback, and the golden corpora that would show that are uninitialized
    /// submodules whose harness asserts nothing — so there is no way to tell a fixed hole from a new
    /// regression.
    RemovesSubtrees { kind: SyntaxKind },
    /// Tokens of `kinds` at `site` may be missing.
    Removes {
        kinds: &'static [SyntaxKind],
        site: Site,
    },
    /// Tokens of `kinds` may appear that the input did not have.
    Inserts { kinds: &'static [SyntaxKind] },
    /// Tokens of `kinds` keep their kind and their count, but may be spelled differently.
    Respells {
        kinds: &'static [SyntaxKind],
        content: Option<Content>,
    },
    /// Tokens of `kinds` at `site` may appear, vanish, and be respelled; `content` is all that
    /// survives.
    ///
    /// This is not a rearrangement. `StringWrapper` re-cuts a concatenation at other boundaries and
    /// splits a lone over-long literal into one, so both the `STRING_LITERAL` and the `PLUS` count
    /// can go up *or* down. The per-site content equality therefore carries the whole burden.
    Redistributes {
        kinds: &'static [SyntaxKind],
        site: Site,
        content: Content,
    },
}

impl Effect {
    /// How narrowly a **lane-producing** effect is scoped: kind *and* site, then site, then kind.
    ///
    /// `None` for an effect that claims no lane. Those exempt nothing from the count — `Reorders` is
    /// documentation, since a multiset comparison already lets a reordering through, and `Respells`
    /// is a transform on the key rather than a lane — so their rows may sit wherever the table reads
    /// best, which is next to the row sharing their config key.
    ///
    /// The lane-producing rows must appear in descending order. See
    /// `tests::the_lanes_are_declared_narrowest_first`.
    ///
    /// [`License::lane`] enforces the ordering by *using* it — first match wins — so this states the
    /// rule the table has to satisfy for that to be correct, and the test below is what holds the
    /// table to it.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "states the table's ordering rule")
    )]
    const fn specificity(self) -> Option<u8> {
        match self {
            Self::Removes { .. } | Self::Redistributes { .. } => Some(3),
            Self::RemovesSubtrees { .. } => Some(2),
            Self::Inserts { .. } => Some(1),
            Self::Reorders | Self::Respells { .. } => None,
        }
    }
}

/// One row of `DESIGN.md` §20.
pub(crate) struct Operation {
    /// The rule id in `DESIGN.md`, for the reader who arrives from the other direction.
    ///
    /// Carried so the table stands on its own instead of needing the prose beside it, and read by
    /// the coherence tests below — which is the only reason the compiler sees it used at all.
    #[cfg_attr(not(test), allow(dead_code, reason = "the row's own documentation"))]
    id: &'static str,
    /// The config key that turns it on, or `None` when it is unconditional.
    ///
    /// The one row with no key is the whole reason this table exists: an operation the old check had
    /// nothing to read. Stated as an absence rather than as a sentinel string, so a reader learns
    /// what it means from the type instead of from the one place that compares it.
    #[cfg_attr(not(test), allow(dead_code, reason = "the row's own documentation"))]
    gate: Option<&'static str>,
    /// Whether `cfg` turns it on.
    enabled: fn(&Config) -> bool,
    /// What it may change.
    effect: Effect,
}

/// Which comparison rule answers for a token.
///
/// Every significant token in either tree lands in exactly one lane, decided by a pure function of
/// the token and its ancestry. The payload is the row index, so two rows over the same kind cannot
/// pool their allowances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Lane {
    /// The default: the count must match exactly.
    Exact,
    /// May gain, never lose.
    Insertable(u8),
    /// May lose, never gain.
    Removable(u8),
    /// Not counted at all; a [`Content`] check answers for it.
    Redistributed(u8),
}

/// Every operation that can change the significant-token multiset, **narrowest scope first**.
///
/// The order is load-bearing. A token a scoped row covers has to reach that row before a broader
/// one can absorb it, or the broad allowance masks the very loss the check exists to catch — a
/// grouped import's trailing comma sits inside an `IMPORT_DECL`, and only the first row may answer
/// for it. Rows scoped by kind *and* site come first, then rows scoped by site, then by kind, then
/// the rows that exempt nothing.
///
/// Adding a token-changing pass means adding a row here. [`TokenBudget`](super::TokenBudget) needs
/// no change, and neither does any test that reads its allowances off a [`License`].
pub(crate) const OPERATIONS: [Operation; 8] = [
    Operation {
        id: "dialect grouped-import trailing comma",
        gate: None,
        // The crate's one token change with no config key (`visit/dialect.rs`).
        enabled: |_| true,
        effect: Effect::Removes {
            kinds: &[SyntaxKind::COMMA],
            site: Site::TrailingGroupComma,
        },
    },
    Operation {
        id: "R4.1 long-string rewrapping",
        gate: Some("[wrapping] reflow-long-strings"),
        enabled: |cfg| cfg.wrapping.reflow_long_strings,
        effect: Effect::Redistributes {
            kinds: &[SyntaxKind::STRING_LITERAL, SyntaxKind::PLUS],
            site: Site::Reflow,
            content: Content::Concatenations,
        },
    },
    Operation {
        id: "R0.2 unused-import removal",
        gate: Some("[imports] remove-unused"),
        enabled: |cfg| cfg.imports.remove_unused,
        effect: Effect::RemovesSubtrees {
            kind: SyntaxKind::IMPORT_DECL,
        },
    },
    Operation {
        id: "R4.1 text-block re-indentation",
        gate: Some("[wrapping] reflow-long-strings"),
        enabled: |cfg| cfg.wrapping.reflow_long_strings,
        // A separate row from the rewrap even though one key gates both: a text block keeps its
        // count and only its incidental whitespace moves, so it is a respelling. Folding the two
        // together is what let a *vanished* text block through — it left the multiset entirely.
        effect: Effect::Respells {
            kinds: &[SyntaxKind::TEXT_BLOCK],
            content: Some(Content::TextBlocks),
        },
    },
    Operation {
        id: "[literals] numeric rewrites",
        gate: Some("[literals]"),
        enabled: |cfg| LiteralRewrite::is_active(cfg.literals),
        // The kinds `LiteralRewrite::apply` gates on, read from the pass rather than restated:
        // a rewrite over a kind this row does not name is the original defect again. Dropping
        // *every* kind's text — which is what a single by-kind flag did — made a renamed
        // identifier invisible too.
        effect: Effect::Respells {
            kinds: LiteralRewrite::KINDS,
            content: None,
        },
    },
    Operation {
        id: "[braces] force-*",
        gate: Some("[braces] force-if / force-for / force-while / force-do-while"),
        enabled: License::forces_braces,
        effect: Effect::Inserts {
            kinds: &[SyntaxKind::LBRACE, SyntaxKind::RBRACE],
        },
    },
    Operation {
        id: "R0.1 import ordering",
        gate: Some("[imports] order"),
        enabled: |cfg| cfg.imports.order != ImportOrder::Preserve,
        effect: Effect::Reorders,
    },
    Operation {
        id: "R0.3 modifier ordering",
        gate: Some("[imports] reorder-modifiers"),
        enabled: |cfg| cfg.imports.reorder_modifiers,
        effect: Effect::Reorders,
    },
];

/// The table has to fit both widths that index it: [`License`]'s bitmask and [`Lane`]'s row payload.
///
/// A compile error rather than a runtime one, because the failure it prevents is *silent*. `active
/// |= 1 << nth` past `u16::BITS` shifts out of range, and a `Lane` payload past `u8::MAX` would let
/// two rows share one row number and pool their allowances — the exact masking the narrowest-first
/// order exists to prevent. Neither shows up in a test: `every_row_is_reachable_from_some_config`
/// reads the mask through the same shift, so it cannot witness the mask being wrong.
///
/// `u16::BITS` is the binding limit of the two, so satisfying it makes the `Lane` payload
/// lossless by construction.
const _: () = assert!(
    OPERATIONS.len() <= u16::BITS as usize,
    "OPERATIONS outgrew License::active; widen the bitmask and Lane's row payload together",
);

/// The nodes a reflow may re-split, in one tree.
///
/// Computed once per tree rather than per token: deciding purity per token would re-walk each `+`
/// chain for every token in it, which is quadratic on a generated concatenation and allocates every
/// time. [`StringWrapper::sites`](super::StringWrapper::sites) yields outermost nodes in source
/// order, so the ranges are sorted and disjoint and a containment test is a binary search.
pub(crate) struct Sites {
    ranges: Vec<TextRange>,
    /// What each site spells, joined so that a boundary between two of them is observable.
    content: String,
}

impl Sites {
    /// The reflowable sites of `root`, or nothing when no row licenses a reflow.
    pub(crate) fn of(root: &SyntaxNode, license: License) -> Self {
        // An empty site list falls straight out of the normal path — `[].join(..)` is `""` — so
        // "what is an empty `Sites`" is not a second construction to keep in step.
        let sites = if license.reflows() {
            StringWrapper::sites(root)
        } else {
            Vec::new()
        };
        let ranges = sites.iter().map(|(node, _)| node.text_range()).collect();
        // No separator *within* a site: where the pieces are cut is layout, and re-cutting them is
        // the whole point of the pass. A separator *between* sites: content that migrates from one
        // concatenation to the next is a bug, and an empty literal that vanishes outright would
        // otherwise contribute nothing and disappear unnoticed.
        let content = sites
            .iter()
            .map(|(_, pieces)| pieces.concat())
            .collect::<Vec<_>>()
            .join("\u{1}");
        Self { ranges, content }
    }

    /// Whether a token starting at `at` sits inside a reflowable site.
    fn holds(&self, at: TextSize) -> bool {
        self.ranges
            .binary_search_by(|range| {
                if range.end() <= at {
                    core::cmp::Ordering::Less
                } else if range.start() > at {
                    core::cmp::Ordering::Greater
                } else {
                    core::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// What every site spells together.
    ///
    /// By value: the one caller builds a ledger field out of it and drops the rest of the `Sites`,
    /// so handing back a borrow only to clone it would copy every literal body a second time.
    pub(crate) fn into_content(self) -> String {
        self.content
    }
}

/// What token changes one format run is authorized to make.
///
/// A bitmask over [`OPERATIONS`] rather than a bag of booleans: the rows are the definition, so
/// asking the license a question is always a question about the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct License {
    /// Bit `n` is set when `OPERATIONS[n]` is enabled.
    active: u16,
}

impl License {
    /// The license `cfg` grants.
    pub(crate) fn of(cfg: &Config) -> Self {
        let mut active = 0u16;
        for (nth, op) in OPERATIONS.iter().enumerate() {
            if (op.enabled)(cfg) {
                active |= 1 << nth;
            }
        }
        Self { active }
    }

    /// The enabled rows, narrowest first, with their index.
    fn rows(self) -> impl Iterator<Item = (usize, &'static Operation)> {
        OPERATIONS
            .iter()
            .enumerate()
            .filter(move |(nth, _)| self.active & (1 << nth) != 0)
    }

    /// Which rule answers for `tok`.
    ///
    /// First match over [`OPERATIONS`]' narrowest-first order, so a scoped row always answers
    /// before a broader one and the broad allowance can never absorb a token the narrow row named.
    pub(crate) fn lane(self, tok: &SyntaxToken, sites: &Sites) -> Lane {
        for (nth, op) in self.rows() {
            // Unreachable: the table is capped at `u16::BITS` rows above. Held to `Exact` anyway
            // rather than folded into `u8::MAX`'s lane — an index that cannot be represented must
            // cost an allowance, never share one with another row.
            let Ok(row) = u8::try_from(nth) else {
                return Lane::Exact;
            };
            match op.effect {
                Effect::Removes { kinds, site }
                    if kinds.contains(&tok.kind()) && site.holds(tok, sites) =>
                {
                    return Lane::Removable(row);
                }
                Effect::Redistributes { kinds, site, .. }
                    if kinds.contains(&tok.kind()) && site.holds(tok, sites) =>
                {
                    return Lane::Redistributed(row);
                }
                Effect::RemovesSubtrees { kind } if Site::within(tok, kind) => {
                    return Lane::Removable(row);
                }
                Effect::Inserts { kinds } if kinds.contains(&tok.kind()) => {
                    return Lane::Insertable(row);
                }
                _ => {}
            }
        }
        Lane::Exact
    }

    /// Whether a row may change how `kind` is spelled, so the key drops its text.
    ///
    /// Orthogonal to [`lane`](Self::lane) on purpose: respelling composes with every lane, so it is
    /// a transform on the key rather than a lane of its own.
    pub(crate) fn respells(self, kind: SyntaxKind) -> bool {
        self.rows().any(|(_, op)| match op.effect {
            Effect::Respells { kinds, .. } => kinds.contains(&kind),
            _ => false,
        })
    }

    /// Whether any row names `kind` among the kinds it may change.
    ///
    /// The complement is what an independent checker can hold to exact equality without knowing
    /// anything else about the table: a kind no row claims is one no operation may add, remove, or
    /// respell. The `invariants` module uses it to state the token property without reimplementing
    /// the lanes — the policy is shared, the comparison is not.
    ///
    /// # It is deliberately coarser than [`lane`](Self::lane)
    ///
    /// This answers by **kind alone**, so a row scoped to a site claims its kinds *file-wide*. The
    /// dialect's row is unconditional and names `COMMA`, which means every comma in the file — an
    /// argument list's, an array initializer's — leaves the invariant property's view under **every**
    /// config, not just the one comma the row can actually reach. That is structurally the same
    /// file-wide looseness [`TokenBudget`](super::TokenBudget) no longer has.
    ///
    /// It is the right trade here and not a hole, for two reasons. Asking [`lane`](Self::lane)
    /// instead would make the property re-run the fail-safe's own dispatch, and then the two would
    /// only ever agree — the independent comparison is the whole point of duplicating the mechanism.
    /// And the loss is still caught, one property over: a comma the fail-safe holds to `Lane::Exact`
    /// makes the run fall back, and `the_fail_safe_never_fires_on_the_corpus` is what fires. The
    /// coarse view is a *token* property that no longer sees commas; it is not the corpus going
    /// unchecked.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read by the invariant properties")
    )]
    pub(crate) fn claims(self, kind: SyntaxKind) -> bool {
        self.rows().any(|(_, op)| match op.effect {
            Effect::Removes { kinds, .. }
            | Effect::Inserts { kinds }
            | Effect::Respells { kinds, .. }
            | Effect::Redistributes { kinds, .. } => kinds.contains(&kind),
            // Claims every kind inside its scope rather than any kind by name.
            Effect::RemovesSubtrees { .. } | Effect::Reorders => false,
        })
    }

    /// The node kinds inside which a row may remove tokens wholesale.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "read by the invariant properties")
    )]
    pub(crate) fn removable_scopes(self) -> impl Iterator<Item = SyntaxKind> {
        self.rows().filter_map(|(_, op)| match op.effect {
            Effect::RemovesSubtrees { kind } => Some(kind),
            _ => None,
        })
    }

    /// Whether `content` is compared, because some row scoped its tokens out of the multiset.
    pub(crate) fn checks(self, content: Content) -> bool {
        self.rows().any(|(_, op)| match op.effect {
            Effect::Respells {
                content: Some(it), ..
            }
            | Effect::Redistributes { content: it, .. } => it == content,
            _ => false,
        })
    }

    /// Whether any row licenses a reflow, so the sites are worth computing.
    fn reflows(self) -> bool {
        self.rows().any(|(_, op)| {
            matches!(
                op.effect,
                Effect::Redistributes {
                    site: Site::Reflow,
                    ..
                }
            )
        })
    }

    /// Whether any `[braces] force-*` rule can insert a brace.
    fn forces_braces(cfg: &Config) -> bool {
        [
            cfg.braces.force_if,
            cfg.braces.force_for,
            cfg.braces.force_while,
            cfg.braces.force_do_while,
        ]
        .iter()
        .any(|force| *force != ForceBraces::Never)
    }

    /// Whether `tok` is a grouped import's trailing comma — one that separates nothing.
    ///
    /// The single definition. `visit::Ctx::visit_import_group` calls it to decide which comma to
    /// drop, and [`lane`](Self::lane) calls it to decide which comma may be missing; a second
    /// implementation of this question is how the two came apart in the first place.
    ///
    /// Stated over the tree rather than over a child index, so it answers the same on the input and
    /// on a re-parse of the output: a `COMMA` whose parent is an `IMPORT_GROUP` in which no member
    /// begins after it. Asked through [`ImportGroup`] rather than by matching `QUALIFIED_NAME`, so
    /// what counts as a member is `java.ungram`'s answer and not a second one kept in step by hand.
    ///
    /// Error-recovery debris cannot disguise a separator as a trailing comma — see
    /// `tests::recovery_debris_never_makes_a_separator_look_trailing`, which pins the two independent
    /// reasons `import a.{B,,C};` keeps both commas.
    pub(crate) fn is_group_trailing_comma(tok: &SyntaxToken) -> bool {
        if tok.kind() != SyntaxKind::COMMA {
            return false;
        }
        let Some(group) = tok.parent().and_then(ImportGroup::cast) else {
            return false;
        };
        let at = tok.text_range().start();
        !group
            .members()
            .any(|member| member.syntax().text_range().start() > at)
    }
}

#[cfg(test)]
mod tests {
    use jals_config::fmt::{Config, ForceBraces, HexLiteralCase, ImportOrder};
    use jals_syntax::{SyntaxElement, SyntaxKind};

    use super::{Effect, License, OPERATIONS};

    #[test]
    fn recovery_debris_never_makes_a_separator_look_trailing() {
        // The one predicate two callers share, on the input that decides how greedy it is. Both
        // commas in `import a.{B,,C};` separate something, and they are saved by *different*
        // halves of the predicate — so a change that breaks either half fails here.
        //
        // The first comma is followed by a zero-width `QUALIFIED_NAME`, which `members()` yields
        // because `AstNode::cast` keys on the kind alone; the second is wrapped in an `ERROR`, so
        // its parent is not an `IMPORT_GROUP` at all. `C` is no help — recovery puts it in a
        // `FIELD_DECL` outside the import entirely.
        let src = "import a.{B,,C};\n";
        let parse = jals_exec::block_on_inline(async { jals_syntax::Parse::parse(src).await });
        let commas: alloc::vec::Vec<_> = parse
            .syntax()
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| tok.kind() == SyntaxKind::COMMA)
            .collect();

        assert_eq!(
            commas.len(),
            2,
            "the parser must keep both commas losslessly"
        );
        for comma in &commas {
            assert!(
                !License::is_group_trailing_comma(comma),
                "the comma at {:?} separates something, so dropping it would lose a token no row \
                 licenses",
                comma.text_range(),
            );
        }
    }

    #[test]
    fn the_lanes_are_declared_narrowest_first() {
        // The whole composition rests on this. A grouped import's trailing comma sits inside an
        // `IMPORT_DECL`, so if `remove-unused`'s broad row came first it would absorb the comma
        // into an allowance that lets *anything* in an import declaration vanish — and the narrow
        // row naming that one comma would never be consulted. Sorting by scope is what keeps a
        // broad allowance from answering for a token a narrow row already claimed.
        let mut previous = u8::MAX;
        for (op, rank) in OPERATIONS
            .iter()
            .filter_map(|op| op.effect.specificity().map(|rank| (op, rank)))
        {
            assert!(
                rank <= previous,
                "{} is scoped more narrowly than the lane before it, so it can never be reached",
                op.id,
            );
            previous = rank;
        }
    }

    #[test]
    fn the_default_config_licenses_exactly_the_unconditional_rows() {
        // `CLAUDE.md`'s invariant, machine-checked: every *configured* token-changing operation is
        // off (or `preserve`) by default, so the only thing the default license permits is the one
        // operation that has no config key to turn off.
        let cfg = Config::default();
        for op in &OPERATIONS {
            assert_eq!(
                (op.enabled)(&cfg),
                op.gate.is_none(),
                "{}: a row is enabled under the default config iff it is the unconditional one \
                 (gate: {})",
                op.id,
                op.gate.unwrap_or("—"),
            );
        }
    }

    #[test]
    fn a_reordering_row_exempts_nothing() {
        // `Reorders` is in the table to be *documentation* — the multiset comparison already lets a
        // reordering through, so the row must not widen anything. If one ever grew an exemption it
        // would claim a lane, and this is where that shows up.
        for op in &OPERATIONS {
            if matches!(op.effect, Effect::Reorders) {
                assert!(
                    op.effect.specificity().is_none(),
                    "{}: a reordering row must not carry an allowance",
                    op.id,
                );
            }
        }
    }

    #[test]
    fn every_row_is_reachable_from_some_config() {
        // A row nothing can enable is a row that does not describe the formatter. The
        // unconditional one is enabled by every config, including the default.
        let mut all = Config::default();
        all.imports.order = ImportOrder::Group;
        all.imports.reorder_modifiers = true;
        all.imports.remove_unused = true;
        all.wrapping.reflow_long_strings = true;
        all.literals.hex_case = HexLiteralCase::Upper;
        all.braces.force_if = ForceBraces::Always;

        let license = License::of(&all);
        for (nth, op) in OPERATIONS.iter().enumerate() {
            assert!(
                license.active & (1 << nth) != 0,
                "{} is unreachable: no config value turns it on",
                op.id,
            );
        }
    }
}
