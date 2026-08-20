//! Handing compiled bytes to the browser as a file download.
//!
//! The only DOM the playground touches outside Monaco, and deliberately the thinnest surface it
//! can be: the `Blob`, the object URL, and the anchor click all live in `js/download.js`, so this
//! crate needs no extra `web-sys` features.

use wasm_bindgen::prelude::*;

// The download glue (see `js/download.js`), pulled in as a wasm-bindgen snippet.
#[wasm_bindgen(module = "/js/download.js")]
extern "C" {
    /// Offer `bytes` to the user as a file named `name`.
    #[wasm_bindgen(js_name = downloadBytes)]
    fn download_bytes(name: &str, bytes: &js_sys::Uint8Array);
}

pub use api::save;

/// Namespace for handing bytes to the browser.
mod api {
    use super::*;

    /// Save `bytes` as a download named `name`.
    ///
    /// A browser-only effect: like every Monaco binding this panics off `wasm32`, so no test may
    /// reach it. [`compile::workspace`] returns bytes rather than saving them for exactly that
    /// reason.
    ///
    /// [`compile::workspace`]: crate::compile::workspace
    pub fn save(name: &str, bytes: &[u8]) {
        download_bytes(name, &js_sys::Uint8Array::from(bytes));
    }
}
