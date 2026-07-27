//! The emission API — google-java-format's `OpsBuilder`, as a document builder.
//!
//! A visitor never constructs a [`Doc`] by hand. It calls [`Ops`] imperatively — open a level,
//! emit tokens and breaks, close it — and the builder maintains the stack. That is the shape GJF
//! uses, and it is the shape the CST walk wants: emission is a side effect of visiting a node,
//! not a value threaded back up.
//!
//! # The one rule every visitor follows
//!
//! **Emit every one of your node's direct significant tokens, and recurse into every child
//! node.** Because each token in a `rowan` tree has exactly one parent, that alone guarantees the
//! significant-token multiset is preserved, whatever coverage the visitor set has — a node with
//! no bespoke rule still falls through to the generic path and emits its tokens. The formatter's
//! central invariant is therefore structural rather than something each rule has to remember.

use alloc::vec::Vec;

use crate::ir::{Break, BreakTag, Doc, FillMode, Indent, Level};

/// Builds a [`Doc`] from a stream of emission calls.
pub(crate) struct Ops {
    /// Open levels, innermost last. Never empty: the root level is pushed on construction.
    stack: Vec<Level>,
    /// The next [`BreakTag`] id to hand out.
    next_tag: u32,
    /// A `//` comment was just emitted, so the next break must be taken whatever its requested
    /// fill mode — a line comment runs to end of line, and anything after it on that line would
    /// be swallowed. google-java-format spells the same rule by giving a `//` `Tok` an infinite
    /// width *and* forcing the following break.
    force_next: bool,
    /// Everything emitted is being dropped: the visitor is walking tokens covered by a
    /// formatter-disabled region, whose text was already emitted verbatim as one node.
    suppressed: bool,
}

impl Ops {
    /// A builder with one open root level at zero indent.
    pub(crate) fn new() -> Self {
        Self {
            stack: alloc::vec![Level::new(Indent::ZERO)],
            next_tag: 0,
            force_next: false,
            suppressed: false,
        }
    }

    /// Whether emission is currently being dropped.
    pub(crate) const fn is_suppressed(&self) -> bool {
        self.suppressed
    }

    /// Start or stop dropping emission.
    pub(crate) const fn set_suppressed(&mut self, suppressed: bool) {
        self.suppressed = suppressed;
    }

    /// Finish, returning the root document and how many break tags were handed out.
    ///
    /// Any level the visitor left open is closed here rather than treated as an error: a
    /// malformed input can end mid-construct, and the formatter never panics.
    pub(crate) fn finish(mut self) -> (Doc, usize) {
        while self.stack.len() > 1 {
            self.close();
        }
        let root = self.stack.pop().unwrap_or_else(|| Level::new(Indent::ZERO));
        (Doc::Level(root), self.next_tag as usize)
    }

    /// Open a level that adds `plus_indent` when it breaks.
    pub(crate) fn open(&mut self, plus_indent: Indent) {
        self.stack.push(Level::new(plus_indent));
    }

    /// Close the innermost level, appending it to its parent.
    ///
    /// An empty level is dropped instead of appended: it would measure zero and render nothing,
    /// but it would still be a split boundary the engine has to walk.
    pub(crate) fn close(&mut self) {
        if self.stack.len() <= 1 {
            return;
        }
        let Some(level) = self.stack.pop() else {
            return;
        };
        if level.docs.is_empty() {
            return;
        }
        // Appended directly, never through `push`: suppression is about what the visitor is
        // *emitting now*, and a level that already has contents was built before it started. A
        // formatter-disabled region opens exactly this way — its verbatim text is emitted, then
        // suppression goes on, then the enclosing level closes — and routing this through `push`
        // would drop the region on the floor.
        if let Some(parent) = self.stack.last_mut() {
            parent.docs.push(Doc::Level(level));
        }
    }

    /// Append a node to the innermost level.
    ///
    /// A pending `force_next` is honored here and not only at the next [`Ops::brk`]: a `//`
    /// comment swallows the rest of its line, so *anything* emitted after one has to start a new
    /// line. Leaving it to the next break assumes a break is coming, and an empty block whose `{`
    /// carries a trailing comment has none — its `}` would end up inside the comment.
    fn push(&mut self, doc: Doc) {
        if self.suppressed {
            return;
        }
        if self.force_next && !matches!(doc, Doc::Break(_) | Doc::Space) {
            self.force_next = false;
            let brk = Break::new(FillMode::Forced, "", Indent::ZERO, None);
            if let Some(level) = self.stack.last_mut() {
                level.docs.push(Doc::Break(brk));
            }
        }
        if let Some(level) = self.stack.last_mut() {
            level.docs.push(doc);
        }
    }

    /// Whether the innermost level has nothing in it yet.
    pub(crate) fn level_is_empty(&self) -> bool {
        self.stack.last().is_some_and(|level| level.docs.is_empty())
    }

    /// Whether *nothing at all* has been emitted yet.
    ///
    /// Distinct from [`Ops::level_is_empty`]: a body's level is empty the moment it opens, but the
    /// `{` above it is already out, so an own-line comment there still needs a line of its own.
    /// Only at the very start of the document does a leading break have nothing to separate.
    pub(crate) fn is_empty(&self) -> bool {
        self.stack.iter().all(|level| level.docs.is_empty())
    }

    // ===== Leaves =====

    /// A significant token.
    pub(crate) fn token(&mut self, text: &str) {
        self.push(Doc::token(text));
    }

    /// A non-breaking space.
    pub(crate) fn space(&mut self) {
        self.push(Doc::Space);
    }

    /// A comment, whose continuation lines follow the code's indent.
    pub(crate) fn comment(&mut self, text: &str) {
        self.push(Doc::comment(text));
    }

    /// Text that must survive byte-identical — a formatter-disabled region.
    pub(crate) fn verbatim(&mut self, text: &str) {
        self.push(Doc::verbatim(text));
    }

    // ===== Breaks =====

    /// A fresh break tag, for correlating a break with an [`Indent::If`] elsewhere.
    pub(crate) const fn new_tag(&mut self) -> BreakTag {
        let tag = BreakTag(self.next_tag);
        self.next_tag += 1;
        tag
    }

    /// Force the next break emitted, whatever fill mode it asks for.
    pub(crate) const fn force_next_break(&mut self) {
        self.force_next = true;
    }

    /// Whether the last node emitted is a break, looking through levels that are still empty.
    ///
    /// A level opens with nothing in it, so asking only the innermost one would miss the break an
    /// enclosing level just emitted — and an own-line comment would then add a second break beside
    /// it, spelling a blank line the author never wrote.
    pub(crate) fn last_is_break(&self) -> bool {
        matches!(self.innermost_written(), Some(Doc::Break(_)))
    }

    /// The last node emitted anywhere, looking outward through empty levels.
    fn innermost_written(&self) -> Option<&Doc> {
        self.stack.iter().rev().find_map(|level| level.docs.last())
    }

    /// Raise the last emitted break to a forced one.
    ///
    /// An own-line comment starts a line by definition, so a break already standing before it has
    /// to be taken. Emitting a second, forced break instead would leave the first one to render as
    /// a space and put a stray blank column before the comment.
    pub(crate) fn force_last_break(&mut self) {
        if let Some(level) = self
            .stack
            .iter_mut()
            .rev()
            .find(|level| !level.docs.is_empty())
            && let Some(Doc::Break(brk)) = level.docs.last_mut()
        {
            brk.fill = FillMode::Forced;
        }
    }

    /// Raise the last emitted break to all-or-nothing.
    ///
    /// `OpsBuilder.build` puts a UNIFIED break in front of every comment it inserts before a
    /// token. When a break already stands there, raising that one says the same thing without
    /// emitting a second break beside it.
    pub(crate) fn unify_last_break(&mut self) {
        if let Some(level) = self.stack.last_mut()
            && let Some(Doc::Break(brk)) = level.docs.last_mut()
            && brk.fill == FillMode::Independent
        {
            brk.fill = FillMode::Unified;
        }
    }

    /// The general form.
    pub(crate) fn brk(
        &mut self,
        fill: FillMode,
        flat: &str,
        plus_indent: Indent,
        tag: Option<BreakTag>,
    ) {
        let fill = if core::mem::take(&mut self.force_next) {
            FillMode::Forced
        } else {
            fill
        };
        self.push(Doc::Break(Break::new(fill, flat, plus_indent, tag)));
    }

    /// A break that renders as a space when it stays: all-or-nothing with its level's other
    /// unified breaks.
    pub(crate) fn break_op(&mut self, plus_indent: Indent) {
        self.brk(FillMode::Unified, " ", plus_indent, None);
    }

    /// A break that always goes, making its level unable to render flat.
    pub(crate) fn forced_break(&mut self, plus_indent: Indent) {
        self.brk(FillMode::Forced, "", plus_indent, None);
    }

    /// A forced break followed by `count` empty lines.
    ///
    /// `count == 0` is exactly [`forced_break`](Self::forced_break); a non-zero count forces the
    /// fill mode, because a blank line only exists on a break that is taken.
    pub(crate) fn blank_lines(&mut self, count: usize, plus_indent: Indent) {
        self.force_next = false;
        let mut brk = Break::new(FillMode::Forced, "", plus_indent, None);
        brk.blank_lines = count;
        self.push(Doc::Break(brk));
    }

    /// Raise the *last* emitted break to at least `count` blank lines, or emit a new forced break
    /// when the last node was not one.
    ///
    /// Members are visited one after another, each responsible for the separation *before* it;
    /// this lets the following member ask for more separation than the previous one left without
    /// emitting a second break.
    pub(crate) fn ensure_blank_lines(&mut self, count: usize, plus_indent: Indent) {
        if let Some(level) = self.stack.last_mut()
            && let Some(Doc::Break(brk)) = level.docs.last_mut()
        {
            brk.fill = FillMode::Forced;
            brk.blank_lines = brk.blank_lines.max(count);
            return;
        }
        if self.level_is_empty() {
            return;
        }
        self.blank_lines(count, plus_indent);
    }
}
