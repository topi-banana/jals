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
//! - **leading-inline**: a leading comment that hugs its anchor token on the same line
//!   (`/* a= */ 1`) instead of sitting above it — parameter-name block comments under
//!   `normalize-parameter-comments`, and (under `inline-block-comments`) any block / doc comment
//!   written immediately before a significant token on the same line (`java.lang./* @A */ String`).
//!   Emitted immediately before the token text so the two wrap together as a unit. Without
//!   `inline-block-comments` such a comment falls into the *trailing* case below and is flushed to
//!   the end of its line.
//!
//! Every comment is anchored exactly once, so as long as every significant token is
//! emitted exactly once through [`CommentMap::token`], no comment is dropped or
//! duplicated. Classification uses the fact that `WHITESPACE` never contains a newline
//! and `NEWLINE` is a standalone token (CRLF is one token).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::doc::{
    CommentKind, Doc, blank_line, comment, concat, hardline, line_suffix, nil, raw, text,
};

struct Comment {
    kind: SyntaxKind,
    text: String,
    /// The number of blank lines preceding this comment in the source.
    blank_lines_before: usize,
    /// An inline (hugging) leading comment: a parameter-name block comment under
    /// `normalize-parameter-comments`, emitted on the same line immediately before its anchor
    /// token (`/* a= */ 1`) rather than on its own line above it.
    inline: bool,
}

/// Comments anchored to significant tokens by source byte offset.
pub struct CommentMap {
    leading: BTreeMap<usize, Vec<Comment>>,
    /// Leading comments that hug their anchor token on the same line (parameter comments under
    /// `normalize-parameter-comments`): emitted immediately before the token text, so they wrap
    /// together with it (`/* a= */ 1`). Keyed by the anchor token's offset, like [`leading`].
    leading_inline: BTreeMap<usize, Vec<Comment>>,
    /// Same-line trailing comments (emitted as line suffixes).
    trailing_inline: BTreeMap<usize, Vec<Comment>>,
    /// Own-line comments after the last significant token (emitted on their own lines so
    /// consecutive line comments are not merged).
    trailing_below: BTreeMap<usize, Vec<Comment>>,
    /// Comments in a file with no significant tokens at all (nothing to anchor to).
    orphans: Vec<Comment>,
}

/// Build the comment map for a tree. `normalize_param_comments` is the resolved
/// `normalize-parameter-comments` policy, threaded down to [`classify`]. `inline_block_comments`
/// is the resolved `inline-block-comments` policy: when set, a block / doc comment written
/// immediately before a significant token on the same line hugs that token as a leading-inline
/// comment instead of trailing the previous one to end of line.
pub fn build(
    root: &SyntaxNode,
    normalize_param_comments: bool,
    inline_block_comments: bool,
) -> CommentMap {
    let mut leading: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();
    let mut leading_inline: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();
    let mut trailing_inline: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();
    let mut trailing_below: BTreeMap<usize, Vec<Comment>> = BTreeMap::new();

    let mut last_sig: Option<usize> = None;
    let mut newlines: usize = 0; // newlines since the last significant token or comment
    let mut pending: Vec<Comment> = Vec::new();

    for tok in root
        .descendants_with_tokens()
        .filter_map(SyntaxElement::into_token)
    {
        let kind = tok.kind();
        if is_comment(kind) {
            let (text, mut inline) = classify(&tok, normalize_param_comments);
            // A block / doc comment written immediately before a significant token *on the same
            // line* hugs that token instead of trailing the previous one to end of line, when
            // `inline-block-comments` is on (e.g. `java.lang./* @A */ String`). The condition is
            // purely "followed by a same-line significant token": the hug glues the comment to that
            // token with one space and no break, so the property survives reformatting and the hug
            // is idempotent — whereas keying on the *preceding* token's line would flip once the
            // following token wraps onto its own line. A line comment runs to end of line, so it is
            // never followed by a same-line token and never qualifies.
            if inline_block_comments
                && !inline
                && matches!(kind, SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT)
                && followed_by_same_line_sig(&tok)
            {
                inline = true;
            }
            let comment = Comment {
                kind,
                text,
                blank_lines_before: newlines.saturating_sub(1),
                inline,
            };
            match last_sig {
                // A parameter comment (`inline`) always *leads* the following token (it hugs it),
                // so it is never attached as a trailing comment of the previous one — route it to
                // `pending` like an own-line leading comment regardless of an intervening newline.
                Some(anchor) if newlines == 0 && pending.is_empty() && !inline => {
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
                // Inline (hugging) parameter comments go to `leading_inline`; own-line comments
                // to `leading`. `partition` keeps each group's source order, and the two groups
                // never interleave at emit time (own-line above, inline hugging the token).
                let (inline_lead, own_lead): (Vec<Comment>, Vec<Comment>) =
                    core::mem::take(&mut pending)
                        .into_iter()
                        .partition(|c| c.inline);
                if !own_lead.is_empty() {
                    leading.insert(offset, own_lead);
                }
                if !inline_lead.is_empty() {
                    leading_inline.insert(offset, inline_lead);
                }
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
        leading_inline,
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
        // Inline (hugging) leading comments sit immediately before the token text on the same
        // line, e.g. `/* a= */ 1`. They are concatenated into the token's own unit so they wrap
        // together with it (a broken argument list keeps each `/* x= */ value` group intact).
        parts.push(match self.leading_inline.get(&offset) {
            None => token_doc,
            Some(inline) => {
                let mut hug: Vec<Doc> = Vec::new();
                for c in inline {
                    hug.push(raw(c.text.clone()));
                    hug.push(text(" "));
                }
                hug.push(token_doc);
                concat(hug)
            }
        });
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
            || self.leading_inline.contains_key(&offset)
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

    /// Whether the token carries any *dangling* comment — an own-line `leading` comment or a
    /// hugging `leading_inline` one — i.e. a comment anchored before it with nothing significant
    /// in between. Used by a braced body's closing brace (emitted directly, never through
    /// [`CommentMap::token`]) to decide whether the body is truly empty. A `leading_inline`
    /// comment on a closing brace (`{ /* b */ }` under `inline-block-comments`) has no following
    /// content token to hug, so it dangles here exactly like an own-line one.
    pub(crate) fn has_dangling(&self, tok: &SyntaxToken) -> bool {
        let offset = usize::from(tok.text_range().start());
        self.leading.contains_key(&offset) || self.leading_inline.contains_key(&offset)
    }

    /// The document for comments dangling before a token (e.g. inside an otherwise empty
    /// block, anchored as leading comments of the closing brace). Both own-line `leading`
    /// comments and hugging `leading_inline` ones are emitted, each on its own line: a
    /// `leading_inline` comment routed here (one written immediately before a body's closing
    /// brace under `inline-block-comments`) has no following content token to hug, so it is
    /// emitted own-line rather than dropped. Reformatting then sees it on its own line — no
    /// longer followed by a same-line significant token — so it stays own-line (idempotent).
    pub(crate) fn dangling(&self, tok: &SyntaxToken) -> Doc {
        let offset = usize::from(tok.text_range().start());
        let lead = self.leading.get(&offset).into_iter().flatten();
        let inline = self.leading_inline.get(&offset).into_iter().flatten();
        let mut parts = Vec::new();
        for c in lead.chain(inline) {
            if !parts.is_empty() {
                parts.push(hardline());
            }
            parts.push(comment_doc(c));
        }
        concat(parts)
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
        parts.push(line_suffix(concat(vec![text(" "), comment_inline(c)])));
        if c.kind == SyntaxKind::LINE_COMMENT {
            force_break = true;
        }
    }
    if force_break {
        parts.push(hardline());
    }
    concat(parts)
}

/// Whether the next significant (non-trivia) token after `tok` is on the same line — i.e. no
/// `NEWLINE` separates them. Intervening comments and whitespace are skipped. Used by
/// `inline-block-comments` to decide whether a comment should hug the following token. Returns
/// `false` at end of input.
fn followed_by_same_line_sig(tok: &SyntaxToken) -> bool {
    let mut cur = tok.next_token();
    while let Some(t) = cur {
        let k = t.kind();
        if k == SyntaxKind::NEWLINE {
            return false;
        }
        if !k.is_trivia() {
            return true;
        }
        // A comment or whitespace on the same line: keep scanning for the next significant token.
        cur = t.next_token();
    }
    false
}

/// Is this token kind a comment?
pub const fn is_comment(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT | SyntaxKind::DOC_COMMENT
    )
}

/// Classify a comment token into its emitted text and whether it is an inline (hugging)
/// parameter comment.
///
/// Line comments have their trailing whitespace stripped; block / doc comments are kept verbatim
/// (their interior alignment is preserved). The one exception is `normalize-parameter-comments`:
/// a `BLOCK_COMMENT` that is a parameter-name label (`/*a=*/`) is rewritten to the canonical
/// `/* a= */` form (see [`crate::rules::parameter_comment`]) and flagged `inline` so it hugs the
/// following token instead of trailing the previous one or sitting on its own line.
fn classify(tok: &SyntaxToken, normalize_param_comments: bool) -> (String, bool) {
    match tok.kind() {
        SyntaxKind::LINE_COMMENT => (tok.text().trim_end().into(), false),
        SyntaxKind::BLOCK_COMMENT if normalize_param_comments => {
            crate::rules::parameter_comment::normalize(tok.text()).map_or_else(
                || (tok.text().into(), false),
                |normalized| (normalized, true),
            )
        }
        _ => (tok.text().into(), false),
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
