//! Method chains: `a.b().c().d()` — google-java-format's `visitDot`, ported.
//!
//! In the CST a chain is a left-leaning spine of `CALL_EXPR` / `FIELD_ACCESS` / `METHOD_REF_EXPR`
//! nodes, so visiting it naively would open one level per link and let the innermost receiver
//! break on its own — the ragged output that "break at the highest level first" exists to
//! prevent. The chain is therefore **flattened at its outermost node** into [`Link`]s, and laid
//! out as one construct.
//!
//! # Prefixes
//!
//! Not every `.` is a link. `com.google.common.collect.ImmutableList.builder()` has one
//! invocation and six dots that spell a *name*; breaking them would be nonsense. A **prefix** is a
//! run of leading links that lay out as a single unit, and there are four sources of them:
//!
//! 1. a leading run that reads as a qualified type name ([`TypePrefix`], a port of
//!    `TypeNameClassifier`'s state machine over Java's case conventions);
//! 2. a chain with exactly one invocation — with no second call to align under, `myField.foo()`
//!    reads better whole than split;
//! 3. `this` / `super`, which are never a link of their own;
//! 4. `.stream()` / `.parallelStream()` / `.toBuilder()`, where the pipeline starts *after* the
//!    call rather than at it.
//!
//! A prefix gets a level of its own, and — this is the load-bearing part — the arguments of the
//! link that *ends* the prefix are emitted **outside** that level. `logger.atInfo().log(…)`
//! therefore keeps its dots flat and wraps inside the argument list, which is what
//! google-java-format prints.
//!
//! # What this deliberately does not do
//!
//! Palantir keeps the *prefix* of an over-long chain on the receiver's line and breaks only the
//! tail (`PartialInlineability`, driven by its backtracking search). That is a different
//! resolution algorithm, not a different rule, so the single engine cannot express it and does
//! not try — it is difference **D3** in `DESIGN.md` §18.2.

use alloc::borrow::ToOwned;
use alloc::vec::Vec;

use jals_config::fmt::WrapPolicy;
use jals_syntax::{SyntaxElement, SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::ir::{BreakTag, FillMode, Indent};
use crate::visit::Ctx;

/// One dereference of a chain: the selector that introduces it, the name it selects, and the
/// argument list when it is an invocation.
struct Link {
    /// The `.` or `::`. Absent only on the name the chain starts from.
    dot: Option<SyntaxToken>,
    /// The name, possibly behind explicit type arguments (`.<K, V>of`).
    name: Vec<SyntaxElement>,
    /// The argument list, when this link invokes something.
    args: Vec<SyntaxElement>,
    /// The array indices written after it, if any.
    indices: Vec<SyntaxElement>,
    /// The simple name, for [`TypePrefix`].
    simple: Option<SyntaxToken>,
    /// How many source columns the sub-expression ending here spans — the input to
    /// `visitRegularDot`'s "don't break after a very short receiver" rule.
    length: usize,
}

impl Link {
    /// Whether this link invokes something.
    const fn is_call(&self) -> bool {
        !self.args.is_empty()
    }
}

/// What may stand beside a chain's selector.
#[derive(Clone, Copy)]
enum Selector {
    /// Nothing — the selector is written against what precedes it.
    Tight,
    /// The break `[wrapping] method-chain` asks for.
    Plain,
    /// A tagged break, so the argument list that follows can indent from its decision.
    Tagged(FillMode, BreakTag),
}

impl Ctx<'_> {
    /// A selector chain, flattened at its outermost node.
    pub(super) async fn visit_chain(&mut self, node: &SyntaxNode) {
        // A link is emitted by the chain's root. Reaching one on its own means the spine walk and
        // the dispatcher disagree; emitting the children keeps every token rather than dropping
        // the node on the floor.
        if Self::is_chain_link(node) {
            self.visit_children(node).await;
            return;
        }
        let (base, links) = Self::flatten(node);
        if links.is_empty() {
            self.visit_children(node).await;
            return;
        }

        let continuation = self.style.continuation();
        // A chain that starts from a primary expression — `new Foo().bar()`, `(a + b).c()` —
        // emits that expression first and indents everything after it. An anonymous class body is
        // the exception: after its `}` there is nothing to gain by breaking.
        let based = base.is_some();
        if let Some(base) = &base {
            let anonymous = base.kind() == S::NEW_EXPR
                && base.children().any(|child| child.kind() == S::CLASS_BODY);
            if anonymous {
                self.open_flat(Indent::ZERO);
            } else {
                self.open(continuation.clone());
            }
            self.visit(base).await;
            if !anonymous {
                self.chain_break(Selector::Plain);
            }
        }

        let prefixes = self.prefixes(&links, based);
        if prefixes.is_empty() {
            self.emit_regular(&links, based).await;
        } else {
            let unified = Self::stream_prefixes(&links)
                .into_iter()
                .any(|at| prefixes.contains(&at));
            self.emit_with_prefixes(&links, &prefixes, unified).await;
        }

        if based {
            self.close_indent(&continuation);
        }
    }

    // ===== Flattening =====

    /// Split a chain into the primary expression it may start from and its links, outermost last.
    fn flatten(node: &SyntaxNode) -> (Option<SyntaxNode>, Vec<Link>) {
        let start = Self::first_token(node).map_or(0, |tok| usize::from(tok.text_range().start()));
        let mut links: Vec<Link> = Vec::new();
        let mut pending: Vec<SyntaxElement> = Vec::new();
        let mut indices: Vec<SyntaxElement> = Vec::new();
        // A link's extent is the whole sub-expression that *ends* at it, arguments and subscripts
        // included — `getLength(items.get(i))`.
        let mut extent = 0usize;
        let mut base = None;
        let mut cursor = Some(node.clone());

        while let Some(current) = cursor {
            let receiver = current.children().next();
            let own = Self::after_receiver(&current, receiver.as_ref());
            let length = usize::from(current.text_range().end()).saturating_sub(start);
            match current.kind() {
                // A call contributes its arguments to the link its callee names.
                S::CALL_EXPR => {
                    pending = own;
                    extent = extent.max(length);
                    cursor = receiver;
                }
                // So does an index, at the far end: `a[0][1].b()` dereferences `a`, and the two
                // subscripts ride with it — `getArrayBase` / `formatArrayIndices`.
                S::INDEX_EXPR => {
                    indices.splice(0..0, own);
                    extent = extent.max(length);
                    cursor = receiver;
                }
                S::FIELD_ACCESS | S::METHOD_REF_EXPR => {
                    links.push(Self::link(
                        own,
                        core::mem::take(&mut pending),
                        core::mem::take(&mut indices),
                        core::mem::replace(&mut extent, 0).max(length),
                    ));
                    let spine = receiver.as_ref().is_some_and(|receiver| {
                        matches!(
                            receiver.kind(),
                            S::CALL_EXPR
                                | S::FIELD_ACCESS
                                | S::METHOD_REF_EXPR
                                | S::NAME_REF
                                | S::INDEX_EXPR
                        )
                    });
                    if spine {
                        cursor = receiver;
                    } else {
                        base = receiver;
                        cursor = None;
                    }
                }
                S::NAME_REF => {
                    links.push(Self::link(
                        Self::children(&current),
                        core::mem::take(&mut pending),
                        core::mem::take(&mut indices),
                        core::mem::replace(&mut extent, 0).max(length),
                    ));
                    cursor = None;
                }
                // Any other receiver is a primary expression the chain hangs off.
                _ => {
                    base = Some(current);
                    cursor = None;
                }
            }
        }
        links.reverse();
        (base, links)
    }

    /// A node's children after its receiver — the tokens the link itself owns.
    fn after_receiver(node: &SyntaxNode, receiver: Option<&SyntaxNode>) -> Vec<SyntaxElement> {
        Self::children(node)
            .into_iter()
            .filter(|child| child.as_node() != receiver)
            .collect()
    }

    /// Build a link from the elements it owns.
    fn link(
        own: Vec<SyntaxElement>,
        args: Vec<SyntaxElement>,
        indices: Vec<SyntaxElement>,
        length: usize,
    ) -> Link {
        let mut name = own;
        let dot = name
            .first()
            .and_then(SyntaxElement::as_token)
            .filter(|tok| matches!(tok.kind(), S::DOT | S::COLON_COLON))
            .cloned();
        if dot.is_some() {
            name.remove(0);
        }
        let simple = name
            .iter()
            .filter_map(SyntaxElement::as_token)
            .find(|tok| matches!(tok.kind(), S::IDENT | S::THIS_KW | S::SUPER_KW))
            .cloned();
        Link {
            dot,
            name,
            args,
            indices,
            simple,
            length,
        }
    }

    // ===== Prefixes =====

    /// The indices of the links that end a prefix, ascending.
    fn prefixes(&self, links: &[Link], based: bool) -> Vec<usize> {
        let mut prefixes: Vec<usize> = Vec::new();
        // `wrap-first-method-in-chain` asks for the first `.` to break like every other one, which
        // is exactly what a prefix suppresses.
        if self.style.cfg.wrapping.wrap_first_method_in_chain {
            return prefixes;
        }
        // A Flogger statement is one call spelled across several: `logger.atInfo().log(…)` wraps
        // inside its arguments, never between its dots — `handleLogStatement`.
        if !based && Self::is_log_statement(links) {
            prefixes.push(links.len() - 1);
            return prefixes;
        }
        if let Some(at) = TypePrefix::length(links) {
            prefixes.push(at);
        }
        // With no second call to align under, `myField.foo()` reads better whole than split.
        let calls = links
            .iter()
            .enumerate()
            .filter(|(at, link)| link.is_call() && (*at > 0 || based))
            .count();
        let first_call = links.iter().position(Link::is_call);
        if calls == 1
            && let Some(at) = first_call
            && at > 0
        {
            prefixes.push(at);
        }
        if prefixes.is_empty()
            && links
                .first()
                .and_then(|link| link.simple.as_ref())
                .is_some_and(|tok| matches!(tok.kind(), S::THIS_KW | S::SUPER_KW))
            && links.len() > 1
        {
            prefixes.push(1);
        }
        prefixes.extend(Self::stream_prefixes(links));
        prefixes.sort_unstable();
        prefixes.dedup();
        prefixes.retain(|at| *at < links.len());
        prefixes
    }

    /// The method names a Flogger chain is built from.
    const LOG_METHODS: [&'static str; 16] = [
        "at",
        "atConfig",
        "atDebug",
        "atFine",
        "atFiner",
        "atFinest",
        "atInfo",
        "atMostEvery",
        "atSevere",
        "atWarning",
        "every",
        "log",
        "logVarargs",
        "perUnique",
        "withCause",
        "withStackTrace",
    ];

    /// Whether the chain is a Flogger log statement: a plain name, then nothing but fluent
    /// logging calls, ending at `log` or `logVarargs`.
    fn is_log_statement(links: &[Link]) -> bool {
        let Some((last, rest)) = links.split_last() else {
            return false;
        };
        let name_of = |link: &Link| link.simple.as_ref().map(|tok| tok.text().to_owned());
        if !name_of(last).is_some_and(|name| matches!(name.as_str(), "log" | "logVarargs")) {
            return false;
        }
        let Some((first, middle)) = rest.split_first() else {
            return false;
        };
        !first.is_call()
            && first.dot.is_none()
            && middle.iter().all(|link| {
                link.is_call()
                    && name_of(link).is_some_and(|name| Self::LOG_METHODS.contains(&name.as_str()))
            })
    }

    /// The links after which a stream pipeline begins.
    ///
    /// `handleStream`: the calls that *produce* the thing being piped read as part of the source,
    /// not as the first stage — `foo.bar().stream().map(…)` aligns `map` under `stream`, not under
    /// `bar`.
    fn stream_prefixes(links: &[Link]) -> Vec<usize> {
        links
            .iter()
            .enumerate()
            .filter(|(_, link)| {
                link.is_call()
                    && link.simple.as_ref().is_some_and(|name| {
                        matches!(name.text(), "stream" | "parallelStream" | "toBuilder")
                    })
            })
            .map(|(at, _)| at)
            .collect()
    }

    // ===== Emission =====

    /// `visitRegularDot`: break before every dot, at one level.
    async fn emit_regular(&mut self, links: &[Link], based: bool) {
        let continuation = self.style.continuation();
        if !based {
            self.open(continuation.clone());
        }
        // Don't break after a receiver that is barely wider than the indent the break would add:
        // `foo.bar()` gains nothing from two lines.
        let minimum = self.style.continuation_cols;
        let mut length = if based { minimum } else { 0 };
        let trailing = links.len() > 1;
        for link in links {
            if let Some(dot) = &link.dot {
                let breaks = length > minimum || self.style.cfg.wrapping.wrap_first_method_in_chain;
                let selector = if breaks {
                    Selector::Plain
                } else {
                    Selector::Tight
                };
                self.emit_selector(dot, selector);
                length += 1;
            }
            if trailing && Self::fills_first_argument(link) {
                // `fillFirstArgument`: a short call at the head of a chain keeps its one argument
                // on its own line, so the chain breaks at the *next* dot rather than inside the
                // receiver — `when(something.happens()).thenReturn(result)`, not
                // `when(\n    something\n        .happens())\n    .thenReturn(result)`.
                self.open_flat(Indent::ZERO);
                for element in &link.name {
                    self.visit_element(element).await;
                }
                // The argument list is written out by hand: `fillFirstArgument` emits the parens
                // and the value with no level and no break of their own, which is the whole point
                // — the receiver reads as one unit.
                for args in link.args.iter().filter_map(SyntaxElement::as_node) {
                    for element in Self::children(args) {
                        self.visit_element(&element).await;
                    }
                }
                self.close();
                length += link.length;
                continue;
            }
            let args_indent = if trailing || link.dot.is_some() {
                continuation.clone()
            } else {
                Indent::ZERO
            };
            let tyarg = self.emit_link_name(link).await;
            self.emit_link_args(link, tyarg, args_indent).await;
            length += link.length;
        }
        if !based {
            self.close_indent(&continuation);
        }
    }

    /// Whether this link is a short call whose single argument is written out whole.
    ///
    /// `fillFirstArgument`: the receiver of `when(x).thenReturn(y)` reads as a unit, and letting
    /// its argument wrap would indent the chain twice over for no gain. The shape is narrow on
    /// purpose — a bare name of at most four characters, no type arguments, exactly one argument.
    fn fills_first_argument(link: &Link) -> bool {
        link.dot.is_none()
            && link.indices.is_empty()
            && link.is_call()
            && link
                .simple
                .as_ref()
                .is_some_and(|name| name.text().chars().count() <= 4)
            && link.name.iter().all(|element| {
                element
                    .as_node()
                    .is_none_or(|node| node.kind() != S::TYPE_ARGS)
            })
            && link.args.len() == 1
            && link
                .args
                .first()
                .and_then(SyntaxElement::as_node)
                .is_some_and(|args| args.kind() == S::ARG_LIST && args.children().count() == 1)
    }

    /// `visitDotWithPrefix`: the links up to each prefix lay out as one unit.
    async fn emit_with_prefixes(&mut self, links: &[Link], prefixes: &[usize], unified: bool) {
        let continuation = self.style.continuation();
        // Is there anything to align *under* the prefix? If not, the prefix's own arguments carry
        // no extra indent, which is what keeps `logger.atInfo().log(` on one line.
        let trailing = prefixes.last().is_some_and(|last| *last < links.len() - 1);
        let prefix_fill = if unified {
            FillMode::Unified
        } else {
            FillMode::Independent
        };

        self.open(continuation.clone());
        for _ in prefixes {
            self.open_flat(Indent::ZERO);
        }
        let name_tag = self.ops.new_tag();
        let mut unconsumed = prefixes.iter().copied().peekable();
        for (at, link) in links.iter().enumerate() {
            if let Some(dot) = &link.dot {
                let inside = unconsumed.peek().is_some_and(|first| at <= *first);
                let fill = if inside {
                    prefix_fill
                } else {
                    FillMode::Unified
                };
                self.emit_selector(dot, Selector::Tagged(fill, name_tag));
            }
            let tyarg = self.emit_link_name(link).await;
            if unconsumed.peek().is_some_and(|first| at == *first) {
                unconsumed.next();
                self.close();
            }
            let args_indent = Indent::when_broken(
                name_tag,
                continuation.clone(),
                if trailing {
                    continuation.clone()
                } else {
                    Indent::ZERO
                },
            );
            self.emit_link_args(link, tyarg, args_indent).await;
        }
        self.close_indent(&continuation);
    }

    /// A link's name, with explicit type arguments in a level of their own.
    ///
    /// Returns the tag of the break between the type arguments and the name, which the argument
    /// list indents from — `dotExpressionUpToArgs`'s `tyargTag`.
    async fn emit_link_name(&mut self, link: &Link) -> Option<BreakTag> {
        let tyargs = link
            .name
            .first()
            .and_then(SyntaxElement::as_node)
            .is_some_and(|node| node.kind() == S::TYPE_ARGS);
        if !tyargs {
            for element in &link.name {
                self.visit_element(element).await;
            }
            return None;
        }
        let tag = self.ops.new_tag();
        let continuation = self.style.continuation();
        self.open(continuation.clone());
        if let Some(first) = link.name.first() {
            self.visit_element(first).await;
        }
        self.ops.brk(FillMode::Unified, "", Indent::ZERO, Some(tag));
        self.space_already_emitted();
        self.close_indent(&continuation);
        for element in link.name.iter().skip(1) {
            self.visit_element(element).await;
        }
        Some(tag)
    }

    /// A link's argument list, opened at `args_indent` inside the type-argument level.
    async fn emit_link_args(&mut self, link: &Link, tyarg: Option<BreakTag>, args_indent: Indent) {
        if !link.args.is_empty() {
            let continuation = self.style.continuation();
            let outer = tyarg.map_or(Indent::ZERO, |tag| {
                Indent::when_broken(tag, continuation, Indent::ZERO)
            });
            self.open_flat(outer);
            self.list_indent = Some(args_indent);
            for element in &link.args {
                self.visit_element(element).await;
            }
            self.list_indent = None;
            self.close();
        }
        for element in &link.indices {
            self.visit_element(element).await;
        }
    }

    /// Emit the `.` or `::` with the break that may stand beside it.
    ///
    /// Which **side** of the dot the break falls on is `[wrapping] before-method-chain-dot`.
    fn emit_selector(&mut self, dot: &SyntaxToken, selector: Selector) {
        if matches!(selector, Selector::Tight) {
            self.token(dot);
            return;
        }
        let before = self.style.cfg.wrapping.before_method_chain_dot;
        if before {
            self.chain_break(selector);
        }
        self.token(dot);
        if !before {
            self.chain_break(selector);
        }
    }

    /// The break standing beside a link's selector.
    fn chain_break(&mut self, selector: Selector) {
        let policy = self.style.cfg.wrapping.method_chain;
        let Selector::Tagged(fill, tag) = selector else {
            self.list_break_tight(policy, Indent::ZERO);
            return;
        };
        let fill = match policy {
            WrapPolicy::Never => return,
            WrapPolicy::AlwaysPerItem => FillMode::Forced,
            WrapPolicy::IfLong | WrapPolicy::IfLongPerItem => fill,
        };
        self.ops.brk(fill, "", Indent::ZERO, Some(tag));
        self.space_already_emitted();
    }

    /// Whether this node is a link inside an enclosing chain rather than its root.
    fn is_chain_link(node: &SyntaxNode) -> bool {
        node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                S::CALL_EXPR | S::FIELD_ACCESS | S::METHOD_REF_EXPR
            )
        })
    }
}

/// google-java-format's `TypeNameClassifier`: how long a leading run of a dotted name reads as a
/// type rather than as a sequence of dereferences.
///
/// The whole judgement is Java's case conventions — `com.google.ClassName.InnerClass.CONSTANT` is
/// a name, `list.builder.add` is three dereferences — so it is a state machine over four case
/// formats and nothing else. It is a heuristic in google-java-format too, and reproducing it
/// exactly is the only way to reproduce where chains break.
struct TypePrefix;

/// The case format of one identifier.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    /// `FOO_BAR` — a constant, or a package written in caps.
    Upper,
    /// `foobar` — a package.
    Lower,
    /// `FooBar` — a type.
    UpperCamel,
    /// `fooBar` — a value.
    LowerCamel,
}

/// Where the classifier's walk stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parse {
    /// Nothing seen yet that decides it.
    Start,
    /// What has been seen so far is a type.
    Type,
    /// A type followed by one static member access.
    Member,
    /// Not a name.
    Reject,
    /// All-caps so far, which a following `UpperCamel` would resolve into a package run.
    Ambiguous,
}

impl TypePrefix {
    /// The index of the last link of the type-shaped prefix, if there is one.
    fn length(links: &[Link]) -> Option<usize> {
        let mut state = Parse::Start;
        let mut found = None;
        for (at, link) in links.iter().enumerate() {
            let name = link.simple.as_ref()?;
            state = Self::next(state, Self::case(name.text()));
            match state {
                Parse::Reject => break,
                Parse::Type | Parse::Member => found = Some(at),
                Parse::Start | Parse::Ambiguous => {}
            }
            // A name stops at the first invocation: `Foo.bar()` is a type and a call, and
            // whatever follows the call is a dereference.
            if link.is_call() || !link.indices.is_empty() {
                break;
            }
        }
        found
    }

    /// The classifier's transition function.
    const fn next(state: Parse, case: Case) -> Parse {
        match state {
            Parse::Start => match case {
                // An `UpperCamel` later would make this a class, so hold the judgement.
                Case::Upper => Parse::Ambiguous,
                Case::LowerCamel => Parse::Reject,
                Case::Lower => Parse::Start,
                Case::UpperCamel => Parse::Type,
            },
            Parse::Type => match case {
                Case::Upper | Case::LowerCamel | Case::Lower => Parse::Member,
                Case::UpperCamel => Parse::Type,
            },
            Parse::Member | Parse::Reject => Parse::Reject,
            Parse::Ambiguous => match case {
                Case::Upper => Parse::Ambiguous,
                Case::LowerCamel | Case::Lower => Parse::Reject,
                Case::UpperCamel => Parse::Type,
            },
        }
    }

    /// Classify an identifier's case format, ignoring everything that is not a letter.
    fn case(name: &str) -> Case {
        let mut first_upper = false;
        let mut has_upper = false;
        let mut has_lower = false;
        let mut first = true;
        for ch in name.chars().filter(|ch| ch.is_alphabetic()) {
            if first {
                first_upper = ch.is_uppercase();
                first = false;
            }
            has_upper |= ch.is_uppercase();
            has_lower |= ch.is_lowercase();
        }
        if first_upper {
            if has_lower || name.chars().count() == 1 {
                Case::UpperCamel
            } else {
                Case::Upper
            }
        } else if has_upper {
            Case::LowerCamel
        } else {
            Case::Lower
        }
    }
}
