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
use gloo_timers::future::TimeoutFuture;
use jals_classpath::{FetchError, Fetcher, NetworkPolicy, RetrySchedule};

/// Downloads dependency jars with the browser's `fetch`, optionally through a CORS proxy.
pub struct BrowserFetcher {
    /// A CORS-proxy base prepended to each URL (e.g. `https://corsproxy.io/?`); empty for a direct
    /// fetch (the default), which only reaches CORS-permissive hosts.
    proxy: String,
    network: NetworkPolicy,
}

impl BrowserFetcher {
    /// A fetcher that prepends `proxy` (empty for a direct fetch) to each requested URL, under
    /// `network`.
    ///
    /// The browser has no cache to fall back on and no second way to reach a host, so the
    /// playground builds these `Online`. The parameter exists because the policy belongs to the
    /// capability, not because this host varies it.
    pub fn new(proxy: String, network: NetworkPolicy) -> Self {
        BrowserFetcher { proxy, network }
    }
}

impl BrowserFetcher {
    /// How a non-`ok` status reads, and whether another attempt could get past it.
    ///
    /// The same rule the native adapter applies: every 5xx but 501, plus 408 and 429. Stated once
    /// here because `fetch_admitted` and `fetch_bounded_admitted` both reach it, and two copies is
    /// how one of them ends up retrying a 404.
    fn status_error(url: &str, status: u16) -> FetchError {
        let message = format!("HTTP {status} for {url}");
        if matches!(status, 408 | 429) || (status >= 500 && status != 501) {
            FetchError::transient(message)
        } else {
            FetchError::permanent(message)
        }
    }

    /// The URL actually requested: `url`, or `url` behind the configured CORS proxy.
    fn target(&self, url: &str) -> String {
        if self.proxy.is_empty() {
            url.to_string()
        } else {
            format!("{}{url}", self.proxy)
        }
    }
}

impl Fetcher for BrowserFetcher {
    fn network(&self) -> NetworkPolicy {
        self.network
    }

    /// The default schedule. A CORS proxy and a public jar host are both things that go away for a
    /// moment, and the browser has no verified cache to fall back on when one does.
    fn retry(&self) -> RetrySchedule {
        RetrySchedule::new(RetrySchedule::DEFAULT_RETRIES)
    }

    /// `setTimeout`, the browser's only wait. `u32` is exactly what `TimeoutFuture` takes.
    async fn delay(&self, millis: u32) {
        TimeoutFuture::new(millis).await;
    }

    async fn fetch_admitted(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        let target = self.target(url);
        let response = Request::get(&target)
            .send()
            .await
            .map_err(|e| FetchError::transient(e.to_string()))?;
        if !response.ok() {
            return Err(Self::status_error(url, response.status()));
        }
        response
            .binary()
            .await
            .map_err(|e| FetchError::permanent(e.to_string()))
    }

    async fn fetch_bounded_admitted(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, FetchError> {
        let target = self.target(url);
        let response = Request::get(&target)
            .send()
            .await
            .map_err(|e| FetchError::transient(e.to_string()))?;
        if !response.ok() {
            return Err(Self::status_error(url, response.status()));
        }
        // Refuse before reading the body where we can. The whole response lands in the wasm
        // linear heap, so without this a jar far larger than the declared limit takes the tab
        // down before the limit is ever consulted — the native fetcher checks `Content-Length`
        // and streams for the same reason.
        if let Some(declared) = response
            .headers()
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            && declared > max_bytes
        {
            return Err(FetchError::permanent(format!(
                "response declares {declared} bytes, exceeding the limit of {max_bytes}"
            )));
        }
        let bytes = response
            .binary()
            .await
            .map_err(|e| FetchError::permanent(e.to_string()))?;
        // A server may omit `Content-Length` (or lie), so the size is still checked afterwards.
        if bytes.len() > max_bytes {
            return Err(FetchError::permanent(format!(
                "response has {} bytes, exceeding the limit of {max_bytes}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }
}
