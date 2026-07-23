//! jals attribute stripping and `cfg` conditional compilation.
//!
//! [`plan`] walks a parsed file and decides, per attribute host (an import, a declaration, or a
//! statement), what the dialect rewrite must do: blank the attribute text itself (an enabled
//! host — `javac` must never see `#[`), or blank the host's whole span (a `cfg` predicate
//! evaluated false against the resolved build features). Blanking is length-preserving, so every
//! other byte offset in the file — and every line number — stays exactly where the author put it.
//!
//! Structural errors (an unknown attribute, a malformed predicate, an unsupported position, a
//! late attribute under the strict-leading rule, a parse error overlapping a span this pass would
//! rewrite) are collected as messages; the caller emits them as error diagnostics and publishes
//! nothing. Content *inside* a disabled host is neither validated nor evaluated (Rust parity).

use alloc::borrow::ToOwned;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_syntax::ast::{AstNode, AttrArg, Attribute, Literal};
use jals_syntax::{Parse, SyntaxKind, SyntaxNode};

/// A byte span to blank in place (length-preserving).
pub(crate) struct Blank {
    pub start: usize,
    pub end: usize,
    /// Write a `;` as the first blanked byte: a stripped *statement* must stay a statement so a
    /// sole control-structure body (`if (c) #[cfg(x)] f();`) remains valid Java.
    pub semicolon: bool,
}

/// What the attribute pass contributes to the rewrite of one file.
#[derive(Default)]
pub(crate) struct AttrPlan {
    /// Spans to blank in place, mutually disjoint.
    pub blanks: Vec<Blank>,
    /// Host spans removed by a false `cfg`. Rewrites planned by other passes (a grouped-import
    /// expansion) inside one of these ranges must be dropped.
    pub disabled: Vec<(usize, usize)>,
    /// Structural errors; any entry makes the file fail instead of rewriting.
    pub errors: Vec<String>,
}

impl AttrPlan {
    /// Whether `span` lies inside a disabled range.
    pub(crate) fn disables(&self, span: (usize, usize)) -> bool {
        self.disabled
            .iter()
            .any(|&(start, end)| start <= span.0 && span.1 <= end)
    }
}

/// Nesting depth cap for `cfg` predicate evaluation. The parser heap-allocates its recursion, so
/// arbitrarily deep predicates parse fine; the sync evaluator caps instead of risking the stack.
const MAX_PREDICATE_DEPTH: usize = 64;

fn malformed_cfg(line: usize) -> String {
    format!(
        "malformed `cfg` attribute on line {line}; expected `#[cfg(<predicate>)]` with \
         `feature = \"…\"`, `all(…)`, `any(…)`, or `not(…)`"
    )
}

fn non_string_feature(line: usize) -> String {
    format!("the `cfg` feature name on line {line} must be a plain string literal")
}

fn unsupported_position(line: usize) -> String {
    format!("the attribute on line {line} is not supported on this construct")
}

fn late_attribute(line: usize) -> String {
    format!("the attribute on line {line} must come before modifiers and annotations")
}

fn attr_overlaps_error(line: usize) -> String {
    format!("the attribute on line {line} overlaps a syntax error; not desugared")
}

fn disabled_has_errors(line: usize) -> String {
    format!("cannot disable the item on line {line}: it contains syntax errors")
}

fn stray_hash(line: usize) -> String {
    format!("the `#` on line {line} does not begin an attribute (`#[...]`)")
}

/// The 1-based line `offset` falls on. `offset` comes from a token range, so it is always a
/// char boundary; `get` rather than an index keeps a defect from becoming a panic in the
/// compile path.
pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(1, |prefix| 1 + prefix.matches('\n').count())
}

/// Compute the attribute rewrite plan for one parsed file. `text` is the parsed source, used
/// to name the offending line in error messages — the lowering they belong to is rejected, so
/// no compiler downstream will restate them and each must locate its construct on its own.
pub(crate) fn plan(parse: &Parse, text: &str, features: &BTreeSet<String>) -> AttrPlan {
    let mut out = AttrPlan::default();
    // Start offsets of every attribute consumed by host processing; an `ATTRIBUTE` node reached
    // outside this set has no supported host (dangling recovery debris, a parameter, a for-init,
    // a late attribute) and is diagnosed individually.
    let mut handled = BTreeSet::new();

    // Depth-first in source order, with an explicit stack so a disabled host's subtree is
    // skipped by simply not pushing its children.
    let mut stack = alloc::vec![parse.syntax()];
    while let Some(node) = stack.pop() {
        if node.kind() == SyntaxKind::ATTRIBUTE {
            // Attributes never nest, so there is nothing to descend into.
            if !handled.contains(&usize::from(node.text_range().start())) {
                // Not part of any supported host's leading run. A later position inside a
                // MODIFIERS run is the strict-leading violation; everything else (a parameter,
                // a record component, a for-init, dangling debris) is an unsupported position.
                let line = node_span(&node).map_or(1, |(start, _)| line_of(text, start));
                let late = node
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::MODIFIERS)
                    && has_preceding_significant(&node);
                out.errors.push(if late {
                    late_attribute(line)
                } else {
                    unsupported_position(line)
                });
            }
            continue;
        }
        if is_attribute_host(node.kind()) {
            let attrs = leading_attributes(&node);
            if !attrs.is_empty() {
                for attr in &attrs {
                    handled.insert(usize::from(attr.syntax().text_range().start()));
                }
                if host_plan(parse, text, &node, &attrs, features, &mut out) == Host::Disabled {
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
        .filter_map(jals_syntax::SyntaxElement::into_token)
        .filter(|token| token.kind() == SyntaxKind::HASH)
    {
        let offset = usize::from(token.text_range().start());
        if out
            .disabled
            .iter()
            .any(|&(start, end)| start <= offset && offset < end)
        {
            continue;
        }
        if token
            .parent_ancestors()
            .any(|node| node.kind() == SyntaxKind::ATTRIBUTE)
        {
            continue;
        }
        out.errors.push(stray_hash(line_of(text, offset)));
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
            jals_syntax::SyntaxElement::Node(n) => n.prev_sibling_or_token(),
            jals_syntax::SyntaxElement::Token(t) => t.prev_sibling_or_token(),
        };
    }
    false
}

/// Whether the attributes on one host left it enabled or disabled.
#[derive(PartialEq, Eq)]
enum Host {
    Enabled,
    Disabled,
}

/// Validate and evaluate one host's leading attributes, extending the plan with either the
/// attribute blanks (enabled) or the whole-host blank (disabled). Every attribute on the host is
/// validated even when an earlier one already disabled it, so a typo next to a false `cfg` is
/// still reported.
fn host_plan(
    parse: &Parse,
    text: &str,
    host: &SyntaxNode,
    attrs: &[Attribute],
    features: &BTreeSet<String>,
    out: &mut AttrPlan,
) -> Host {
    let before = out.errors.len();
    let mut enabled = true;
    for attr in attrs {
        let Some(span) = node_span(attr.syntax()) else {
            out.errors.push(malformed_cfg(1));
            continue;
        };
        let line = line_of(text, span.0);
        if overlaps_error(parse, span) {
            out.errors.push(attr_overlaps_error(line));
            continue;
        }
        match eval_attribute(attr, features, line) {
            Ok(value) => enabled &= value,
            Err(message) => out.errors.push(message),
        }
    }
    if out.errors.len() > before {
        // The host's spans are only planned from a fully valid attribute list.
        return Host::Enabled;
    }
    if enabled {
        for attr in attrs {
            if let Some((start, end)) = node_span(attr.syntax()) {
                out.blanks.push(Blank {
                    start,
                    end,
                    semicolon: false,
                });
            }
        }
        return Host::Enabled;
    }
    let Some((start, end)) = node_span(host) else {
        return Host::Enabled;
    };
    // Error recovery can mis-extend a node, so a disabled host overlapping a parse error is
    // never best-effort stripped.
    if overlaps_error(parse, (start, end)) {
        out.errors.push(disabled_has_errors(line_of(text, start)));
        return Host::Enabled;
    }
    out.blanks.push(Blank {
        start,
        end,
        semicolon: needs_semicolon(host),
    });
    out.disabled.push((start, end));
    Host::Disabled
}

/// The node kinds an attribute may govern. Declarations reached through `modifiers()` and
/// statements (whose leading attributes are direct children); `BLOCK` counts because a block is a
/// statement. Everything else — parameters, record components, catch/resource/for-header
/// modifiers, enum constants — is rejected by the dangling-attribute check in [`plan`].
const fn is_attribute_host(kind: SyntaxKind) -> bool {
    use SyntaxKind as S;
    matches!(
        kind,
        S::IMPORT_DECL
            | S::CLASS_DECL
            | S::INTERFACE_DECL
            | S::ENUM_DECL
            | S::RECORD_DECL
            | S::ANNOTATION_TYPE_DECL
            | S::FIELD_DECL
            | S::METHOD_DECL
            | S::CONSTRUCTOR_DECL
            | S::INITIALIZER
            | S::BLOCK
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
            | S::EMPTY_STMT
    )
}

/// The host's leading attribute run: direct `ATTRIBUTE` children before any other significant
/// element, plus the same leading run inside a direct `MODIFIERS` child (where `modifiers()`
/// parses them for declarations). Attributes *after* another significant element — recovery
/// debris dangling in a block, or a late attribute inside `MODIFIERS` — are not part of any run
/// and are diagnosed individually by [`plan`].
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

/// Whether stripping this host must leave a `;` behind: as the sole body of a control structure
/// (or a labeled statement's target), removing the statement entirely would leave the structure
/// without a body.
fn needs_semicolon(host: &SyntaxNode) -> bool {
    use SyntaxKind as S;
    matches!(
        host.parent().map(|p| p.kind()),
        Some(
            S::IF_STMT
                | S::WHILE_STMT
                | S::DO_WHILE_STMT
                | S::FOR_STMT
                | S::FOR_EACH_STMT
                | S::LABELED_STMT
        )
    )
}

/// The significant span of a node: its first through last non-trivia token, excluding the leading
/// trivia rowan parks inside the node (the previous line's newline) — which is exactly what keeps
/// blanking line-stable.
pub(crate) fn node_span(node: &SyntaxNode) -> Option<(usize, usize)> {
    let mut significant = node
        .descendants_with_tokens()
        .filter_map(jals_syntax::SyntaxElement::into_token)
        .filter(|token| !token.kind().is_trivia());
    let first = significant.next()?;
    let last = significant.last().unwrap_or_else(|| first.clone());
    Some((
        usize::from(first.text_range().start()),
        usize::from(last.text_range().end()),
    ))
}

/// Whether any parse error lies strictly inside `span` (half-open, matching the grouped-import
/// overlap test).
fn overlaps_error(parse: &Parse, span: (usize, usize)) -> bool {
    parse.errors().iter().any(|error| {
        usize::from(error.range().start()) < span.1 && span.0 < usize::from(error.range().end())
    })
}

/// Validate one attribute (on 1-based `line`) and evaluate its `cfg` predicate against the
/// enabled feature set.
fn eval_attribute(
    attr: &Attribute,
    features: &BTreeSet<String>,
    line: usize,
) -> Result<bool, String> {
    let Some(meta) = attr.meta() else {
        return Err(malformed_cfg(line));
    };
    let Some(name) = meta.name_text() else {
        return Err(malformed_cfg(line));
    };
    if name != "cfg" {
        return Err(format!(
            "unknown attribute `{name}` on line {line}; only `cfg` is supported"
        ));
    }
    // `#[cfg]` and `#[cfg = "x"]` carry no predicate; `#[cfg(p)]` carries exactly one.
    if meta.value().is_some() {
        return Err(malformed_cfg(line));
    }
    let Some(args) = meta.args() else {
        return Err(malformed_cfg(line));
    };
    let mut predicates = args.args();
    let (Some(predicate), None) = (predicates.next(), predicates.next()) else {
        return Err(malformed_cfg(line));
    };
    eval_predicate(&predicate, features, line, 0)
}

/// Evaluate one `cfg` predicate: `feature = "name"`, `all(…)` (empty: true), `any(…)` (empty:
/// false), or `not(p)`. An unknown feature *name* is simply false (features are additive —
/// Cargo/Rust semantics); an unknown predicate *shape* is an error.
fn eval_predicate(
    predicate: &AttrArg,
    features: &BTreeSet<String>,
    line: usize,
    depth: usize,
) -> Result<bool, String> {
    if depth > MAX_PREDICATE_DEPTH {
        return Err(malformed_cfg(line));
    }
    let AttrArg::AttrMeta(meta) = predicate else {
        return Err(malformed_cfg(line));
    };
    let Some(name) = meta.name_text() else {
        return Err(malformed_cfg(line));
    };
    match name.as_str() {
        "feature" => {
            if meta.args().is_some() {
                return Err(malformed_cfg(line));
            }
            let Some(value) = meta.value() else {
                return Err(non_string_feature(line));
            };
            Ok(features.contains(&feature_name(&value, line)?))
        }
        "all" | "any" | "not" => {
            if meta.value().is_some() {
                return Err(malformed_cfg(line));
            }
            let Some(args) = meta.args() else {
                // Bare `all` / `any` / `not` without a parenthesized list.
                return Err(malformed_cfg(line));
            };
            let mut nested = args.args();
            match name.as_str() {
                "all" => nested.try_fold(true, |acc, p| {
                    Ok(acc & eval_predicate(&p, features, line, depth + 1)?)
                }),
                "any" => nested.try_fold(false, |acc, p| {
                    Ok(acc | eval_predicate(&p, features, line, depth + 1)?)
                }),
                _ => {
                    let (Some(inner), None) = (nested.next(), nested.next()) else {
                        return Err(malformed_cfg(line));
                    };
                    Ok(!eval_predicate(&inner, features, line, depth + 1)?)
                }
            }
        }
        _ => Err(malformed_cfg(line)),
    }
}

/// Decode a `feature = "…"` value (on 1-based `line`): a plain (escape-free) string literal.
fn feature_name(value: &Literal, line: usize) -> Result<String, String> {
    let Some(token) = value.token() else {
        return Err(non_string_feature(line));
    };
    if token.kind() != SyntaxKind::STRING_LITERAL {
        return Err(non_string_feature(line));
    }
    let text = token.text();
    let inner = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| non_string_feature(line))?;
    // Feature names are plain identifiers; an escape (or an embedded quote, impossible without
    // one) would need interpretation this pass deliberately does not do.
    if inner.contains('\\') || inner.contains('"') {
        return Err(non_string_feature(line));
    }
    Ok(inner.to_owned())
}
