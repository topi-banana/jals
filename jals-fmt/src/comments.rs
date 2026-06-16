//! Comment attachment.
//!
//! Comments are trivia tokens interleaved in the CST. A single pre-pass over the token
//! stream classifies each comment relative to its neighbouring significant tokens and
//! anchors it to one of them:
//!
//! - **trailing**: on the same line as the preceding significant token (no newline
//!   between) — anchored to that token, emitted at the end of its line.
//! - **leading**: starts its own line — anchored to the following significant token,
//!   emitted on its own line(s) above it.
//!
//! Every comment is anchored exactly once, so as long as every significant token is
//! emitted exactly once through [`CommentMap::token`], no comment is dropped or
//! duplicated. Classification uses the fact that `WHITESPACE` never contains a newline
//! and `NEWLINE` is a standalone token (CRLF is one token).

use std::collections::HashMap;

use jals_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::doc::{
    CommentKind, Doc, blank_line, comment, concat, hardline, line_suffix, nil, raw, text,
};

struct Comment {
    kind: SyntaxKind,
    text: String,
    /// The number of blank lines preceding this comment in the source.
    blank_lines_before: usize,
}

/// Comments anchored to significant tokens by source byte offset.
pub(crate) struct CommentMap {
    leading: HashMap<usize, Vec<Comment>>,
    /// Same-line trailing comments (emitted as line suffixes).
    trailing_inline: HashMap<usize, Vec<Comment>>,
    /// Own-line comments after the last significant token (emitted on their own lines so
    /// consecutive line comments are not merged).
    trailing_below: HashMap<usize, Vec<Comment>>,
    /// Comments in a file with no significant tokens at all (nothing to anchor to).
    orphans: Vec<Comment>,
}

/// Build the comment map for a tree.
pub(crate) fn build(root: &SyntaxNode) -> CommentMap {
    let mut leading: HashMap<usize, Vec<Comment>> = HashMap::new();
    let mut trailing_inline: HashMap<usize, Vec<Comment>> = HashMap::new();
    let mut trailing_below: HashMap<usize, Vec<Comment>> = HashMap::new();

    let mut last_sig: Option<usize> = None;
    let mut newlines: usize = 0; // newlines since the last significant token or comment
    let mut pending: Vec<Comment> = Vec::new();

    for tok in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        let kind = tok.kind();
        if is_comment(kind) {
            let comment = Comment {
                kind,
                text: content(&tok),
                blank_lines_before: newlines.saturating_sub(1),
            };
            match last_sig {
                Some(anchor) if newlines == 0 && pending.is_empty() => {
                    trailing_inline.entry(anchor).or_default().push(comment);
                }
                _ => pending.push(comment),
            }
            newlines = 0;
        } else if kind == SyntaxKind::NEWLINE {
            newlines += 1;
        } else if !kind.is_trivia() {
            let offset = usize::from(tok.text_range().start());
            if !pending.is_empty() {
                leading.insert(offset, std::mem::take(&mut pending));
            }
            last_sig = Some(offset);
            newlines = 0;
        }
    }
    // Comments after the last significant token (e.g. end of file): keep them on the last
    // token, each on its own line so they are never merged or lost. If the file has no
    // significant tokens at all, keep them as orphans (emitted at the root).
    let mut orphans = Vec::new();
    if !pending.is_empty() {
        match last_sig {
            Some(off) => trailing_below.entry(off).or_default().extend(pending),
            None => orphans = pending,
        }
    }

    CommentMap {
        leading,
        trailing_inline,
        trailing_below,
        orphans,
    }
}

impl CommentMap {
    /// The document for a significant token, with its leading comments above and its
    /// trailing comments deferred to the end of the line. Blank lines *before* the first
    /// leading comment are emitted by the caller (see [`CommentMap::blank_before_first`]);
    /// here leading comments are simply separated by single line breaks.
    pub(crate) fn token(&self, tok: &SyntaxToken, token_doc: Doc) -> Doc {
        let offset = usize::from(tok.text_range().start());
        let mut parts = Vec::new();
        if let Some(lead) = self.leading.get(&offset) {
            // A break before each leading comment (so it is always on its own line) plus a
            // break before the token. Redundant breaks coalesce in the renderer, so this is
            // safe whether or not the caller already broke the line.
            for c in lead {
                parts.push(if c.blank_lines_before > 0 {
                    blank_line(c.blank_lines_before)
                } else {
                    hardline()
                });
                parts.push(comment_doc(c));
            }
            parts.push(hardline());
        }
        parts.push(token_doc);
        if let Some(trail) = self.trailing_inline.get(&offset) {
            parts.push(trailing_inline_doc(trail));
        }
        if let Some(trail) = self.trailing_below.get(&offset) {
            for c in trail {
                parts.push(hardline());
                parts.push(comment_doc(c));
            }
        }
        concat(parts)
    }

    /// The document for orphan comments (a file with no significant tokens), one per line.
    pub(crate) fn orphan_doc(&self) -> Doc {
        if self.orphans.is_empty() {
            return nil();
        }
        let mut parts = Vec::new();
        for (i, c) in self.orphans.iter().enumerate() {
            if i > 0 {
                parts.push(hardline());
            }
            parts.push(comment_doc(c));
        }
        concat(parts)
    }

    /// Whether the token has any leading comments.
    pub(crate) fn has_leading(&self, tok: &SyntaxToken) -> bool {
        let offset = usize::from(tok.text_range().start());
        self.leading.contains_key(&offset)
    }

    /// Whether the token has any attached comment at all — leading, same-line trailing, or
    /// own-line trailing. Used to keep a trailing comma that carries a comment when the
    /// `trailing-comma` policy would otherwise drop it (comments are never dropped).
    pub(crate) fn has_comments(&self, tok: &SyntaxToken) -> bool {
        let offset = usize::from(tok.text_range().start());
        self.leading.contains_key(&offset)
            || self.trailing_inline.contains_key(&offset)
            || self.trailing_below.contains_key(&offset)
    }

    /// Whether the token carries a trailing comment — same-line (`trailing_inline`) or own-line
    /// below (`trailing_below`) — as opposed to a leading comment. A header token's trailing
    /// comment is emitted as a line suffix that flushes at the body's first newline; collapsing a
    /// single-statement body to one line would relocate it past the closing brace, so such a body
    /// must stay multi-line (see `fn-single-line`).
    pub(crate) fn has_trailing(&self, tok: &SyntaxToken) -> bool {
        let offset = usize::from(tok.text_range().start());
        self.trailing_inline.contains_key(&offset) || self.trailing_below.contains_key(&offset)
    }

    /// The number of blank lines that should precede this token's first leading comment (or
    /// the token itself, when it has none).
    pub(crate) fn blank_lines_before_first(&self, tok: &SyntaxToken) -> usize {
        let offset = usize::from(tok.text_range().start());
        match self.leading.get(&offset) {
            Some(lead) if !lead.is_empty() => lead[0].blank_lines_before,
            _ => 0,
        }
    }

    /// The trailing comments of a token (no leading comments): same-line trailing comments
    /// as line suffixes, then own-line comments below the file's last significant token on
    /// their own lines — exactly the trailing halves of [`CommentMap::token`], for callers
    /// that emit the token text themselves (the closing brace of a braced body).
    pub(crate) fn trailing_doc(&self, tok: &SyntaxToken) -> Doc {
        let offset = usize::from(tok.text_range().start());
        let mut parts = Vec::new();
        if let Some(trail) = self.trailing_inline.get(&offset) {
            parts.push(trailing_inline_doc(trail));
        }
        if let Some(trail) = self.trailing_below.get(&offset) {
            for c in trail {
                parts.push(hardline());
                parts.push(comment_doc(c));
            }
        }
        concat(parts)
    }

    /// The document for comments dangling before a token (e.g. inside an otherwise empty
    /// block, anchored as leading comments of the closing brace).
    pub(crate) fn dangling(&self, tok: &SyntaxToken) -> Doc {
        let offset = usize::from(tok.text_range().start());
        match self.leading.get(&offset) {
            None => nil(),
            Some(lead) => {
                let mut parts = Vec::new();
                for (i, c) in lead.iter().enumerate() {
                    if i > 0 {
                        parts.push(hardline());
                    }
                    parts.push(comment_doc(c));
                }
                concat(parts)
            }
        }
    }
}

/// The document for a token's same-line trailing comments: each emitted as a line suffix
/// (deferred to the end of the line). A `//` line comment runs to end of line, so anything
/// after it — the next token *or another trailing comment* — must start a fresh line; a
/// trailing [`hardline`] forces that break (it coalesces with any following break). Shared by
/// [`CommentMap::token`] and [`CommentMap::trailing_doc`] so a closing brace's trailing line
/// comment forces the break too, never colliding with the next comment under error recovery.
fn trailing_inline_doc(trail: &[Comment]) -> Doc {
    let mut parts = Vec::new();
    let mut force_break = false;
    for c in trail {
        parts.push(line_suffix(concat(vec![text("  "), comment_inline(c)])));
        if c.kind == SyntaxKind::LINE_COMMENT {
            force_break = true;
        }
    }
    if force_break {
        parts.push(hardline());
    }
    concat(parts)
}

/// Is this token kind a comment?
pub(crate) fn is_comment(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
    )
}

/// Lightly normalize comment text: line comments have trailing whitespace stripped;
/// block/doc comments are kept verbatim (their interior alignment is preserved).
fn content(tok: &SyntaxToken) -> String {
    match tok.kind() {
        SyntaxKind::LINE_COMMENT => tok.text().trim_end().to_string(),
        _ => tok.text().to_string(),
    }
}

/// The document for a standalone comment (leading, dangling, orphan, or own-line
/// trailing). These are reflowable: under `wrap-comments` the renderer rewraps them to
/// `comment-width` at their final indentation.
fn comment_doc(c: &Comment) -> Doc {
    let kind = match c.kind {
        SyntaxKind::DOC_COMMENT => CommentKind::Doc,
        SyntaxKind::BLOCK_COMMENT => CommentKind::Block,
        _ => CommentKind::Line,
    };
    comment(kind, c.text.clone())
}

/// The document for a same-line trailing comment, emitted verbatim as a line suffix. These
/// are never reflowed: they sit after code, so wrapping them onto new lines is ambiguous.
fn comment_inline(c: &Comment) -> Doc {
    match c.kind {
        SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT => raw(c.text.clone()),
        _ => text(c.text.clone()),
    }
}
