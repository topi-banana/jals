//! A browser [`Fetcher`] backed by the Fetch API (via [`gloo_net`]).
//!
//! This is the wasm counterpart of `jals-classpath`'s native `ReqwestFetcher`:
//! [`ProjectInputs::assemble`](jals_classpath::ProjectInputs::assemble) runs in the browser with
//! this [`Fetcher`] and the editor's detached
//! [`MemoryStorage`](jals_storage::MemoryStorage) snapshot.
//!
//! **CORS caveat.** `fetch` is subject to the browser's same-origin policy, so a jar host that does
//! not send permissive CORS headers (Maven Central `repo1.maven.org` among them) cannot be fetched
//! directly. A CORS-permissive host (e.g. a jsDelivr-served jar) works as-is; anything else needs a
//! proxy — [`BrowserFetcher`] prepends an optional proxy base to every URL for exactly that.

use gloo_net::http::Request;
use jals_classpath::Fetcher;

/// Downloads dependency jars with the browser's `fetch`, optionally through a CORS proxy.
pub struct BrowserFetcher {
    /// A CORS-proxy base prepended to each URL (e.g. `https://corsproxy.io/?`); empty for a direct
    /// fetch (the default), which only reaches CORS-permissive hosts.
    proxy: String,
}

impl BrowserFetcher {
    /// A fetcher that prepends `proxy` (empty for a direct fetch) to each requested URL.
    pub fn new(proxy: String) -> Self {
        BrowserFetcher { proxy }
    }
}

impl Fetcher for BrowserFetcher {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let target = if self.proxy.is_empty() {
            url.to_string()
        } else {
            format!("{}{url}", self.proxy)
        };
        let response = Request::get(&target)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.ok() {
            return Err(format!("HTTP {} for {url}", response.status()));
        }
        response.binary().await.map_err(|e| e.to_string())
    }
}
