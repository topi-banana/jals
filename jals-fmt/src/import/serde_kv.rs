//! The bridge from a flat `key → value` map (as a native config file lowers to) into a typed,
//! `Deserialize`-derived importer model.
//!
//! Every native config format in scope — Eclipse `.prefs` (Java properties), Eclipse exported
//! XML, IntelliJ `.editorconfig`, IntelliJ code-style XML — is, once parsed, a flat set of
//! **string-valued** settings. Rather than hand-write a `Deserialize` for each importer, we lift
//! that map into a `serde_json::Value::Object` whose every value is a JSON string and let
//! `serde_json::from_value` populate the model. This keeps the models plain `#[derive(Deserialize)]`
//! structs (with `#[serde(default)]`, so the huge tail of native options we do **not** model is
//! silently dropped) and keeps the whole path `no_std + alloc` / wasm-safe.
//!
//! Because every leaf arrives as a JSON *string*:
//! - enum fields deserialize straight from their native token (`end_of_line`, `split_into_lines`,
//!   …) via `#[serde(rename_all = …)]` / `#[serde(rename = …)]`;
//! - numeric / bitmask / bool fields take a `#[serde(deserialize_with = …)]` coercer from the
//!   [`Kv`] helpers below, which parse the string and yield `None` on anything unparsable (native
//!   configs carry stray / tool-specific values we treat as "unset" rather than a hard error).

// Native token examples (`split_into_lines`, …) appear in prose without being Rust items.
#![allow(clippy::doc_markdown)]

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use core::str::FromStr;

use serde::de::value::StrDeserializer;
use serde::de::{DeserializeOwned, IntoDeserializer};
use serde::{Deserialize, Deserializer};

use super::ImportError;

/// The key/value → model bridge and its string-coercing `deserialize_with` helpers, grouped as
/// associated functions so `deserialize_with = "Kv::…"` references them by path.
pub(crate) struct Kv;

impl Kv {
    /// Deserialize a native-config key/value map into an importer model `T`.
    ///
    /// Keys absent from `T` are ignored (the model covers only the common-rule subset), and keys
    /// present in `T` but absent from `pairs` fall back to `T`'s `#[serde(default)]`.
    pub(crate) fn from_pairs<T: DeserializeOwned>(
        pairs: BTreeMap<String, String>,
    ) -> Result<T, ImportError> {
        let object: serde_json::Map<String, serde_json::Value> = pairs
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        serde_json::from_value(serde_json::Value::Object(object))
            .map_err(|err| ImportError::Deserialize(err.to_string()))
    }

    /// Coerce a stringly-typed number into the field's own type, yielding `None` on anything
    /// unparsable. Serves both the plain counts (`tabulation.size`, `indent_size`, …) and
    /// Eclipse's `alignment_for_*` bitmasks, a decimal integer whose *bits* encode the wrap policy
    /// (see [`super::eclipse::Alignment`]).
    pub(crate) fn opt_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: FromStr,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(raw.trim().parse().ok())
    }

    /// Coerce a stringly-typed `true` / `false` into `Option<bool>` (`None` otherwise). Matching is
    /// case-insensitive: `insert_final_newline` and friends are editorconfig core properties whose
    /// values the spec treats case-insensitively (`= TRUE`), while every native source emits
    /// canonical lowercase, so lowercasing only widens what parses.
    pub(crate) fn opt_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.trim().to_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        })
    }

    /// Coerce a stringly-typed enum token into `Option<T>`, yielding `None` for any token that is
    /// not one of `T`'s variants. This keeps every enum field as lenient as the numeric / bool
    /// coercers: a native value we do not model — including editorconfig's spec-valid `unset`
    /// (DESIGN §A.2) — leaves the option unset rather than failing the whole import.
    ///
    /// The token is lowercased first: editorconfig property *values* are case-insensitive per
    /// spec (`indent_style = Tab`, `end_of_line = CRLF`), and every native enum token we model is
    /// canonically lowercase, so this only widens what deserializes and never changes a match.
    pub(crate) fn opt_enum<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: DeserializeOwned,
    {
        let raw = String::deserialize(deserializer)?.to_lowercase();
        let token: StrDeserializer<'_, serde::de::value::Error> = raw.as_str().into_deserializer();
        Ok(T::deserialize(token).ok())
    }
}
