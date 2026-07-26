//! Comment attachment — google-java-format's line-based rules.
//!
//! Comments are trivia interleaved in the CST. One pre-pass over the token stream anchors each
//! comment to a neighbouring significant token; the visitors then emit it when they emit that
//! token. Because every comment is anchored exactly once and every significant token is emitted
//! exactly once, no comment can be dropped or duplicated — the same structural argument that
//! carries the token invariant.
//!
//! # The rules (`DESIGN.md` §2.7)
//!
//! 1. **Line-based.** A comment on the same line as the preceding significant token *trails* it.
//!    Once a newline intervenes, that comment and every later one *lead* the following token.
//! 2. **Three exceptions**, all about a block comment that would otherwise trail:
//!    - `/* … */` does not trail `(`, `<`, or `.` — those open something the comment belongs
//!      *inside* of, so it leads the next token instead;
//!    - Javadoc does not trail `;` — a `/** … */` after a statement terminator documents what
//!      comes next;
//!    - a parameter-name comment (`/*name=*/`) always starts a new token, hugging the value it
//!      labels.
//! 3. **Fill modes.** A `//` comment runs to end of line, so it forces the next break; a
//!    `/* … */` participates as a unified break. [`Ops`](crate::ops::Ops) implements both.
//!
//! # Reflow
//!
//! [`Comments::format_line`] / `format_block` / `format_javadoc` reflow comment prose. The width
//! budget needs the comment's final column, which the engine has not decided yet, so the visitor
//! passes its **structural** indent (nesting depth × indent width) instead. That is a function of
//! the tree, not of the engine's output, which keeps reflow deterministic and idempotent — a
//! comment never moves because a *sibling* expression happened to wrap.
//!
//! [`Comments`]: jals_config::fmt::Comments

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// One attached comment.
#[derive(Debug, Clone)]
pub(crate) struct Comment {
    /// `LINE_COMMENT`, `BLOCK_COMMENT`, or `DOC_COMMENT`.
    pub(crate) kind: SyntaxKind,
    /// The comment text, trailing whitespace already stripped from a `//`.
    pub(crate) text: String,
    /// Blank lines between this comment and whatever preceded it in the source.
    pub(crate) blank_lines_before: usize,
}

impl Comment {
    /// Whether this comment swallows the rest of its line.
    pub(crate) const fn is_line(&self) -> bool {
        matches!(self.kind, SyntaxKind::LINE_COMMENT)
    }
}

/// Comments anchored to significant tokens by source byte offset.
#[derive(Debug, Default)]
pub(crate) struct CommentMap {
    /// Comments on their own line(s) above their anchor token.
    leading: BTreeMap<usize, Vec<Comment>>,
    /// Comments hugging their anchor token on the same line (`/* a= */ 1`).
    leading_inline: BTreeMap<usize, Vec<Comment>>,
    /// Comments on the same line *after* their anchor token.
    trailing: BTreeMap<usize, Vec<Comment>>,
    /// Own-line comments after the file's last significant token.
    trailing_below: BTreeMap<usize, Vec<Comment>>,
    /// Comments in a file with no significant token to anchor to.
    orphans: Vec<Comment>,
    /// How many comments were anchored, for the completeness assertion.
    anchored: usize,
}

impl CommentMap {
    /// Build the map for a tree.
    ///
    /// `normalize_parameter_comments` rewrites `/*a=*/` into google-java-format's canonical
    /// `/* a= */`; `inline_block_comments` lets any block comment written immediately before a
    /// same-line significant token hug that token instead of trailing the previous one.
    pub(crate) async fn build(
        root: &SyntaxNode,
        normalize_parameter_comments: bool,
        inline_block_comments: bool,
        disabled: &[text_size::TextRange],
    ) -> Self {
        let mut map = Self::default();
        let mut yielder = jals_exec::Yielder::new();

        let mut last_sig: Option<SyntaxToken> = None;
        let mut newlines = 0usize;
        let mut pending: Vec<(Comment, bool)> = Vec::new();

        for tok in root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
        {
            yielder.tick().await;
            let kind = tok.kind();

            if Self::is_comment(kind) {
                // A comment inside a formatter-disabled region is part of that region's verbatim
                // text; anchoring it as well would emit it twice.
                if disabled
                    .iter()
                    .any(|region| region.contains(tok.text_range().start()))
                {
                    continue;
                }
                map.anchored += 1;
                let (text, mut hugs) = Self::classify(&tok, normalize_parameter_comments);
                if inline_block_comments
                    && !hugs
                    && matches!(kind, SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT)
                    && Self::followed_by_same_line_token(&tok)
                {
                    hugs = true;
                }
                let comment = Comment {
                    kind,
                    text,
                    blank_lines_before: newlines.saturating_sub(1),
                };

                let trails = newlines == 0
                    && pending.is_empty()
                    && !hugs
                    && last_sig
                        .as_ref()
                        .is_some_and(|prev| Self::may_trail(prev.kind(), kind));
                if trails {
                    if let Some(prev) = &last_sig {
                        map.trailing
                            .entry(Self::offset(prev))
                            .or_default()
                            .push(comment);
                    }
                } else {
                    pending.push((comment, hugs));
                }
                newlines = 0;
            } else if kind == SyntaxKind::NEWLINE {
                newlines += 1;
            } else if !kind.is_trivia() {
                let offset = Self::offset(&tok);
                if !pending.is_empty() {
                    let (hugging, own_line): (Vec<_>, Vec<_>) =
                        core::mem::take(&mut pending).into_iter().partition(|c| c.1);
                    if !own_line.is_empty() {
                        map.leading
                            .insert(offset, own_line.into_iter().map(|c| c.0).collect());
                    }
                    if !hugging.is_empty() {
                        map.leading_inline
                            .insert(offset, hugging.into_iter().map(|c| c.0).collect());
                    }
                }
                last_sig = Some(tok);
                newlines = 0;
            }
        }

        if !pending.is_empty() {
            let rest = pending.into_iter().map(|c| c.0);
            match &last_sig {
                Some(prev) => map
                    .trailing_below
                    .entry(Self::offset(prev))
                    .or_default()
                    .extend(rest),
                None => map.orphans.extend(rest),
            }
        }
        map
    }

    /// How many comments the map holds — the denominator of the completeness assertion.
    pub(crate) const fn anchored(&self) -> usize {
        self.anchored
    }

    /// The byte offset a token is keyed by.
    fn offset(tok: &SyntaxToken) -> usize {
        usize::from(tok.text_range().start())
    }

    /// Whether `kind` is a comment.
    pub(crate) const fn is_comment(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
        )
    }

    /// Whether a comment of kind `comment` may trail a token of kind `prev` — google-java-format's
    /// three attachment exceptions.
    const fn may_trail(prev: SyntaxKind, comment: SyntaxKind) -> bool {
        match comment {
            // A block comment right after an opener belongs inside what was opened.
            SyntaxKind::BLOCK_COMMENT => {
                !matches!(prev, SyntaxKind::LPAREN | SyntaxKind::LT | SyntaxKind::DOT)
            }
            // Javadoc after a `;` documents what follows, not what ended.
            SyntaxKind::DOC_COMMENT => !matches!(
                prev,
                SyntaxKind::LPAREN | SyntaxKind::LT | SyntaxKind::DOT | SyntaxKind::SEMICOLON
            ),
            _ => true,
        }
    }

    /// Classify a comment token into the text to emit and whether it hugs the following token.
    fn classify(tok: &SyntaxToken, normalize_parameter_comments: bool) -> (String, bool) {
        match tok.kind() {
            SyntaxKind::LINE_COMMENT => (tok.text().trim_end().into(), false),
            SyntaxKind::BLOCK_COMMENT => {
                ParameterComment::normalize(tok.text(), normalize_parameter_comments)
                    .map_or_else(|| (tok.text().into(), false), |text| (text, true))
            }
            _ => (tok.text().into(), false),
        }
    }

    /// Whether the next significant token after `tok` is on the same line.
    fn followed_by_same_line_token(tok: &SyntaxToken) -> bool {
        let mut cursor = tok.next_token();
        while let Some(next) = cursor {
            let kind = next.kind();
            if kind == SyntaxKind::NEWLINE {
                return false;
            }
            if !kind.is_trivia() {
                return true;
            }
            cursor = next.next_token();
        }
        false
    }

    // ===== Queries used by the visitors =====

    /// The own-line comments above `tok`.
    pub(crate) fn leading(&self, tok: &SyntaxToken) -> &[Comment] {
        self.leading
            .get(&Self::offset(tok))
            .map_or(&[], Vec::as_slice)
    }

    /// The comments hugging `tok` on its own line.
    pub(crate) fn leading_inline(&self, tok: &SyntaxToken) -> &[Comment] {
        self.leading_inline
            .get(&Self::offset(tok))
            .map_or(&[], Vec::as_slice)
    }

    /// The comments on the same line after `tok`.
    pub(crate) fn trailing(&self, tok: &SyntaxToken) -> &[Comment] {
        self.trailing
            .get(&Self::offset(tok))
            .map_or(&[], Vec::as_slice)
    }

    /// The own-line comments below `tok` (only ever the file's last significant token).
    pub(crate) fn trailing_below(&self, tok: &SyntaxToken) -> &[Comment] {
        self.trailing_below
            .get(&Self::offset(tok))
            .map_or(&[], Vec::as_slice)
    }

    /// Comments anchored to a file with no significant token at all.
    pub(crate) fn orphans(&self) -> &[Comment] {
        &self.orphans
    }

    /// Whether anything at all is anchored before `tok` — an own-line or hugging comment with no
    /// significant token in between. A body's closing brace uses this to tell "empty" from
    /// "holds only a comment".
    pub(crate) fn has_dangling(&self, tok: &SyntaxToken) -> bool {
        let offset = Self::offset(tok);
        self.leading.contains_key(&offset) || self.leading_inline.contains_key(&offset)
    }

    /// Blank lines the source had before `tok`'s first leading comment, or before `tok` itself
    /// when it has none.
    pub(crate) fn blank_lines_before(&self, tok: &SyntaxToken) -> usize {
        self.leading(tok)
            .first()
            .map_or(0, |first| first.blank_lines_before)
    }
}

/// google-java-format's parameter-name comment normalization.
///
/// A block comment whose whole body is `name=` (or `name =`) labels the argument that follows.
/// GJF rewrites it to the canonical `/* name= */` and glues it to that argument. Recognition is
/// unconditional — such a comment always hugs — but the *rewrite* is gated, because changing a
/// comment's text is a change the user has to ask for.
pub(crate) struct ParameterComment;

impl ParameterComment {
    /// The canonical form of `text` when it is a parameter-name comment, else `None`.
    ///
    /// When `normalize` is off the text is returned unchanged, so the comment still hugs its
    /// argument but keeps the spelling the author wrote.
    pub(crate) fn normalize(text: &str, normalize: bool) -> Option<String> {
        let body = text.strip_prefix("/*")?.strip_suffix("*/")?;
        let name = body.trim().strip_suffix('=')?.trim_end();
        if name.is_empty() || !Self::is_identifier(name) {
            return None;
        }
        if !normalize {
            return Some(text.into());
        }
        let mut out = String::with_capacity(name.len() + 8);
        out.push_str("/* ");
        out.push_str(name);
        out.push_str("= */");
        Some(out)
    }

    /// Whether `name` is a plain Java identifier — the label has to name a parameter, and
    /// anything else is prose that must not be rewritten.
    fn is_identifier(name: &str) -> bool {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    }
}
