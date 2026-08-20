//! The jals dialect: grouped imports and attributes.
//!
//! These are jals's own syntax, so no vendor rule governs them and the formatter simply picks a
//! canonical form.
//!
//! # The one unconditional token change
//!
//! A grouped import's **trailing comma is dropped** (`import a.{B,};` → `import a.{B};`). This is
//! the crate's only token change that is not behind a config key, and it is confined to this node
//! kind, where the comma separates nothing: the dialect's canonical form has no trailing comma at
//! all, and the desugaring ignores it either way.
//!
//! The comma's *comments* are still emitted, so no comment is lost — but the **token is**, and that
//! is what [`TokenBudget`](crate::passes::TokenBudget) checks. It is therefore a declared row in
//! [`token_license`](crate::passes::token_license), gated on nothing; before it was declared the
//! fail-safe rejected the drop and handed back the whole file unformatted.
//!
//! The justification used to be narrower — "a group is always laid out flat, so a trailing comma
//! has no vertical form to serve" — and `[wrapping] import-group` retired it by giving the group a
//! vertical form. The drop stays **unconditional** regardless: the dialect's canonical form has
//! no trailing comma, and gating the drop on `[wrapping] import-group` would put that form at the
//! mercy of a wrapping rule.
//!
//! Which comma is "the trailing one" is [`License::is_group_trailing_comma`], not a predicate of
//! this module's own: the pass that drops it and the check that licenses it have to agree, and two
//! implementations of that question are how they came apart.

use crate::passes::import_granularity;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::passes::Unit;
use crate::passes::token_license::License;
use crate::visit::Ctx;

impl Ctx<'_> {
    /// Emit one import declaration the way `[imports] granularity` decided to cut it.
    ///
    /// [`Unit::Whole`] is the ordinary path and the only one that emits nothing synthetic. The
    /// other two re-cut the block, so each source token is still emitted **exactly once** — with
    /// its comments — and the copies a re-cut needs are synthetic. A token the new shape has no
    /// room for is handed to [`emit_comments_without_token`](Self::emit_comments_without_token),
    /// so the token goes and its comments stay.
    pub(super) async fn visit_import_unit(&mut self, unit: &Unit) {
        match unit {
            Unit::Whole(node) => self.visit(node).await,
            Unit::Split { decl, member, lead } => {
                self.visit_import_split(decl, member, *lead).await;
            }
            Unit::Merge(decls) => self.visit_import_merge(decls).await,
        }
    }

    /// One member of a grouped import, as a plain declaration of its own.
    ///
    /// The member that leads the group inherits the declaration's real tokens; the rest spell the
    /// same prefix synthetically. Which one leads is the plan's decision, not this function's —
    /// emitting the real tokens twice would emit their comments twice.
    async fn visit_import_split(&mut self, decl: &SyntaxNode, member: &SyntaxNode, lead: bool) {
        if lead {
            for child in import_granularity::parts::lead(decl) {
                self.visit_element(&child).await;
            }
        } else {
            self.synthetic("import");
            self.space();
            if decl
                .children_with_tokens()
                .any(|c| c.kind() == S::STATIC_KW)
            {
                self.synthetic("static");
                self.space();
            }
            let prefix = decl
                .children()
                .find(|child| child.kind() == S::QUALIFIED_NAME)
                .map_or_else(alloc::string::String::new, |name| {
                    import_granularity::import_names::text_of(&name)
                });
            self.synthetic(&prefix);
        }
        self.synthetic(".");
        self.visit(member).await;
        // The group's delimiters and the declaration's `;` belong to the whole group, so only the
        // leading member emits them — the `;` as itself, the delimiters as comments alone.
        if lead {
            self.drop_group_delimiters(decl);
        }
        self.synthetic(";");
    }

    /// Several declarations sharing a prefix, as one grouped import.
    ///
    /// The first supplies the real leading tokens and the prefix; every other declaration's
    /// `import`, prefix and `;` are dropped, and only the segment it contributes survives.
    async fn visit_import_merge(&mut self, decls: &[SyntaxNode]) {
        let Some((first, rest)) = decls.split_first() else {
            return;
        };
        for child in import_granularity::parts::lead(first) {
            // The group's prefix is the *shared* part of the first declaration's name. A plain
            // declaration's name also carries the segment it contributes as a member, and emitting
            // the whole name here is what produced `import java.util.HashMap.{HashMap, List};`.
            if child.kind() == S::QUALIFIED_NAME && !Self::has_group(first) {
                self.emit_qualifier(&child);
                continue;
            }
            self.visit_element(&child).await;
        }
        self.synthetic(".");
        self.synthetic("{");
        self.emit_merged_members(first, true).await;
        for decl in rest {
            self.synthetic(",");
            self.space();
            self.emit_merged_members(decl, false).await;
        }
        self.synthetic("}");
        self.drop_group_delimiters(first);
        self.synthetic(";");
    }

    /// The members `decl` contributes to a merged group, comma-separated.
    ///
    /// A declaration that already carries a group contributes its members as written; a plain one
    /// contributes its last segment, and the qualifier tokens the prefix already spells are
    /// dropped down to their comments.
    async fn emit_merged_members(&mut self, decl: &SyntaxNode, leads: bool) {
        let members: Vec<SyntaxNode> = decl
            .children()
            .find(|child| child.kind() == S::IMPORT_GROUP)
            .map(|group| {
                group
                    .children()
                    .filter(|child| child.kind() == S::QUALIFIED_NAME)
                    .collect()
            })
            .unwrap_or_default();

        if members.is_empty() {
            // A plain declaration: everything but its last segment is already in the prefix.
            if !leads {
                for child in import_granularity::parts::lead(decl) {
                    self.drop_element(&child);
                }
            }
            if let Some(segment) = import_granularity::parts::last_segment(decl) {
                self.synthetic(&segment);
            }
            if !leads {
                self.drop_group_delimiters(decl);
            }
            return;
        }

        if !leads {
            for child in import_granularity::parts::lead(decl) {
                self.drop_element(&child);
            }
        }
        for (nth, member) in members.iter().enumerate() {
            if nth > 0 {
                self.synthetic(",");
                self.space();
            }
            self.visit(member).await;
        }
        if !leads {
            self.drop_group_delimiters(decl);
        }
    }

    /// Whether a declaration already carries a grouped import, so its name *is* the prefix.
    fn has_group(decl: &SyntaxNode) -> bool {
        decl.children().any(|child| child.kind() == S::IMPORT_GROUP)
    }

    /// Emit a qualified name without its last segment: `java.util.HashMap` as `java.util`.
    ///
    /// The dropped tokens are the trailing identifier and the `.` in front of it; both keep their
    /// comments, and the identifier itself comes back as the group member the declaration
    /// contributes.
    fn emit_qualifier(&mut self, element: &SyntaxElement) {
        let Some(name) = element.as_node() else {
            return;
        };
        let tokens: Vec<SyntaxToken> = name
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|tok| !tok.kind().is_trivia())
            .collect();
        // Everything but the last identifier and the dot that introduces it.
        let keep = tokens.len().saturating_sub(2);
        for (nth, tok) in tokens.iter().enumerate() {
            if nth < keep {
                self.token(tok);
            } else {
                self.emit_comments_without_token(tok);
            }
        }
    }

    /// Emit the comments of everything after the declaration's prefix, dropping the tokens.
    fn drop_group_delimiters(&mut self, decl: &SyntaxNode) {
        for child in import_granularity::parts::tail(decl) {
            self.drop_element(&child);
        }
    }

    /// Emit an element's comments while dropping every significant token it holds.
    fn drop_element(&mut self, element: &SyntaxElement) {
        match element {
            SyntaxElement::Token(tok) if !tok.kind().is_trivia() => {
                self.emit_comments_without_token(tok);
            }
            SyntaxElement::Token(_) => {}
            SyntaxElement::Node(node) => {
                for tok in node
                    .descendants_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .filter(|tok| !tok.kind().is_trivia())
                {
                    self.emit_comments_without_token(&tok);
                }
            }
        }
    }

    /// A grouped import's `.{ A, B }`, emitted in the compact form `.{A, B}` — or broken one
    /// member per line, when `[wrapping] import-group` asks for it.
    ///
    /// The default is [`WrapPolicy::Never`], which is the canonical form the dialect's own
    /// desugaring reads and the only shape the construct had before the rule existed.
    pub(super) async fn visit_import_group(&mut self, node: &SyntaxNode) {
        let policy = self.style.cfg.wrapping.import_group;
        let continuation = self.style.continuation();
        let flat = Self::flat_space(self.style.cfg.spacing.after_comma);
        let mut opened = false;
        for child in Self::children(node) {
            if let Some(tok) = child.as_token() {
                if License::is_group_trailing_comma(tok) {
                    self.emit_comments_without_token(tok);
                    continue;
                }
                match tok.kind() {
                    S::LBRACE if policy != jals_config::fmt::WrapPolicy::Never => {
                        self.visit_element(&child).await;
                        self.open(continuation.clone());
                        opened = true;
                        self.list_break_flat(policy, "", Indent::ZERO);
                        continue;
                    }
                    S::RBRACE if opened => {
                        self.close_indent(&continuation);
                        opened = false;
                        self.list_break_flat(policy, "", Indent::ZERO);
                        self.visit_element(&child).await;
                        continue;
                    }
                    S::COMMA if opened => {
                        self.visit_element(&child).await;
                        self.list_break_flat(policy, flat, Indent::ZERO);
                        continue;
                    }
                    _ => {}
                }
            }
            self.visit_element(&child).await;
        }
        if opened {
            self.close_indent(&continuation);
        }
    }

    /// A jals attribute, `#[cfg(feature = "x")]`.
    pub(super) async fn visit_attribute(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// Emit a token's comments while dropping the token's own text.
    ///
    /// Every rule that removes a significant token goes through here, so "the token is gone" never
    /// means "its comment is gone": the dialect's trailing comma, a re-granulated import's
    /// delimiters, and `[wrapping] remove-nested-parens`' outer pair all keep what was written
    /// around them.
    pub(super) fn emit_comments_without_token(&mut self, tok: &SyntaxToken) {
        for comment in self.comments.leading(tok).to_vec() {
            self.forced_break(Indent::ZERO);
            self.emit_comment(&comment, true);
        }
        for comment in self.comments.leading_inline(tok).to_vec() {
            self.space();
            self.emit_comment(&comment, false);
        }
        for comment in self.comments.trailing(tok).to_vec() {
            self.space();
            self.emit_comment(&comment, false);
        }
        for comment in self.comments.trailing_below(tok).to_vec() {
            self.forced_break(Indent::ZERO);
            self.emit_comment(&comment, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use jals_config::fmt::{Config, WrapPolicy};
    use jals_config::{Feature, FeatureSet};

    fn formatted(src: &str, policy: WrapPolicy) -> String {
        let mut cfg = Config::default();
        cfg.wrapping.import_group = policy;
        let out = jals_exec::block_on_inline(crate::FormatOutput::format_source(
            src,
            &cfg,
            FeatureSet::resolve(&[Feature::GroupedImports]),
        ));
        assert!(!out.fell_back(), "the fail-safe refused its own output");
        out.formatted
    }

    const SRC: &str = "import java.util.{HashMap, List, Map};\nclass Z {}\n";

    #[test]
    fn never_keeps_a_group_on_one_line() {
        assert!(
            formatted(SRC, WrapPolicy::Never).contains("import java.util.{HashMap, List, Map};"),
        );
    }

    #[test]
    fn always_per_item_puts_one_member_on_each_line() {
        let out = formatted(SRC, WrapPolicy::AlwaysPerItem);
        assert!(out.contains("HashMap,\n"), "{out}");
        assert!(out.contains("List,\n"), "{out}");
    }

    #[test]
    fn the_trailing_comma_still_goes_when_the_group_breaks() {
        // The unconditional row, under the layout that retired its original justification.
        let out = formatted(
            "import java.util.{HashMap, List,};\nclass Z {}\n",
            WrapPolicy::AlwaysPerItem,
        );
        assert!(!out.contains("List,\n}"), "{out}");
        assert!(out.contains("List"), "{out}");
    }
}
