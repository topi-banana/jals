//! The non-default half of a [`Config`], as a section → key → value map.

use alloc::string::String;

use jals_config::fmt::Config;
use serde_json::{Map, Value};

pub(crate) use api::non_default;

/// The diff of a config against its defaults.
pub(crate) mod api {
    use super::{Config, Map, String, Value};

    /// The leaves of `config` that differ from [`Config::default`], grouped by section. Sections
    /// with no differing key are dropped entirely, so the caller can emit a `[section]` header
    /// exactly when this map has one.
    ///
    /// Returns `None` when the serialized config is not the two-level shape the emitter assumes —
    /// a root object of section objects of non-object leaves. That cannot happen with today's
    /// schema (`tests::the_schema_is_two_levels_deep` pins it), but a future section holding a
    /// nested struct would need real TOML sub-table handling, and silently flattening it would
    /// produce a file that does not round-trip. Refusing is the honest answer.
    pub(crate) fn non_default(config: &Config) -> Option<Map<String, Value>> {
        let (Value::Object(actual), Value::Object(default)) = (
            serde_json::to_value(config).ok()?,
            serde_json::to_value(Config::default()).ok()?,
        ) else {
            return None;
        };

        let mut out = Map::new();
        for (section, values) in actual {
            let Value::Object(values) = values else {
                return None;
            };
            let Some(Value::Object(defaults)) = default.get(&section) else {
                return None;
            };
            let mut changed = Map::new();
            for (key, value) in values {
                if value.is_object() {
                    return None;
                }
                if defaults.get(&key) != Some(&value) {
                    changed.insert(key, value);
                }
            }
            if !changed.is_empty() {
                out.insert(section, Value::Object(changed));
            }
        }
        Some(out)
    }
}
