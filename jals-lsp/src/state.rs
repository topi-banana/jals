//! In-memory server state: open documents and memoized config discovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_lsp::lsp_types::{TextDocumentContentChangeEvent, Url};
use jals_fmt::Config;
use jals_syntax::Parse;

use crate::line_index::LineIndex;

/// An open document: its text, the client's version, a precomputed line index, and the
/// parsed CST.
///
/// `text`, `line_index`, and `parse` are behind `Arc` so a snapshot can be cheaply cloned
/// out of the store and moved into an async request handler. The CST is parsed once here,
/// when the document is built, so each request handler reuses it instead of reparsing.
#[derive(Clone)]
pub(crate) struct Document {
    pub(crate) text: Arc<str>,
    pub(crate) version: i32,
    pub(crate) line_index: Arc<LineIndex>,
    pub(crate) parse: Arc<Parse>,
}

impl Document {
    fn new(text: String, version: i32) -> Document {
        let line_index = Arc::new(LineIndex::new(&text));
        let parse = Arc::new(jals_syntax::parse(&text));
        Document {
            text: Arc::from(text),
            version,
            line_index,
            parse,
        }
    }
}

/// In-memory store of open documents, keyed by URI. Incremental text sync:
/// `apply_changes` splices `didChange` events into the stored text and rebuilds the
/// line index, while `upsert` (didOpen) replaces the document wholesale.
#[derive(Default)]
pub(crate) struct DocumentStore {
    docs: HashMap<Url, Document>,
}

impl DocumentStore {
    pub(crate) fn upsert(&mut self, uri: Url, text: String, version: i32) {
        self.docs.insert(uri, Document::new(text, version));
    }

    /// Apply `didChange` content changes to the document at `uri`, recording `version`.
    ///
    /// A change for a document that is not open is ignored (client protocol error;
    /// splicing into a nonexistent base would fabricate text). The version is recorded
    /// even when `changes` is empty.
    pub(crate) fn apply_changes(
        &mut self,
        uri: &Url,
        changes: &[TextDocumentContentChangeEvent],
        version: i32,
    ) {
        let Some(doc) = self.docs.get_mut(uri) else {
            return;
        };
        if changes.is_empty() {
            doc.version = version;
            return;
        }
        *doc = Document::new(apply_content_changes(&doc.text, changes), version);
    }

    /// Snapshot the document for `uri` (cheap `Arc` clones), if open.
    pub(crate) fn get(&self, uri: &Url) -> Option<Document> {
        self.docs.get(uri).cloned()
    }

    pub(crate) fn remove(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }
}

/// Apply LSP `didChange` content changes to `text`, in order.
///
/// Per the LSP spec each event's range refers to the document state after the previous
/// event, so a fresh `LineIndex` is built per ranged event. An event without a range
/// replaces the whole document. Reversed ranges are normalized and out-of-range
/// positions are clamped by `LineIndex::offset`, so this never panics.
pub(crate) fn apply_content_changes(
    text: &str,
    changes: &[TextDocumentContentChangeEvent],
) -> String {
    let mut text = text.to_owned();
    for change in changes {
        let Some(range) = change.range else {
            text = change.text.clone();
            continue;
        };
        let index = LineIndex::new(&text);
        let start = u32::from(index.offset(&text, range.start)) as usize;
        let end = u32::from(index.offset(&text, range.end)) as usize;
        text.replace_range(start.min(end)..start.max(end), &change.text);
    }
    text
}

/// A config the LSP discovers by walking up from a document's directory to a well-known TOML
/// file. Implemented for both `jals_fmt::Config` and `jals_lint::Config` so one [`Discovery`]
/// cache serves the formatter and the linter alike.
pub(crate) trait DiscoverableConfig: Clone + Default {
    /// The config file name searched for (e.g. `jalsfmt.toml`).
    const FILE_NAME: &'static str;
    /// Discover the config from `dir` upward, falling back to the default on any error.
    fn discover_or_default(dir: &Path) -> Self;
}

impl DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalsfmt.toml";
    fn discover_or_default(dir: &Path) -> Self {
        Config::discover(dir).unwrap_or_default()
    }
}

impl DiscoverableConfig for jals_lint::Config {
    const FILE_NAME: &'static str = "jalslint.toml";
    fn discover_or_default(dir: &Path) -> Self {
        jals_lint::Config::discover(dir).unwrap_or_default()
    }
}

/// Resolves a `C` for a document by discovering its
/// [`FILE_NAME`](DiscoverableConfig::FILE_NAME) from the file's directory upward, memoized per
/// directory. Mirrors the `jals` CLI behavior.
#[derive(Default)]
pub(crate) struct Discovery<C> {
    cache: HashMap<PathBuf, C>,
}

impl<C: DiscoverableConfig> Discovery<C> {
    /// Discover the config for a document URI. Falls back to `C::default()` for non-file URIs
    /// (e.g. `untitled:`) and when discovery fails.
    pub(crate) fn for_uri(&mut self, uri: &Url) -> C {
        let Some(dir) = uri
            .to_file_path()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            return C::default();
        };
        if let Some(cfg) = self.cache.get(&dir) {
            return cfg.clone();
        }
        let cfg = C::discover_or_default(&dir);
        self.cache.insert(dir, cfg.clone());
        cfg
    }

    /// Forget all memoized configs, e.g. after a config file changes on disk. Discovery
    /// reruns lazily on the next request that needs a config.
    pub(crate) fn clear(&mut self) {
        self.cache.clear();
    }
}

/// Whether `uri` refers to a config file named `C::FILE_NAME` (e.g. `jalsfmt.toml`).
fn is_config_file_for<C: DiscoverableConfig>(uri: &Url) -> bool {
    uri.to_file_path()
        .is_ok_and(|path| path.file_name().is_some_and(|name| name == C::FILE_NAME))
}

/// Whether a watched-file URI refers to a `jalsfmt.toml` config file.
pub(crate) fn is_config_file(uri: &Url) -> bool {
    is_config_file_for::<Config>(uri)
}

/// Whether a watched-file URI refers to a `jalslint.toml` config file.
pub(crate) fn is_lint_config_file(uri: &Url) -> bool {
    is_config_file_for::<jals_lint::Config>(uri)
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{Position, Range};

    use super::*;

    /// Helper: a ranged (incremental) change event from (line, character) pairs.
    fn ranged(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_owned(),
        }
    }

    /// Helper: a full-document replacement event (no range).
    fn full(text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn apply_single_insert() {
        let out = apply_content_changes("class A {}", &[ranged((0, 9), (0, 9), "int x;")]);
        assert_eq!(out, "class A {int x;}");
    }

    #[test]
    fn apply_single_delete() {
        let out = apply_content_changes("abcdef", &[ranged((0, 1), (0, 4), "")]);
        assert_eq!(out, "aef");
    }

    #[test]
    fn apply_single_replace() {
        let out = apply_content_changes("abc", &[ranged((0, 1), (0, 2), "XY")]);
        assert_eq!(out, "aXYc");
    }

    #[test]
    fn apply_batch_uses_post_edit_coordinates() {
        // The second event's range is only meaningful against "aXYb", the state
        // after the first event: (0,2)..(0,3) deletes the "Y".
        let changes = [ranged((0, 1), (0, 1), "XY"), ranged((0, 2), (0, 3), "")];
        assert_eq!(apply_content_changes("ab", &changes), "aXb");
    }

    #[test]
    fn apply_counts_utf16_columns() {
        // '😀' = 4 UTF-8 bytes, 2 UTF-16 units, so 'y' starts at character 3.
        let out = apply_content_changes("x😀y", &[ranged((0, 1), (0, 3), "Z")]);
        assert_eq!(out, "xZy");
        let out = apply_content_changes("x😀y", &[ranged((0, 3), (0, 3), "!")]);
        assert_eq!(out, "x😀!y");
    }

    #[test]
    fn apply_full_replacement_mid_batch() {
        // A no-range event discards everything before it; later events apply to it.
        let changes = [
            ranged((0, 0), (0, 1), "Z"),
            full("new"),
            ranged((0, 0), (0, 0), "A"),
        ];
        assert_eq!(apply_content_changes("abc", &changes), "Anew");
    }

    #[test]
    fn apply_reversed_range_is_normalized() {
        let out = apply_content_changes("abcde", &[ranged((0, 3), (0, 1), "X")]);
        assert_eq!(out, "aXde");
    }

    #[test]
    fn apply_newline_insert_then_edit_new_line() {
        // After the first event the document has two lines; the second event
        // addresses the freshly created line 1.
        let changes = [ranged((0, 2), (0, 2), "\n"), ranged((1, 1), (1, 1), "X")];
        assert_eq!(apply_content_changes("abcd", &changes), "ab\ncXd");
    }

    #[test]
    fn apply_delete_spanning_newline_joins_lines() {
        let out = apply_content_changes("ab\ncd", &[ranged((0, 2), (1, 0), "")]);
        assert_eq!(out, "abcd");
    }

    #[test]
    fn apply_range_past_eof_clamps_to_append() {
        let out = apply_content_changes("ab", &[ranged((5, 0), (5, 0), "!")]);
        assert_eq!(out, "ab!");
    }

    #[test]
    fn apply_empty_changes_keeps_text() {
        assert_eq!(apply_content_changes("abc", &[]), "abc");
    }

    #[test]
    fn store_apply_changes_updates_text_version_and_index() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "ab\ncd".into(), 1);
        store.apply_changes(&uri, &[ranged((1, 0), (1, 2), "XYZ")], 2);
        let doc = store.get(&uri).unwrap();
        assert_eq!(&*doc.text, "ab\nXYZ");
        assert_eq!(doc.version, 2);
        // A stale index (built from "ab\ncd") would clamp this to 5.
        let end = doc.line_index.offset(&doc.text, Position::new(1, 3));
        assert_eq!(u32::from(end), 6);
    }

    #[test]
    fn store_apply_changes_ignores_unopened_document() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.apply_changes(&uri, &[ranged((0, 0), (0, 0), "x")], 1);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn store_apply_changes_empty_batch_bumps_version_only() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "abc".into(), 1);
        let before = store.get(&uri).unwrap();
        store.apply_changes(&uri, &[], 2);
        let after = store.get(&uri).unwrap();
        assert_eq!(&*after.text, "abc");
        assert_eq!(after.version, 2);
        // The text and line index are untouched, not rebuilt.
        assert!(Arc::ptr_eq(&before.line_index, &after.line_index));
    }

    #[test]
    fn store_upsert_get_remove() {
        let mut store = DocumentStore::default();
        let uri = Url::parse("file:///a/B.java").unwrap();
        store.upsert(uri.clone(), "class B {}".into(), 1);
        let doc = store.get(&uri).unwrap();
        assert_eq!(&*doc.text, "class B {}");
        assert_eq!(doc.version, 1);
        store.remove(&uri);
        assert!(store.get(&uri).is_none());
    }

    #[test]
    fn discovery_non_file_uri_uses_default() {
        let mut discovery = Discovery::<Config>::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(discovery.for_uri(&uri), Config::default());
    }

    #[test]
    fn discovery_clear_picks_up_config_edits() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("jalsfmt.toml");
        let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

        let mut discovery = Discovery::<Config>::default();
        std::fs::write(&config_path, "indent-width = 7\n").unwrap();
        assert_eq!(discovery.for_uri(&uri).indent_width, 7);

        // The cached config survives an edit on disk until the cache is cleared.
        std::fs::write(&config_path, "indent-width = 3\n").unwrap();
        assert_eq!(discovery.for_uri(&uri).indent_width, 7);

        discovery.clear();
        assert_eq!(discovery.for_uri(&uri).indent_width, 3);
    }

    #[test]
    fn is_config_file_matches_only_jalsfmt_toml() {
        let config = Url::parse("file:///p/jalsfmt.toml").unwrap();
        assert!(is_config_file(&config));
        let other = Url::parse("file:///p/other.toml").unwrap();
        assert!(!is_config_file(&other));
        let non_file = Url::parse("untitled:jalsfmt.toml").unwrap();
        assert!(!is_config_file(&non_file));
    }

    #[test]
    fn lint_discovery_non_file_uri_uses_default() {
        let mut discovery = Discovery::<jals_lint::Config>::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(discovery.for_uri(&uri), jals_lint::Config::default());
    }

    #[test]
    fn lint_discovery_clear_picks_up_config_edits() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("jalslint.toml");
        let uri = Url::from_file_path(dir.path().join("A.java")).unwrap();

        let mut discovery = Discovery::<jals_lint::Config>::default();
        // The resolved severity of `wildcard-import` under the on-disk config.
        let wildcard = |d: &mut Discovery<jals_lint::Config>| {
            d.for_uri(&uri).rules.get("wildcard-import").copied()
        };

        std::fs::write(&config_path, "[rules]\nwildcard-import = \"allow\"\n").unwrap();
        assert_eq!(wildcard(&mut discovery), Some(jals_lint::Severity::Allow));

        // The cached config survives an edit on disk until the cache is cleared.
        std::fs::write(&config_path, "[rules]\nwildcard-import = \"error\"\n").unwrap();
        assert_eq!(wildcard(&mut discovery), Some(jals_lint::Severity::Allow));

        discovery.clear();
        assert_eq!(wildcard(&mut discovery), Some(jals_lint::Severity::Error));
    }

    #[test]
    fn is_lint_config_file_matches_only_jalslint_toml() {
        let config = Url::parse("file:///p/jalslint.toml").unwrap();
        assert!(is_lint_config_file(&config));
        let other = Url::parse("file:///p/jalsfmt.toml").unwrap();
        assert!(!is_lint_config_file(&other));
        let non_file = Url::parse("untitled:jalslint.toml").unwrap();
        assert!(!is_lint_config_file(&non_file));
    }
}
