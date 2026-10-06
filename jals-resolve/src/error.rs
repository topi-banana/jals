//! Resolution failures.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::id::{PackageId, PackageName};
use crate::lock::LockError;
use crate::summary::SourceRequest;
use crate::version::VersionReq;

/// One requirement as it was collected, with the requester that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementOrigin {
    /// The requirement.
    pub requirement: VersionReq,
    /// The package whose manifest declared it, or the root label.
    pub requester: String,
}

/// Why a resolution could not produce a graph.
#[derive(Debug)]
pub enum ResolveError<E> {
    /// A provider operation failed.
    Provider {
        /// What the resolver was doing (`listing versions`, `reading a summary`).
        context: String,
        /// The provider's own error.
        source: E,
    },
    /// No available version satisfies every requirement on a package.
    NoMatchingVersion {
        /// The package that could not be satisfied.
        name: PackageName,
        /// Every requirement collected, with its requester.
        requirements: Vec<RequirementOrigin>,
    },
    /// One package name was requested from two different sources.
    ConflictingSources {
        /// The package in conflict.
        name: PackageName,
        /// The first source seen.
        first: Box<SourceRequest>,
        /// The second source seen.
        second: Box<SourceRequest>,
    },
    /// A provider returned a summary whose id is not the id that was requested.
    SummaryMismatch {
        /// The id the resolver asked about.
        requested: Box<PackageId>,
        /// The id the summary carried.
        found: Box<PackageId>,
    },
    /// Resolution did not stabilize within the round budget.
    NotConverged {
        /// The number of rounds attempted.
        rounds: usize,
    },
    /// A lockfile could not be read.
    Lock(Box<LockError>),
}

impl<E: fmt::Display> fmt::Display for ResolveError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { context, source } => {
                write!(f, "dependency resolution failed while {context}: {source}")
            }
            Self::NoMatchingVersion { name, requirements } => {
                write!(
                    f,
                    "no available version of `{name}` satisfies every requirement"
                )?;
                for origin in requirements {
                    write!(
                        f,
                        "\n  `{}` requires `{}`",
                        origin.requester, origin.requirement
                    )?;
                }
                Ok(())
            }
            Self::ConflictingSources {
                name,
                first,
                second,
            } => write!(
                f,
                "`{name}` is requested from two different sources: `{first}` and `{second}`"
            ),
            Self::SummaryMismatch { requested, found } => write!(
                f,
                "the provider returned a summary for `{found}` when `{requested}` was requested"
            ),
            Self::NotConverged { rounds } => write!(
                f,
                "dependency resolution did not stabilize after {rounds} rounds"
            ),
            Self::Lock(error) => error.fmt(f),
        }
    }
}

impl<E: fmt::Display + fmt::Debug> core::error::Error for ResolveError<E> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Lock(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

impl<E> From<LockError> for ResolveError<E> {
    fn from(error: LockError) -> Self {
        Self::Lock(Box::new(error))
    }
}

/// A warning resolution produced without failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveWarning {
    /// A locked package is no longer part of the resolved graph.
    UnusedLockEntry {
        /// The lock entry that went unused.
        package: String,
    },
}

impl fmt::Display for ResolveWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnusedLockEntry { package } => {
                write!(f, "`{package}` is locked but no longer resolved")
            }
        }
    }
}
