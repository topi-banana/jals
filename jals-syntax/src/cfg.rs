//! jals attribute (`#[cfg(...)]`) evaluation shared by compilation and analysis.
//!
//! [`CfgMap::compute`] walks a parsed file once and classifies every attribute host (an import, a
//! declaration, or a statement) against a resolved build-feature set: an *enabled* host
//! contributes its attributes' spans (the text the compile frontend must strip), a *disabled* host
//! (a `cfg` predicate evaluated false) contributes its whole significant span, and everything
//! structurally wrong — an unknown attribute, a malformed predicate, an unsupported position, a
//! late attribute under the strict-leading rule, a stray `#` — becomes a [`CfgError`] carrying the
//! offending span.
//!
//! The map is consumed on both sides of the pipeline. The compile frontend (`jals-frontend`)
//! turns it into a length-preserving blanking plan; the analysis side (`jals-hir`, `jals-lint`,
//! the editor) skips disabled ranges during its own tree walks, so a `cfg`-false declaration is
//! neither indexed nor resolved nor linted — and reports the same structural errors as edit-time
//! diagnostics. Content *inside* a disabled host is neither validated nor evaluated (Rust
//! parity): the walk simply never descends into one.
//!
//! An unknown feature *name* is simply false (features are additive — Cargo/Rust semantics); an
//! unknown predicate *shape* is an error.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use text_size::TextRange;

use crate::ast::{AstNode, AttrArg, Attribute, Literal};
use crate::language::{SyntaxElement, SyntaxNode};
use crate::parser::Parse;
use crate::syntax_kind::SyntaxKind;

/// One `cfg`-disabled attribute host: its significant span and the host node itself (the compile
/// frontend inspects the node to decide whether stripping must leave a `;` behind).
#[derive(Debug, Clone)]
pub struct DisabledHost {
    /// The host's significant span: first through last non-trivia token, leading attributes
    /// included, the leading trivia rowan parks inside the node excluded.
    pub range: TextRange,
    /// The disabled host node.
    pub host: SyntaxNode,
}

/// A structural attribute error, located by span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgError {
    /// The span of the offending attribute (or `#` token, or disabled host).
    pub range: TextRange,
    pub kind: CfgErrorKind,
}

/// What is structurally wrong with an attribute.
///
/// Rendered per host: the compile frontend names the line
/// ([`render_with_line`](CfgErrorKind::render_with_line), since a rejected lowering means
/// nothing downstream restates the problem), the editor keeps the span and uses the line-free
/// [`message`](CfgErrorKind::message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgErrorKind {
    /// An attribute other than `cfg` (the only supported name).
    UnknownAttribute(String),
    /// A `cfg` whose shape is not `#[cfg(<predicate>)]` with a supported predicate.
    MalformedCfg,
    /// A `feature = …` value that is not a plain, escape-free string literal.
    NonStringFeature,
    /// An attribute on a construct that cannot host one (a parameter, an enum constant, a
    /// mandatory `{…}` body, recovery debris).
    UnsupportedPosition,
    /// An attribute after a modifier or annotation (the strict-leading rule).
    LateAttribute,
    /// An attribute overlapping a parse error; never evaluated.
    AttrOverlapsError,
    /// A false `cfg` on an item that contains parse errors; never disabled (error recovery can
    /// mis-extend a node, so the span is not trusted).
    DisabledHasErrors,
    /// A `#` token that does not begin an attribute (`#[...]`) — error recovery shredded the
    /// input before an `ATTRIBUTE` node could form.
    StrayHash,
}

impl CfgErrorKind {
    /// The line-free message, for span-carrying diagnostics (the editor, the LSP).
    pub fn message(&self) -> String {
        match self {
            Self::UnknownAttribute(name) => {
                format!("unknown attribute `{name}`; only `cfg` is supported")
            }
            Self::MalformedCfg => "malformed `cfg` attribute; expected `#[cfg(<predicate>)]` \
                 with `feature = \"…\"`, `all(…)`, `any(…)`, or `not(…)`"
                .to_owned(),
            Self::NonStringFeature => {
                "the `cfg` feature name must be a plain string literal".to_owned()
            }
            Self::UnsupportedPosition => {
                "the attribute is not supported on this construct".to_owned()
            }
            Self::LateAttribute => {
                "the attribute must come before modifiers and annotations".to_owned()
            }
            Self::AttrOverlapsError => {
                "the attribute overlaps a syntax error; not desugared".to_owned()
            }
            Self::DisabledHasErrors => {
                "cannot disable this item: it contains syntax errors".to_owned()
            }
            Self::StrayHash => "the `#` does not begin an attribute (`#[...]`)".to_owned(),
        }
    }

    /// The message naming the 1-based `line`, exactly as the compile frontend reports it.
    pub fn render_with_line(&self, line: usize) -> String {
        match self {
            Self::UnknownAttribute(name) => {
                format!("unknown attribute `{name}` on line {line}; only `cfg` is supported")
            }
            Self::MalformedCfg => format!(
                "malformed `cfg` attribute on line {line}; expected `#[cfg(<predicate>)]` with \
                 `feature = \"…\"`, `all(…)`, `any(…)`, or `not(…)`"
            ),
            Self::NonStringFeature => {
                format!("the `cfg` feature name on line {line} must be a plain string literal")
            }
            Self::UnsupportedPosition => {
                format!("the attribute on line {line} is not supported on this construct")
            }
            Self::LateAttribute => {
                format!("the attribute on line {line} must come before modifiers and annotations")
            }
            Self::AttrOverlapsError => {
                format!("the attribute on line {line} overlaps a syntax error; not desugared")
            }
            Self::DisabledHasErrors => {
                format!("cannot disable the item on line {line}: it contains syntax errors")
            }
            Self::StrayHash => {
                format!("the `#` on line {line} does not begin an attribute (`#[...]`)")
            }
        }
    }
}

/// The `cfg` evaluation of one parsed file against one build-feature set.
#[derive(Debug, Clone, Default)]
pub struct CfgMap {
    /// `cfg`-disabled hosts, mutually disjoint, in source order.
    disabled: Vec<DisabledHost>,
    /// The significant spans of every *enabled* host's attributes (what the compile frontend
    /// blanks so `javac` never sees a `#[`), in walk order.
    attr_spans: Vec<TextRange>,
    /// Structural errors, in source order (stray-`#` errors last).
    errors: Vec<CfgError>,
}

/// Whether the attributes on one host left it enabled or disabled.
#[derive(PartialEq, Eq)]
enum Host {
    Enabled,
    Disabled,
}

impl CfgMap {
    /// Nesting depth cap for `cfg` predicate evaluation. The parser heap-allocates its recursion,
    /// so arbitrarily deep predicates parse fine; the sync evaluator caps instead of risking the
    /// stack.
    const MAX_PREDICATE_DEPTH: usize = 64;

    /// Whether `range` lies inside a disabled host's span.
    pub fn is_disabled(&self, range: TextRange) -> bool {
        self.disabled
            .iter()
            .any(|host| host.range.contains_range(range))
    }

    /// [`is_disabled`](Self::is_disabled) over plain byte offsets, for consumers that carry
    /// `usize` ranges. An offset beyond `u32` cannot come from a parsed file, so it is never
    /// disabled.
    pub fn is_disabled_span(&self, start: usize, end: usize) -> bool {
        let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(end)) else {
            return false;
        };
        start <= end && self.is_disabled(TextRange::new(start.into(), end.into()))
    }

    /// The disabled hosts' spans, in source order.
    pub fn disabled_ranges(&self) -> impl Iterator<Item = TextRange> + '_ {
        self.disabled.iter().map(|host| host.range)
    }

    /// Whether `node` is itself one of the disabled hosts. Matched structurally — by kind and
    /// full node range — so a map computed over an identical reparse of the same text still
    /// applies. Tree walks (the resolver, the project-index extraction) use this to skip a
    /// disabled host; by not descending they skip its whole subtree, which is what keeps content
    /// inside a disabled host unanalyzed (Rust parity).
    pub fn disables_node(&self, node: &SyntaxNode) -> bool {
        self.disabled
            .iter()
            .any(|h| h.host.kind() == node.kind() && h.host.text_range() == node.text_range())
    }

    /// The disabled hosts, in source order.
    pub fn disabled_hosts(&self) -> &[DisabledHost] {
        &self.disabled
    }

    /// The enabled hosts' attribute spans, in walk order.
    pub fn attr_spans(&self) -> &[TextRange] {
        &self.attr_spans
    }

    /// The structural errors, in source order (stray-`#` errors last).
    pub fn errors(&self) -> &[CfgError] {
        &self.errors
    }

    /// Whether the file has no attribute at all (nothing disabled, nothing to strip, no error) —
    /// consumers can skip their filtering entirely.
    pub const fn is_empty(&self) -> bool {
        self.disabled.is_empty() && self.attr_spans.is_empty() && self.errors.is_empty()
    }

    /// Compute the `cfg` map of one parsed file against the resolved build `features`.
    ///
    /// Sync by design: one full-tree walk, no allocation proportional to anything but the
    /// attribute count. (Callers on a cooperative runtime compute it once per parse and cache
    /// it; thread a `Yielder` through here only if profiling ever shows the walk mattering.)
    pub fn compute(parse: &Parse, features: &BTreeSet<String>) -> Self {
        let mut out = Self::default();
        // Start offsets of every attribute consumed by host processing; an `ATTRIBUTE` node
        // reached outside this set has no supported host (dangling recovery debris, a parameter,
        // a detached mandatory body, a late attribute) and is diagnosed individually.
        let mut handled = BTreeSet::new();

        // Depth-first in source order, with an explicit stack so a disabled host's subtree is
        // skipped by simply not pushing its children.
        let mut stack = alloc::vec![parse.syntax()];
        while let Some(node) = stack.pop() {
            if node.kind() == SyntaxKind::ATTRIBUTE {
                // Attributes never nest, so there is nothing to descend into.
                if !handled.contains(&node.text_range().start()) {
                    // Not part of any supported host's leading run. A later position inside a
                    // MODIFIERS run is the strict-leading violation; everything else (a
                    // parameter, a record component, a for-init, dangling debris) is an
                    // unsupported position.
                    let range = Self::node_span(&node).unwrap_or_else(|| node.text_range());
                    let late = node
                        .parent()
                        .is_some_and(|p| p.kind() == SyntaxKind::MODIFIERS)
                        && Self::has_preceding_significant(&node);
                    out.errors.push(CfgError {
                        range,
                        kind: if late {
                            CfgErrorKind::LateAttribute
                        } else {
                            CfgErrorKind::UnsupportedPosition
                        },
                    });
                }
                continue;
            }
            if Self::is_attribute_host(&node) {
                let attrs = Self::leading_attributes(&node);
                if !attrs.is_empty() {
                    for attr in &attrs {
                        handled.insert(attr.syntax().text_range().start());
                    }
                    if out.evaluate_host(parse, &node, &attrs, features) == Host::Disabled {
                        continue;
                    }
                }
            }
            // Push in reverse so children pop in source order (stable error order).
            let base = stack.len();
            stack.extend(node.children());
            stack[base..].reverse();
        }
        // `javac` must never see a `#`: one that error recovery left outside any ATTRIBUTE node —
        // an unsupported position the parser could not even shape (a for-init, an expression) —
        // fails the file too, unless a disabled host's blanking already covers it.
        for token in parse
            .syntax()
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::HASH)
        {
            let offset = token.text_range().start();
            if out.disabled.iter().any(|host| host.range.contains(offset)) {
                continue;
            }
            if token
                .parent_ancestors()
                .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
            {
                continue;
            }
            out.errors.push(CfgError {
                range: token.text_range(),
                kind: CfgErrorKind::StrayHash,
            });
        }
        out
    }

    /// Whether a significant, non-attribute sibling precedes `node` under its parent.
    fn has_preceding_significant(node: &SyntaxNode) -> bool {
        let mut prev = node.prev_sibling_or_token();
        while let Some(element) = prev {
            if !element.kind().is_trivia() && element.kind() != SyntaxKind::ATTRIBUTE {
                return true;
            }
            prev = match element {
                SyntaxElement::Node(n) => n.prev_sibling_or_token(),
                SyntaxElement::Token(t) => t.prev_sibling_or_token(),
            };
        }
        false
    }

    /// Validate and evaluate one host's leading attributes, extending the map with either the
    /// attribute spans (enabled) or the whole-host span (disabled). Every attribute on the host
    /// is validated even when an earlier one already disabled it, so a typo next to a false `cfg`
    /// is still reported.
    fn evaluate_host(
        &mut self,
        parse: &Parse,
        host: &SyntaxNode,
        attrs: &[Attribute],
        features: &BTreeSet<String>,
    ) -> Host {
        let before = self.errors.len();
        let mut enabled = true;
        for attr in attrs {
            let Some(range) = Self::node_span(attr.syntax()) else {
                // An ATTRIBUTE node always holds at least its `#`; defensive only.
                self.errors.push(CfgError {
                    range: attr.syntax().text_range(),
                    kind: CfgErrorKind::MalformedCfg,
                });
                continue;
            };
            if Self::overlaps_error(parse, range) {
                self.errors.push(CfgError {
                    range,
                    kind: CfgErrorKind::AttrOverlapsError,
                });
                continue;
            }
            match Self::eval_attribute(attr, features) {
                Ok(value) => enabled &= value,
                Err(kind) => self.errors.push(CfgError { range, kind }),
            }
        }
        if self.errors.len() > before {
            // The host's spans are only planned from a fully valid attribute list.
            return Host::Enabled;
        }
        if enabled {
            for attr in attrs {
                if let Some(range) = Self::node_span(attr.syntax()) {
                    self.attr_spans.push(range);
                }
            }
            return Host::Enabled;
        }
        let Some(range) = Self::node_span(host) else {
            return Host::Enabled;
        };
        // Error recovery can mis-extend a node, so a disabled host overlapping a parse error is
        // never best-effort stripped.
        if Self::overlaps_error(parse, range) {
            self.errors.push(CfgError {
                range,
                kind: CfgErrorKind::DisabledHasErrors,
            });
            return Host::Enabled;
        }
        self.disabled.push(DisabledHost {
            range,
            host: host.clone(),
        });
        Host::Disabled
    }

    /// Whether `node` may govern attributes. Imports, declarations reached through `modifiers()`,
    /// and statements (whose leading attributes are direct children). `BLOCK` and `INITIALIZER`
    /// are position-checked ([`block_is_host`](Self::block_is_host),
    /// [`is_detached_body`](Self::is_detached_body)): a mandatory `{…}` body — a
    /// method/constructor/initializer body, a `synchronized`/`try`/`catch`/`finally` body — must
    /// not be `cfg`-removed (no `;` can stand in for it), and the parser reads such a body only
    /// when `{` immediately follows, so an interposed attribute *detaches* the block into
    /// statement/member position where only this check can reject it. Everything else —
    /// parameters, record components, catch/resource/for-header modifiers, enum constants — is
    /// rejected by the dangling-attribute check in [`compute`](Self::compute).
    fn is_attribute_host(node: &SyntaxNode) -> bool {
        use SyntaxKind as S;
        match node.kind() {
            S::BLOCK => Self::block_is_host(node),
            S::INITIALIZER => !Self::is_detached_body(node),
            S::IMPORT_DECL
            | S::CLASS_DECL
            | S::INTERFACE_DECL
            | S::ENUM_DECL
            | S::RECORD_DECL
            | S::ANNOTATION_TYPE_DECL
            | S::FIELD_DECL
            | S::METHOD_DECL
            | S::CONSTRUCTOR_DECL
            | S::LOCAL_VAR_DECL
            | S::EXPR_STMT
            | S::RETURN_STMT
            | S::IF_STMT
            | S::WHILE_STMT
            | S::DO_WHILE_STMT
            | S::FOR_STMT
            | S::FOR_EACH_STMT
            | S::BREAK_STMT
            | S::CONTINUE_STMT
            | S::THROW_STMT
            | S::YIELD_STMT
            | S::ASSERT_STMT
            | S::SYNCHRONIZED_STMT
            | S::TRY_STMT
            | S::SWITCH_STMT
            | S::LABELED_STMT
            | S::EMPTY_STMT => true,
            _ => false,
        }
    }

    /// Whether a `BLOCK` carrying attributes is a legal host: a free-standing statement (and not
    /// a detached mandatory body that reparsed there), or the body slot of a control structure
    /// where a `;` can stand in for the removed block. Fail-safe: any other parent — a
    /// method/constructor/initializer body, a `synchronized`/`try`/`catch`/`finally` body, a
    /// switch-rule body, recovery debris — is rejected.
    fn block_is_host(block: &SyntaxNode) -> bool {
        use SyntaxKind as S;
        match block.parent().map(|p| p.kind()) {
            // Statement position (a block statement in a block or a colon-style switch group).
            Some(S::BLOCK | S::SWITCH_GROUP) => !Self::is_detached_body(block),
            // A `;`-rescuable body slot (the frontend writes `;` over the first blanked byte).
            Some(
                S::IF_STMT
                | S::WHILE_STMT
                | S::DO_WHILE_STMT
                | S::FOR_STMT
                | S::FOR_EACH_STMT
                | S::LABELED_STMT,
            ) => true,
            _ => false,
        }
    }

    /// Whether `node` (a `BLOCK` in statement position or an `INITIALIZER`) is really the
    /// detached mandatory body of the construct just before it. The parser reads a mandatory
    /// body only when `{` immediately follows, so `synchronized (x) #[cfg(…)] {…}` produces a
    /// body-less `SYNCHRONIZED_STMT` followed by a free-standing attributed block — legal-looking
    /// in the tree, but `cfg`-removing it would leave invalid Java with no diagnostic.
    fn is_detached_body(node: &SyntaxNode) -> bool {
        node.prev_sibling()
            .is_some_and(|prev| Self::lacks_required_body(&prev))
    }

    /// Whether `node` is a body-requiring construct missing a mandatory `{…}`.
    fn lacks_required_body(node: &SyntaxNode) -> bool {
        use SyntaxKind as S;
        match node.kind() {
            S::SYNCHRONIZED_STMT => !Self::has_block_child(node),
            // The try's own block, or any catch/finally clause's block.
            S::TRY_STMT => {
                !Self::has_block_child(node)
                    || node.children().any(|clause| {
                        matches!(clause.kind(), S::CATCH_CLAUSE | S::FINALLY_CLAUSE)
                            && !Self::has_block_child(&clause)
                    })
            }
            // A method/constructor missing both its body and its `;` — an abstract/interface
            // method legitimately has no body but does end with `;`.
            S::METHOD_DECL | S::CONSTRUCTOR_DECL => {
                !Self::has_block_child(node)
                    && !node
                        .children_with_tokens()
                        .filter_map(SyntaxElement::into_token)
                        .any(|token| token.kind() == S::SEMICOLON)
            }
            _ => false,
        }
    }

    /// Whether `node` has a direct `BLOCK` child.
    fn has_block_child(node: &SyntaxNode) -> bool {
        node.children()
            .any(|child| child.kind() == SyntaxKind::BLOCK)
    }

    /// The host's leading attribute run: direct `ATTRIBUTE` children before any other significant
    /// element, plus the same leading run inside a direct `MODIFIERS` child (where `modifiers()`
    /// parses them for declarations). Attributes *after* another significant element — recovery
    /// debris dangling in a block, or a late attribute inside `MODIFIERS` — are not part of any
    /// run and are diagnosed individually by [`compute`](Self::compute).
    fn leading_attributes(host: &SyntaxNode) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        for element in host.children_with_tokens() {
            if element.kind().is_trivia() {
                continue;
            }
            if element.kind() == SyntaxKind::ATTRIBUTE {
                if let Some(attr) = element.as_node().cloned().and_then(Attribute::cast) {
                    attrs.push(attr);
                }
                continue;
            }
            // A declaration's attributes live at the front of its (first-child) MODIFIERS node.
            if element.kind() == SyntaxKind::MODIFIERS
                && let Some(modifiers) = element.as_node()
            {
                for child in modifiers.children_with_tokens() {
                    if child.kind().is_trivia() {
                        continue;
                    }
                    let Some(attr) = child
                        .as_node()
                        .filter(|n| n.kind() == SyntaxKind::ATTRIBUTE)
                        .cloned()
                        .and_then(Attribute::cast)
                    else {
                        break;
                    };
                    attrs.push(attr);
                }
            }
            break;
        }
        attrs
    }

    /// The significant span of a node: its first through last non-trivia token, excluding the
    /// leading trivia rowan parks inside the node (the previous line's newline) — which is
    /// exactly what keeps the compile frontend's blanking line-stable.
    fn node_span(node: &SyntaxNode) -> Option<TextRange> {
        let mut significant = node
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| !token.kind().is_trivia());
        let first = significant.next()?;
        let last = significant.last().unwrap_or_else(|| first.clone());
        Some(TextRange::new(
            first.text_range().start(),
            last.text_range().end(),
        ))
    }

    /// Whether any parse error lies strictly inside `range` (half-open overlap).
    fn overlaps_error(parse: &Parse, range: TextRange) -> bool {
        parse
            .errors()
            .iter()
            .any(|error| error.range().start() < range.end() && range.start() < error.range().end())
    }

    /// Validate one attribute and evaluate its `cfg` predicate against the enabled feature set.
    fn eval_attribute(attr: &Attribute, features: &BTreeSet<String>) -> Result<bool, CfgErrorKind> {
        let Some(meta) = attr.meta() else {
            return Err(CfgErrorKind::MalformedCfg);
        };
        let Some(name) = meta.name_text() else {
            return Err(CfgErrorKind::MalformedCfg);
        };
        if name != "cfg" {
            return Err(CfgErrorKind::UnknownAttribute(name));
        }
        // `#[cfg]` and `#[cfg = "x"]` carry no predicate; `#[cfg(p)]` carries exactly one.
        if meta.value().is_some() {
            return Err(CfgErrorKind::MalformedCfg);
        }
        let Some(args) = meta.args() else {
            return Err(CfgErrorKind::MalformedCfg);
        };
        let mut predicates = args.args();
        let (Some(predicate), None) = (predicates.next(), predicates.next()) else {
            return Err(CfgErrorKind::MalformedCfg);
        };
        Self::eval_predicate(&predicate, features, 0)
    }

    /// Evaluate one `cfg` predicate: `feature = "name"`, `all(…)` (empty: true), `any(…)`
    /// (empty: false), or `not(p)`. An unknown feature *name* is simply false (features are
    /// additive — Cargo/Rust semantics); an unknown predicate *shape* is an error.
    fn eval_predicate(
        predicate: &AttrArg,
        features: &BTreeSet<String>,
        depth: usize,
    ) -> Result<bool, CfgErrorKind> {
        if depth > Self::MAX_PREDICATE_DEPTH {
            return Err(CfgErrorKind::MalformedCfg);
        }
        let AttrArg::AttrMeta(meta) = predicate else {
            return Err(CfgErrorKind::MalformedCfg);
        };
        let Some(name) = meta.name_text() else {
            return Err(CfgErrorKind::MalformedCfg);
        };
        match name.as_str() {
            "feature" => {
                if meta.args().is_some() {
                    return Err(CfgErrorKind::MalformedCfg);
                }
                let Some(value) = meta.value() else {
                    return Err(CfgErrorKind::NonStringFeature);
                };
                Ok(features.contains(&Self::feature_name(&value)?))
            }
            "all" | "any" | "not" => {
                if meta.value().is_some() {
                    return Err(CfgErrorKind::MalformedCfg);
                }
                let Some(args) = meta.args() else {
                    // Bare `all` / `any` / `not` without a parenthesized list.
                    return Err(CfgErrorKind::MalformedCfg);
                };
                let mut nested = args.args();
                match name.as_str() {
                    "all" => nested.try_fold(true, |acc, p| {
                        Ok(acc & Self::eval_predicate(&p, features, depth + 1)?)
                    }),
                    "any" => nested.try_fold(false, |acc, p| {
                        Ok(acc | Self::eval_predicate(&p, features, depth + 1)?)
                    }),
                    _ => {
                        let (Some(inner), None) = (nested.next(), nested.next()) else {
                            return Err(CfgErrorKind::MalformedCfg);
                        };
                        Ok(!Self::eval_predicate(&inner, features, depth + 1)?)
                    }
                }
            }
            _ => Err(CfgErrorKind::MalformedCfg),
        }
    }

    /// Decode a `feature = "…"` value: a plain (escape-free) string literal.
    fn feature_name(value: &Literal) -> Result<String, CfgErrorKind> {
        let Some(token) = value.token() else {
            return Err(CfgErrorKind::NonStringFeature);
        };
        if token.kind() != SyntaxKind::STRING_LITERAL {
            return Err(CfgErrorKind::NonStringFeature);
        }
        let text = token.text();
        let inner = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or(CfgErrorKind::NonStringFeature)?;
        // Feature names are plain identifiers; an escape (or an embedded quote, impossible
        // without one) would need interpretation this pass deliberately does not do.
        if inner.contains('\\') || inner.contains('"') {
            return Err(CfgErrorKind::NonStringFeature);
        }
        Ok(inner.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;
    use alloc::string::String;
    use alloc::vec::Vec;

    use text_size::TextRange;

    use super::{CfgErrorKind, CfgMap};
    use crate::parser::Parse;

    fn compute(src: &str, features: &[&str]) -> CfgMap {
        let parse = jals_exec::block_on_inline(Parse::parse(src));
        let features: BTreeSet<String> = features.iter().map(|f| (*f).to_owned()).collect();
        CfgMap::compute(&parse, &features)
    }

    fn kinds(map: &CfgMap) -> Vec<CfgErrorKind> {
        map.errors().iter().map(|e| e.kind.clone()).collect()
    }

    #[test]
    fn enabled_attribute_contributes_its_span_only() {
        let src = "#[cfg(feature = \"x\")]\nclass C {}";
        let map = compute(src, &["x"]);
        assert!(map.errors().is_empty());
        assert!(map.disabled_hosts().is_empty());
        assert_eq!(map.attr_spans().len(), 1);
        let span = map.attr_spans()[0];
        assert_eq!(
            &src[span.start().into()..span.end().into()],
            "#[cfg(feature = \"x\")]"
        );
        assert!(!map.is_empty());
    }

    #[test]
    fn false_cfg_disables_the_whole_host() {
        let src = "class C {\n    #[cfg(feature = \"x\")]\n    void gone() { f(); }\n    void kept() {}\n}";
        let map = compute(src, &[]);
        assert!(map.errors().is_empty());
        assert_eq!(map.disabled_hosts().len(), 1);
        let range = map.disabled_hosts()[0].range;
        assert!(
            src[range.start().into()..range.end().into()].starts_with("#[cfg"),
            "the disabled span starts at the attribute"
        );
        assert!(
            src[range.start().into()..range.end().into()].ends_with('}'),
            "the disabled span covers the method body"
        );
        // A range inside the disabled method is disabled; the survivor is not.
        assert!(map.is_disabled(range));
        let kept = u32::try_from(src.find("kept").unwrap()).unwrap();
        assert!(!map.is_disabled(TextRange::empty(kept.into())));
    }

    #[test]
    fn attributes_inside_a_disabled_host_are_not_validated() {
        // The bogus inner attribute would be an error, but the walk never descends into a
        // disabled host (Rust parity).
        let map = compute(
            "class C { #[cfg(feature = \"y\")] void m() { #[bogus] f(); } }",
            &[],
        );
        assert!(map.errors().is_empty());
        assert_eq!(map.disabled_hosts().len(), 1);
    }

    #[test]
    fn predicate_combinators_evaluate_like_rust_cfg() {
        let on = |src: &str, features: &[&str]| {
            let map = compute(src, features);
            assert!(
                map.errors().is_empty(),
                "unexpected errors: {:?}",
                map.errors()
            );
            map.disabled_hosts().is_empty()
        };
        assert!(on(
            "#[cfg(all(feature = \"a\", feature = \"b\"))] class C {}",
            &["a", "b"]
        ));
        assert!(!on(
            "#[cfg(all(feature = \"a\", feature = \"b\"))] class C {}",
            &["a"]
        ));
        assert!(on(
            "#[cfg(any(feature = \"a\", feature = \"b\"))] class C {}",
            &["b"]
        ));
        assert!(!on(
            "#[cfg(any(feature = \"a\", feature = \"b\"))] class C {}",
            &[]
        ));
        assert!(on("#[cfg(not(feature = \"a\"))] class C {}", &[]));
        assert!(!on("#[cfg(not(feature = \"a\"))] class C {}", &["a"]));
        assert!(on("#[cfg(all())] class C {}", &[]));
        assert!(!on("#[cfg(any())] class C {}", &[]));
        // An unknown feature name is simply false.
        assert!(!on("#[cfg(feature = \"nope\")] class C {}", &["x"]));
    }

    #[test]
    fn predicate_depth_is_capped() {
        let mut src = String::from("#[cfg(");
        for _ in 0..80 {
            src.push_str("not(");
        }
        src.push_str("feature = \"x\"");
        for _ in 0..80 {
            src.push(')');
        }
        src.push_str(")] class C {}");
        let map = compute(&src, &[]);
        assert_eq!(kinds(&map), [CfgErrorKind::MalformedCfg]);
    }

    #[test]
    fn structural_errors_carry_their_kind_and_span() {
        let map = compute("#[derive(Debug)] class C {}", &[]);
        assert_eq!(
            kinds(&map),
            [CfgErrorKind::UnknownAttribute("derive".to_owned())]
        );

        let map = compute("#[cfg] class C {}", &[]);
        assert_eq!(kinds(&map), [CfgErrorKind::MalformedCfg]);

        let map = compute("#[cfg(feature = 3)] class C {}", &[]);
        assert_eq!(kinds(&map), [CfgErrorKind::NonStringFeature]);

        // A parameter is not a host: the attribute dangles.
        let map = compute("class C { void m(#[cfg(feature = \"x\")] int a) {} }", &[]);
        assert_eq!(kinds(&map), [CfgErrorKind::UnsupportedPosition]);

        // Strict-leading: an attribute after a modifier.
        let map = compute(
            "class C { public #[cfg(feature = \"x\")] void m() {} }",
            &[],
        );
        assert_eq!(kinds(&map), [CfgErrorKind::LateAttribute]);

        // A `#` error recovery shredded before any ATTRIBUTE node could form.
        let map = compute("class C { void m() { int a = 1 # 2; } }", &[]);
        assert_eq!(kinds(&map), [CfgErrorKind::StrayHash]);
    }

    #[test]
    fn mandatory_body_blocks_are_not_hosts() {
        // Each shape detaches the `{…}` from its construct (the parser reads a mandatory body
        // only when `{` immediately follows), so blanking it would leave invalid Java with no
        // diagnostic — the classifier must reject every one of them.
        for src in [
            "class C { void m() { synchronized (this) #[cfg(feature = \"y\")] { f(); } } }",
            "class C { void m() { try { f(); } finally #[cfg(feature = \"y\")] { g(); } } }",
            "class C { void m() { try { f(); } catch (Exception e) #[cfg(feature = \"y\")] { g(); } } }",
            "class C { void m() #[cfg(feature = \"y\")] { } }",
            "class C { C() #[cfg(feature = \"y\")] { } }",
        ] {
            let map = compute(src, &[]);
            assert!(
                map.disabled_hosts().is_empty(),
                "must not disable a mandatory body: {src}"
            );
            assert!(
                kinds(&map).contains(&CfgErrorKind::UnsupportedPosition),
                "expected an unsupported-position error for {src}, got {:?}",
                map.errors()
            );
        }
        // `try #[cfg] { … } catch …` parses with recovery errors; whatever shape recovery
        // produced, the attribute must not silently disable anything.
        let map = compute(
            "class C { void m() { try #[cfg(feature = \"y\")] { f(); } catch (Exception e) {} } }",
            &[],
        );
        assert!(map.disabled_hosts().is_empty());
        assert!(!map.errors().is_empty());
    }

    #[test]
    fn legitimate_block_and_initializer_hosts_still_work() {
        // A free-standing block statement.
        let map = compute(
            "class C { void m() { #[cfg(feature = \"y\")] { f(); } } }",
            &[],
        );
        assert!(map.errors().is_empty());
        assert_eq!(map.disabled_hosts().len(), 1);

        // A control-structure body slot (`;`-rescuable).
        let map = compute(
            "class C { void m() { if (a()) #[cfg(feature = \"y\")] { f(); } } }",
            &[],
        );
        assert!(map.errors().is_empty());
        assert_eq!(map.disabled_hosts().len(), 1);

        // A class initializer, including one after a legitimate body-less abstract method.
        let map = compute("class C { #[cfg(feature = \"y\")] { } }", &[]);
        assert!(map.errors().is_empty());
        assert_eq!(map.disabled_hosts().len(), 1);
        let map = compute(
            "abstract class C { abstract void m(); #[cfg(feature = \"y\")] { } }",
            &[],
        );
        assert!(map.errors().is_empty(), "unexpected: {:?}", map.errors());
        assert_eq!(map.disabled_hosts().len(), 1);

        // A free-standing block after a *complete* synchronized statement.
        let map = compute(
            "class C { void m() { synchronized (this) { } #[cfg(feature = \"y\")] { } } }",
            &[],
        );
        assert!(map.errors().is_empty());
        assert_eq!(map.disabled_hosts().len(), 1);
    }

    #[test]
    fn messages_render_with_and_without_line() {
        assert_eq!(
            CfgErrorKind::StrayHash.render_with_line(3),
            "the `#` on line 3 does not begin an attribute (`#[...]`)"
        );
        assert_eq!(
            CfgErrorKind::StrayHash.message(),
            "the `#` does not begin an attribute (`#[...]`)"
        );
        assert_eq!(
            CfgErrorKind::UnknownAttribute("derive".to_owned()).render_with_line(1),
            "unknown attribute `derive` on line 1; only `cfg` is supported"
        );
        assert_eq!(
            CfgErrorKind::MalformedCfg.render_with_line(2),
            "malformed `cfg` attribute on line 2; expected `#[cfg(<predicate>)]` with \
             `feature = \"…\"`, `all(…)`, `any(…)`, or `not(…)`"
        );
    }

    #[test]
    fn empty_map_for_attribute_free_source() {
        let map = compute("class C { void m() { f(); } }", &["x"]);
        assert!(map.is_empty());
    }
}
