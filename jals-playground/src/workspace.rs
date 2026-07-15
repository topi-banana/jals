//! A minimal in-browser workspace: several Java files held in an in-memory tree, with a
//! cross-file symbol index computed on demand.
//!
//! This is the wasm-compatible core of what `jals-lsp`'s host-only `Workspace` wraps — an
//! [`InMemoryFileTree`] as the single source of truth and a [`ProjectIndex`] built over every
//! file — with the LSP-specific plumbing (async, URIs, classpath/dependency I/O) left out. It is
//! deliberately Yew-agnostic so the UI layer stays thin.

use core::ops::Range;

use jals_config::Severity;
use jals_config::fmt::Config as FmtConfig;
use jals_config::lint::Config as LintConfig;
use jals_editor::{CompletionKind, FileRange, ProjectQueries, QueryFile};
use jals_fmt::FormatOutput;
use jals_fs::{FileTree, InMemoryFileTree};
use jals_hir::{DefKind, FileId, LoweredClasspath, ProjectIndex, Resolved};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{Parse, SyntaxNode};
use text_size::TextRange;

use crate::line_index::LineIndex;

/// One diagnostic over the active file, in Monaco coordinates — the playground's neutral shape,
/// marshalled straight into a Monaco marker by the UI layer. Aggregates syntax errors, lint rule
/// findings (including the cross-file `type-mismatch`), and cross-file unresolved type names.
pub struct PlaygroundDiagnostic {
    /// Range in Monaco coordinates (one-based UTF-16, both ends).
    pub range: MonacoRange,
    /// Human-readable message.
    pub message: String,
    /// Resolved severity.
    pub severity: Severity,
}

/// A range in Monaco coordinates — one-based line and one-based UTF-16 column, both ends. The
/// neutral shape the language-feature methods return; the UI layer marshals it into Monaco's
/// `IRange`. Produced from a byte range by [`RangeMapper::range`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MonacoRange {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// A navigation target: a workspace file path plus a range within it. Returned by
/// [`Workspace::goto_definition`] and, one per occurrence, by [`Workspace::references`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    pub path: String,
    pub range: MonacoRange,
}

/// One node of the document-symbol outline: a named declaration, its full range, and its members.
/// `kind` is a [`DefKind`]; the UI maps it to a Monaco `SymbolKind`.
pub struct SymbolNode {
    pub name: String,
    pub kind: DefKind,
    pub range: MonacoRange,
    pub children: Vec<SymbolNode>,
}

/// One occurrence highlight: its range and whether it is a write (declaration/binding name or a
/// mutating use) as opposed to a read.
#[derive(Debug)]
pub struct Highlight {
    pub range: MonacoRange,
    pub write: bool,
}

/// One completion candidate: its label, its [`DefKind`] (driving the editor icon; ignored when
/// `keyword`), the detail shown beside it, and whether it is a Java keyword rather than a semantic
/// binding.
pub struct CompletionEntry {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: String,
}

/// One signature in signature help: its rendered label and, per parameter, the `(start, end)`
/// UTF-16 code-unit offsets of that parameter's span within the label.
pub struct SigInfo {
    pub label: String,
    pub parameters: Vec<(u32, u32)>,
}

/// Signature help for a call: the overloads and which signature / parameter is active.
pub struct SigHelp {
    pub signatures: Vec<SigInfo>,
    pub active_signature: u32,
    pub active_parameter: u32,
}

/// The parsed, resolved, and indexed view of the workspace with the active file taken from the
/// editor's live text — the shared context every language-feature query is computed over. Every
/// field is owned, so the borrowed `files`/`parses` used to build it can be dropped.
struct ActiveContext {
    /// The active file's live source (the editor buffer), reused for offset/range mapping.
    source: String,
    /// The active file's parse (its `syntax()` is the analysis root).
    parse: Parse,
    /// File-local name resolution over the active file.
    resolved: Resolved,
    /// The cross-file symbol index over every file (with the embedded stdlib stubs).
    index: ProjectIndex,
    /// The active file's [`FileId`] (its index into the sorted path list).
    active_id: FileId,
    /// Every Java file path, sorted — a [`FileId`] is an index into this list. Computed once while
    /// building the index and kept so per-file lookups need not re-walk the tree.
    paths: Vec<String>,
}

/// Seed files, deliberately unformatted so the formatter has visible work to do, and
/// cross-referencing so the project index resolves `Main`'s use of `Greeter` across files.
const SAMPLE_FILES: &[(&str, &str)] = &[
    (
        "com/example/Greeter.java",
        "package com.example;\n\
         public class Greeter {\n\
         private final String name;\n\
         public Greeter(String name){this.name=name;}\n\
         public String greet(){return \"Hello, \"+name+\"!\";}\n\
         }\n",
    ),
    (
        "com/example/Main.java",
        "package com.example;\n\
         public class Main {\n\
         public static void main(String[] args){\n\
         String who=args.length>0?\"there\":\"world\";\n\
         Greeter g=new Greeter(who);\n\
         System.out.println(g.greet());\n\
         }\n\
         }\n",
    ),
];

/// Several Java files backed by an [`InMemoryFileTree`], plus the path of the active file.
///
/// `fs` is the single source of truth for the file set — the sorted path list and the active
/// file's contents are read back from it, so there is no parallel state to keep in sync.
pub struct Workspace {
    fs: InMemoryFileTree,
    /// Path of the active file — a key into `fs`, and the editor's backing store.
    active: String,
    /// The external classpath folded into every analysis, when a `[dependencies]` spec has been
    /// resolved (`None` until then). Lowered once from the downloaded `.class` files
    /// ([`ProjectIndex::lower_classpath`]) and reused across rebuilds, mirroring `jals-lsp`. Owned, so
    /// [`active_context`](Self::active_context) borrows it into the builder each time.
    classpath: Option<LoweredClasspath>,
}

impl Workspace {
    /// A workspace seeded with the [`SAMPLE_FILES`]; the first (sorted) file is active.
    pub fn new() -> Self {
        let fs = InMemoryFileTree::from_files(SAMPLE_FILES.iter().copied());
        // The first (sorted) Java file is active on load.
        let active = fs
            .walk_ext("", "java")
            .unwrap_or_default()
            .into_iter()
            .next()
            .unwrap_or_default();
        Workspace {
            fs,
            active,
            classpath: None,
        }
    }

    /// Replace the external classpath folded into analysis (from a resolved `[dependencies]` spec),
    /// or clear it with `None`. The next analysis picks it up — `Main`'s use of a library type then
    /// resolves through the downloaded `.class` files.
    pub fn set_classpath(&mut self, classpath: Option<LoweredClasspath>) {
        self.classpath = classpath;
    }

    /// Every Java file path, sorted. A path's index into this vec is its [`FileId`].
    fn paths(&self) -> Vec<String> {
        self.fs.walk_ext("", "java").unwrap_or_default()
    }

    /// The path of the active file.
    pub fn active(&self) -> &str {
        &self.active
    }

    /// Make `path` the active file, if it exists in the tree.
    pub fn set_active(&mut self, path: &str) {
        if self.fs.is_file(path) {
            self.active = path.to_string();
        }
    }

    /// The active file's current text (empty if it somehow cannot be read).
    pub fn active_source(&self) -> String {
        self.read(&self.active)
    }

    /// Overwrite the active file's contents (called on every editor keystroke).
    pub fn edit_active(&mut self, text: &str) {
        let _ = self.fs.write(&self.active, text.as_bytes());
    }

    /// Every Java file as `(path, text)`, sorted — for seeding the editor's per-file Monaco models.
    pub fn file_texts(&self) -> Vec<(String, String)> {
        self.paths()
            .into_iter()
            .map(|path| {
                let text = self.read(&path);
                (path, text)
            })
            .collect()
    }

    /// The immediate children of directory `dir`, as full paths, sorted (sidebar rendering).
    pub fn read_dir(&self, dir: &str) -> Vec<String> {
        self.fs.read_dir(dir).unwrap_or_default()
    }

    /// Whether `path` is a directory in the tree.
    pub fn is_dir(&self, path: &str) -> bool {
        self.fs.is_dir(path)
    }

    fn read(&self, path: &str) -> String {
        self.fs.read_to_string(path).unwrap_or_default()
    }

    /// Format the active file (file-local; no project index needed).
    pub fn format_active(&self, config: &FmtConfig) -> FormatOutput {
        jals_fmt::FormatOutput::format_source(&self.active_source(), config)
    }

    /// Parse the active file for the syntax-tree dump.
    pub fn syntax_active(&self) -> Parse {
        jals_syntax::Parse::parse(&self.active_source())
    }

    /// Build a [`ProjectIndex`] over *every* file (with the embedded stdlib stubs), analyse the
    /// active file across the whole workspace, and return its diagnostics already mapped to Monaco
    /// coordinates — so the UI layer only marshals them, never re-opening the byte↔position
    /// boundary this module owns.
    ///
    /// This is the payoff of a real workspace: `Main`'s reference to `Greeter` resolves through
    /// the *other* file's declaration, while a genuinely unknown type is reported. The result
    /// aggregates, over the active file:
    /// - cross-file unresolved type names (the "cannot resolve symbol" the LSP surfaces
    ///   separately from lint), as errors;
    /// - the parser's syntax errors; and
    /// - every enabled lint rule plus the index-aware cross-file `type-mismatch`.
    pub fn analyze_active(&self, config: &LintConfig) -> Vec<PlaygroundDiagnostic> {
        let ctx = self.active_context(&self.active_source());
        let mapper = RangeMapper::new(&ctx.source);

        let mut diags = Vec::new();

        // Cross-file unresolved type names.
        for range in ctx.index.unresolved_types(ctx.active_id, &ctx.resolved) {
            let name = ctx.source.get(range.clone()).unwrap_or("");
            diags.push(PlaygroundDiagnostic {
                message: format!("cannot resolve `{name}`"),
                range: mapper.range(&range),
                severity: Severity::Error,
            });
        }

        // The parser's syntax errors plus every enabled lint rule (and the index-aware cross-file
        // `type-mismatch`). `lint.parse_errors` already carries the syntax errors, so the raw
        // `Parse::errors` are not counted separately.
        let lint = jals_lint::LintOutput::lint_parse_with_index(
            &ctx.parse,
            config,
            Some((&ctx.index, ctx.active_id)),
        );
        for diag in lint.parse_errors.iter().chain(lint.diagnostics.iter()) {
            diags.push(PlaygroundDiagnostic {
                message: format!("{}: {}", diag.rule, diag.message),
                range: mapper.range(&diag.range),
                severity: diag.severity,
            });
        }

        diags
    }

    /// Build the shared analysis context over the whole workspace, taking the active file from
    /// `active_text` (the editor's live buffer) and every other file from the tree. Parses every
    /// file, builds a stdlib-folded [`ProjectIndex`], and resolves the active file. Every field of
    /// the result is owned, so the borrowed trees used to build the index are released here.
    fn active_context(&self, active_text: &str) -> ActiveContext {
        let paths = self.paths();
        let active_idx = paths.iter().position(|p| p == &self.active).unwrap_or(0);

        // Parse every file: the active one from the live editor text, the rest from the tree. A
        // path's index into `paths` is its `FileId`.
        let mut parses: Vec<(FileId, Parse)> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let parse = if i == active_idx {
                    jals_syntax::Parse::parse(active_text)
                } else {
                    jals_syntax::Parse::parse(&self.read(path))
                };
                (FileId(i as u32), parse)
            })
            .collect();
        let files: Vec<(FileId, SyntaxNode)> = parses
            .iter()
            .map(|(id, parse)| (*id, parse.syntax()))
            .collect();
        // Fold in the resolved external classpath (when a `[dependencies]` spec has been resolved),
        // so a library type resolves for hover / completion / type-checking, exactly as `jals-lsp`
        // folds its downloaded jars in.
        let mut builder = ProjectIndex::builder(&files).with_stdlib();
        if let Some(classpath) = &self.classpath {
            builder = builder.with_classpath(classpath);
        }
        let index = builder.build();
        // The built index owns its trees, so release the borrowed `files` before moving the active
        // parse out of `parses`.
        drop(files);

        let parse = parses.swap_remove(active_idx).1;
        let resolved = Resolved::resolve_node(&parse.syntax());
        ActiveContext {
            source: active_text.to_string(),
            parse,
            resolved,
            index,
            active_id: FileId(active_idx as u32),
            paths,
        }
    }

    /// Build the active-file analysis context (see [`active_context`](Self::active_context)) and, in
    /// the same pass, map the Monaco position `(line, col)` to a byte offset — the shared preamble of
    /// every position-based query that needs a one-off offset but not a reusable [`RangeMapper`].
    fn context_at(&self, active_text: &str, line: u32, col: u32) -> (ActiveContext, usize) {
        let ctx = self.active_context(active_text);
        let offset = RangeMapper::new(&ctx.source).offset(line, col);
        (ctx, offset)
    }

    /// The `(path, source)` of a target [`FileId`] — the active file's live buffer when it is the
    /// active file (so a same-file target maps against the unsaved edits), otherwise the tree's
    /// stored text. `None` if the id has no path (out of range).
    fn path_text(&self, file: FileId, active: &ActiveContext) -> Option<(String, String)> {
        let path = active.paths.get(file.0 as usize)?.clone();
        let text = if file == active.active_id {
            active.source.clone()
        } else {
            self.read(&path)
        };
        Some((path, text))
    }

    /// The hover for the cursor at the Monaco position `(line, col)` in the active file: the inferred
    /// type of the expression there, rendered as a Java code block, with reference type names
    /// resolved against the project. `None` if the expression has no useful inferred type.
    pub fn hover(&self, active_text: &str, line: u32, col: u32) -> Option<String> {
        let (ctx, offset) = self.context_at(active_text, line, col);
        let ty = ctx.queries().hover(offset)?;
        Some(format!("```java\n{ty}\n```"))
    }

    /// Completions for the cursor at `(line, col)` in the active file: the members after a `.`,
    /// otherwise the in-scope bindings and project types plus the Java keywords.
    pub fn completions(&self, active_text: &str, line: u32, col: u32) -> Vec<CompletionEntry> {
        let (ctx, offset) = self.context_at(active_text, line, col);
        ctx.queries()
            .completions(offset)
            .into_iter()
            .map(CompletionEntry::from_completion)
            .collect()
    }

    /// Signature help for the call at `(line, col)` in the active file, with cross-file type
    /// resolution. `None` if the cursor is in no resolvable call.
    pub fn signature_help(&self, active_text: &str, line: u32, col: u32) -> Option<SigHelp> {
        /// The number of UTF-16 code units in `s` (Monaco parameter offsets are counted in UTF-16).
        fn utf16_len(s: &str) -> u32 {
            s.encode_utf16().count() as u32
        }
        let (ctx, offset) = self.context_at(active_text, line, col);
        let help = ctx.queries().signature_help(offset)?;
        let signatures = help
            .signatures
            .iter()
            .map(|sig| SigInfo {
                label: sig.label.clone(),
                parameters: sig
                    .parameters
                    .iter()
                    // LSP/Monaco parameter offsets are counted in UTF-16 code units.
                    .map(|r| {
                        (
                            utf16_len(&sig.label[..r.start]),
                            utf16_len(&sig.label[..r.end]),
                        )
                    })
                    .collect(),
            })
            .collect();
        Some(SigHelp {
            signatures,
            active_signature: help.active_signature as u32,
            active_parameter: help.active_parameter as u32,
        })
    }

    /// The document-symbol outline of the active file (types with their members nested).
    pub fn document_symbols(&self, active_text: &str) -> Vec<SymbolNode> {
        let parse = jals_syntax::Parse::parse(active_text);
        let Some(file) = ast::SourceFile::cast(parse.syntax()) else {
            return Vec::new();
        };
        let mapper = RangeMapper::new(active_text);
        file.decls()
            .map(|decl| SymbolNode::for_decl(&decl, &mapper))
            .collect()
    }

    /// Occurrence highlights for the cursor at `(line, col)` in the active file: every occurrence of
    /// the binding under the cursor (a cross-file type resolved precisely through the index), else a
    /// lexical fallback over same-text identifiers. Empty if the cursor is not on an identifier.
    pub fn document_highlight(&self, active_text: &str, line: u32, col: u32) -> Vec<Highlight> {
        let ctx = self.active_context(active_text);
        let mapper = RangeMapper::new(&ctx.source);
        let offset = mapper.offset(line, col);
        ctx.queries()
            .highlights(offset)
            .into_iter()
            .map(|highlight| Highlight {
                range: mapper.range(&highlight.range),
                write: highlight.kind == jals_editor::HighlightKind::Write,
            })
            .collect()
    }

    /// Go-to-definition for the cursor at `(line, col)` in the active file: a file-local binding,
    /// then the project type a reference names, then — for a member access — the member the receiver
    /// type declares. `None` if nothing resolves.
    pub fn goto_definition(&self, active_text: &str, line: u32, col: u32) -> Option<Target> {
        let (ctx, offset) = self.context_at(active_text, line, col);
        let target = ctx.queries().definition(offset)?;
        let (path, text) = self.path_text(target.file, &ctx)?;
        Some(Target {
            path,
            range: RangeMapper::new(&text).range(&target.range),
        })
    }

    /// Find-references for the cursor at `(line, col)` in the active file: every occurrence of the
    /// symbol under the cursor — across the whole project when it is a project type, or within this
    /// one file for a file-local binding. The declaration is included when `include_declaration`.
    /// Empty if the cursor is on no resolvable symbol.
    pub fn references(
        &self,
        active_text: &str,
        line: u32,
        col: u32,
        include_declaration: bool,
    ) -> Vec<Target> {
        let ctx = self.active_context(active_text);
        let mapper = RangeMapper::new(&ctx.source);
        let offset = mapper.offset(line, col);
        // References are the sole project-wide query, so resolve the remaining files only here.
        let analyzed: Vec<_> = ctx
            .paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let file = FileId(index as u32);
                let (root, resolved) = if file == ctx.active_id {
                    (ctx.parse.syntax(), ctx.resolved.clone())
                } else {
                    let root = Parse::parse(&self.read(path)).syntax();
                    let resolved = Resolved::resolve_node(&root);
                    (root, resolved)
                };
                (file, root, resolved)
            })
            .collect();
        ctx.queries()
            .references(
                offset,
                include_declaration,
                analyzed
                    .iter()
                    .map(|(file, root, resolved)| QueryFile::new(*file, root.clone(), resolved)),
            )
            .into_iter()
            .filter_map(|target| self.target(&ctx, target))
            .collect()
    }

    fn target(&self, ctx: &ActiveContext, target: FileRange) -> Option<Target> {
        let (path, text) = self.path_text(target.file, ctx)?;
        Some(Target {
            path,
            range: RangeMapper::new(&text).range(&target.range),
        })
    }
}

/// Small CST helpers shared by the language-feature queries — token lookup and range conversion over
/// the parsed tree, with no natural owning type of their own.
struct Cst;

impl Cst {
    /// A `text_size::TextRange` as a plain `Range<usize>` of byte offsets.
    fn to_std_range(range: TextRange) -> Range<usize> {
        usize::from(range.start())..usize::from(range.end())
    }
}

/// Maps positions/ranges within one document between `jals` byte offsets and Monaco coordinates,
/// reusing a single precomputed [`LineIndex`] so a query that maps many ranges over the same text
/// scans it only once (rather than rebuilding the index per range).
struct RangeMapper<'a> {
    index: LineIndex,
    text: &'a str,
}

impl<'a> RangeMapper<'a> {
    fn new(text: &'a str) -> Self {
        RangeMapper {
            index: LineIndex::new(text),
            text,
        }
    }

    /// Byte offset of the Monaco position `(line, col)` (one-based UTF-16 coords).
    fn offset(&self, line: u32, col: u32) -> usize {
        self.index.offset(self.text, line, col)
    }

    /// Map a byte range to a [`MonacoRange`] (one-based UTF-16, both ends).
    fn range(&self, range: &Range<usize>) -> MonacoRange {
        let (sl, sc, el, ec) = self.index.to_monaco(self.text, range);
        MonacoRange {
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
        }
    }
}

impl CompletionEntry {
    /// Map a shared completion to the playground payload.
    fn from_completion(completion: jals_editor::Completion) -> Self {
        Self {
            label: completion.label,
            kind: completion.kind,
            detail: completion.detail,
        }
    }
}

impl ActiveContext {
    fn queries(&self) -> ProjectQueries<'_> {
        ProjectQueries::new(
            &self.index,
            QueryFile::new(self.active_id, self.parse.syntax(), &self.resolved),
        )
    }
}

impl SymbolNode {
    /// The document symbol for a top-level declaration. Mirrors `jals-lsp`'s `handlers/symbols.rs`,
    /// with the LSP `SymbolKind` replaced by a `DefKind` the UI maps.
    fn for_decl(decl: &ast::Decl, mapper: &RangeMapper<'_>) -> Self {
        match decl {
            ast::Decl::Class(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Class, d.body(), mapper)
            }
            ast::Decl::Interface(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Interface, d.body(), mapper)
            }
            ast::Decl::Record(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Record, d.body(), mapper)
            }
            ast::Decl::AnnotationType(d) => Self::for_type(
                d.syntax(),
                d.name(),
                DefKind::AnnotationType,
                d.body(),
                mapper,
            ),
            ast::Decl::Enum(d) => Self::for_enum(d, mapper),
            // Top-level field / method of a compact source file (JEP 512).
            ast::Decl::Field(d) => Self::leaf(d.syntax(), d.name(), DefKind::Field, mapper),
            ast::Decl::Method(d) => Self::leaf(d.syntax(), d.name(), DefKind::Method, mapper),
        }
    }

    /// The document symbol for a type member, or `None` for an unnamed initializer block.
    fn for_member(member: &ast::Member, mapper: &RangeMapper<'_>) -> Option<Self> {
        let sym = match member {
            ast::Member::Field(d) => Self::leaf(d.syntax(), d.name(), DefKind::Field, mapper),
            ast::Member::Method(d) => Self::leaf(d.syntax(), d.name(), DefKind::Method, mapper),
            ast::Member::Constructor(d) => {
                Self::leaf(d.syntax(), d.name(), DefKind::Constructor, mapper)
            }
            ast::Member::Initializer(_) => return None,
            ast::Member::Class(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Class, d.body(), mapper)
            }
            ast::Member::Interface(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Interface, d.body(), mapper)
            }
            ast::Member::Record(d) => {
                Self::for_type(d.syntax(), d.name(), DefKind::Record, d.body(), mapper)
            }
            ast::Member::AnnotationType(d) => Self::for_type(
                d.syntax(),
                d.name(),
                DefKind::AnnotationType,
                d.body(),
                mapper,
            ),
            ast::Member::Enum(d) => Self::for_enum(d, mapper),
        };
        Some(sym)
    }

    /// A type-like symbol (class/interface/record/annotation) whose children are its members.
    fn for_type(
        node: &SyntaxNode,
        name: Option<String>,
        kind: DefKind,
        body: Option<ast::ClassBody>,
        mapper: &RangeMapper<'_>,
    ) -> Self {
        let children = body
            .map(|b| {
                b.members()
                    .filter_map(|m| Self::for_member(&m, mapper))
                    .collect()
            })
            .unwrap_or_default();
        Self::new(node, name, kind, children, mapper)
    }

    /// An enum symbol, whose children are its constants followed by its members.
    fn for_enum(d: &ast::EnumDecl, mapper: &RangeMapper<'_>) -> Self {
        let children = d
            .body()
            .map(|b| {
                let constants = b
                    .constants()
                    .map(|c| Self::leaf(c.syntax(), c.name(), DefKind::EnumConstant, mapper));
                let members = b.members().filter_map(|m| Self::for_member(&m, mapper));
                constants.chain(members).collect()
            })
            .unwrap_or_default();
        Self::new(d.syntax(), d.name(), DefKind::Enum, children, mapper)
    }

    /// A symbol with no children.
    fn leaf(
        node: &SyntaxNode,
        name: Option<String>,
        kind: DefKind,
        mapper: &RangeMapper<'_>,
    ) -> Self {
        Self::new(node, name, kind, Vec::new(), mapper)
    }

    /// Assemble a [`SymbolNode`], mapping the node's byte range to Monaco coordinates.
    fn new(
        node: &SyntaxNode,
        name: Option<String>,
        kind: DefKind,
        children: Vec<Self>,
        mapper: &RangeMapper<'_>,
    ) -> Self {
        Self {
            name: name.unwrap_or_else(|| "<anonymous>".to_string()),
            kind,
            range: mapper.range(&Cst::to_std_range(node.text_range())),
            children,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_files_parse_clean() {
        for (path, contents) in SAMPLE_FILES {
            let parse = jals_syntax::Parse::parse(contents);
            assert!(
                parse.errors().is_empty(),
                "seed file {path} has syntax errors: {:?}",
                parse.errors()
            );
        }
    }

    #[test]
    fn tree_lists_the_package_then_the_files() {
        let ws = Workspace::new();
        assert_eq!(ws.read_dir(""), vec!["com".to_string()]);
        assert_eq!(ws.read_dir("com"), vec!["com/example".to_string()]);
        assert_eq!(
            ws.read_dir("com/example"),
            vec![
                "com/example/Greeter.java".to_string(),
                "com/example/Main.java".to_string(),
            ]
        );
        assert!(ws.is_dir("com/example"));
        assert!(!ws.is_dir("com/example/Main.java"));
        // The first sorted file is active on load.
        assert_eq!(ws.active(), "com/example/Greeter.java");
    }

    #[test]
    fn cross_file_reference_resolves_and_seed_is_clean() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        let diags = ws.analyze_active(&LintConfig::default());
        // `Greeter` (another file), `String`/`System` (stdlib stubs) all resolve — the seed must
        // stay clean so the diagnostic-free demo holds.
        assert!(
            diags.is_empty(),
            "seed workspace should be diagnostic-free, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unknown_type_is_reported_across_the_workspace() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        // Introduce a reference to a type declared nowhere in the workspace.
        ws.edit_active(
            "package com.example;\npublic class Main { void f(){ Missing m = null; } }\n",
        );
        let diags = ws.analyze_active(&LintConfig::default());
        assert!(
            diags.iter().any(|d| d.message.contains("Missing")),
            "expected an unresolved-type diagnostic for `Missing`, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn format_active_rewrites_messy_source() {
        let ws = Workspace::new();
        let out = ws.format_active(&FmtConfig::default());
        assert!(out.formatted.contains("class Greeter"));
        // The seed is deliberately unformatted, so formatting must change it.
        assert_ne!(out.formatted, ws.active_source());
    }

    /// The Monaco `(line, col)` position of byte `offset` within `text` (one-based UTF-16).
    fn monaco_pos(text: &str, offset: usize) -> (u32, u32) {
        let (line, col, _, _) = LineIndex::new(text).to_monaco(text, &(offset..offset));
        (line, col)
    }

    #[test]
    fn hover_shows_inferred_type() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        let src = ws.active_source();
        // The `g` receiver in `g.greet()` is a local of the cross-file type `Greeter`.
        let byte = src.find("g.greet").unwrap();
        let (line, col) = monaco_pos(&src, byte);
        assert_eq!(
            ws.hover(&src, line, col),
            Some("```java\nGreeter\n```".to_string())
        );
    }

    #[test]
    fn goto_definition_navigates_cross_file() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        let src = ws.active_source();
        // The type name `Greeter` in `Greeter g` is declared in the other file.
        let byte = src.find("Greeter g").unwrap();
        let (line, col) = monaco_pos(&src, byte);
        let target = ws.goto_definition(&src, line, col).expect("type resolves");
        assert_eq!(target.path, "com/example/Greeter.java");
    }

    #[test]
    fn goto_definition_navigates_to_a_member() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        let src = ws.active_source();
        // The method name in `g.greet()` resolves to `Greeter.greet` in the other file.
        let byte = src.find("greet()").unwrap();
        let (line, col) = monaco_pos(&src, byte);
        let target = ws
            .goto_definition(&src, line, col)
            .expect("member resolves");
        assert_eq!(target.path, "com/example/Greeter.java");
    }

    #[test]
    fn references_span_the_workspace() {
        let ws = Workspace::new();
        // `Greeter` is the default active file; anchor on its class-name declaration.
        let src = ws.active_source();
        let byte = src.find("Greeter {").unwrap();
        let (line, col) = monaco_pos(&src, byte);

        let without_decl = ws.references(&src, line, col, false);
        // Used twice in `Main.java`: `Greeter g` and `new Greeter(who)`.
        let main_refs = without_decl
            .iter()
            .filter(|t| t.path == "com/example/Main.java")
            .count();
        assert_eq!(main_refs, 2, "got {without_decl:?}");

        // Including the declaration adds exactly one more target, in `Greeter.java`.
        let with_decl = ws.references(&src, line, col, true);
        assert_eq!(with_decl.len(), without_decl.len() + 1);
        assert!(
            with_decl
                .iter()
                .any(|t| t.path == "com/example/Greeter.java")
        );
    }

    #[test]
    fn document_symbols_lists_the_type_and_members() {
        let ws = Workspace::new(); // `Greeter.java` is active by default.
        let syms = ws.document_symbols(&ws.active_source());
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Greeter");
        let child_names: Vec<&str> = syms[0].children.iter().map(|c| c.name.as_str()).collect();
        assert!(child_names.contains(&"name"), "got {child_names:?}"); // field
        assert!(child_names.contains(&"Greeter"), "got {child_names:?}"); // constructor
        assert!(child_names.contains(&"greet"), "got {child_names:?}"); // method
    }

    #[test]
    fn completions_after_dot_list_members_without_keywords() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        let src = ws.active_source();
        // Just after `g.` in `g.greet()`.
        let byte = src.find("g.greet").unwrap() + 2;
        let (line, col) = monaco_pos(&src, byte);
        let entries = ws.completions(&src, line, col);
        assert!(
            entries
                .iter()
                .any(|e| e.label == "greet" && e.kind != CompletionKind::Keyword)
        );
        // A member-access context never offers keywords.
        assert!(entries.iter().all(|e| e.kind != CompletionKind::Keyword));
    }

    #[test]
    fn completions_at_a_bare_position_include_keywords_and_locals() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        ws.edit_active(
            "package com.example;\npublic class Main { void m() { int x = 1; int y = } }\n",
        );
        let src = ws.active_source();
        let byte = src.find("int y = ").unwrap() + "int y = ".len();
        let (line, col) = monaco_pos(&src, byte);
        let entries = ws.completions(&src, line, col);
        assert!(
            entries
                .iter()
                .any(|e| e.kind == CompletionKind::Keyword && e.label == "return")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.kind != CompletionKind::Keyword && e.label == "x")
        );
    }

    #[test]
    fn signature_help_marks_the_active_parameter() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        ws.edit_active(
            "package com.example;\npublic class C { int area(int w, int h){return 0;} void g(){ area(1, ); } }\n",
        );
        let src = ws.active_source();
        let byte = src.find("area(1, ").unwrap() + "area(1, ".len();
        let (line, col) = monaco_pos(&src, byte);
        let help = ws.signature_help(&src, line, col).expect("inside a call");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "area(int w, int h)");
        assert_eq!(help.active_parameter, 1);
    }

    #[test]
    fn document_highlight_covers_a_local() {
        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        let src = ws.active_source();
        // `who` is declared (`String who=...`) and used (`new Greeter(who)`).
        let byte = src.find("who=").unwrap();
        let (line, col) = monaco_pos(&src, byte);
        let highlights = ws.document_highlight(&src, line, col);
        assert_eq!(highlights.len(), 2, "got {highlights:?}");
        assert!(highlights.iter().any(|h| h.write)); // the declaration
        assert!(highlights.iter().any(|h| !h.write)); // the use
    }

    #[test]
    fn folded_classpath_resolves_an_external_library_type() {
        // A compiled `Box<T>` fed to the same wasm-compatible core the browser uses — loaded off an
        // in-memory tree, then lowered for the index. This is the payoff of external dependencies in
        // the playground: a library type resolves without any of its `.java` in the workspace.
        let cache = InMemoryFileTree::new().with_file(
            "deps/Box.class",
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../jals-classpath/tests/fixtures/Box.class"
            )),
        );
        let load = jals_classpath::ClasspathLoad::load_classpath_in(
            &cache,
            &["deps/Box.class".to_string()],
        );
        assert!(load.warnings.is_empty(), "{:?}", load.warnings);
        let lowered = jals_hir::ProjectIndex::lower_classpath(&load.classes);

        let mut ws = Workspace::new();
        ws.set_active("com/example/Main.java");
        // A default-package class using the external `Box` type (the package `Box.class` declares).
        ws.edit_active("class Uses { void f() { Box<String> b = null; } }\n");

        // Unresolved before folding the classpath: no `Box` declaration exists in the workspace.
        let before = ws.analyze_active(&LintConfig::default());
        assert!(
            before.iter().any(|d| d.message.contains("Box")),
            "expected `Box` unresolved before folding the classpath, got: {:?}",
            before.iter().map(|d| &d.message).collect::<Vec<_>>()
        );

        // Resolved after folding `Box.class` in.
        ws.set_classpath(Some(lowered));
        let after = ws.analyze_active(&LintConfig::default());
        assert!(
            !after.iter().any(|d| d.message.contains("Box")),
            "expected `Box` to resolve once the classpath is folded, got: {:?}",
            after.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }
}
