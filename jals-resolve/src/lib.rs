#![cfg_attr(not(test), no_std)]
//! `jals-resolve`: dependency resolution as a pure, portable algorithm.
//!
//! This crate is the missing layer between a `jals.toml` and everything that consumes it. It owns
//! four vocabularies and one procedure:
//!
//! - [`version`] — Maven-compatible version ordering and version requirements (Cargo caret
//!   semantics for a bare version, Maven range syntax verbatim).
//! - [`id`] — [`PackageName`](id::PackageName), [`SourceId`](id::SourceId), and
//!   [`PackageId`](id::PackageId): identity is `(name, version, resolved source)`, never a
//!   locator string, so a diamond is one package and a cycle is one edge.
//! - [`summary`] — what a manifest contributes to resolution ([`Summary`](summary::Summary),
//!   [`DependencyRequest`](summary::DependencyRequest), the feature graph).
//! - [`lock`] — the `jals.lock` model, parsed with `toml` and rendered by a deterministic writer
//!   (the workspace's `toml` dependency deliberately has no `display` feature).
//!
//! [`Resolver`](resolve::Resolver) walks a workspace's roots through a [`Provider`](resolve::Provider),
//! unifies features, picks one version per name, and returns a [`ResolveGraph`](resolve::ResolveGraph)
//! — the value `jals-project` builds its graph from and `jals-classpath` turns into classpath
//! entries. No network, no filesystem, no clock: everything external arrives through the provider
//! seam, which is what lets the browser playground resolve in-memory projects with the same code
//! the CLI runs.
//!
//! ```
//! use jals_resolve::version::{Version, VersionReq};
//!
//! let req: VersionReq = "1.2.3".parse().unwrap();
//! assert!(req.matches(&Version::parse("1.9.0").unwrap()));
//! assert!(!req.matches(&Version::parse("2.0.0").unwrap()));
//! // Maven ranges are accepted verbatim.
//! let range: VersionReq = "[1.2.3,2.0.0)".parse().unwrap();
//! assert!(range.matches(&Version::parse("1.9.9").unwrap()));
//!
//! // Maven's own ordering: a qualifier sorts before its release.
//! assert!(Version::parse("1.0-alpha-1").unwrap() < Version::parse("1.0").unwrap());
//! assert_eq!(
//!     Version::parse("1.0.0").unwrap(),
//!     Version::parse("1").unwrap(),
//! );
//! ```

extern crate alloc;

pub mod error;
pub mod id;
pub mod lock;
pub mod resolve;
pub mod summary;
pub mod version;
