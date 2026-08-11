//! The compilation unit: `package`, the import block, top-level types, and `module-info.java`.
//!
//! This is where the two L0 plans are consumed. [`ImportPlan`] decides the order and grouping of
//! the import block; the declarations themselves are emitted through the ordinary token path, so
//! their comments travel with them and the token multiset is preserved by construction.

use alloc::vec::Vec;

use jals_syntax::ast::{AstNode, ImportDecl};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::passes::{ImportPlan, Unit};
use crate::visit::Ctx;

impl Ctx<'_> {
    /// The whole file: a package declaration, an import block, then type declarations.
    ///
    /// Separation is negotiated between neighbours: each item states the blank lines it wants
    /// *before* it and the ones it owes *after* it, and the larger of the two — raised again by
    /// whatever the source already had, capped by `max-in-declarations` — is what gets emitted.
    pub(super) async fn visit_source_file(&mut self, node: &SyntaxNode) {
        let children = Self::children(node);
        let blank = self.style.cfg.blank_lines;
        let mut at = 0usize;
        let mut first = true;
        let mut owed = 0usize;

        while at < children.len() {
            let Some(child) = children[at].as_node().cloned() else {
                // A stray top-level token (error recovery): emit it and move on.
                self.visit_element(&children[at]).await;
                at += 1;
                continue;
            };

            if child.kind() == S::IMPORT_DECL {
                let run: Vec<SyntaxNode> = children[at..]
                    .iter()
                    .map_while(|element| element.as_node().cloned())
                    .take_while(|node| node.kind() == S::IMPORT_DECL)
                    .collect();
                at += run.len();
                // An import block that loses every member leaves *one* blank line here, not the
                // two google-java-format is left with. Its `RemoveUnusedImports` runs as a text
                // pass after layout and deletes only the import's own line, so the blank lines
                // that framed the block survive next to each other — output its own second pass
                // would then collapse. Reproducing that would trade this crate's idempotence for
                // one file; it is difference **D10** in `DESIGN.md` §18.2.
                self.visit_import_block(&run, first, owed).await;
                first = false;
                owed = blank.after_imports;
                continue;
            }

            // The first item of a file is separated by whatever the source had:
            // `visitCompilationUnit` asks for a blank line only *after* something has been
            // emitted. The exception is the package declaration, whose own rule is exactly about
            // the gap under a header comment.
            let wanted = if first && child.kind() != S::PACKAGE_DECL {
                0
            } else {
                self.wanted_before(&child)
            };
            self.separate(&child, owed.max(wanted), first);
            first = false;
            self.visit(&child).await;
            owed = if child.kind() == S::PACKAGE_DECL {
                blank.after_package
            } else {
                self.wanted_before(&child)
            };
            at += 1;
        }
    }

    /// The blank lines `[blank-lines]` wants around a top-level item.
    fn wanted_before(&self, node: &SyntaxNode) -> usize {
        let blank = &self.style.cfg.blank_lines;
        match node.kind() {
            S::PACKAGE_DECL => blank.before_package,
            S::CLASS_DECL
            | S::INTERFACE_DECL
            | S::ENUM_DECL
            | S::RECORD_DECL
            | S::ANNOTATION_TYPE_DECL
            | S::MODULE_DECL => blank.around_type,
            _ => 0,
        }
    }

    /// Emit a run of consecutive imports, in the order [`ImportPlan`] chose.
    ///
    /// `owed` is the separation the item before the block asked for; `before-imports` raises it.
    async fn visit_import_block(&mut self, run: &[SyntaxNode], first: bool, owed: usize) {
        let decls: Vec<ImportDecl> = run.iter().cloned().filter_map(ImportDecl::cast).collect();
        let plan = ImportPlan::build(&decls, self.used.as_ref(), self.style);
        let lead = owed.max(self.style.cfg.blank_lines.before_imports);

        let entries: Vec<(Unit, usize)> = plan.as_ref().map_or_else(
            || {
                run.iter()
                    .map(|node| (Unit::Whole(node.clone()), 0))
                    .collect()
            },
            |plan| {
                plan.entries()
                    .map(|(unit, separation)| (unit.clone(), separation))
                    .collect()
            },
        );

        // A deleted import's comments are emitted first, at the head of the block.
        //
        // `remove-unused` deletes *declarations*; the prose written above one is not part of the
        // declaration, and google-java-format's own pass — which removes the import's source range
        // and nothing else — leaves it standing too. Emitting it here rather than at the position
        // the deleted import held is the concession the reordering forces: the plan has already
        // decided where every surviving declaration goes, and there is no gap left to put it in.
        //
        // Without this the comment vanished, and the only thing that noticed was a `debug_assert`
        // that fires in no shipped build.
        let dropped: Vec<SyntaxNode> = plan
            .as_ref()
            .map(|plan| plan.dropped().to_vec())
            .unwrap_or_default();

        // A planned block states its own separation exactly: the plan decides where a group
        // boundary is, so a blank line the author left *inside* a group is not preserved but
        // removed. Only an unplanned block (`order = "preserve"`) keeps what the source had.
        let planned = plan.is_some();
        let flushed = self.flush_dropped_imports(&dropped, lead, first);
        for (nth, (unit, separation)) in entries.iter().enumerate() {
            let enforced = if nth == 0 && !flushed {
                lead
            } else {
                *separation
            };
            // A unit the re-granulation *added* has no source node to read a blank line off, so
            // it takes the enforced count alone — which is what `ensure_blank_lines` already does
            // for every planned entry after the first.
            match (planned && nth > 0, unit.source()) {
                (false, Some(node)) => {
                    self.separate(node, enforced, first && nth == 0 && !flushed);
                }
                _ => self.ensure_blank_lines(enforced, Indent::ZERO),
            }
            self.visit_import_unit(unit).await;
        }
    }

    /// Emit the comments of the imports `remove-unused` deleted, each on a line of its own.
    ///
    /// Returns whether anything was emitted, because that decides who leads the block: a comment
    /// flushed here takes the separation the first surviving import would otherwise have asked
    /// for, and the import follows it on the next line.
    ///
    /// A comment that *trailed* a deleted import gets its own line too. It has nothing left to
    /// trail — the tokens it sat behind are gone — and leaving it hugging whatever came next is
    /// how ` //why` ended up in front of a class declaration.
    fn flush_dropped_imports(&mut self, dropped: &[SyntaxNode], lead: usize, first: bool) -> bool {
        let mut flushed = false;
        for node in dropped {
            for tok in node
                .descendants_with_tokens()
                .filter_map(SyntaxElement::into_token)
                .filter(|tok| !tok.kind().is_trivia())
            {
                let comments: Vec<_> = self
                    .comments
                    .leading(&tok)
                    .iter()
                    .chain(self.comments.leading_inline(&tok))
                    .chain(self.comments.trailing(&tok))
                    .chain(self.comments.trailing_below(&tok))
                    .cloned()
                    .collect();
                for comment in comments {
                    if flushed {
                        self.forced_break(Indent::ZERO);
                    } else {
                        self.separate(node, lead, first);
                        flushed = true;
                    }
                    self.emit_comment(&comment, true);
                }
            }
        }
        if flushed {
            self.forced_break(Indent::ZERO);
        }
        flushed
    }

    /// Emit the separation before an item: the enforced count, raised by whatever the source had
    /// (capped by `max-in-declarations`).
    ///
    /// This is the composition every native formatter uses — an enforced count is a *minimum* and
    /// a `max-*` is a *cap on a run the source already wrote* — and it is the only place input
    /// whitespace reaches the document (`DESIGN.md` §17).
    ///
    /// `first` is not a guard here: `Ops::ensure_blank_lines` already does nothing on an empty
    /// level, so the *first* item is separated exactly when something (a header comment) has
    /// already been emitted — which is what `before-package` means.
    fn separate(&mut self, node: &SyntaxNode, enforced: usize, first: bool) {
        let source = self
            .blank_lines_before(node)
            .min(self.style.cfg.blank_lines.max_in_declarations);
        if first {
            // Nothing precedes the first item but its own leading comments, so the separation it
            // asks for goes *between* those and the item rather than above them.
            self.owed_after_comments = enforced;
        }
        self.ensure_blank_lines(enforced.max(source), Indent::ZERO);
    }

    /// `module-info.java`'s `[open] module Name { … }`.
    pub(super) async fn visit_module_decl(&mut self, node: &SyntaxNode) {
        self.visit_children(node).await;
    }

    /// A module body: one directive per line, indented one level.
    pub(super) async fn visit_module_body(&mut self, node: &SyntaxNode) {
        let children = Self::children(node);
        let body: Vec<SyntaxElement> = children
            .iter()
            .filter(|child| {
                !matches!(
                    child.as_token().map(SyntaxToken::kind),
                    Some(S::LBRACE | S::RBRACE)
                )
            })
            .cloned()
            .collect();

        if let Some(brace) = Self::token_of(node, S::LBRACE) {
            self.token(&brace);
        }
        let indent = self.style.indent();
        self.open(indent.clone());
        // A run of directives of the same kind is a group, and `visitModule` puts a blank line
        // between groups — the only separation a module body has, since nothing else about it
        // varies.
        let mut previous: Option<S> = None;
        for child in &body {
            self.forced_break(Indent::ZERO);
            let kind = child.as_node().map(SyntaxNode::kind);
            let enforced = usize::from(previous.is_some() && kind.is_some() && previous != kind);
            let source = child.as_node().map_or(0, |directive| {
                self.blank_lines_before(directive)
                    .min(self.style.cfg.blank_lines.max_in_declarations)
            });
            let blanks = enforced.max(source);
            if blanks > 0 {
                self.ensure_blank_lines(blanks, Indent::ZERO);
            }
            if kind.is_some() {
                previous = kind;
            }
            self.visit_element(child).await;
        }
        self.close_indent(&indent);
        if let Some(brace) = Self::token_of(node, S::RBRACE) {
            if !body.is_empty() {
                self.forced_break(Indent::ZERO);
            }
            self.token(&brace);
        }
    }

    /// One module directive (`requires`, `exports … to …`, `provides … with …`).
    ///
    /// A `to` / `with` list wraps at the continuation indent like any other comma list.
    pub(super) async fn visit_directive(&mut self, node: &SyntaxNode) {
        // `visitDirective` opens its level at the `to` / `with`, breaks after the keyword, and
        // puts one name per line: a module's exports are a list to be read down, not prose to be
        // packed.
        let continuation = self.style.continuation();
        let children = Self::children(node);
        let separator = children.iter().position(|child| {
            child
                .as_token()
                .is_some_and(|tok| matches!(tok.kind(), S::TO_KW | S::WITH_KW))
        });
        let mut opened = false;
        for (nth, child) in children.iter().enumerate() {
            if Some(nth) == separator {
                self.open(continuation.clone());
                opened = true;
                self.visit_element(child).await;
                self.forced_break(Indent::ZERO);
                continue;
            }
            if opened
                && nth > 0
                && matches!(
                    children[nth - 1].as_token().map(SyntaxToken::kind),
                    Some(S::COMMA)
                )
            {
                self.forced_break(Indent::ZERO);
            }
            self.visit_element(child).await;
        }
        if opened {
            self.close_indent(&continuation);
        }
    }
}
