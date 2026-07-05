//! The host-capability traits `jals-classpath`'s resolution is generic over.
//!
//! The classpath/dependency resolution logic (`load.rs`, `resolve.rs`) is otherwise pure and
//! `wasm32`-compatible: it reads and writes through a [`jals_fs::FileTree`] and never touches the
//! network or spawns a process directly. The two capabilities it *cannot* express purely are
//! **fetching a URL** and **cloning a git repository** — those are injected here as traits, so the
//! same resolver runs against a real filesystem + `reqwest` + `git` on a host (the
//! [`native`](crate::native) implementations, behind the default `native` feature) and against an
//! [`InMemoryFileTree`](jals_fs::InMemoryFileTree) + the browser's `fetch` in wasm (the playground).

use jals_config::GitRef;

/// Fetch the bytes at a URL — the one capability the resolver cannot express purely.
///
/// Async because the browser implementation (`fetch`) is inherently asynchronous; the native
/// implementation wraps a *blocking* HTTP client whose returned future resolves in a single poll, so
/// a synchronous host drives it with `futures::executor::block_on`. Used only as a generic bound
/// `F: Fetcher` (never as `dyn Fetcher`), so **no `Send` bound is imposed** — the browser future is
/// `!Send`, and every caller drives the future on a single thread, so `Send`-ness is decided (and
/// satisfied) at each concrete instantiation rather than forced on all of them.
#[allow(async_fn_in_trait)]
pub trait Fetcher {
    /// Download `url`, returning its bytes or a human-readable error (the caller turns an `Err` into
    /// a [`Warning`](crate::Warning) and skips the dependency).
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Clone a git repository and check out a ref — the source-dependency capability the resolver cannot
/// express purely (it shells out to the `git` binary on a host; the browser has no such capability).
///
/// Object-safe and synchronous: injected as `Option<&dyn Git>`, with `None` where git is unavailable
/// (the browser), which warns and skips `git` source dependencies. `dest` is a `/`-separated virtual
/// path — the checkout's target directory.
pub trait Git {
    /// Ensure a checkout of `url` at `reference` exists at `dest` (idempotent — a cached checkout of
    /// an immutable ref is reused). Returns a human-readable message on failure.
    fn clone_checkout(&self, url: &str, reference: &GitRef, dest: &str) -> Result<(), String>;
}
