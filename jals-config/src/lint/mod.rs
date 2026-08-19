//! Linting configuration, deserialized from `jalslint.toml`.
//!
//! # Shape
//!
//! The config is a set of **sections**, one per rule [`Category`], each a TOML table and each its
//! own module here. Every key is optional; an omitted key — or an omitted whole section — leaves
//! the rule at its built-in level. Keys are kebab-case and are the rule names diagnostics carry,
//! so a reported `unused-variables` is turned off by the key spelled `unused-variables`.
//!
//! ```toml
//! [correctness]
//! type-mismatch = "error"
//!
//! [unused]
//! dead-code = "allow"
//!
//! [naming.naming-convention]
//! fields = "any"
//! ```
//!
//! A rule's value is its [`LintLevel`], or a table of that level and the rule's own options
//! ([`Lint`] documents both spellings, and why the table form may omit `level`).
//!
//! # Why sections, and what a section is
//!
//! A section is a **defect class**: what kind of thing the rule found, not how loudly it says so
//! and not how mature it is. That is the whole rule for placing a rule, and it makes the
//! categories exclusive — every rule is in exactly one, which is what lets `jals-lint/README.md`
//! and `jals-lint/MAPPING-rustc-clippy.md` carry one section column per rule and still add up.
//!
//! It is deliberately *not* clippy's group list. `clippy::pedantic`, `clippy::nursery` and the
//! opt-in half of `clippy::restriction` answer "how eagerly is this on by default?", which is a
//! different question from "what did it find?" — mixing the two into one axis is what makes
//! `clippy::pedantic` unplaceable next to `clippy::style`. Here the first question is answered by
//! the section and the second by the rule's built-in [`LintLevel`], so a pedantic style rule is a
//! `[style]` rule that defaults to [`Allow`](LintLevel::Allow).
//!
//! [`Restriction`](Category::Restriction) survives that split as a genuine class, because its
//! findings really are about something else: the code is correct, idiomatic and fast, and the
//! project has still chosen to ban the construct.
//!
//! # What belongs here
//!
//! A key exists here when the rule behind it exists. A schema key that reaches no rule is a
//! promise the linter does not keep, so planned rules live in `jals-lint/README.md`'s roadmap
//! rather than as `allow`-by-default keys nothing reads — the same criterion
//! [`jals_fmt`'s coverage test] holds for the formatter, and `jals-lint/tests/registry.rs` holds
//! it here.
//!
//! [`jals_fmt`'s coverage test]: https://docs.rs/jals-fmt

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

pub use crate::loader::ConfigError;
use crate::manifest::FeatureSet;

/// Declare a `jalslint.toml` section: one struct, its per-rule built-in levels, and its
/// key-by-key deserialization.
///
/// Each rule line is `"<rule-name>" => <field>: <options-type> = <built-in level>`. The name
/// literal is the single home of the rule's spelling — it is both the serde rename and the entry
/// in [`RULES`](Correctness::RULES) — and the level literal is the single home of the built-in
/// default, which is why [`RuleMeta`](jals_lint) carries an accessor into this schema rather than
/// a copy of the level.
///
/// `Deserialize` is written out rather than derived because a rule key is a **patch**: a table
/// that sets only options must keep the built-in level (see [`Lint`]), and an unrecognized key
/// must be recorded rather than rejected or dropped (see [`UnknownKeys`]). Neither is expressible
/// with derive attributes.
macro_rules! lint_section {
    (
        $(#[$section_doc:meta])*
        $section:ident : $category:ident {
            $(
                $(#[$rule_doc:meta])*
                $name:literal => $field:ident : $options:ty = $level:ident
            ),* $(,)?
        }
    ) => {
        $(#[$section_doc])*
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize)]
        pub struct $section {
            $(
                $(#[$rule_doc])*
                #[serde(rename = $name)]
                pub $field: $crate::lint::Lint<$options>,
            )*
            /// Keys written under this section that the schema does not define. Recorded, not
            /// rejected — [`UnknownKeys`](crate::lint::UnknownKeys) says why.
            #[serde(skip)]
            pub unknown: $crate::lint::UnknownKeys,
        }

        impl $section {
            /// Every rule this section declares, in declaration order. The registry test joins
            /// this against the linter's own table, so a rule declared here and implemented
            /// nowhere fails the build by name.
            pub const RULES: &'static [&'static str] = &[$($name),*];

            /// The [`Category`](crate::lint::Category) every rule in this section belongs to.
            pub const CATEGORY: $crate::lint::Category = $crate::lint::Category::$category;
        }

        impl Default for $section {
            /// The built-in level of every rule in this section — the **only** place they are
            /// stated.
            fn default() -> Self {
                Self {
                    $($field: $crate::lint::Lint::at($crate::lint::LintLevel::$level),)*
                    unknown: $crate::lint::UnknownKeys::default(),
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $section {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                struct SectionVisitor;

                impl<'de> ::serde::de::Visitor<'de> for SectionVisitor {
                    type Value = $section;

                    fn expecting(
                        &self,
                        f: &mut ::core::fmt::Formatter<'_>,
                    ) -> ::core::fmt::Result {
                        f.write_str(concat!("the `[", stringify!($section), "]` lint section"))
                    }

                    fn visit_map<A: ::serde::de::MapAccess<'de>>(
                        self,
                        mut map: A,
                    ) -> Result<Self::Value, A::Error> {
                        let mut out = <$section as Default>::default();
                        while let Some(key) =
                            map.next_key::<::alloc::string::String>()?
                        {
                            match key.as_str() {
                                $(
                                    $name => map
                                        .next_value::<$crate::lint::LintPatch<$options>>()?
                                        .apply(&mut out.$field),
                                )*
                                _ => out.unknown.record(key, &mut map)?,
                            }
                        }
                        Ok(out)
                    }
                }

                deserializer.deserialize_map(SectionVisitor)
            }
        }
    };
}

mod compatibility;
mod complexity;
mod correctness;
mod documentation;
mod level;
mod naming;
mod performance;
mod restriction;
mod style;
mod suspicious;
mod unused;

#[cfg(test)]
mod tests;

pub use compatibility::Compatibility;
pub use complexity::Complexity;
pub use correctness::Correctness;
pub use documentation::Documentation;
pub(crate) use level::LintPatch;
pub use level::{Lint, LintLevel, LintOptions, NoOptions, UnknownKeys};
pub use naming::{Case, Naming, NamingConvention};
pub use performance::Performance;
pub use restriction::{ConsoleStreams, PrintToConsole, Restriction};
pub use style::{BracePolicy, MissingBraces, StaticWildcard, Style, WildcardImport};
pub use suspicious::{EmptyCatch, IgnoredCatch, Suspicious};
pub use unused::{AnnotatedMembers, DeadCode, Unused, UnusedVariables};

/// What kind of defect a rule reports — the section it is configured under.
///
/// Exclusive by construction: a rule is declared inside exactly one section, so it has exactly one
/// category. See the module docs for why the vocabulary is defect classes and not clippy's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// The code is wrong: it does not compile, cannot resolve, or contradicts its own types.
    Correctness,
    /// The code is legal here but not everywhere it must run: a feature the project has not
    /// enabled, a deprecated API, a construct the targeted release does not have.
    Compatibility,
    /// The code compiles and is probably not what was meant.
    Suspicious,
    /// Something is declared, computed or written and never read.
    Unused,
    /// The same meaning is available in a simpler form.
    Complexity,
    /// The same meaning is available in a cheaper form.
    Performance,
    /// The code is correct and does not read the way Java is conventionally written.
    Style,
    /// A declaration's name breaks the project's naming convention.
    Naming,
    /// A doc comment is missing, empty, or disagrees with what it documents.
    Documentation,
    /// The code is correct, idiomatic and fast, and the project has chosen to ban the construct
    /// anyway. Every rule here is [`Allow`](LintLevel::Allow) by default: a restriction nobody
    /// asked for is not a defect.
    Restriction,
}

impl Category {
    /// The `jalslint.toml` section this category is written as.
    pub const fn config_name(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Compatibility => "compatibility",
            Self::Suspicious => "suspicious",
            Self::Unused => "unused",
            Self::Complexity => "complexity",
            Self::Performance => "performance",
            Self::Style => "style",
            Self::Naming => "naming",
            Self::Documentation => "documentation",
            Self::Restriction => "restriction",
        }
    }

    /// Every category, in the order the sections are declared on [`Config`].
    pub const ALL: &'static [Self] = &[
        Self::Correctness,
        Self::Compatibility,
        Self::Suspicious,
        Self::Unused,
        Self::Complexity,
        Self::Performance,
        Self::Style,
        Self::Naming,
        Self::Documentation,
        Self::Restriction,
    ];
}

impl core::fmt::Display for Category {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.config_name())
    }
}

/// Linter configuration: ten sections, one per [`Category`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Config {
    /// `[correctness]` — the code is wrong.
    pub correctness: Correctness,
    /// `[compatibility]` — the code will not compile everywhere it must.
    pub compatibility: Compatibility,
    /// `[suspicious]` — the code is probably not what was meant.
    pub suspicious: Suspicious,
    /// `[unused]` — something is declared and never read.
    pub unused: Unused,
    /// `[complexity]` — the same meaning, more simply.
    pub complexity: Complexity,
    /// `[performance]` — the same meaning, more cheaply.
    pub performance: Performance,
    /// `[style]` — the code does not read the way Java is written.
    pub style: Style,
    /// `[naming]` — a declaration's name breaks the convention.
    pub naming: Naming,
    /// `[documentation]` — a doc comment is missing, empty, or wrong.
    pub documentation: Documentation,
    /// `[restriction]` — a construct this project has chosen to ban.
    pub restriction: Restriction,
    /// The project's resolved language [`FeatureSet`], injected by the host from the manifest's
    /// `[package] features` (see [`Manifest::feature_set`](crate::Manifest::feature_set)) — **not**
    /// a `jalslint.toml` key.
    ///
    /// It drives the `[compatibility]` rules: a construct whose [`Feature`](crate::Feature) is
    /// *absent* from this set is flagged (e.g. a top-level `main` when `compact-source-files` is
    /// not enabled). An empty set (the default — no `[package] features` declared) disables every
    /// such gate.
    #[serde(skip)]
    pub features: FeatureSet,
    /// Top-level tables the file wrote that are not sections. Per-section unknown keys are on the
    /// section; [`unknown_keys`](Self::unknown_keys) joins both.
    #[serde(skip)]
    unknown: UnknownKeys,
}

impl Config {
    /// This config with the project's resolved language [`FeatureSet`] attached.
    ///
    /// The only field a host sets by hand, and the reason it is set through a method: `Config`
    /// carries a private [`unknown`](Self::unknown_keys) field, so struct-update syntax
    /// (`Config { features, ..Default::default() }`) cannot reach it from outside this crate.
    #[must_use]
    pub const fn with_features(mut self, features: FeatureSet) -> Self {
        self.features = features;
        self
    }

    /// Every key the file wrote that this schema does not define, section-qualified
    /// (`"style.no-such-rule"`) or bare for a whole unknown table (`"rules"`), in sorted order.
    ///
    /// A host reports these: the keys were **kept** rather than rejected, so that one stale name
    /// cannot stop the rest of the file from loading ([`UnknownKeys`]), and reporting them is what
    /// keeps "kept" from meaning "silently dropped". The flat `[rules]` table this schema replaced
    /// shows up here as the single key `rules`.
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = self.unknown.iter().map(str::to_owned).collect();
        let mut qualified = |section: &str, keys: &UnknownKeys| {
            for key in keys.iter() {
                let mut name = String::from(section);
                name.push('.');
                name.push_str(key);
                out.push(name);
            }
        };
        qualified("correctness", &self.correctness.unknown);
        qualified("compatibility", &self.compatibility.unknown);
        qualified("suspicious", &self.suspicious.unknown);
        qualified("unused", &self.unused.unknown);
        qualified("complexity", &self.complexity.unknown);
        qualified("performance", &self.performance.unknown);
        qualified("style", &self.style.unknown);
        qualified("naming", &self.naming.unknown);
        qualified("documentation", &self.documentation.unknown);
        qualified("restriction", &self.restriction.unknown);
        out.sort_unstable();
        out
    }
}

impl<'de> Deserialize<'de> for Config {
    /// Section by section, keeping an unrecognized table rather than failing the file — the same
    /// policy the sections apply to their own keys, for the same reason.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ConfigVisitor;

        impl<'de> Visitor<'de> for ConfigVisitor {
            type Value = Config;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a jalslint.toml document")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Config, A::Error> {
                let mut out = Config::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "correctness" => out.correctness = map.next_value()?,
                        "compatibility" => out.compatibility = map.next_value()?,
                        "suspicious" => out.suspicious = map.next_value()?,
                        "unused" => out.unused = map.next_value()?,
                        "complexity" => out.complexity = map.next_value()?,
                        "performance" => out.performance = map.next_value()?,
                        "style" => out.style = map.next_value()?,
                        "naming" => out.naming = map.next_value()?,
                        "documentation" => out.documentation = map.next_value()?,
                        "restriction" => out.restriction = map.next_value()?,
                        _ => out.unknown.record(key, &mut map)?,
                    }
                }
                Ok(out)
            }
        }

        deserializer.deserialize_map(ConfigVisitor)
    }
}

impl crate::DiscoverableConfig for Config {
    const FILE_NAME: &'static str = "jalslint.toml";
}
