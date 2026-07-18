//! Enum bodies.
//!
//! Lowers an `ENUM_BODY` GJF-style: an empty body collapses to `{}` (no inner space), while any
//! non-empty body always breaks — each constant on its own line, the terminator `;` glued onto
//! the last constant (when there is no trailing comma and no comment between), and each member on
//! its own line, exactly like a class body. This is pure layout: every token is still emitted once
//! in source order, so the significant-token sequence is preserved. Blank lines between constants /
//! members are preserved (clamped by the renderer) via the same machinery class bodies use.
//!
//! An enum body that error recovery left malformed (a missing brace, or anything other than
//! constants and commas before the terminator) falls back to inline emission ([`Ctx::lower_generic`]),
//! which lays the children out in source order — never reordering or synthesizing tokens.

use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::config::AnnotationPlacement;
use crate::doc::Doc;
use crate::lower::Ctx;

/// The three sections of an `ENUM_BODY`, split at the terminator `;` (the first body-level
/// `SEMICOLON` after the constants).
struct EnumParts {
    /// Each constant node paired with the `COMMA` that follows it (if any). The comma of the
    /// final pair is the list's optional trailing comma.
    constants: Vec<(SyntaxNode, Option<SyntaxToken>)>,
    /// The terminator `;`, if present.
    terminator: Option<SyntaxToken>,
    /// Members after the terminator: a `Member` node, or a stray `;` token (empty declaration).
    members: Vec<SyntaxElement>,
}

impl EnumParts {
    const fn is_empty(&self) -> bool {
        self.constants.is_empty() && self.terminator.is_none() && self.members.is_empty()
    }

    /// Whether the final constant carries a trailing comma (which keeps the terminator on its
    /// own line rather than glued).
    fn has_trailing_comma(&self) -> bool {
        self.constants.last().is_some_and(|(_, c)| c.is_some())
    }
}

impl Ctx<'_> {
    /// Partition an `ENUM_BODY`'s children into constants / terminator / members. Returns `None`
    /// (signalling a fall back to inline emission) when the pre-terminator region holds anything
    /// other than constants and single inter-constant commas — an error-recovery shape where
    /// bucketing into sections would reorder or drop tokens.
    fn partition(node: &SyntaxNode) -> Option<EnumParts> {
        let mut constants: Vec<(SyntaxNode, Option<SyntaxToken>)> = Vec::new();
        let mut terminator: Option<SyntaxToken> = None;
        let mut members: Vec<SyntaxElement> = Vec::new();
        let mut seen_terminator = false;

        for el in node.children_with_tokens() {
            match &el {
                // Braces and trivia carry no body content.
                SyntaxElement::Token(t)
                    if t.kind().is_trivia() || matches!(t.kind(), S::LBRACE | S::RBRACE) =>
                {
                    continue;
                }
                // An empty recovery node (e.g. an empty `MODIFIERS`) has no tokens to emit; skipping
                // it avoids a spurious separator row, matching `lower_items`.
                SyntaxElement::Node(n) if Self::first_sig_token(n).is_none() => continue,
                _ => {}
            }

            if seen_terminator {
                members.push(el);
                continue;
            }

            match &el {
                SyntaxElement::Node(n) if n.kind() == S::ENUM_CONSTANT => {
                    constants.push((n.clone(), None));
                }
                SyntaxElement::Token(t) if t.kind() == S::COMMA => match constants.last_mut() {
                    // A comma before any constant, or a second comma on the same constant, is
                    // malformed; bail so no comma is dropped or misattributed.
                    Some(last) if last.1.is_none() => last.1 = Some(t.clone()),
                    _ => return None,
                },
                SyntaxElement::Token(t) if t.kind() == S::SEMICOLON => {
                    terminator = Some(t.clone());
                    seen_terminator = true;
                }
                // Anything else before the terminator is error recovery; fall back to inline.
                _ => return None,
            }
        }

        Some(EnumParts {
            constants,
            terminator,
            members,
        })
    }

    /// Lower an `ENUM_BODY`. See the module docs for the GJF layout rules.
    pub(crate) async fn lower_enum_body(&self, node: &SyntaxNode) -> Doc {
        let tokens: Vec<SyntaxToken> = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .collect();
        let lbrace = tokens.iter().find(|t| t.kind() == S::LBRACE);
        let rbrace = tokens.iter().rfind(|t| t.kind() == S::RBRACE);

        // Malformed (a brace missing from error recovery): never synthesize a brace — fall back to
        // inline emission so the significant-token sequence is preserved (mirrors `lower_braced`).
        let (Some(lbrace), Some(rbrace)) = (lbrace, rbrace) else {
            return self.lower_generic(node).await;
        };
        // Anything other than constants/commas before the terminator means error recovery; inline
        // emission keeps the tokens in source order rather than reordering them into sections.
        let Some(parts) = Self::partition(node) else {
            return self.lower_generic(node).await;
        };

        let open = self.tok(lbrace);
        let has_dangling = self.comments.has_dangling(rbrace);
        let close = Doc::concat(vec![Doc::text("}"), self.comments.trailing_doc(rbrace)]);

        // An empty body with no dangling comment stays `{}` on the header's line — no inner space.
        // Unlike `lower_braced`, this is unconditional: GJF never expands an empty enum body.
        if parts.is_empty() && !has_dangling {
            return Doc::concat(vec![open, close]);
        }

        // Non-empty (or dangling-only): always break, one constant / member per line.
        let mut body: Vec<Doc> = vec![Doc::hardline()];
        if !parts.is_empty() {
            body.push(self.build_inner(&parts).await);
        }
        if has_dangling {
            // A plain break before the dangling comment, matching `lower_braced` (a blank line there
            // is dropped, as it is for a class body's dangling comment).
            if !parts.is_empty() {
                body.push(Doc::hardline());
            }
            body.push(self.comments.dangling(rbrace));
        }
        Doc::concat(vec![
            open,
            Doc::indent(Doc::concat(body)),
            Doc::hardline(),
            close,
        ])
    }

    /// Build the indented body rows: the constants (with their commas and blank lines), the
    /// terminator `;`, then the members — each row separated by a blank-line-aware break.
    async fn build_inner(&self, parts: &EnumParts) -> Doc {
        let mut rows: Vec<Doc> = Vec::new();

        // Glue the terminator onto the last constant only when there are constants, no trailing
        // comma sits between, and the `;` carries no leading comment (which would otherwise be
        // relocated above the constant, dropping it off its anchor and breaking idempotency).
        // Carrying the token (not a bool) lets the gluing site emit it directly without re-deriving
        // it.
        let glued_terminator: Option<&SyntaxToken> =
            if parts.constants.is_empty() || parts.has_trailing_comma() {
                None
            } else {
                parts
                    .terminator
                    .as_ref()
                    .filter(|s| !self.comments.has_leading(s))
            };

        let last = parts.constants.len().saturating_sub(1);
        for (i, (constant, comma)) in parts.constants.iter().enumerate() {
            if !rows.is_empty() {
                rows.push(self.row_sep(Self::first_sig_token(constant).as_ref()));
            }
            // This comma/last/else ladder reads far more clearly than nested `map_or_else` closures.
            #[allow(clippy::option_if_let_else)]
            let tail = if let Some(c) = comma {
                self.tok(c)
            } else if i == last {
                glued_terminator.map_or_else(Doc::nil, |s| self.tok(s))
            } else {
                Doc::nil()
            };
            rows.push(Doc::concat(vec![
                self.lower_enum_constant(constant).await,
                tail,
            ]));
        }

        // The terminator on its own line, when it was not glued onto the last constant.
        if let Some(semi) = parts.terminator.as_ref()
            && glued_terminator.is_none()
        {
            if !rows.is_empty() {
                rows.push(self.row_sep(Some(semi)));
            }
            rows.push(self.tok(semi));
        }

        // Members (and any stray `;`), each on its own line.
        for el in &parts.members {
            let first = match el {
                SyntaxElement::Node(n) => Self::first_sig_token(n),
                SyntaxElement::Token(t) => Some(t.clone()),
            };
            if !rows.is_empty() {
                rows.push(self.row_sep(first.as_ref()));
            }
            rows.push(match el {
                SyntaxElement::Node(n) => self.lower(n).await,
                SyntaxElement::Token(t) => self.tok(t),
            });
        }

        Doc::concat(rows)
    }

    /// The break before a body row anchored at significant token `anchor`: delegates to
    /// [`Ctx::break_before`] (the same blank-line-aware break class bodies use), but tolerates a
    /// missing anchor — an empty node (already filtered) or any token-less row — by falling back to a
    /// plain `hardline`. Anchoring on a token (not an item node) lets it serve the bare `;` rows too.
    fn row_sep(&self, anchor: Option<&SyntaxToken>) -> Doc {
        anchor.map_or_else(Doc::hardline, |t| self.break_before(t))
    }

    /// Lower one `ENUM_CONSTANT`. Under `annotation-placement = expanded` each annotation in the
    /// leading run is broken onto its own line (matching GJF's `@A` / `ONE`); under the default
    /// `compact` the constant is laid out inline (`@A ONE`), byte-identical to the generic fallback.
    /// The constant's arg list and class body recurse through [`Ctx::lower`] either way. Enum-constant
    /// annotations never live in a `MODIFIERS` node, so [`crate::rules::modifiers`]'s expanded path
    /// never reaches them — this re-implements the same leading-run break locally.
    async fn lower_enum_constant(&self, node: &SyntaxNode) -> Doc {
        if self.cfg.annotation_placement != AnnotationPlacement::Expanded {
            return self.lower_generic(node).await;
        }
        let mut parts: Vec<Doc> = Vec::new();
        let mut prev: Option<SyntaxToken> = None;
        // Whether the previous emitted element was an annotation in the leading run (so the next
        // element starts a fresh line), and whether we are still inside that leading run.
        let mut prev_was_leading_annotation = false;
        let mut still_leading = true;

        for el in node.children_with_tokens() {
            if el.as_token().is_some_and(|t| t.kind().is_trivia()) {
                continue;
            }
            let is_annotation = el.kind() == S::ANNOTATION;
            if !is_annotation {
                still_leading = false;
            }
            let (first, last) = match &el {
                SyntaxElement::Node(n) => (Self::first_sig_token(n), Self::last_sig_token(n)),
                SyntaxElement::Token(t) => (Some(t.clone()), Some(t.clone())),
            };
            if let Some(f) = first.as_ref() {
                let s = if prev_was_leading_annotation {
                    Doc::hardline()
                } else {
                    self.sep(prev.as_ref(), f).await
                };
                parts.push(s);
            }
            parts.push(match &el {
                SyntaxElement::Node(n) => self.lower(n).await,
                SyntaxElement::Token(t) => self.tok(t),
            });
            if last.is_some() {
                prev = last;
            }
            prev_was_leading_annotation = is_annotation && still_leading;
        }
        Doc::concat(parts)
    }
}
