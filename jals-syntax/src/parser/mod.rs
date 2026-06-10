//! An error-resilient recursive-descent parser. The grammar emits a stream of events ([`event`]),
//! and [`sink`] assembles the `rowan` green tree. Returning a tree without panicking even on
//! malformed input is an invariant.

mod event;
mod grammar;
mod input;
mod marker;
mod sink;
mod token_set;

#[cfg(test)]
mod prop;
#[cfg(test)]
mod tests;

use std::cell::Cell;

use rowan::GreenNode;

use crate::language::SyntaxNode;
use crate::syntax_error::SyntaxError;
use crate::syntax_kind::SyntaxKind;
use event::Event;
use input::Input;
use marker::Marker;
use token_set::TokenSet;

/// Fuel for detecting infinite loops where lookahead repeats at the same position without advancing.
const PARSER_FUEL: u32 = 256;

/// The parser core. Scans the sequence of significant tokens at position `pos` and assembles a stream of events.
pub(crate) struct Parser<'a> {
    input: &'a Input<'a>,
    pos: usize,
    pub(crate) events: Vec<Event>,
    fuel: Cell<u32>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a Input<'a>) -> Self {
        Parser {
            input,
            pos: 0,
            events: Vec::new(),
            fuel: Cell::new(PARSER_FUEL),
        }
    }

    /// Open a new node.
    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len();
        self.events.push(Event::tombstone());
        Marker::new(pos)
    }

    pub(crate) fn push_event(&mut self, e: Event) {
        self.events.push(e);
    }

    /// Current significant token position. Used for the loop's progress guarantee (if the value
    /// does not change, the token was not consumed).
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Kind of the significant token `n` positions ahead.
    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        assert_ne!(
            self.fuel.get(),
            0,
            "parser fuel exhausted (possible infinite loop)"
        );
        self.fuel.set(self.fuel.get() - 1);
        self.input.kind(self.pos + n)
    }

    /// Kind of the current significant token.
    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.nth(0) == kind
    }

    pub(crate) fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        self.nth(n) == kind
    }

    pub(crate) fn at_ts(&self, set: TokenSet) -> bool {
        set.contains(self.nth(0))
    }

    pub(crate) fn at_eof(&self) -> bool {
        self.at(SyntaxKind::EOF)
    }

    /// Whether the current token is `IDENT` and its text matches `kw` (contextual-keyword check).
    pub(crate) fn at_contextual_kw(&self, kw: &str) -> bool {
        self.at(SyntaxKind::IDENT) && self.current_text() == kw
    }

    /// Whether the significant token `n` positions ahead is adjacent to the next significant token
    /// (no trivia in between). Used for fusing `>>` and similar.
    pub(crate) fn nth_adjacent(&self, n: usize) -> bool {
        self.input.adjacent(self.pos + n)
    }

    /// Text of the current significant token (for contextual-keyword checks).
    pub(crate) fn current_text(&self) -> &'a str {
        self.input.text(self.pos)
    }

    pub(crate) fn nth_text(&self, n: usize) -> &'a str {
        self.input.text(self.pos + n)
    }

    /// Fuel-free lookahead (kind `n` positions ahead). For bounded scans in lambda / cast only.
    /// Cannot loop forever because it always stops at input length (out of range yields [`SyntaxKind::EOF`]).
    pub(crate) fn nth_nofuel(&self, n: usize) -> SyntaxKind {
        self.input.kind(self.pos + n)
    }

    fn do_bump(&mut self, remap: Option<SyntaxKind>) {
        self.pos += 1;
        self.fuel.set(PARSER_FUEL);
        self.events.push(Event::Token { remap });
    }

    /// Consume the current token (no-op at EOF).
    pub(crate) fn bump_any(&mut self) {
        if self.at_eof() {
            return;
        }
        self.do_bump(None);
    }

    /// Assert the current token is `kind` and consume it.
    pub(crate) fn bump(&mut self, kind: SyntaxKind) {
        assert!(
            self.at(kind),
            "tried to bump {kind:?} but current was {:?}",
            self.current()
        );
        self.do_bump(None);
    }

    /// Reclassify the current token as `kind` and consume it (contextual-keyword promotion).
    pub(crate) fn bump_remap(&mut self, kind: SyntaxKind) {
        self.do_bump(Some(kind));
    }

    /// If the current token is `kind`, consume it and return `true`.
    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.do_bump(None);
            true
        } else {
            false
        }
    }

    /// Record an error (does not consume a token).
    pub(crate) fn error(&mut self, msg: impl Into<String>) {
        self.events.push(Event::Error { msg: msg.into() });
    }

    /// Expect `kind`. If present, consume it and return `true`; otherwise record an error and
    /// return `false` (does not consume).
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error(format!("expected {kind:?}"));
            false
        }
    }

    /// Wrap the current token in an `ERROR` node and consume it (guarantees progress while recovering).
    pub(crate) fn err_and_bump(&mut self, msg: impl Into<String>) {
        let m = self.start();
        self.error(msg);
        self.bump_any();
        m.complete(self, SyntaxKind::ERROR);
    }

    /// Record an error and wrap one token in `ERROR` unless it is in the recovery set `recovery`.
    /// Does not consume when the token is in the recovery set (i.e., the caller can handle it) or at EOF.
    pub(crate) fn err_recover(&mut self, msg: impl Into<String>, recovery: TokenSet) {
        if self.at_eof() || self.at_ts(recovery) {
            self.error(msg);
            return;
        }
        self.err_and_bump(msg);
    }

    fn finish(self) -> Vec<Event> {
        self.events
    }
}

/// Parse the source and return a [`Parse`].
pub fn parse(src: &str) -> Parse {
    let input = Input::new(src);
    let mut p = Parser::new(&input);
    grammar::root(&mut p);
    let events = p.finish();
    let (green, errors) = sink::build(&input, events);
    Parse { green, errors }
}

/// Parse result. Holds the green tree and the list of syntax errors.
pub struct Parse {
    green: GreenNode,
    errors: Vec<SyntaxError>,
}

impl Parse {
    /// The root node of the syntax tree.
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    /// The list of syntax errors.
    pub fn errors(&self) -> &[SyntaxError] {
        &self.errors
    }

    /// A reference to the green tree.
    pub fn green(&self) -> &GreenNode {
        &self.green
    }
}
