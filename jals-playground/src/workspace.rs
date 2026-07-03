//! A minimal in-browser workspace: several Java files held in an in-memory tree, with a
//! cross-file symbol index computed on demand.
//!
//! This is the wasm-compatible core of what `jals-lsp`'s host-only `Workspace` wraps — an
//! [`InMemoryFileTree`] as the single source of truth and a [`ProjectIndex`] built over every
//! file — with the LSP-specific plumbing (async, URIs, classpath/dependency I/O) left out. It is
//! deliberately Yew-agnostic so the UI layer stays thin.

use core::ops::Range;

use jals_fmt::{Config as FmtConfig, FormatOutput};
use jals_fs::{FileTree, InMemoryFileTree};
use jals_hir::{FileId, ProjectIndex};
use jals_lint::{Config as LintConfig, Severity};
use jals_syntax::{Parse, SyntaxNode};

/// One diagnostic over the active file, in source byte offsets — the playground's neutral shape,
/// converted to Monaco markers by the UI layer. Aggregates syntax errors, lint rule findings
/// (including the cross-file `type-mismatch`), and cross-file unresolved type names.
pub struct PlaygroundDiagnostic {
    /// Byte range in the active source.
    pub range: Range<usize>,
    /// Human-readable message.
    pub message: String,
    /// Resolved severity.
    pub severity: Severity,
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
        Workspace { fs, active }
    }

    /// Every Java file path, sorted. A path's index into this vec is its [`FileId`].
    pub fn paths(&self) -> Vec<String> {
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
        jals_fmt::format_source(&self.active_source(), config)
    }

    /// Parse the active file for the syntax-tree dump.
    pub fn syntax_active(&self) -> Parse {
        jals_syntax::parse(&self.active_source())
    }

    /// Build a [`ProjectIndex`] over *every* file (with the embedded stdlib stubs), analyse the
    /// active file across the whole workspace, and return the active file's source (read once)
    /// together with its diagnostics in source byte offsets.
    ///
    /// The source is returned so the caller can reuse it — for parsing the active file, extracting
    /// unresolved-type names, and mapping byte ranges to editor positions, it is read exactly once.
    ///
    /// This is the payoff of a real workspace: `Main`'s reference to `Greeter` resolves through
    /// the *other* file's declaration, while a genuinely unknown type is reported. The result
    /// aggregates, over the active file:
    /// - cross-file unresolved type names (the "cannot resolve symbol" the LSP surfaces
    ///   separately from lint), as errors;
    /// - the parser's syntax errors; and
    /// - every enabled lint rule plus the index-aware cross-file `type-mismatch`.
    pub fn analyze_active(&self, config: &LintConfig) -> (String, Vec<PlaygroundDiagnostic>) {
        // The active file's source, read once and reused throughout (parse, name extraction, and
        // the returned value for the caller's line mapping).
        let source = self.active_source();

        // Parse every file once. The owned `SyntaxNode`s in `files` are what the builder indexes;
        // `parses` is retained only to reuse the active file's `Parse` below (for resolution and
        // the index-aware lint). A path's index into `paths` is its `FileId`.
        let paths = self.paths();
        let parses: Vec<(FileId, Parse)> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                // Reuse the already-read active source; read the other files from the tree.
                let parse = if path == &self.active {
                    jals_syntax::parse(&source)
                } else {
                    jals_syntax::parse(&self.read(path))
                };
                (FileId(i as u32), parse)
            })
            .collect();
        let files: Vec<(FileId, SyntaxNode)> = parses
            .iter()
            .map(|(id, parse)| (*id, parse.syntax()))
            .collect();
        let index = ProjectIndex::builder(&files).with_stdlib().build();

        let active_idx = paths.iter().position(|p| p == &self.active).unwrap_or(0);
        let active_id = FileId(active_idx as u32);
        let active_parse = &parses[active_idx].1;
        let root = active_parse.syntax();
        let resolved = jals_hir::resolve_node(&root);

        let mut diags = Vec::new();

        // Cross-file unresolved type names.
        for range in index.unresolved_types(active_id, &resolved) {
            let name = source.get(range.clone()).unwrap_or("");
            diags.push(PlaygroundDiagnostic {
                message: format!("cannot resolve `{name}`"),
                range,
                severity: Severity::Error,
            });
        }

        // The parser's syntax errors plus every enabled lint rule (and the index-aware cross-file
        // `type-mismatch`). `lint.parse_errors` already carries the syntax errors, so the raw
        // `Parse::errors` are not counted separately.
        let lint =
            jals_lint::lint_parse_with_index(active_parse, config, Some((&index, active_id)));
        for diag in lint.parse_errors.iter().chain(lint.diagnostics.iter()) {
            diags.push(PlaygroundDiagnostic {
                message: format!("{}: {}", diag.rule, diag.message),
                range: diag.range.clone(),
                severity: diag.severity,
            });
        }

        (source, diags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_files_parse_clean() {
        for (path, contents) in SAMPLE_FILES {
            let parse = jals_syntax::parse(contents);
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
        let (_, diags) = ws.analyze_active(&LintConfig::default());
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
        let (_, diags) = ws.analyze_active(&LintConfig::default());
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
}
