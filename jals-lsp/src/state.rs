//! In-memory server state: open documents and memoized config discovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_lsp::lsp_types::Url;
use jals_fmt::Config;

use crate::line_index::LineIndex;

/// An open document: its text, the client's version, and a precomputed line index.
///
/// `text` and `line_index` are behind `Arc` so a snapshot can be cheaply cloned out of
/// the store and moved into an async request handler.
#[derive(Clone)]
pub(crate) struct Document {
    pub(crate) text: Arc<str>,
    pub(crate) version: i32,
    pub(crate) line_index: Arc<LineIndex>,
}

impl Document {
    fn new(text: String, version: i32) -> Document {
        let line_index = Arc::new(LineIndex::new(&text));
        Document {
            text: Arc::from(text),
            version,
            line_index,
        }
    }
}

/// In-memory store of open documents, keyed by URI. Full text sync, so each update
/// replaces the whole document and rebuilds its line index.
#[derive(Default)]
pub(crate) struct DocumentStore {
    docs: HashMap<Url, Document>,
}

impl DocumentStore {
    pub(crate) fn upsert(&mut self, uri: Url, text: String, version: i32) {
        self.docs.insert(uri, Document::new(text, version));
    }

    /// Snapshot the document for `uri` (cheap `Arc` clones), if open.
    pub(crate) fn get(&self, uri: &Url) -> Option<Document> {
        self.docs.get(uri).cloned()
    }

    pub(crate) fn remove(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }
}

/// Resolves a `jals-fmt` `Config` for a document by discovering `jalsfmt.toml` from the
/// file's directory, memoized per directory. Mirrors the `jals fmt` CLI behavior.
#[derive(Default)]
pub(crate) struct Discovery {
    cache: HashMap<PathBuf, Config>,
}

impl Discovery {
    /// Discover the config for a document URI. Falls back to `Config::default()` for
    /// non-file URIs (e.g. `untitled:`) and when discovery fails.
    pub(crate) fn for_uri(&mut self, uri: &Url) -> Config {
        let Some(dir) = uri
            .to_file_path()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
        else {
            return Config::default();
        };
        if let Some(cfg) = self.cache.get(&dir) {
            return cfg.clone();
        }
        let cfg = Config::discover(&dir).unwrap_or_default();
        self.cache.insert(dir, cfg.clone());
        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut discovery = Discovery::default();
        let uri = Url::parse("untitled:Untitled-1").unwrap();
        assert_eq!(discovery.for_uri(&uri), Config::default());
    }
}
