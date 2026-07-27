//! The compilation unit: `package`, the import block, top-level types, and `module-info.java`.
//!
//! This is where the two L0 plans are consumed. [`ImportPlan`] decides the order and grouping of
//! the import block; the declarations themselves are emitted through the ordinary token path, so
//! their comments travel with them and the token multiset is preserved by construction.

use alloc::vec::Vec;

use jals_syntax::ast::{AstNode, ImportDecl};
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::Indent;
use crate::passes::ImportPlan;
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

        let entries: Vec<(SyntaxNode, usize)> = plan.as_ref().map_or_else(
            || run.iter().map(|node| (node.clone(), 0)).collect(),
            |plan| {
                plan.entries()
                    .map(|(node, separation)| (node.clone(), separation))
                    .collect()
            },
        );

        // A planned block states its own separation exactly: the plan decides where a group
        // boundary is, so a blank line the author left *inside* a group is not preserved but
        // removed. Only an unplanned block (`order = "preserve"`) keeps what the source had.
        let planned = plan.is_some();
        for (nth, (node, separation)) in entries.iter().enumerate() {
            let enforced = if nth == 0 { lead } else { *separation };
            if planned && nth > 0 {
                self.ensure_blank_lines(enforced, Indent::ZERO);
            } else {
                self.separate(node, enforced, first && nth == 0);
            }
            self.visit(node).await;
        }
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
        for child in &body {
            self.forced_break(Indent::ZERO);
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
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        for child in Self::children(node) {
            if let Some(tok) = child.as_token()
                && matches!(tok.kind(), S::TO_KW | S::WITH_KW)
            {
                self.break_op(Indent::ZERO);
            }
            self.visit_element(&child).await;
        }
        self.close_indent(&continuation);
    }
}
