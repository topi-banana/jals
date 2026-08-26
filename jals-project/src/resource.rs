//! Which `[build] resource-dirs` files are rendered as templates, and the engine that renders them.
//!
//! A resource is whatever the author put in the directory — a PNG, an `.nbt`, a font — so the
//! default is still the byte-for-byte copy it always was. `[build.resources] template` names the
//! ones that are rendered instead, and nothing else in the tree is decoded at all.
//!
//! The engine is a deliberately small Jinja subset, written here rather than taken from a crate:
//! every engine on crates.io needs `std`, and this crate is `no_std + alloc` in its portable
//! configuration. It is crate-internal because `ResourcePlan` is its only consumer.
//!
//! Two divergences from Jinja are on purpose, and both are documented in `jals-build/README.md`:
//!
//! - A block tag alone on its line takes the whole line with it (Jinja's `trim_blocks` plus
//!   `lstrip_blocks`, always on). Resources are JSON and XML, where a stray blank line is a diff.
//!   There is no `{%- -%}` spelling, so there is one rule rather than two.
//! - Emitting a value that is not there is an error. In a build tool an undefined name is a typo
//!   far more often than an intention, and a silently empty `"version": ""` reaches the jar and
//!   fails at load time instead. `| default("…")` is how the intentional case is spelled.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use jals_config::{Manifest, ResolvedBuildFeatures, ResourcePattern};
use jals_storage::{DirKey, ProjectView, RelativePath};

/// The `[build] resource-dirs` to read, which of their files are rendered, and what the render
/// sees.
///
/// Lowered once, exactly where `[build] remap`'s mapping set is, so a host never reads
/// `[build.resources]` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourcePlan {
    dirs: Vec<DirKey>,
    /// Each declared glob beside the text it was written as, which is what an error names.
    templates: Vec<(String, ResourcePattern)>,
    context: TemplateContext,
}

impl ResourcePlan {
    /// Lower `[build] resource-dirs` and `[build.resources]` under one feature selection.
    pub(crate) fn lower(manifest: &Manifest, features: &ResolvedBuildFeatures) -> Self {
        // An entry `Manifest::validate` accepted always parses; one that does not is a manifest
        // that reached here unvalidated, and dropping it is the same answer the missing directory
        // in `entries` gets.
        let dirs = manifest
            .build
            .resource_dirs
            .iter()
            .filter_map(|dir| DirKey::parse(dir).ok())
            .collect();
        let templates = manifest
            .build
            .resources
            .template
            .iter()
            .filter_map(|pattern| {
                ResourcePattern::parse(pattern)
                    .ok()
                    .map(|glob| (pattern.clone(), glob))
            })
            .collect();
        Self {
            dirs,
            templates,
            context: TemplateContext::new(manifest, features.features()),
        }
    }

    /// The lowered `[build] resource-dirs`, for the test that pins the lowering.
    #[cfg(test)]
    pub(crate) fn dirs(&self) -> &[DirKey] {
        &self.dirs
    }

    /// Every resource in `view`, addressed by its path below the directory it was declared under —
    /// exactly as a class is addressed below `classes-dir` — rendered where one was declared.
    ///
    /// Sorted by that path, per directory, because the jar's member order is part of its bytes.
    /// The sort happens **before** anything is rendered, not after: `files_under` yields keys in
    /// segment order while the sort is over the joined string, so rendering during the walk would
    /// make *which* failure gets reported depend on an order the output never has.
    ///
    /// # Errors
    /// A message naming the resource that could not be rendered, or the declaration that named
    /// nothing.
    pub(crate) fn entries(
        &self,
        view: &ProjectView,
    ) -> Result<Vec<(RelativePath, Vec<u8>)>, String> {
        let mut entries = Vec::new();
        let mut seen = BTreeSet::new();
        let mut present = 0usize;
        for dir in &self.dirs {
            // A declared directory that is not there is not a mistake: `[build] resource-dirs`
            // defaults onto every project, and most projects have no resources.
            if view.directory(dir).is_err() {
                continue;
            }
            present += 1;
            let mut found: Vec<_> = view
                .tree()
                .files_under(dir)
                .filter_map(|file| {
                    file.key()
                        .path()
                        .strip_prefix(dir.path())
                        .filter(|path| !path.is_root())
                        .map(|path| (path, file))
                })
                .collect();
            found.sort_by_key(|(path, _)| path.to_string());
            for (path, file) in found {
                let Some(index) = self.matched(&path) else {
                    entries.push((path, file.bytes().to_vec()));
                    continue;
                };
                seen.insert(index);
                let text = file.text().map_err(|error| {
                    format!(
                        "`{path}` is declared in `[build.resources] template` but is not UTF-8: \
                         {error}"
                    )
                })?;
                let rendered = Template::parse(text)
                    .and_then(|template| template.render(&self.context))
                    .map_err(|error| format!("`{path}`: {error}"))?;
                entries.push((path, rendered.into_bytes()));
            }
        }
        self.check_matched(&seen, present)?;
        Ok(entries)
    }

    /// The declaration this member path is rendered by, if any. First match wins; the index is what
    /// records that the declaration was used.
    fn matched(&self, path: &RelativePath) -> Option<usize> {
        self.templates
            .iter()
            .position(|(_, glob)| glob.matches(path))
    }

    /// Fail on a declaration that rendered nothing.
    ///
    /// Unlike a missing `resource-dirs` entry, which is tolerated because the default lands on
    /// every project, a pattern here was written on purpose — so a typo that quietly ships an
    /// unrendered file is the silent wrong answer, not the failure. The two messages are separate
    /// because the fix is: one says make the directory, the other says fix the glob.
    fn check_matched(&self, seen: &BTreeSet<usize>, present: usize) -> Result<(), String> {
        let mut unmatched = self
            .templates
            .iter()
            .enumerate()
            .filter(|(index, _)| !seen.contains(index))
            .map(|(_, (pattern, _))| pattern.as_str());
        let Some(first) = unmatched.next() else {
            return Ok(());
        };
        if present == 0 {
            return Err(format!(
                "`[build.resources] template` names `{first}`, but no `[build] resource-dirs` \
                 directory exists in this project"
            ));
        }
        Err(format!(
            "`[build.resources] template` entry `{first}` matched no file under `[build] \
             resource-dirs`"
        ))
    }
}

/// The values a resource template can read.
///
/// Two namespaces and nothing else. Environment variables are deliberately absent: a value read
/// from the ambient environment is not part of any cache identity here, so a build that changed
/// nothing else would still have to be assumed stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateContext {
    package: BTreeMap<String, Value>,
    features: BTreeSet<String>,
}

impl TemplateContext {
    /// The `[package]` metadata and the resolved build features, as one render sees them.
    fn new(manifest: &Manifest, features: &BTreeSet<String>) -> Self {
        let mut package = BTreeMap::new();
        package.insert(
            "name".to_owned(),
            Self::optional(manifest.package.name.as_deref()),
        );
        package.insert(
            "version".to_owned(),
            Self::optional(manifest.package.version.as_deref()),
        );
        Self {
            package,
            features: features.clone(),
        }
    }

    /// A `[package]` key that is declared but unset is *known and absent*, which is what makes
    /// `| default("…")` meaningful — as opposed to a key that does not exist, which is a typo.
    fn optional(value: Option<&str>) -> Value {
        value.map_or(Value::Undefined, |value| Value::Text(value.to_owned()))
    }

    /// The root namespace a path starts from.
    fn root(&self, name: &str) -> Option<Value> {
        match name {
            "package" => Some(Value::Map(self.package.clone())),
            "features" => Some(Value::Set(self.features.clone())),
            _ => None,
        }
    }

    /// The roots a template may name, in a fixed order so an error reads the same every run.
    const ROOTS: [&'static str; 2] = ["features", "package"];
}

/// A value during rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Bool(bool),
    Text(String),
    /// The resolved build features. Indexing it asks membership, so it answers for *any* name.
    Set(BTreeSet<String>),
    Map(BTreeMap<String, Self>),
    /// Known, and not there.
    Undefined,
}

impl Value {
    /// Read a field, by either spelling — `a.b` and `a["b"]` are the same access.
    fn field(&self, name: &str, root: &str, at: Position) -> Result<Self, TemplateError> {
        match self {
            // A feature set answers membership for every name: features are additive, so "is X on"
            // is well-formed whether or not X was ever declared. Checking it against the declared
            // map instead would bind this engine to `[features]` to buy only typo detection.
            Self::Set(set) => Ok(Self::Bool(set.contains(name))),
            Self::Map(map) => map.get(name).cloned().ok_or_else(|| TemplateError {
                at,
                kind: TemplateErrorKind::UnknownField {
                    root: root.to_owned(),
                    field: name.to_owned(),
                },
            }),
            _ => Err(TemplateError {
                at,
                kind: TemplateErrorKind::NotIndexable {
                    field: name.to_owned(),
                },
            }),
        }
    }

    /// Append this value's text form.
    fn write(&self, out: &mut String, at: Position) -> Result<(), TemplateError> {
        match self {
            Self::Bool(value) => {
                out.push_str(if *value { "true" } else { "false" });
                Ok(())
            }
            Self::Text(text) => {
                out.push_str(text);
                Ok(())
            }
            Self::Undefined => Err(TemplateError {
                at,
                kind: TemplateErrorKind::UndefinedValue,
            }),
            Self::Set(_) | Self::Map(_) => Err(TemplateError {
                at,
                kind: TemplateErrorKind::NotAScalar,
            }),
        }
    }

    fn truth(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Text(text) => !text.is_empty(),
            Self::Set(set) => !set.is_empty(),
            Self::Map(_) => true,
            Self::Undefined => false,
        }
    }
}

/// Where in the template something is, counted in characters from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Position {
    line: u32,
    column: u32,
}

impl Position {
    const START: Self = Self { line: 1, column: 1 };

    const fn advance(&mut self, ch: char) {
        if ch == '\n' {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else {
            self.column = self.column.saturating_add(1);
        }
    }
}

/// A template that failed to parse or to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateError {
    at: Position,
    kind: TemplateErrorKind,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.at.line, self.at.column, self.kind
        )
    }
}

/// What was wrong. The file it was wrong in is the caller's to name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplateErrorKind {
    Unclosed { opener: &'static str },
    UnknownTag { tag: String },
    UnexpectedTag { tag: String },
    UnclosedBlock { tag: &'static str },
    Malformed { reason: &'static str },
    UnknownRoot { name: String },
    UnknownField { root: String, field: String },
    NotIndexable { field: String },
    NotAScalar,
    NotIterable,
    UndefinedValue,
    UnknownFilter { name: String },
    TooDeep,
}

impl fmt::Display for TemplateErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unclosed { opener } => write!(f, "`{opener}` is never closed"),
            Self::UnknownTag { tag } => write!(f, "unknown tag `{tag}`"),
            Self::UnexpectedTag { tag } => write!(f, "`{tag}` has no block to close"),
            Self::UnclosedBlock { tag } => write!(f, "`{tag}` is never closed"),
            Self::Malformed { reason } => f.write_str(reason),
            Self::UnknownRoot { name } => {
                write!(f, "unknown name `{name}`; a template can read ")?;
                for (index, root) in TemplateContext::ROOTS.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" and ")?;
                    }
                    write!(f, "`{root}`")?;
                }
                Ok(())
            }
            Self::UnknownField { root, field } => write!(f, "`{root}` has no field `{field}`"),
            Self::NotIndexable { field } => {
                write!(f, "this value has no fields, so `{field}` cannot be read")
            }
            Self::NotAScalar => f.write_str("this is a namespace, not a value that can be written"),
            Self::NotIterable => f.write_str("only `features` can be iterated"),
            Self::UndefinedValue => f.write_str(
                "this value is not set; write `| default(\"…\")` to say what to use instead",
            ),
            Self::UnknownFilter { name } => write!(f, "unknown filter `{name}`"),
            Self::TooDeep => f.write_str("blocks are nested too deeply"),
        }
    }
}

/// A parsed resource template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Template {
    nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Text(String),
    Emit {
        expr: Expr,
        at: Position,
    },
    If {
        arms: Vec<Arm>,
        otherwise: Vec<Self>,
    },
    For {
        binding: String,
        source: Expr,
        at: Position,
        body: Vec<Self>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Arm {
    condition: Expr,
    at: Position,
    body: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Path { root: String, accesses: Vec<String> },
    Text(String),
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Default(Box<Self>, String),
}

/// Which delimiter pair a tag is written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagKind {
    /// `{{ … }}`, which never affects the whitespace around it.
    Emit,
    /// `{% … %}`.
    Block,
    /// `{# … #}`, dropped entirely.
    Comment,
}

impl TagKind {
    const fn opener(self) -> &'static str {
        match self {
            Self::Emit => "{{",
            Self::Block => "{%",
            Self::Comment => "{#",
        }
    }

    const fn closer(self) -> [u8; 2] {
        match self {
            Self::Emit => *b"}}",
            Self::Block => *b"%}",
            Self::Comment => *b"#}",
        }
    }
}

/// One tag as it sits in the source, before the whitespace rule widens it.
#[derive(Debug, Clone)]
struct RawTag {
    start: usize,
    end: usize,
    at: Position,
    kind: TagKind,
    body: String,
}

/// The flat sequence a template parses from: text and tags, comments already dropped.
#[derive(Debug, Clone)]
enum Item {
    Text(String),
    Emit { body: String, at: Position },
    Block { body: String, at: Position },
}

impl Template {
    /// How deeply blocks may nest before the parser refuses rather than recurses.
    const MAX_DEPTH: u32 = 64;

    /// Parse one resource template.
    ///
    /// # Errors
    /// A [`TemplateError`] carrying the line and column the template broke at.
    fn parse(source: &str) -> Result<Self, TemplateError> {
        let tags = Self::scan(source)?;
        let mut parser = Parser {
            items: Self::items(source, &tags),
            index: 0,
        };
        let (nodes, terminator) = parser.nodes(0)?;
        if let Some((directive, at)) = terminator {
            return Err(TemplateError {
                at,
                kind: TemplateErrorKind::UnexpectedTag {
                    tag: directive.name().to_owned(),
                },
            });
        }
        Ok(Self { nodes })
    }

    /// Render against one context.
    ///
    /// # Errors
    /// A [`TemplateError`] carrying the line and column of the expression that could not be
    /// evaluated.
    fn render(&self, context: &TemplateContext) -> Result<String, TemplateError> {
        let mut out = String::new();
        let mut scope = Vec::new();
        Self::nodes_text(&self.nodes, context, &mut scope, &mut out)?;
        Ok(out)
    }

    /// Every tag in the source, in order, with the position of its opening delimiter.
    fn scan(source: &str) -> Result<Vec<RawTag>, TemplateError> {
        let bytes = source.as_bytes();
        let mut tags = Vec::new();
        let mut at = Position::START;
        let mut index = 0usize;
        while index < bytes.len() {
            let kind = if bytes[index] == b'{' {
                match bytes.get(index + 1) {
                    Some(b'{') => Some(TagKind::Emit),
                    Some(b'%') => Some(TagKind::Block),
                    Some(b'#') => Some(TagKind::Comment),
                    _ => None,
                }
            } else {
                None
            };
            let Some(kind) = kind else {
                // A lone `{` is never a delimiter, which is what lets a JSON or XML resource be
                // written exactly as it is read.
                let Some(ch) = source[index..].chars().next() else {
                    break;
                };
                at.advance(ch);
                index += ch.len_utf8();
                continue;
            };
            let body_end = Self::close(source, index + 2, kind).ok_or_else(|| TemplateError {
                at,
                kind: TemplateErrorKind::Unclosed {
                    opener: kind.opener(),
                },
            })?;
            let end = body_end + 2;
            tags.push(RawTag {
                start: index,
                end,
                at,
                kind,
                body: source[index + 2..body_end].trim().to_owned(),
            });
            for ch in source[index..end].chars() {
                at.advance(ch);
            }
            index = end;
        }
        Ok(tags)
    }

    /// Where this tag's closing delimiter starts, skipping over string literals.
    ///
    /// The scan has to know about strings: `{{ "}}" }}` is how a template writes a literal `}}`,
    /// and a plain search for the closer would end the tag in the middle of it.
    fn close(source: &str, from: usize, kind: TagKind) -> Option<usize> {
        let bytes = source.as_bytes();
        let closer = kind.closer();
        let mut index = from;
        let mut in_string = false;
        while index < bytes.len() {
            if kind != TagKind::Comment && bytes[index] == b'"' {
                in_string = !in_string;
                index += 1;
                continue;
            }
            if in_string {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
                continue;
            }
            if bytes[index] == closer[0] && bytes.get(index + 1) == Some(&closer[1]) {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    /// Split the source into text and tags, applying the whole-line rule as it goes.
    fn items(source: &str, tags: &[RawTag]) -> Vec<Item> {
        let mut items = Vec::new();
        let mut cursor = 0usize;
        for tag in tags {
            let (start, end) = Self::span(source, tag);
            let start = start.max(cursor);
            if start > cursor {
                items.push(Item::Text(source[cursor..start].to_owned()));
            }
            cursor = end.max(cursor);
            match tag.kind {
                TagKind::Comment => {}
                TagKind::Emit => items.push(Item::Emit {
                    body: tag.body.clone(),
                    at: tag.at,
                }),
                TagKind::Block => items.push(Item::Block {
                    body: tag.body.clone(),
                    at: tag.at,
                }),
            }
        }
        if cursor < source.len() {
            items.push(Item::Text(source[cursor..].to_owned()));
        }
        items
    }

    /// A tag's effective span: widened over its own line when it is the only thing on it.
    ///
    /// Both sides have to be blank for the rule to apply, so two tags sharing a line keep every
    /// byte between them.
    fn span(source: &str, tag: &RawTag) -> (usize, usize) {
        if tag.kind == TagKind::Emit {
            return (tag.start, tag.end);
        }
        let bytes = source.as_bytes();
        let mut start = tag.start;
        while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        if start > 0 && bytes[start - 1] != b'\n' {
            return (tag.start, tag.end);
        }
        let mut end = tag.end;
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
        if end < bytes.len() {
            if bytes[end] == b'\n' {
                end += 1;
            } else if bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n') {
                end += 2;
            } else {
                return (tag.start, tag.end);
            }
        }
        (start, end)
    }

    fn nodes_text(
        nodes: &[Node],
        context: &TemplateContext,
        scope: &mut Vec<(String, Value)>,
        out: &mut String,
    ) -> Result<(), TemplateError> {
        for node in nodes {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Emit { expr, at } => {
                    Self::value(expr, context, scope, *at)?.write(out, *at)?;
                }
                Node::If { arms, otherwise } => {
                    let mut taken = false;
                    for arm in arms {
                        if Self::value(&arm.condition, context, scope, arm.at)?.truth() {
                            Self::nodes_text(&arm.body, context, scope, out)?;
                            taken = true;
                            break;
                        }
                    }
                    if !taken {
                        Self::nodes_text(otherwise, context, scope, out)?;
                    }
                }
                Node::For {
                    binding,
                    source,
                    at,
                    body,
                } => Self::loop_text(binding, source, *at, body, context, scope, out)?,
            }
        }
        Ok(())
    }

    fn loop_text(
        binding: &str,
        source: &Expr,
        at: Position,
        body: &[Node],
        context: &TemplateContext,
        scope: &mut Vec<(String, Value)>,
        out: &mut String,
    ) -> Result<(), TemplateError> {
        // Only the feature set is iterable, and it is a `BTreeSet`, so the order is the same on
        // every host and every run.
        let Value::Set(items) = Self::value(source, context, scope, at)? else {
            return Err(TemplateError {
                at,
                kind: TemplateErrorKind::NotIterable,
            });
        };
        let length = items.len();
        for (index, item) in items.iter().enumerate() {
            scope.push((binding.to_owned(), Value::Text(item.clone())));
            scope.push(("loop".to_owned(), Self::loop_value(index, length)));
            let rendered = Self::nodes_text(body, context, scope, out);
            scope.truncate(scope.len().saturating_sub(2));
            rendered?;
        }
        Ok(())
    }

    fn loop_value(index: usize, length: usize) -> Value {
        let mut map = BTreeMap::new();
        map.insert("index".to_owned(), Value::Text((index + 1).to_string()));
        map.insert("index0".to_owned(), Value::Text(index.to_string()));
        map.insert("first".to_owned(), Value::Bool(index == 0));
        map.insert("last".to_owned(), Value::Bool(index + 1 == length));
        map.insert("length".to_owned(), Value::Text(length.to_string()));
        Value::Map(map)
    }

    fn value(
        expr: &Expr,
        context: &TemplateContext,
        scope: &[(String, Value)],
        at: Position,
    ) -> Result<Value, TemplateError> {
        match expr {
            Expr::Text(text) => Ok(Value::Text(text.clone())),
            Expr::Not(inner) => Ok(Value::Bool(
                !Self::value(inner, context, scope, at)?.truth(),
            )),
            Expr::And(left, right) => Ok(Value::Bool(
                Self::value(left, context, scope, at)?.truth()
                    && Self::value(right, context, scope, at)?.truth(),
            )),
            Expr::Or(left, right) => Ok(Value::Bool(
                Self::value(left, context, scope, at)?.truth()
                    || Self::value(right, context, scope, at)?.truth(),
            )),
            Expr::Default(inner, fallback) => {
                let value = Self::value(inner, context, scope, at)?;
                Ok(if value == Value::Undefined {
                    Value::Text(fallback.clone())
                } else {
                    value
                })
            }
            Expr::Path { root, accesses } => {
                let mut value = scope
                    .iter()
                    .rev()
                    .find(|(name, _)| name == root)
                    .map(|(_, value)| value.clone())
                    .or_else(|| context.root(root))
                    .ok_or_else(|| TemplateError {
                        at,
                        kind: TemplateErrorKind::UnknownRoot { name: root.clone() },
                    })?;
                for access in accesses {
                    value = value.field(access, root, at)?;
                }
                Ok(value)
            }
        }
    }
}

/// The tree builder over the flat item sequence.
struct Parser {
    items: Vec<Item>,
    index: usize,
}

impl Parser {
    /// Nodes up to the first tag this level does not own, which is handed back to the caller.
    fn nodes(
        &mut self,
        depth: u32,
    ) -> Result<(Vec<Node>, Option<(Directive, Position)>), TemplateError> {
        let mut nodes = Vec::new();
        while let Some(item) = self.items.get(self.index).cloned() {
            self.index += 1;
            match item {
                Item::Text(text) => nodes.push(Node::Text(text)),
                Item::Emit { body, at } => nodes.push(Node::Emit {
                    expr: Expr::parse(&body, at)?,
                    at,
                }),
                Item::Block { body, at } => match Directive::parse(&body, at)? {
                    Directive::If(condition) => nodes.push(self.if_node(condition, at, depth)?),
                    Directive::For { binding, source } => {
                        nodes.push(self.for_node(binding, source, at, depth)?);
                    }
                    other => return Ok((nodes, Some((other, at)))),
                },
            }
        }
        Ok((nodes, None))
    }

    fn if_node(
        &mut self,
        condition: Expr,
        at: Position,
        depth: u32,
    ) -> Result<Node, TemplateError> {
        if depth >= Template::MAX_DEPTH {
            return Err(TemplateError {
                at,
                kind: TemplateErrorKind::TooDeep,
            });
        }
        let mut arms = Vec::new();
        let mut condition = condition;
        let mut condition_at = at;
        loop {
            let (body, terminator) = self.nodes(depth + 1)?;
            let Some((directive, tag_at)) = terminator else {
                return Err(TemplateError {
                    at,
                    kind: TemplateErrorKind::UnclosedBlock { tag: "if" },
                });
            };
            match directive {
                Directive::Elif(next) => {
                    arms.push(Arm {
                        condition,
                        at: condition_at,
                        body,
                    });
                    condition = next;
                    condition_at = tag_at;
                }
                Directive::Else => {
                    arms.push(Arm {
                        condition,
                        at: condition_at,
                        body,
                    });
                    let (otherwise, terminator) = self.nodes(depth + 1)?;
                    let Some((directive, tag_at)) = terminator else {
                        return Err(TemplateError {
                            at,
                            kind: TemplateErrorKind::UnclosedBlock { tag: "if" },
                        });
                    };
                    if !matches!(directive, Directive::EndIf) {
                        return Err(TemplateError {
                            at: tag_at,
                            kind: TemplateErrorKind::UnexpectedTag {
                                tag: directive.name().to_owned(),
                            },
                        });
                    }
                    return Ok(Node::If { arms, otherwise });
                }
                Directive::EndIf => {
                    arms.push(Arm {
                        condition,
                        at: condition_at,
                        body,
                    });
                    return Ok(Node::If {
                        arms,
                        otherwise: Vec::new(),
                    });
                }
                other => {
                    return Err(TemplateError {
                        at: tag_at,
                        kind: TemplateErrorKind::UnexpectedTag {
                            tag: other.name().to_owned(),
                        },
                    });
                }
            }
        }
    }

    fn for_node(
        &mut self,
        binding: String,
        source: Expr,
        at: Position,
        depth: u32,
    ) -> Result<Node, TemplateError> {
        if depth >= Template::MAX_DEPTH {
            return Err(TemplateError {
                at,
                kind: TemplateErrorKind::TooDeep,
            });
        }
        let (body, terminator) = self.nodes(depth + 1)?;
        let Some((directive, tag_at)) = terminator else {
            return Err(TemplateError {
                at,
                kind: TemplateErrorKind::UnclosedBlock { tag: "for" },
            });
        };
        if !matches!(directive, Directive::EndFor) {
            return Err(TemplateError {
                at: tag_at,
                kind: TemplateErrorKind::UnexpectedTag {
                    tag: directive.name().to_owned(),
                },
            });
        }
        Ok(Node::For {
            binding,
            source,
            at,
            body,
        })
    }
}

/// What a `{% … %}` tag says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Directive {
    If(Expr),
    Elif(Expr),
    Else,
    EndIf,
    For { binding: String, source: Expr },
    EndFor,
}

impl Directive {
    fn parse(body: &str, at: Position) -> Result<Self, TemplateError> {
        let (keyword, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
        let rest = rest.trim();
        match keyword {
            "if" => Ok(Self::If(Expr::parse(rest, at)?)),
            "elif" => Ok(Self::Elif(Expr::parse(rest, at)?)),
            "for" => {
                let (binding, source) = rest.split_once(" in ").ok_or(TemplateError {
                    at,
                    kind: TemplateErrorKind::Malformed {
                        reason: "expected `for <name> in <expression>`",
                    },
                })?;
                let binding = binding.trim();
                if !Expr::is_name(binding) {
                    return Err(TemplateError {
                        at,
                        kind: TemplateErrorKind::Malformed {
                            reason: "the loop variable must be a plain name",
                        },
                    });
                }
                Ok(Self::For {
                    binding: binding.to_owned(),
                    source: Expr::parse(source.trim(), at)?,
                })
            }
            "else" | "endif" | "endfor" if !rest.is_empty() => Err(TemplateError {
                at,
                kind: TemplateErrorKind::Malformed {
                    reason: "this tag takes no expression",
                },
            }),
            "else" => Ok(Self::Else),
            "endif" => Ok(Self::EndIf),
            "endfor" => Ok(Self::EndFor),
            other => Err(TemplateError {
                at,
                kind: TemplateErrorKind::UnknownTag {
                    tag: other.to_owned(),
                },
            }),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::If(_) => "if",
            Self::Elif(_) => "elif",
            Self::Else => "else",
            Self::EndIf => "endif",
            Self::For { .. } => "for",
            Self::EndFor => "endfor",
        }
    }
}

/// One lexed piece of an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Name(String),
    Text(String),
    Dot,
    OpenBracket,
    CloseBracket,
    OpenParen,
    CloseParen,
    Pipe,
}

impl Expr {
    fn parse(source: &str, at: Position) -> Result<Self, TemplateError> {
        let mut parser = ExprParser {
            tokens: Self::tokens(source, at)?,
            index: 0,
            at,
        };
        let expr = parser.any()?;
        if parser.index != parser.tokens.len() {
            return Err(parser.malformed("the expression has trailing tokens"));
        }
        Ok(expr)
    }

    fn is_name(text: &str) -> bool {
        let mut chars = text.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }

    fn tokens(source: &str, at: Position) -> Result<Vec<Token>, TemplateError> {
        let mut tokens = Vec::new();
        let mut chars = source.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                ch if ch.is_whitespace() => {}
                '.' => tokens.push(Token::Dot),
                '[' => tokens.push(Token::OpenBracket),
                ']' => tokens.push(Token::CloseBracket),
                '(' => tokens.push(Token::OpenParen),
                ')' => tokens.push(Token::CloseParen),
                '|' => tokens.push(Token::Pipe),
                '"' => {
                    let mut text = String::new();
                    loop {
                        let Some(ch) = chars.next() else {
                            return Err(TemplateError {
                                at,
                                kind: TemplateErrorKind::Malformed {
                                    reason: "a string literal is never closed",
                                },
                            });
                        };
                        match ch {
                            '"' => break,
                            '\\' => match chars.next() {
                                Some(escaped @ ('"' | '\\')) => text.push(escaped),
                                _ => {
                                    return Err(TemplateError {
                                        at,
                                        kind: TemplateErrorKind::Malformed {
                                            reason: "only `\\\"` and `\\\\` are escapes",
                                        },
                                    });
                                }
                            },
                            ch => text.push(ch),
                        }
                    }
                    tokens.push(Token::Text(text));
                }
                ch if ch.is_ascii_alphanumeric() || ch == '_' => {
                    let mut name = String::new();
                    name.push(ch);
                    while let Some(next) = chars.peek() {
                        if next.is_ascii_alphanumeric() || *next == '_' {
                            name.push(*next);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(Token::Name(name));
                }
                _ => {
                    return Err(TemplateError {
                        at,
                        kind: TemplateErrorKind::Malformed {
                            reason: "unexpected character in the expression",
                        },
                    });
                }
            }
        }
        Ok(tokens)
    }
}

struct ExprParser {
    tokens: Vec<Token>,
    index: usize,
    at: Position,
}

impl ExprParser {
    fn any(&mut self) -> Result<Expr, TemplateError> {
        let mut left = self.all()?;
        while self.eat_name("or") {
            let right = self.all()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn all(&mut self) -> Result<Expr, TemplateError> {
        let mut left = self.unary()?;
        while self.eat_name("and") {
            let right = self.unary()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, TemplateError> {
        if self.eat_name("not") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        let mut value = self.primary()?;
        while self.eat(&Token::Pipe) {
            value = self.filter(value)?;
        }
        Ok(value)
    }

    /// Filters are a table of one. A second entry is a match arm, which is the point of keeping the
    /// shape rather than special-casing `default` in `primary`.
    fn filter(&mut self, value: Expr) -> Result<Expr, TemplateError> {
        let Some(Token::Name(name)) = self.next() else {
            return Err(self.malformed("expected a filter name after `|`"));
        };
        match name.as_str() {
            "default" => {
                if !self.eat(&Token::OpenParen) {
                    return Err(self.malformed("`default` takes one string argument"));
                }
                let Some(Token::Text(fallback)) = self.next() else {
                    return Err(self.malformed("`default` takes one string argument"));
                };
                if !self.eat(&Token::CloseParen) {
                    return Err(self.malformed("`default` takes one string argument"));
                }
                Ok(Expr::Default(Box::new(value), fallback))
            }
            other => Err(TemplateError {
                at: self.at,
                kind: TemplateErrorKind::UnknownFilter {
                    name: other.to_owned(),
                },
            }),
        }
    }

    fn primary(&mut self) -> Result<Expr, TemplateError> {
        match self.next() {
            Some(Token::OpenParen) => {
                let inner = self.any()?;
                if !self.eat(&Token::CloseParen) {
                    return Err(self.malformed("expected `)`"));
                }
                Ok(inner)
            }
            Some(Token::Text(text)) => Ok(Expr::Text(text)),
            Some(Token::Name(root)) if !matches!(root.as_str(), "and" | "or" | "not") => {
                let mut accesses = Vec::new();
                loop {
                    if self.eat(&Token::Dot) {
                        let Some(Token::Name(field)) = self.next() else {
                            return Err(self.malformed("expected a name after `.`"));
                        };
                        accesses.push(field);
                    } else if self.eat(&Token::OpenBracket) {
                        // The bracket spelling is not a convenience: a build feature may be named
                        // `1.20.1` or `mixin-extras`, neither of which is a name `a.b` can carry.
                        let Some(Token::Text(field)) = self.next() else {
                            return Err(self.malformed("expected a string inside `[…]`"));
                        };
                        if !self.eat(&Token::CloseBracket) {
                            return Err(self.malformed("expected `]`"));
                        }
                        accesses.push(field);
                    } else {
                        break;
                    }
                }
                Ok(Expr::Path { root, accesses })
            }
            _ => Err(self.malformed("expected a value")),
        }
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.index).cloned();
        if token.is_some() {
            self.index += 1;
        }
        token
    }

    fn eat(&mut self, token: &Token) -> bool {
        let found = self.tokens.get(self.index) == Some(token);
        if found {
            self.index += 1;
        }
        found
    }

    fn eat_name(&mut self, name: &str) -> bool {
        let found =
            matches!(self.tokens.get(self.index), Some(Token::Name(found)) if found == name);
        if found {
            self.index += 1;
        }
        found
    }

    const fn malformed(&self, reason: &'static str) -> TemplateError {
        TemplateError {
            at: self.at,
            kind: TemplateErrorKind::Malformed { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use jals_storage::{CodeTree, Entry, FileKey, MemoryStorage};

    use super::*;

    fn manifest(text: &str) -> Manifest {
        text.parse().expect("test manifest is valid")
    }

    fn features(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn context(package: &str, active: &[&str]) -> TemplateContext {
        TemplateContext::new(&manifest(package), &features(active))
    }

    fn render(source: &str, context: &TemplateContext) -> Result<String, TemplateError> {
        Template::parse(source).and_then(|template| template.render(context))
    }

    fn kind(source: &str, context: &TemplateContext) -> TemplateErrorKind {
        render(source, context)
            .expect_err("this template fails")
            .kind
    }

    fn view(files: &[(&str, &[u8])]) -> ProjectView {
        MemoryStorage::memory(
            CodeTree::new(files.iter().map(|(path, bytes)| {
                Entry::File(
                    FileKey::parse(path).expect("path is portable"),
                    bytes.to_vec(),
                )
            }))
            .expect("tree is well-formed"),
        )
        .view()
    }

    fn plan(text: &str) -> ResourcePlan {
        let manifest = manifest(text);
        let features = manifest
            .resolve_build_features(&[], false, false)
            .expect("selection is declared");
        ResourcePlan::lower(&manifest, &features)
    }

    const PACKAGE: &str = "[package]\nname = \"hellomod\"\nversion = \"0.1.0\"\n";

    #[test]
    fn text_with_no_tags_renders_to_itself() {
        // The property the whole default rests on: a resource nobody declared is not merely copied
        // by a different code path, it is text this engine leaves alone.
        let context = context(PACKAGE, &[]);
        for source in [
            "{\n  \"a\": { \"b\": 1 }\n}\n",
            "<?xml version=\"1.0\"?>\n<config><item/></config>\n",
            "\u{feff}{ \"bom\": true }\n",
            "a\r\nb\r\n",
            "{ } { {  }",
            "",
        ] {
            assert_eq!(
                render(source, &context).as_deref(),
                Ok(source),
                "{source:?}"
            );
        }
    }

    #[test]
    fn package_metadata_renders_and_an_unset_key_needs_a_default() {
        let full = context(PACKAGE, &[]);
        assert_eq!(
            render("{{ package.name }}-{{ package.version }}", &full).as_deref(),
            Ok("hellomod-0.1.0")
        );
        // Both spellings reach the same value.
        assert_eq!(
            render("{{ package[\"name\"] }}", &full).as_deref(),
            Ok("hellomod")
        );

        // A `[package]` key that is declared but unset is known and absent: emitting it is an
        // error, testing it is false, and `default` is how the intentional case is written.
        let bare = context("[package]\n", &[]);
        assert_eq!(
            kind("{{ package.version }}", &bare),
            TemplateErrorKind::UndefinedValue
        );
        assert_eq!(
            render("{{ package.version | default(\"0.0.0\") }}", &bare).as_deref(),
            Ok("0.0.0")
        );
        assert_eq!(
            render(
                "{% if package.version %}set{% else %}unset{% endif %}",
                &bare
            )
            .as_deref(),
            Ok("unset")
        );
    }

    #[test]
    fn a_typo_is_an_error_and_names_where_it_is() {
        let context = context(PACKAGE, &[]);
        assert_eq!(
            kind("{{ package.licence }}", &context),
            TemplateErrorKind::UnknownField {
                root: "package".to_owned(),
                field: "licence".to_owned(),
            }
        );
        // Environment variables are out of scope, so `env` is refused rather than empty.
        assert_eq!(
            kind("{{ env.HOME }}", &context),
            TemplateErrorKind::UnknownRoot {
                name: "env".to_owned()
            }
        );
        // A namespace has no text form.
        assert_eq!(
            kind("{{ package }}", &context),
            TemplateErrorKind::NotAScalar
        );

        let error = render("a\nb\n  {{ nope }}\n", &context).expect_err("unknown root");
        assert_eq!(error.at, Position { line: 3, column: 3 });
        assert_eq!(
            error.to_string(),
            "line 3, column 3: unknown name `nope`; a template can read `features` and `package`"
        );
    }

    #[test]
    fn features_answer_membership_for_any_name() {
        let context = context(PACKAGE, &["server", "1.20.1", "mixin-extras"]);
        assert_eq!(
            render("{{ features.server }}", &context).as_deref(),
            Ok("true")
        );
        // A name that is not active is `false`, never an error: features are additive, so "is X
        // on" is a well-formed question about any name at all.
        assert_eq!(
            render("{{ features.client }}", &context).as_deref(),
            Ok("false")
        );
        // The bracket spelling is not sugar. `1.20.1` and `mixin-extras` are real feature names in
        // `examples/minecraft_mod/jals.toml`, and neither is a name `a.b` can carry.
        assert_eq!(
            render(
                "{{ features[\"1.20.1\"] }} {{ features[\"mixin-extras\"] }}",
                &context
            )
            .as_deref(),
            Ok("true true")
        );
        assert_eq!(
            render(
                "{% if features[\"1.21\"] %}y{% else %}n{% endif %}",
                &context
            )
            .as_deref(),
            Ok("n")
        );
    }

    #[test]
    fn conditionals_take_exactly_one_arm() {
        let source = "{% if features.a %}A{% elif features.b %}B{% else %}C{% endif %}";
        assert_eq!(
            render(source, &context(PACKAGE, &["b"])).as_deref(),
            Ok("B")
        );
        assert_eq!(
            render(source, &context(PACKAGE, &["a", "b"])).as_deref(),
            Ok("A")
        );
        assert_eq!(render(source, &context(PACKAGE, &[])).as_deref(), Ok("C"));

        // `not`, `and`, `or`, and parentheses.
        let only_a = context(PACKAGE, &["a"]);
        assert_eq!(
            render(
                "{% if not features.b and features.a %}y{% endif %}",
                &only_a
            )
            .as_deref(),
            Ok("y")
        );
        assert_eq!(
            render(
                "{% if (features.b or features.a) and not features.c %}y{% endif %}",
                &only_a
            )
            .as_deref(),
            Ok("y")
        );
    }

    #[test]
    fn a_block_tag_alone_on_its_line_takes_the_line_with_it() {
        // Without this rule every `{% if %}` in a JSON resource leaves a blank line behind, so the
        // rendered file differs from a hand-written one by whitespace nobody asked for.
        let source =
            "{\n{% if features.server %}\n  \"env\": \"server\",\n{% endif %}\n  \"x\": 1\n}\n";
        assert_eq!(
            render(source, &context(PACKAGE, &["server"])).as_deref(),
            Ok("{\n  \"env\": \"server\",\n  \"x\": 1\n}\n")
        );
        assert_eq!(
            render(source, &context(PACKAGE, &[])).as_deref(),
            Ok("{\n  \"x\": 1\n}\n")
        );

        // Sharing a line with anything at all switches the rule off, so text around a tag is never
        // eaten by surprise.
        assert_eq!(
            render(
                "a {% if features.server %}b{% endif %} c",
                &context(PACKAGE, &["server"])
            )
            .as_deref(),
            Ok("a b c")
        );

        // A comment is a block tag for this purpose, and disappears either way.
        assert_eq!(
            render("a\n{# gone #}\nb\n", &context(PACKAGE, &[])).as_deref(),
            Ok("a\nb\n")
        );
        assert_eq!(
            render("a {# gone #} b", &context(PACKAGE, &[])).as_deref(),
            Ok("a  b")
        );
    }

    #[test]
    fn a_string_literal_is_how_a_delimiter_is_written() {
        let context = context(PACKAGE, &[]);
        assert_eq!(render("{{ \"{{\" }}", &context).as_deref(), Ok("{{"));
        // The closing scan has to know about strings, or this tag ends inside the literal.
        assert_eq!(render("{{ \"}}\" }}", &context).as_deref(), Ok("}}"));
        assert_eq!(
            render("{{ \"{%\" }}{{ \"%}\" }}", &context).as_deref(),
            Ok("{%%}")
        );
        assert_eq!(
            render("{{ \"a\\\"b\\\\c\" }}", &context).as_deref(),
            Ok("a\"b\\c")
        );
    }

    #[test]
    fn a_loop_walks_the_feature_set_in_sorted_order() {
        let context = context(PACKAGE, &["server", "a", "mixin"]);
        assert_eq!(
            render(
                "{% for f in features %}{{ f }}{% if not loop.last %},{% endif %}{% endfor %}",
                &context
            )
            .as_deref(),
            Ok("a,mixin,server")
        );
        assert_eq!(
            render(
                "{% for f in features %}{{ loop.index }}/{{ loop.length }}{% endfor %}",
                &context
            )
            .as_deref(),
            Ok("1/32/33/3")
        );
        // Only the feature set is iterable; `package` is a namespace, not a sequence.
        assert_eq!(
            kind("{% for x in package %}{{ x }}{% endfor %}", &context),
            TemplateErrorKind::NotIterable
        );
    }

    #[test]
    fn a_malformed_template_says_where_and_why() {
        let context = context(PACKAGE, &[]);
        assert_eq!(
            kind("a {{ b", &context),
            TemplateErrorKind::Unclosed { opener: "{{" }
        );
        assert_eq!(
            kind("{% if features.a %}x", &context),
            TemplateErrorKind::UnclosedBlock { tag: "if" }
        );
        assert_eq!(
            kind("{% endif %}", &context),
            TemplateErrorKind::UnexpectedTag {
                tag: "endif".to_owned()
            }
        );
        assert_eq!(
            kind("{% while features.a %}{% endwhile %}", &context),
            TemplateErrorKind::UnknownTag {
                tag: "while".to_owned()
            }
        );
        assert_eq!(
            kind("{% if features.a %}x{% endfor %}", &context),
            TemplateErrorKind::UnexpectedTag {
                tag: "endfor".to_owned()
            }
        );
        assert_eq!(
            kind("{{ package.name | upper }}", &context),
            TemplateErrorKind::UnknownFilter {
                name: "upper".to_owned()
            }
        );

        let error = render("ok\n{% if %}\n{% endif %}\n", &context).expect_err("no condition");
        assert_eq!(error.at, Position { line: 2, column: 1 });
    }

    #[test]
    fn deeply_nested_blocks_are_refused_rather_than_recursed() {
        // Malformed input must never take the process down with it.
        let mut source = String::new();
        for _ in 0..80 {
            source.push_str("{% if features.a %}");
        }
        for _ in 0..80 {
            source.push_str("{% endif %}");
        }
        assert_eq!(
            kind(&source, &context(PACKAGE, &[])),
            TemplateErrorKind::TooDeep
        );
    }

    const DECLARED: &str = "[package]\nname = \"hellomod\"\nversion = \"0.1.0\"\n\
                            [features]\nserver = []\n\
                            [build.resources]\ntemplate = [\"meta.json\", \"cfg/*.xml\"]\n";

    #[test]
    fn only_declared_resources_are_rendered() {
        let plan = plan(DECLARED);
        let entries = plan
            .entries(&view(&[
                (
                    "src/main/resources/meta.json",
                    b"{\"v\":\"{{ package.version }}\"}",
                ),
                ("src/main/resources/cfg/a.xml", b"<v>{{ package.name }}</v>"),
                // Undeclared, and it contains `{{` on purpose: selection is by declaration, never
                // by content, so this has to come back untouched.
                ("src/main/resources/keep.txt", b"{{ package.version }}"),
                (
                    "src/main/resources/icon.png",
                    &[0x89, b'P', b'N', b'G', 0xff],
                ),
            ]))
            .expect("every declaration matches");
        let rendered: Vec<(String, Vec<u8>)> = entries
            .into_iter()
            .map(|(path, bytes)| (path.to_string(), bytes))
            .collect();
        assert_eq!(
            rendered,
            alloc::vec![
                ("cfg/a.xml".to_owned(), b"<v>hellomod</v>".to_vec()),
                (
                    "icon.png".to_owned(),
                    alloc::vec![0x89, b'P', b'N', b'G', 0xff]
                ),
                ("keep.txt".to_owned(), b"{{ package.version }}".to_vec()),
                ("meta.json".to_owned(), b"{\"v\":\"0.1.0\"}".to_vec()),
            ]
        );
    }

    #[test]
    fn a_declaration_that_matches_nothing_fails() {
        // A `resource-dirs` entry that is not there is tolerated because the default lands on every
        // project. A pattern here was written on purpose, so a typo is a failure rather than a file
        // that quietly ships unrendered.
        let error = plan("[build.resources]\ntemplate = [\"typo.json\"]\n")
            .entries(&view(&[("src/main/resources/meta.json", b"{}")]))
            .expect_err("the pattern matches nothing");
        assert!(error.contains("`typo.json` matched no file"), "{error}");

        // No resource directory at all is a different fix, so it is a different sentence.
        let error = plan("[build.resources]\ntemplate = [\"typo.json\"]\n")
            .entries(&view(&[("src/main/java/A.java", b"class A {}")]))
            .expect_err("there is nowhere to match");
        assert!(
            error.contains("no `[build] resource-dirs` directory exists"),
            "{error}"
        );
    }

    #[test]
    fn a_declared_resource_that_is_not_text_fails() {
        let error = plan("[build.resources]\ntemplate = [\"blob.bin\"]\n")
            .entries(&view(&[("src/main/resources/blob.bin", &[0xff, 0xfe])]))
            .expect_err("a template has to be text");
        assert!(error.contains("is not UTF-8"), "{error}");
    }

    #[test]
    fn a_render_failure_names_the_resource() {
        let error = plan("[build.resources]\ntemplate = [\"meta.json\"]\n")
            .entries(&view(&[("src/main/resources/meta.json", b"{{ nope }}")]))
            .expect_err("the template does not render");
        assert_eq!(
            error,
            "`meta.json`: line 1, column 1: unknown name `nope`; a template can read `features` \
             and `package`"
        );
    }

    #[test]
    fn a_missing_resource_directory_is_skipped_in_silence() {
        // The existing behaviour, unchanged: the default lands on every project, and a project with
        // no resources is not a project with a mistake.
        assert!(
            plan("[package]\nname = \"x\"\n")
                .entries(&view(&[("src/main/java/A.java", b"class A {}")]))
                .expect("nothing declared, nothing to fail")
                .is_empty()
        );
    }
}
