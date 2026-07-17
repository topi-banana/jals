//! Capabilities which cannot be represented by project storage.

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
}
