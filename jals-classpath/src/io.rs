//! Capabilities which cannot be represented by project storage.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Fetch bytes from an external locator.
///
/// Project-relative files are never passed through this seam. They are read from a
/// [`jals_storage::ProjectView`]; only genuinely external content (normally HTTP) is fetched.
#[allow(async_fn_in_trait)]
pub trait Fetcher {
    /// Fetch `locator`, returning a diagnostic-ready error message on failure.
    async fn fetch(&self, locator: &str) -> Result<Vec<u8>, String>;

    /// Fetch at most `max_bytes`, rejecting an oversized result.
    async fn fetch_bounded(&self, locator: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        let bytes = self.fetch(locator).await?;
        if bytes.len() > max_bytes {
            return Err(format!(
                "response has {} bytes, exceeding the limit of {max_bytes}",
                bytes.len()
            ));
        }
        Ok(bytes)
    }
}
