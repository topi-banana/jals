//! How a package is *found*: the route from a name in a manifest to the two halves behind it.
//!
//! A binary that embeds `jals` decides which packages exist. Some are compiled into it — the
//! platform library, anything its author wrote — and some are declared by the project being built,
//! as Java and no Rust. Both are packages by the time anything downstream sees one, and neither
//! route is privileged.
//!
//! That shape is [rhai's `ModuleResolver`][rhai] and [boa's `ModuleLoader`][boa]: resolution is a
//! trait, the built-in set is one implementation, and several are consulted in order through a
//! collection. What is *not* borrowed is shadowing. rhai lets an earlier resolver win a name an
//! later one also offers; here a name offered twice is refused, with both routes named.
//!
//! [rhai]: https://rhai.rs/book/rust/modules/resolvers.html
//! [boa]: https://docs.rs/boa_engine/latest/boa_engine/module/trait.ModuleLoader.html
//!
//! # Why an ambiguous name is an error and not a shadow
//!
//! `jals-config` already answers this for a dependency named in both `[dependencies]` and
//! `[dev-dependencies]`: it is rejected rather than overridden as Cargo does, because one name
//! denotes one entry wherever it is read. A package name is read in more places than that — it is
//! a manifest entry, a set of Java files folded into an index, and a table of import keys the
//! engine links against — so a silent shadow is a project analysed against one `java.lang` and
//! linked against another, with nothing said.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::package::{JavaPackage, SourceKind};
use crate::selection::PackageSelection;

/// Why a name did not resolve to exactly one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Nothing offers this name.
    Unknown {
        /// The name that was asked for.
        name: String,
        /// Every name that *is* offered, in resolution order, so a typo is answerable.
        available: Vec<String>,
    },
    /// Two routes offer this name, so it denotes two different packages.
    ///
    /// Deliberately not resolved by precedence. The two would be one project's analysis and that
    /// same project's linked module disagreeing about a type, which no diagnostic downstream is
    /// positioned to notice.
    Ambiguous {
        /// The contested name.
        name: String,
        /// The routes offering it, in resolution order.
        routes: Vec<String>,
    },
}

impl core::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown { name, available } if available.is_empty() => {
                write!(f, "there is no package named `{name}`; this build offers none")
            }
            Self::Unknown { name, available } => write!(
                f,
                "there is no package named `{name}`; this build offers {}",
                available.join(", ")
            ),
            Self::Ambiguous { name, routes } => write!(
                f,
                "`{name}` is offered by {}: one name denotes one package, so rename one of them",
                routes.join(" and ")
            ),
        }
    }
}

impl core::error::Error for ResolveError {}

/// One route by which a package name becomes a package.
///
/// Implemented outside this crate as readily as inside it: a binary embedding `jals` supplies its
/// own, and so does a host that lets a project declare packages of its own.
pub trait PackageResolver {
    /// A short name for this route, used when an ambiguity has to say where each side came from.
    fn route(&self) -> &str;

    /// The package `name` denotes on this route, if any.
    fn resolve(&self, name: &str) -> Option<Rc<JavaPackage>>;

    /// Every name this route offers, in a deterministic order.
    ///
    /// Used to answer a typo and to detect an ambiguity, so a route that cannot enumerate itself
    /// cannot participate in either.
    fn offered(&self) -> Vec<&str>;
}

/// The packages a binary was built with: the route for anything compiled in.
///
/// This is rhai's `StaticModuleResolver`, and it is a `BTreeMap` for the reason that one is a
/// sorted collection: enumeration order is part of a diagnostic and of the ambiguity check, and
/// neither may depend on registration order.
#[derive(Default)]
pub struct StaticResolver {
    route: String,
    packages: BTreeMap<String, Rc<JavaPackage>>,
}

impl StaticResolver {
    /// An empty registry whose route is called `name` in diagnostics.
    pub fn new(name: &str) -> Self {
        Self {
            route: String::from(name),
            packages: BTreeMap::new(),
        }
    }

    /// Offer `package`, replacing one already offered under its name.
    pub fn add(&mut self, package: JavaPackage) -> &mut Self {
        self.packages
            .insert(String::from(package.name()), Rc::new(package));
        self
    }
}

impl PackageResolver for StaticResolver {
    fn route(&self) -> &str {
        &self.route
    }

    fn resolve(&self, name: &str) -> Option<Rc<JavaPackage>> {
        self.packages.get(name).map(Rc::clone)
    }

    fn offered(&self) -> Vec<&str> {
        self.packages.keys().map(String::as_str).collect()
    }
}

/// Several routes, consulted in order — rhai's `ModuleResolversCollection`.
///
/// Order decides nothing about *which* package a name denotes, because a name offered twice is an
/// error rather than a shadow. What it decides is the order names are listed in when a resolution
/// fails, so the built-in route comes first and reads first.
#[derive(Default)]
pub struct ResolverChain {
    routes: Vec<Box<dyn PackageResolver>>,
}

impl ResolverChain {
    /// An empty chain, which offers nothing and resolves nothing.
    #[must_use]
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// Append a route.
    #[must_use]
    pub fn push(mut self, resolver: Box<dyn PackageResolver>) -> Self {
        self.routes.push(resolver);
        self
    }

    /// Every name any route offers, in route order then name order, without duplicates.
    pub fn offered(&self) -> Vec<&str> {
        let mut names = Vec::new();
        for route in &self.routes {
            for name in route.offered() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// The package `name` denotes, or why it denotes none or more than one.
    ///
    /// Every route is consulted even after one answers, which is the whole ambiguity check: a
    /// chain that stopped at the first hit could not tell a unique name from a contested one.
    pub fn resolve(&self, name: &str) -> Result<Rc<JavaPackage>, ResolveError> {
        let mut found: Option<Rc<JavaPackage>> = None;
        let mut routes: Vec<String> = Vec::new();
        for route in &self.routes {
            if let Some(package) = route.resolve(name) {
                routes.push(String::from(route.route()));
                if found.is_none() {
                    found = Some(package);
                }
            }
        }
        match (found, routes.len()) {
            (Some(package), 1) => Ok(package),
            (Some(_), _) => Err(ResolveError::Ambiguous {
                name: String::from(name),
                routes,
            }),
            (None, _) => Err(ResolveError::Unknown {
                name: String::from(name),
                available: self.offered().into_iter().map(String::from).collect(),
            }),
        }
    }

    /// Resolve every name in `names` into one selection.
    ///
    /// Order follows `names` only as far as deduplicating it: the selection is sorted by package
    /// name, so two manifests that select the same packages produce one cache key and one module
    /// layout however each spelled the list.
    pub fn select(&self, names: &[String]) -> Result<PackageSelection, ResolveError> {
        let mut selected: BTreeMap<String, Rc<JavaPackage>> = BTreeMap::new();
        for name in names {
            let package = self.resolve(name)?;
            selected.insert(String::from(package.name()), package);
        }
        Ok(PackageSelection {
            packages: selected.into_values().collect(),
        })
    }
}

/// Packages a **project** declared: Java it ships itself, with no Rust behind it.
///
/// rhai's `FileModuleResolver` in shape — the route by which something outside the binary becomes a
/// module a name resolves to — and it is what makes package definition programmable without writing
/// a Rust crate. A project fills a gap the platform leaves (a `java.util` it implements itself, an
/// API it wants its own declarations for) by pointing at a directory of its own `.java`.
///
/// # A Java-only package with a `native` method is refused where it matters, not here
///
/// This crate cannot parse Java, so nothing here can notice that a declared package's Java says
/// `native`. It does not have to. Such a method becomes a wasm import nothing supplies, refused
/// when the module is instantiated with the owner and the descriptor both in hand — which is a
/// better failure than a bespoke check would give, and the same one a *Rust* package gets when its
/// two halves disagree about a signature. One mechanism, not two.
pub struct SourceResolver {
    route: String,
    packages: BTreeMap<String, Rc<JavaPackage>>,
}

impl SourceResolver {
    /// An empty route whose name reads `name` in diagnostics.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            route: String::from(name),
            packages: BTreeMap::new(),
        }
    }

    /// Declare a package from Java the host read.
    ///
    /// No version is taken, and that is not an omission. A package.s version exists because a
    /// consumer memoizes against everything it observed and **a Rust closure.s body is the one
    /// input it cannot observe** — see [`JavaPackage::new`]. A package declared this way has no
    /// closures at all, so [`JavaPackage::describe`]'s fold over every path and body is already
    /// complete, and a number beside it would be a second identity that could disagree with the
    /// first.
    pub fn declare(
        &mut self,
        name: &str,
        sources: impl IntoIterator<Item = (String, String, SourceKind)>,
    ) -> &mut Self {
        let mut package = JavaPackage::new(name, 0);
        for (path, text, kind) in sources {
            package.source(path, text, kind);
        }
        self.packages
            .insert(String::from(name), Rc::new(package));
        self
    }

    /// Whether nothing has been declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

impl PackageResolver for SourceResolver {
    fn route(&self) -> &str {
        &self.route
    }

    fn resolve(&self, name: &str) -> Option<Rc<JavaPackage>> {
        self.packages.get(name).map(Rc::clone)
    }

    fn offered(&self) -> Vec<&str> {
        self.packages.keys().map(String::as_str).collect()
    }
}
