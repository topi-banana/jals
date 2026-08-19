//! The value of a lint key: a [`LintLevel`], and the [`Lint`] that pairs one with a rule's own
//! options.
//!
//! # One key, two spellings
//!
//! A rule is written either as a bare level or as a table that adds its options:
//!
//! ```toml
//! [style]
//! wildcard-import = "error"                               # level only
//! missing-braces = { level = "warn", policy = "always" }  # level + options
//!
//! [suspicious.empty-catch]                                # the same, spelled long
//! allowed-names = ["ignored"]
//! ```
//!
//! The two spellings are one type ([`Lint`]), and `level` is the reserved key inside the table
//! form — an option may not be named `level`. Everything else in the table is the rule's own
//! options struct, flattened, so an option is spelled beside the level rather than under a second
//! `options` heading.
//!
//! # Why the table form may omit `level`
//!
//! A rule's built-in level lives in exactly one place: the [`Default`] impl of the section that
//! declares it. A table that sets only options must therefore *keep* that level rather than
//! restate it, which is why deserialization is a **patch** ([`LintPatch`]) applied onto the
//! default rather than a fresh value: `[suspicious.empty-catch] allowed-names = [...]` leaves the
//! level at its built-in `warn`, and a built-in that changes later moves with it. Making `level`
//! mandatory in the table form would have copied every built-in into every config file that
//! configures an option, where it would silently go stale.

use alloc::collections::BTreeSet;
use alloc::string::String;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What a rule does when it fires.
///
/// The configured half of the severity vocabulary — what a `jalslint.toml` sets a rule *to* — as
/// opposed to the presented [`DiagnosticSeverity`](crate::DiagnosticSeverity), which is what a
/// destination draws. [`Allow`](Self::Allow) is why they are two types: it is a level and not a
/// severity, because a rule set to it never runs and so never reaches a destination.
///
/// There are three levels and not rustc's four: `deny` and `forbid` differ from each other only in
/// whether an **in-source** attribute may override them, and jals has no in-source suppression to
/// override with (`jals-lint/README.md` § Roadmap records that as planned work). A fourth level
/// whose whole meaning is a mechanism that does not exist would be a level that behaves exactly
/// like [`Error`](Self::Error) and reads as though it did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LintLevel {
    /// The rule is disabled; the engine skips it entirely, so it costs nothing.
    Allow,
    /// The finding is reported as a warning.
    Warn,
    /// The finding is reported as an error.
    Error,
}

impl LintLevel {
    /// The lowercase name (`"allow"` / `"warn"` / `"error"`) — the spelling a `jalslint.toml` uses.
    /// Private: a consumer reads the same spelling through [`Display`](core::fmt::Display).
    const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl core::fmt::Display for LintLevel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The options a rule takes.
///
/// Implemented by every rule options struct and by [`NoOptions`]. It carries one fact:
/// [`HAS_KEYS`](Self::HAS_KEYS), which decides the *shape* a [`Lint`] serializes to.
///
/// The shape is a property of the options **type** and not of the values a particular config
/// holds, which is what makes the serialized document a readable schema: a rule that takes options
/// always shows them, so a test — or a person — can enumerate every key by walking one serialized
/// [`Config`](crate::lint::Config) rather than by knowing in advance which rules have options.
/// Collapsing a rule whose options merely *happen* to be untouched would have hidden exactly the
/// keys such a walk exists to find.
pub trait LintOptions: Default + PartialEq + Serialize {
    /// Whether this type contributes any keys to a rule's table form. `false` for [`NoOptions`]
    /// alone.
    const HAS_KEYS: bool = true;
}

/// A rule with no options of its own — the flattened tail of its table form is empty.
///
/// Written `Lint<NoOptions>` rather than `Lint<()>` because the tail has to deserialize from the
/// table's remaining keys, and a unit type deserializes from a unit, not from a map.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct NoOptions {}

impl LintOptions for NoOptions {
    const HAS_KEYS: bool = false;
}

/// One rule's configuration: the level it fires at, and its own options.
///
/// `O` is the rule's options struct — [`NoOptions`] for the rules that have none. See the module
/// docs for the two TOML spellings and for why the table form may omit `level`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lint<O = NoOptions> {
    /// What the rule does when it fires.
    pub level: LintLevel,
    /// The rule's own options, flattened beside `level` in the table form.
    pub options: O,
}

impl<O: Default> Lint<O> {
    /// The rule at `level` with default options — how a section's [`Default`] declares a rule's
    /// built-in level, and the only caller. A consumer builds one from its two public fields.
    pub(crate) fn at(level: LintLevel) -> Self {
        Self {
            level,
            options: O::default(),
        }
    }
}

impl<O: Default> Default for Lint<O> {
    /// [`Warn`](LintLevel::Warn) with default options. Present for completeness; a section states
    /// each rule's built-in level with [`at`](Self::at) instead of relying on this.
    fn default() -> Self {
        Self::at(LintLevel::Warn)
    }
}

/// The table form of a [`Lint`], for `Serialize` only: the level plus the options flattened beside
/// it, borrowed so serializing allocates nothing extra.
#[derive(Serialize)]
struct LintTable<'a, O> {
    level: LintLevel,
    #[serde(flatten)]
    options: &'a O,
}

impl<O: LintOptions> Serialize for Lint<O> {
    /// The bare level for a rule that takes no options, the table form for one that does — the
    /// shape follows the type, never the values (see [`LintOptions`]).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if O::HAS_KEYS {
            LintTable {
                level: self.level,
                options: &self.options,
            }
            .serialize(serializer)
        } else {
            self.level.serialize(serializer)
        }
    }
}

/// What one `jalslint.toml` key says about a rule: a level, options, or both.
///
/// The deserialized form of a rule key. It is a *patch* rather than a value because an absent
/// `level` must keep the rule's built-in one (see the module docs), so a section applies this onto
/// its own [`Default`] instead of replacing it.
pub(crate) struct LintPatch<O> {
    level: Option<LintLevel>,
    options: O,
}

impl<O> LintPatch<O> {
    /// Apply this key onto the rule's default: the level only when one was written, the options
    /// always (an absent option key already deserialized to its own default, which is the value
    /// the target holds).
    pub(crate) fn apply(self, target: &mut Lint<O>) {
        if let Some(level) = self.level {
            target.level = level;
        }
        target.options = self.options;
    }
}

/// The table spelling of [`LintPatch`], for `Deserialize` only. `level` is optional here and
/// nowhere else: this is the one place an unwritten level has to stay unwritten.
#[derive(Deserialize)]
#[serde(
    default,
    rename_all = "kebab-case",
    bound = "O: Default + Deserialize<'de>"
)]
struct LintPatchTable<O> {
    level: Option<LintLevel>,
    #[serde(flatten)]
    options: O,
}

impl<O: Default> Default for LintPatchTable<O> {
    fn default() -> Self {
        Self {
            level: None,
            options: O::default(),
        }
    }
}

impl<'de, O: Default + Deserialize<'de>> Deserialize<'de> for LintPatch<O> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Accepts both spellings: a string is a level, a map is a level plus flattened options.
        struct Either<O>(core::marker::PhantomData<O>);

        impl<'de, O: Default + Deserialize<'de>> Visitor<'de> for Either<O> {
            type Value = LintPatch<O>;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a lint level (\"allow\" / \"warn\" / \"error\") or a table of it and the rule's options")
            }

            fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<Self::Value, E> {
                let level = match name {
                    "allow" => LintLevel::Allow,
                    "warn" => LintLevel::Warn,
                    "error" => LintLevel::Error,
                    other => {
                        return Err(E::invalid_value(serde::de::Unexpected::Str(other), &self));
                    }
                };
                Ok(LintPatch {
                    level: Some(level),
                    options: O::default(),
                })
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let table = LintPatchTable::<O>::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )?;
                Ok(LintPatch {
                    level: table.level,
                    options: table.options,
                })
            }
        }

        deserializer.deserialize_any(Either(core::marker::PhantomData))
    }
}

/// The keys a config file wrote that this schema does not define.
///
/// A `jalslint.toml` naming a rule jals does not have — a typo, a rule from a newer release, a key
/// left over from the flat `[rules]` table this schema replaced — is **kept, not rejected**. The
/// alternative is `deny_unknown_fields`, under which one stale key fails the whole file and every
/// *other* rule in it silently stops being configured; `jals_config::fmt`'s `bool_or_named!` makes
/// the same choice for the same reason.
///
/// Ignoring a key silently would be the other failure, so the names are recorded here and a host
/// reports them ([`Config::unknown_keys`](crate::lint::Config::unknown_keys)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnknownKeys(BTreeSet<String>);

impl UnknownKeys {
    /// Record a key this schema does not define, and swallow its value.
    ///
    /// # Errors
    /// Propagates the deserializer's own error from reading the ignored value.
    pub(crate) fn record<'de, A: MapAccess<'de>>(
        &mut self,
        key: String,
        map: &mut A,
    ) -> Result<(), A::Error> {
        map.next_value::<IgnoredAny>()?;
        self.0.insert(key);
        Ok(())
    }

    /// The recorded keys, in sorted order.
    ///
    /// Crate-private, with [`Config::unknown_keys`](crate::lint::Config::unknown_keys) as the whole
    /// public surface: a section records a *bare* key and only the config knows which section it
    /// was under, so a consumer reading one section's set would get names it cannot act on.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}
