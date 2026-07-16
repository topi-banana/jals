//! The per-file cache unit both editor hosts share.

use alloc::string::String;
use alloc::sync::Arc;

use jals_syntax::Parse;

use crate::LineIndex;

/// One file's text with its derived caches: the coordinate map and the parsed CST.
///
/// Everything is behind `Arc` so a snapshot can be cheaply cloned out of a store (into an async
/// request handler, or between an open-document overlay and the workspace's copy of the same
/// file) without reparsing.
#[derive(Clone)]
pub struct Document {
    /// The source text.
    pub text: Arc<str>,
    /// The byte ↔ UTF-16 coordinate map for `text`.
    pub line_index: Arc<LineIndex>,
    /// The parsed CST of `text`.
    pub parse: Arc<Parse>,
}

impl Document {
    /// Parse `text` and build its coordinate map, once.
    pub fn new(text: String) -> Self {
        let line_index = Arc::new(LineIndex::new(&text));
        let parse = Arc::new(Parse::parse(&text));
        Self {
            text: Arc::from(text),
            line_index,
            parse,
        }
    }
}
