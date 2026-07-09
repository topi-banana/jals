//! Generation of `jals-syntax/src/ast/generated.rs` from `jals-syntax/java.ungram`.
//!
//! The grammar drives the typed AST: every rule becomes a node struct (its
//! `SyntaxKind` is the SCREAMING_SNAKE_CASE form of the rule name) or, when the
//! rule is an alternation of plain node references, an enum over those kinds.
//! Only *labeled* grammar elements generate accessors; the four accessor forms
//! are documented in the header of `java.ungram`. Anything that does not fit
//! them is hand-written in `jals-syntax/src/ast/ext.rs`.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use ungrammar::{Grammar, Rule};

/// Path of the grammar, relative to the project root.
const GRAMMAR: &str = "jals-syntax/java.ungram";
/// Path of the generated file, relative to the project root.
const TARGET: &str = "jals-syntax/src/ast/generated.rs";

/// Renders the generated file and writes it; with `check`, renders to memory and
/// fails if the committed file differs.
pub(crate) fn run(check: bool) -> Result<()> {
    let root = project_root();
    let grammar_path = root.join(GRAMMAR);
    let grammar_text = fs::read_to_string(&grammar_path)
        .with_context(|| format!("failed to read {}", grammar_path.display()))?;
    let grammar: Grammar = grammar_text
        .parse()
        .with_context(|| format!("failed to parse {}", grammar_path.display()))?;
    let code = generate(&grammar)?;

    let target = root.join(TARGET);
    if check {
        // A missing file is "stale"; any other read failure is a real error and
        // must not masquerade as the out-of-date message.
        let committed = match fs::read_to_string(&target) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", target.display()));
            }
        };
        ensure!(
            committed == code,
            "{} is out of date; run `cargo run -p xtask -- codegen`",
            target.display()
        );
    } else {
        fs::write(&target, code)
            .with_context(|| format!("failed to write {}", target.display()))?;
    }
    Ok(())
}

/// The workspace root (the parent of the `xtask` crate).
fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives directly under the project root")
        .to_path_buf()
}

// ===== Lowering (ungrammar -> AST description) =====

/// One generated item, in grammar order.
enum Item {
    Node(NodeSrc),
    Enum(EnumSrc),
}

/// A node struct wrapping a single `SyntaxKind`.
struct NodeSrc {
    name: String,
    /// The `SyntaxKind` variant name (`ClassDecl` -> `CLASS_DECL`).
    kind: String,
    accessors: Vec<Accessor>,
}

/// An enum over several node kinds (a rule that is an alternation of nodes).
struct EnumSrc {
    name: String,
    /// `(variant name, node name, SyntaxKind name)` per alternative, in order.
    variants: Vec<(String, String, String)>,
}

/// A generated accessor (one labeled grammar element).
struct Accessor {
    label: String,
    kind: AccessorKind,
}

enum AccessorKind {
    /// `foo:Bar` — the first child castable to `Bar`.
    OptionalNode(String),
    /// `foo:Bar*` or a labeled group such as `foo:(Bar (',' Bar)*)` — all children.
    ManyNodes(String),
    /// `is_x:'kw'` — presence of the token (the mapped `SyntaxKind` name).
    TokenFlag(&'static str),
    /// `foo:'ident'` — text of the first `IDENT` token child.
    NameText,
}

impl Accessor {
    /// The node type this accessor selects children by, if it is a node accessor.
    fn target_node(&self) -> Option<&str> {
        match &self.kind {
            AccessorKind::OptionalNode(node) | AccessorKind::ManyNodes(node) => Some(node),
            AccessorKind::TokenFlag(_) | AccessorKind::NameText => None,
        }
    }
}

fn lower(grammar: &Grammar) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for node in grammar.iter() {
        let data = &grammar[node];
        let name = data.name.clone();
        if let Some(node_names) = enum_variants(grammar, &data.rule) {
            let variants: Vec<_> = node_names
                .into_iter()
                .map(|node_name| {
                    let kind = screaming_snake(&node_name);
                    (variant_name(&name, &node_name), node_name, kind)
                })
                .collect();
            // Suffix stripping can collide (e.g. a future `Class` next to
            // `ClassDecl`); fail here rather than with an opaque rustc error
            // inside the do-not-edit generated file.
            for (i, (variant, node, _)) in variants.iter().enumerate() {
                if let Some((_, earlier, _)) = variants[..i].iter().find(|(v, _, _)| v == variant) {
                    bail!(
                        "`{name}`: alternatives `{earlier}` and `{node}` both name \
                         their variant `{variant}`; rename a node in java.ungram"
                    );
                }
            }
            items.push(Item::Enum(EnumSrc { name, variants }));
        } else {
            let mut accessors = Vec::new();
            collect_accessors(grammar, &name, &data.rule, &mut accessors)?;
            check_conflicts(&name, &accessors)?;
            let kind = screaming_snake(&name);
            items.push(Item::Node(NodeSrc {
                name,
                kind,
                accessors,
            }));
        }
    }
    Ok(items)
}

/// If `rule` is an alternation of plain node references, returns the node names.
fn enum_variants(grammar: &Grammar, rule: &Rule) -> Option<Vec<String>> {
    let Rule::Alt(alternatives) = rule else {
        return None;
    };
    alternatives
        .iter()
        .map(|alt| match alt {
            Rule::Node(node) => Some(grammar[*node].name.clone()),
            _ => None,
        })
        .collect()
}

/// Variant naming: strip the enum name as a suffix (`ClassDecl` -> `Class` in
/// `Decl`, `ExprStmt` -> `Expr` in `Stmt`); failing that, strip a `Decl` suffix
/// (`FieldDecl` -> `Field` in `Member`); otherwise use the node name verbatim
/// (`Block` in `Stmt`, `FieldAccess` in `Expr`).
fn variant_name(enum_name: &str, node_name: &str) -> String {
    for suffix in [enum_name, "Decl"] {
        if let Some(stripped) = node_name.strip_suffix(suffix)
            && !stripped.is_empty()
        {
            return stripped.to_owned();
        }
    }
    node_name.to_owned()
}

/// Walks `rule` and lowers every labeled element into an accessor.
fn collect_accessors(
    grammar: &Grammar,
    owner: &str,
    rule: &Rule,
    out: &mut Vec<Accessor>,
) -> Result<()> {
    match rule {
        Rule::Labeled { label, rule } => out.push(lower_labeled(grammar, owner, label, rule)?),
        Rule::Node(_) | Rule::Token(_) => {}
        Rule::Seq(rules) | Rule::Alt(rules) => {
            for rule in rules {
                collect_accessors(grammar, owner, rule, out)?;
            }
        }
        Rule::Opt(rule) | Rule::Rep(rule) => collect_accessors(grammar, owner, rule, out)?,
    }
    Ok(())
}

fn lower_labeled(grammar: &Grammar, owner: &str, label: &str, rule: &Rule) -> Result<Accessor> {
    let mut shape = Shape::default();
    flatten(grammar, rule, &mut shape);
    ensure!(
        !shape.has_label,
        "`{owner}`: label `{label}` must not contain nested labels"
    );

    let kind = if shape.nodes.is_empty() {
        // Token accessor: `is_x:'kw'` or `foo:'ident'`.
        ensure!(
            shape.tokens.len() == 1,
            "`{owner}`: label `{label}` must cover exactly one token"
        );
        let token = shape.tokens[0].as_str();
        if label.starts_with("is_") {
            let kind = token_kind(token).with_context(|| {
                format!("`{owner}`: no SyntaxKind mapping for labeled token `'{token}'`")
            })?;
            AccessorKind::TokenFlag(kind)
        } else if token == "ident" {
            AccessorKind::NameText
        } else {
            bail!(
                "`{owner}`: label `{label}` on token `'{token}'` fits no accessor form \
                 (use an `is_` label for presence checks, or `'ident'` for name text)"
            );
        }
    } else {
        // Node accessor: all referenced nodes must agree on one type.
        ensure!(
            !label.starts_with("is_"),
            "`{owner}`: label `{label}` looks like a token flag but covers a node"
        );
        let first = shape.nodes[0].clone();
        ensure!(
            shape.nodes.iter().all(|node| *node == first),
            "`{owner}`: label `{label}` mixes node types"
        );
        if shape.has_rep || shape.nodes.len() > 1 {
            AccessorKind::ManyNodes(first)
        } else {
            AccessorKind::OptionalNode(first)
        }
    };
    Ok(Accessor {
        label: label.to_owned(),
        kind,
    })
}

/// What a labeled subtree contains, ignoring sequencing/alternation structure.
#[derive(Default)]
struct Shape {
    nodes: Vec<String>,
    tokens: Vec<String>,
    has_rep: bool,
    has_label: bool,
}

fn flatten(grammar: &Grammar, rule: &Rule, shape: &mut Shape) {
    match rule {
        Rule::Labeled { .. } => shape.has_label = true,
        Rule::Node(node) => shape.nodes.push(grammar[*node].name.clone()),
        Rule::Token(token) => shape.tokens.push(grammar[*token].name.clone()),
        Rule::Seq(rules) | Rule::Alt(rules) => {
            for rule in rules {
                flatten(grammar, rule, shape);
            }
        }
        Rule::Opt(rule) => flatten(grammar, rule, shape),
        Rule::Rep(rule) => {
            shape.has_rep = true;
            flatten(grammar, rule, shape);
        }
    }
}

/// Maps a *labeled* grammar token to the `SyntaxKind` name used by its accessor.
/// Unlabeled tokens are documentation only and are never consulted here.
fn token_kind(token: &str) -> Option<&'static str> {
    let kind = match token {
        // `'ident'` is deliberately absent: an `is_x:'ident'` flag would be true
        // for nearly every node, so it must fail loudly (use `x:'ident'` instead).
        "static" => "STATIC_KW",
        "module" => "MODULE_KW",
        "open" => "OPEN_KW",
        "transitive" => "TRANSITIVE_KW",
        "sealed" => "SEALED_KW",
        "default" => "DEFAULT_KW",
        "*" => "STAR",
        _ => return None,
    };
    Some(kind)
}

/// Generated accessors select children purely by node type (`support::child` /
/// `support::children`), so two labels with the same target on one node are
/// indistinguishable — such accessors must be hand-written in `ast/ext.rs`.
fn check_conflicts(owner: &str, accessors: &[Accessor]) -> Result<()> {
    for (i, accessor) in accessors.iter().enumerate() {
        for earlier in &accessors[..i] {
            ensure!(
                accessor.label != earlier.label,
                "`{owner}`: duplicate label `{}`",
                accessor.label
            );
            if let (Some(a), Some(b)) = (accessor.target_node(), earlier.target_node()) {
                ensure!(
                    a != b,
                    "`{owner}`: labels `{}` and `{}` both target `{a}`; \
                     implement one of them by hand in jals-syntax/src/ast/ext.rs",
                    earlier.label,
                    accessor.label
                );
            }
        }
    }
    Ok(())
}

/// `ClassDecl` -> `CLASS_DECL` (the rule-name -> `SyntaxKind` convention).
fn screaming_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.char_indices() {
        if i != 0 && ch.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(ch.to_ascii_uppercase());
    }
    out
}

// ===== Rendering =====

fn generate(grammar: &Grammar) -> Result<String> {
    let items = lower(grammar)?;
    let mut buf = String::new();
    buf.push_str(
        "//! GENERATED by `cargo run -p xtask -- codegen` from `java.ungram` — do not edit.\n\n",
    );
    buf.push_str("// @generated\n\n");
    buf.push_str("use alloc::string::String;\n\n");
    buf.push_str("use rowan::ast::{AstChildren, AstNode, support};\n\n");
    buf.push_str("use super::name_text;\n");
    buf.push_str("use crate::language::{JavaLanguage, SyntaxNode};\n");
    // An explicit import list rather than a glob: a `use …::*` would trip
    // `clippy::enum_glob_use`. rustfmt sorts and wraps the names, so the exact
    // order emitted here does not matter.
    let _ = writeln!(
        buf,
        "use crate::syntax_kind::SyntaxKind::{{self, {}}};",
        referenced_kinds(&items).join(", ")
    );
    for item in &items {
        match item {
            Item::Node(node) => render_node(&mut buf, node),
            Item::Enum(enm) => render_enum(&mut buf, enm),
        }
    }
    reformat(&buf)
}

/// Every `SyntaxKind` variant the generated code names, deduplicated. Feeds the
/// explicit `use crate::syntax_kind::SyntaxKind::{…}` import (see `generate`).
fn referenced_kinds(items: &[Item]) -> Vec<String> {
    let mut kinds = std::collections::BTreeSet::new();
    for item in items {
        match item {
            Item::Node(node) => {
                kinds.insert(node.kind.clone());
                for accessor in &node.accessors {
                    if let AccessorKind::TokenFlag(kind) = &accessor.kind {
                        kinds.insert((*kind).to_owned());
                    }
                }
            }
            Item::Enum(enm) => {
                for (_, _, kind) in &enm.variants {
                    kinds.insert(kind.clone());
                }
            }
        }
    }
    kinds.into_iter().collect()
}

fn render_node(buf: &mut String, node: &NodeSrc) {
    let NodeSrc {
        name,
        kind,
        accessors,
    } = node;
    let _ = writeln!(
        buf,
        "\n\
         #[derive(Debug, Clone, PartialEq, Eq, Hash)]\n\
         #[repr(transparent)]\n\
         pub struct {name} {{ pub(crate) syntax: SyntaxNode }}"
    );
    if !accessors.is_empty() {
        let _ = writeln!(buf, "\nimpl {name} {{");
        for accessor in accessors {
            let label = &accessor.label;
            let _ = match &accessor.kind {
                AccessorKind::OptionalNode(target) => writeln!(
                    buf,
                    "pub fn {label}(&self) -> Option<{target}> {{ support::child(&self.syntax) }}"
                ),
                AccessorKind::ManyNodes(target) => writeln!(
                    buf,
                    "pub fn {label}(&self) -> AstChildren<{target}> {{ support::children(&self.syntax) }}"
                ),
                AccessorKind::TokenFlag(kind) => writeln!(
                    buf,
                    "pub fn {label}(&self) -> bool {{ support::token(&self.syntax, {kind}).is_some() }}"
                ),
                AccessorKind::NameText => writeln!(
                    buf,
                    "pub fn {label}(&self) -> Option<String> {{ name_text(&self.syntax) }}"
                ),
            };
        }
        buf.push_str("}\n");
    }
    let _ = writeln!(
        buf,
        "\nimpl AstNode for {name} {{\n\
         type Language = JavaLanguage;\n\
         fn can_cast(kind: SyntaxKind) -> bool {{ kind == {kind} }}\n\
         fn cast(syntax: SyntaxNode) -> Option<Self> {{\n\
         if Self::can_cast(syntax.kind()) {{ Some(Self {{ syntax }}) }} else {{ None }}\n\
         }}\n\
         fn syntax(&self) -> &SyntaxNode {{ &self.syntax }}\n\
         }}"
    );
}

fn render_enum(buf: &mut String, enm: &EnumSrc) {
    let name = &enm.name;
    let _ = writeln!(
        buf,
        "\n#[derive(Debug, Clone, PartialEq, Eq, Hash)]\npub enum {name} {{"
    );
    for (variant, node, _) in &enm.variants {
        let _ = writeln!(buf, "{variant}({node}),");
    }
    buf.push_str("}\n");

    let kinds = enm
        .variants
        .iter()
        .map(|(_, _, kind)| kind.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    let _ = writeln!(
        buf,
        "\nimpl AstNode for {name} {{\n\
         type Language = JavaLanguage;\n\
         fn can_cast(kind: SyntaxKind) -> bool {{ matches!(kind, {kinds}) }}\n\
         fn cast(syntax: SyntaxNode) -> Option<Self> {{\n\
         let res = match syntax.kind() {{"
    );
    for (variant, node, kind) in &enm.variants {
        let _ = writeln!(buf, "{kind} => Self::{variant}({node} {{ syntax }}),");
    }
    buf.push_str("_ => return None,\n};\nSome(res)\n}\n");
    buf.push_str("fn syntax(&self) -> &SyntaxNode {\nmatch self {\n");
    for (variant, _, _) in &enm.variants {
        let _ = writeln!(buf, "Self::{variant}(it) => it.syntax(),");
    }
    buf.push_str("}\n}\n}\n");
}

/// Pipes `text` through `rustfmt --edition 2024` so the committed file satisfies
/// `cargo fmt --all --check`. Runs from the project root so config discovery
/// finds the repo's `rustfmt.toml` and stops there (a user-level
/// `~/.rustfmt.toml` would otherwise make the output machine-dependent).
fn reformat(text: &str) -> Result<String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .current_dir(project_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn `rustfmt` (is it installed?)")?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(text.as_bytes())
        .context("failed to write to rustfmt's stdin")?;
    let output = child
        .wait_with_output()
        .context("failed to wait for rustfmt")?;
    ensure!(
        output.status.success(),
        "rustfmt failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("rustfmt produced non-UTF-8 output")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grammar() -> Grammar {
        let path = project_root().join(GRAMMAR);
        fs::read_to_string(&path).unwrap().parse().unwrap()
    }

    /// Every node rule's SCREAMING_SNAKE_CASE name must exist as a `SyntaxKind`
    /// variant. This is a friendly early sync check; the hard enforcement is
    /// that `generated.rs` does not compile when a kind is missing.
    #[test]
    fn node_kinds_exist_in_syntax_kind() {
        let syntax_kind =
            fs::read_to_string(project_root().join("jals-syntax/src/syntax_kind.rs")).unwrap();
        for item in lower(&grammar()).unwrap() {
            if let Item::Node(node) = item {
                assert!(
                    syntax_kind.contains(&format!(" {},", node.kind)),
                    "`{}` needs `SyntaxKind::{}` in jals-syntax/src/syntax_kind.rs",
                    node.name,
                    node.kind,
                );
            }
        }
    }

    /// The grammar must lower without errors and produce both structs and enums.
    /// (Formatting and freshness are exercised by `codegen --check` in CI.)
    #[test]
    fn grammar_lowers() {
        let items = lower(&grammar()).unwrap();
        assert!(items.iter().any(|item| matches!(item, Item::Node(_))));
        assert!(items.iter().any(|item| matches!(item, Item::Enum(_))));
    }
}
