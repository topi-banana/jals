//! A minimal in-browser workspace: several Java files held in an in-memory tree, with a
//! cross-file symbol index computed on demand.
//!
//! This is the wasm-compatible core of what `jals-lsp`'s host-only `Workspace` wraps — an
//! [`InMemoryFileTree`] as the single source of truth and a [`ProjectIndex`] built over every
//! file — with the LSP-specific plumbing (async, URIs, classpath/dependency I/O) left out. It is
//! deliberately Yew-agnostic so the UI layer stays thin.

use jals_fmt::{Config as FmtConfig, FormatOutput};
use jals_fs::{FileTree, InMemoryFileTree};
use jals_hir::{FileId, ProjectIndex};
use jals_lint::Config as LintConfig;
use jals_syntax::{Parse, SyntaxNode};

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
    /// active file across the whole workspace, and render the result as a display report: a header
    /// line (`<active> — <N> file(s) indexed (stdlib on)`), a blank line, then either
    /// `No diagnostics.` or one `severity  rule  start..end  message` line per diagnostic.
    ///
    /// This is the payoff of a real workspace: `Main`'s reference to `Greeter` resolves through
    /// the *other* file's declaration, while a genuinely unknown type is reported.
    pub fn lint_active(&self, config: &LintConfig) -> String {
        // Parse every file once. The owned `SyntaxNode`s in `files` are what the builder indexes;
        // `parses` is retained only to reuse the active file's `Parse` below (for resolution and
        // the index-aware lint). A path's index into `paths` is its `FileId`.
        let paths = self.paths();
        let parses: Vec<(FileId, Parse)> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| (FileId(i as u32), jals_syntax::parse(&self.read(path))))
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
        let source = self.active_source();

        let mut lines = Vec::new();

        // Cross-file unresolved type names (the "cannot resolve symbol" spans the LSP surfaces
        // separately from lint).
        for range in index.unresolved_types(active_id, &resolved) {
            let name = source.get(range.clone()).unwrap_or("");
            lines.push(format!(
                "error  unresolved-type  {}..{}  cannot resolve `{}`",
                range.start, range.end, name
            ));
        }

        // Every enabled lint rule plus the index-aware cross-file `type-mismatch`.
        let lint =
            jals_lint::lint_parse_with_index(active_parse, config, Some((&index, active_id)));
        for diag in lint.parse_errors.iter().chain(lint.diagnostics.iter()) {
            lines.push(format!(
                "{}  {}  {}..{}  {}",
                diag.severity.as_str(),
                diag.rule,
                diag.range.start,
                diag.range.end,
                diag.message
            ));
        }

        let header = format!(
            "{} — {} file(s) indexed (stdlib on)",
            self.active,
            paths.len()
        );
        let body = if lines.is_empty() {
            "No diagnostics.".to_string()
        } else {
            lines.join("\n")
        };
        format!("{header}\n\n{body}")
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
        let report = ws.lint_active(&LintConfig::default());
        // `Greeter` (another file), `String`/`System` (stdlib stubs) all resolve — the seed must
        // stay clean so the "No diagnostics" demo holds.
        assert!(
            report.contains("No diagnostics."),
            "seed workspace should be diagnostic-free, got: {report}"
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
        let report = ws.lint_active(&LintConfig::default());
        assert!(
            report
                .lines()
                .any(|l| l.contains("unresolved-type") && l.contains("Missing")),
            "expected an unresolved-type diagnostic for `Missing`, got: {report}"
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
