//! The layout engine — google-java-format's `Doc.Level.computeBreaks`, ported.
//!
//! # The algorithm, and what it is not
//!
//! Resolution is **greedy, single-pass, left-to-right, and never backtracks**. A [`Level`] is
//! measured by its own precomputed flat width; if that fits from the current column it renders on
//! one line and the walk **stops there** — it does not look past the level to see what follows.
//! Otherwise the level breaks, its indent is added, and its top-level breaks are decided one at a
//! time, each seeing only the state the ones before it left.
//!
//! Three details carry most of the difference from a Wadler/prettier printer:
//!
//! 1. **Level-local fit.** Prettier's `fits` scans forward past the group to the next hard
//!    newline. This does not.
//! 2. **Mixed fill modes.** One level may hold both [`FillMode::Unified`] breaks (all-or-nothing)
//!    and [`FillMode::Independent`] ones (fill). Prettier needs two different node types.
//! 3. **`must_break` propagates forward.** When a split overflows, the *next* break is forced
//!    regardless of what would fit. There is no prettier equivalent.
//!
//! None of it is configurable. `Config` moves the constants ([`Style::max_width`], the indents)
//! and what the visitors emit — never this file's control flow (`DESIGN.md` §8.1, §8.3).
//!
//! # Three walks
//!
//! [`Engine::render`] runs [`Engine::measure_level`] (bottom-up widths), then
//! [`Engine::compute_level`] (break decisions, recorded into the tree), then [`Writer`] (text).
//! Each boxes its recursion **once, at the level-to-level back edge**, which is the walk's only
//! cycle; leaves are reached by a plain loop.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use jals_exec::{LocalBoxFuture, Yielder};

use crate::ir::{BreakTag, Doc, FillMode, Indent, Level, Width};
use crate::style::Style;

/// The engine's left-to-right state.
///
/// `indent` is the enclosing level's indent and `last_indent` the indent a break in *this* level
/// starts from; they differ only while a level is broken. Both are signed because a
/// [`Indent::Const`] may be negative (a dedent), and are clamped at zero when materialized.
#[derive(Debug, Clone, Copy)]
struct State {
    /// The indent a break in the current level computes from.
    last_indent: i32,
    /// The current level's indent.
    indent: i32,
    /// The column the next character lands in.
    column: usize,
    /// The previous split overflowed, so the next break must be taken.
    must_break: bool,
}

impl State {
    /// The same state at a different column.
    const fn with_column(self, column: usize) -> Self {
        Self { column, ..self }
    }
}

/// Resolves a document's breaks and renders it.
pub(crate) struct Engine<'s> {
    /// The resolved style.
    style: &'s Style,
    /// Whether each [`BreakTag`]'s break was taken, indexed by tag id. A flat vector rather than
    /// a map because [`Ops`](crate::ops::Ops) hands out ids sequentially.
    tags: Vec<bool>,
    /// Amortized cooperative yielding across the whole walk.
    yielder: Yielder,
}

impl<'s> Engine<'s> {
    /// An engine for `style`, sized for the `tag_count` break tags the document uses.
    pub(crate) fn new(style: &'s Style, tag_count: usize) -> Self {
        Self {
            style,
            tags: vec![false; tag_count],
            yielder: Yielder::new(),
        }
    }

    /// Measure, resolve, and render `doc`.
    pub(crate) async fn render(&mut self, doc: &mut Doc) -> String {
        if let Doc::Level(level) = doc {
            Measure::level(level).await;
            let state = State {
                last_indent: 0,
                indent: 0,
                column: 0,
                must_break: false,
            };
            self.compute_level(level, state).await;
        }
        let mut writer = Writer::new(self.style);
        writer.write(doc).await;
        writer.finish()
    }

    // ===== Pass 2: break decisions =====

    /// Resolve one node, returning the state after it.
    async fn compute(&mut self, doc: &mut Doc, state: State) -> State {
        match doc {
            Doc::Level(level) => self.compute_level_boxed(level, state).await,
            Doc::Break(_) => state, // decided by the enclosing level, never on its own
            Doc::Space => state.with_column(state.column.saturating_add(1)),
            Doc::Token { text } | Doc::Tok { text, .. } => {
                state.with_column(Writer::advance(state.column, text))
            }
        }
    }

    /// The one boxed shim of the resolution recursion.
    fn compute_level_boxed<'a>(
        &'a mut self,
        level: &'a mut Level,
        state: State,
    ) -> LocalBoxFuture<'a, State> {
        Box::pin(self.compute_level(level, state))
    }

    /// `Doc.Level.computeBreaks`: fit the level flat by its own width, or break it.
    async fn compute_level(&mut self, level: &mut Level, state: State) -> State {
        self.yielder.tick().await;
        if state.column.saturating_add(level.width) <= self.style.max_width() {
            level.one_line = true;
            return state.with_column(state.column + level.width);
        }
        level.one_line = false;
        let indent = state.indent.saturating_add(self.eval(&level.plus_indent));
        let inner = State {
            last_indent: indent,
            indent,
            column: state.column,
            must_break: false,
        };
        let broken = self.compute_broken(level, inner).await;
        state.with_column(broken.column)
    }

    /// `Doc.Level.computeBroken`: walk the level as `split (break split)*`, deciding each break
    /// against the state the previous split left behind.
    async fn compute_broken(&mut self, level: &mut Level, mut state: State) -> State {
        let breaks: Vec<usize> = level
            .docs
            .iter()
            .enumerate()
            .filter(|(_, doc)| matches!(doc, Doc::Break(_)))
            .map(|(index, _)| index)
            .collect();

        let first_end = breaks.first().copied().unwrap_or(level.docs.len());
        state = self
            .compute_break_and_split(level, None, 0, first_end, state)
            .await;
        for (nth, &at) in breaks.iter().enumerate() {
            let end = breaks.get(nth + 1).copied().unwrap_or(level.docs.len());
            state = self
                .compute_break_and_split(level, Some(at), at + 1, end, state)
                .await;
        }
        state
    }

    /// `Doc.Level.computeBreakAndSplit`: decide the break at `at` (if any), then resolve the
    /// split `start..end` that follows it.
    ///
    /// The break goes when it is [`FillMode::Unified`] (its level is broken, so all of them go),
    /// when the previous split overflowed, or when the break plus the split would not fit. An
    /// [`FillMode::Independent`] break passes the first test, which is what makes it a fill.
    async fn compute_break_and_split(
        &mut self,
        level: &mut Level,
        at: Option<usize>,
        start: usize,
        end: usize,
        mut state: State,
    ) -> State {
        let max = self.style.max_width();
        let break_width = at.map_or(0, |index| level.docs[index].width());
        let split_width = Doc::width_of(&level.docs[start..end]);

        let unified = at.is_some_and(
            |index| matches!(&level.docs[index], Doc::Break(brk) if brk.fill == FillMode::Unified),
        );
        let should_break = unified
            || state.must_break
            || state
                .column
                .saturating_add(break_width)
                .saturating_add(split_width)
                > max;

        if let Some(index) = at {
            state = self.decide_break(level, index, should_break, state);
        }

        let enough_room = state.column.saturating_add(split_width) <= max;
        state.must_break = false;
        for index in start..end {
            state = self.compute(&mut level.docs[index], state).await;
        }
        if !enough_room {
            state.must_break = true;
        }
        state
    }

    /// `Doc.Break.computeBreaks`: record the decision on the break (and on its tag), and return
    /// the column the following text starts at.
    fn decide_break(
        &mut self,
        level: &mut Level,
        index: usize,
        broken: bool,
        state: State,
    ) -> State {
        let Doc::Break(brk) = &level.docs[index] else {
            return state;
        };
        // Record the tag before evaluating any indent, as google-java-format does, so an
        // `Indent::If` reading this very tag sees the decision.
        let tag = brk.tag;
        if let Some(tag) = tag {
            self.record(tag, broken);
        }

        let (flat_width, plus_indent) = {
            let Doc::Break(brk) = &level.docs[index] else {
                return state;
            };
            (Width::utf16(&brk.flat), self.eval(&brk.plus_indent))
        };

        let new_indent =
            usize::try_from(state.last_indent.saturating_add(plus_indent).max(0)).unwrap_or(0);
        let Doc::Break(brk) = &mut level.docs[index] else {
            return state;
        };
        brk.broken = broken;
        brk.new_indent = new_indent;

        if broken {
            state.with_column(new_indent)
        } else {
            state.with_column(state.column.saturating_add(flat_width))
        }
    }

    /// Record whether `tag`'s break was taken.
    fn record(&mut self, tag: BreakTag, broken: bool) {
        if let Some(slot) = self.tags.get_mut(tag.0 as usize) {
            *slot = broken;
        }
    }

    /// Evaluate an indent amount, resolving [`Indent::If`] against the recorded decisions.
    ///
    /// Plain recursion: an `Indent` is a handful of nodes deep at most, never a function of the
    /// input's size.
    fn eval(&self, indent: &Indent) -> i32 {
        match indent {
            Indent::Const(columns) => *columns,
            Indent::If { tag, broken, flat } => {
                if self.tags.get(tag.0 as usize).copied().unwrap_or(false) {
                    self.eval(broken)
                } else {
                    self.eval(flat)
                }
            }
        }
    }
}

/// Pass 1: fill in every [`Level::width`] bottom-up, so the engine can measure a level in O(1).
///
/// Its own namespace rather than a method on [`Engine`] because it reads no style and no state —
/// widths are a property of the document alone.
struct Measure;

impl Measure {
    /// Measure one level and everything under it.
    async fn level(level: &mut Level) {
        for child in &mut level.docs {
            if let Doc::Level(inner) = child {
                Self::level_boxed(inner).await;
            }
        }
        level.width = Doc::width_of(&level.docs);
    }

    /// The one boxed shim of the measuring recursion.
    fn level_boxed(level: &mut Level) -> LocalBoxFuture<'_, ()> {
        Box::pin(Self::level(level))
    }
}

/// Pass 3: turn a resolved document into text.
///
/// The writer always emits `\n`; converting to the configured terminator is
/// [`Finalize`](crate::passes::Finalize)'s job, so every width computation upstream can assume a
/// one-character break.
pub(crate) struct Writer<'s> {
    /// The resolved style, for rendering indentation.
    style: &'s Style,
    /// The text built so far.
    out: String,
    /// The column the next character lands in.
    column: usize,
    /// Amortized cooperative yielding.
    yielder: Yielder,
}

impl<'s> Writer<'s> {
    /// An empty writer for `style`.
    fn new(style: &'s Style) -> Self {
        Self {
            style,
            out: String::new(),
            column: 0,
            yielder: Yielder::new(),
        }
    }

    /// The rendered text.
    fn finish(self) -> String {
        self.out
    }

    /// The column after emitting `text` from `column`.
    fn advance(column: usize, text: &str) -> usize {
        match text.rfind('\n') {
            Some(at) => Width::utf16(text[at + 1..].trim_end_matches('\r')),
            None => column.saturating_add(Width::utf16(text)),
        }
    }

    /// Emit one node.
    async fn write(&mut self, doc: &Doc) {
        match doc {
            Doc::Level(level) => self.write_level_boxed(level).await,
            Doc::Token { text } => self.write_raw(text),
            Doc::Space => {
                self.out.push(' ');
                self.column += 1;
            }
            Doc::Break(brk) => {
                if brk.broken {
                    for _ in 0..=brk.blank_lines {
                        self.out.push('\n');
                    }
                    self.style.write_indent(brk.new_indent, &mut self.out);
                    self.column = brk.new_indent;
                } else {
                    self.out.push_str(&brk.flat);
                    self.column += Width::utf16(&brk.flat);
                }
            }
            Doc::Tok { text, reindent } => {
                if *reindent {
                    self.write_comment(text);
                } else {
                    self.write_raw(text);
                }
            }
        }
    }

    /// The one boxed shim of the writing recursion.
    fn write_level_boxed<'a>(&'a mut self, level: &'a Level) -> LocalBoxFuture<'a, ()> {
        Box::pin(self.write_level(level))
    }

    /// A level: its flat form when it fit, otherwise its children.
    async fn write_level(&mut self, level: &Level) {
        self.yielder.tick().await;
        if level.one_line {
            let before = self.out.len();
            for child in &level.docs {
                child.write_flat(&mut self.out);
            }
            self.column = Self::advance(self.column, &self.out[before..]);
            return;
        }
        for child in &level.docs {
            self.write(child).await;
        }
    }

    /// Emit text exactly as it is, tracking the column across any newline it contains.
    fn write_raw(&mut self, text: &str) {
        self.out.push_str(text);
        self.column = Self::advance(self.column, text);
    }

    /// Emit a comment, re-aligning its continuation lines under the opening delimiter.
    ///
    /// Only the conventional shape is re-aligned — every continuation line starting with `*`,
    /// which is what `/* … */` and Javadoc look like once written. Anything else (ASCII art, an
    /// embedded snippet whose relative indentation carries meaning) is emitted verbatim, because
    /// trimming it would destroy information the formatter has no way to reconstruct.
    fn write_comment(&mut self, text: &str) {
        let start = self.column;
        let mut lines = text.split('\n');
        let Some(first) = lines.next() else {
            return;
        };
        // Only the two conventional shapes are re-aligned: a `*`-prefixed block comment and a
        // refilled `//` run. Anything else (ASCII art, an embedded snippet) keeps its own
        // relative indentation, because trimming would destroy information the formatter cannot
        // reconstruct.
        let conventional = text.split('\n').skip(1).all(|line| {
            let body = line.trim_start();
            body.starts_with('*') || body.starts_with("//")
        });
        if !conventional {
            self.write_raw(text);
            return;
        }

        self.out.push_str(first.trim_end_matches('\r'));
        let mut last_width = Width::utf16(first);
        for line in text.split('\n').skip(1) {
            self.out.push('\n');
            self.style.write_indent(start + 1, &mut self.out);
            let body = line.trim_end_matches('\r').trim_start();
            self.out.push_str(body);
            last_width = start + 1 + Width::utf16(body);
        }
        self.column = last_width;
    }
}
