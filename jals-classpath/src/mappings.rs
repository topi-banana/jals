//! Mojang official ("mojmap") mapping files: the ProGuard-style text format Mojang publishes for
//! each Minecraft release (`server_mappings` / `client_mappings` in the version metadata).
//!
//! The format is line-oriented: a class line `official.Name -> obfuscated:` introduces a class,
//! and indented member lines below it map fields (`type name -> obfuscated`) and methods
//! (`start:end:return name(params) -> obfuscated`). Everything is written in *dotted* Java names;
//! this module converts to the internal (`/`-separated) form class files use and precomputes the
//! obfuscated descriptors so a remapper can look members up by `(owner, obfuscated name,
//! obfuscated descriptor)`.
//!
//! The parser is strict: a malformed line fails the whole file, because silently dropping rename
//! information would produce an inconsistent jar.
//!
//! One file describes one *pair* of namespaces, so it serves both directions: deobfuscating a
//! library into the names a project is written against, and reobfuscating that project's own output
//! back into the names its runtime loads. [`RemapDirection`] chooses which way the indices are built,
//! and everything downstream — the hierarchy walk, the descriptor rewrite, the member lookup — is
//! written against "source" and "target" rather than against either namespace by name.

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{MappingFormat, RemapDirection};

/// A parsed mapping file, indexed in one [`RemapDirection`] (source → target lookups).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mappings {
    /// Source internal name → target internal name.
    classes: BTreeMap<String, String>,
    /// **Target** internal owner name → its declared members, keyed by source identity.
    ///
    /// Keyed by the target owner because a caller remaps the owner class first and then asks about
    /// its members; that is the order the constant pool forces, since a member ref names its owner
    /// through a `Class` entry that has already been rewritten.
    members: BTreeMap<String, ClassMembers>,
}

/// The renamed members of one class, keyed by their source identities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ClassMembers {
    /// `(source name, source descriptor)` → target name.
    methods: BTreeMap<(String, String), String>,
    /// `(source name, source descriptor)` → target name.
    fields: BTreeMap<(String, String), String>,
    /// Source name → target name, only while exactly one method carries the source name.
    methods_by_name: BTreeMap<String, Option<String>>,
    /// Source name → target name, only while exactly one field carries the source name.
    fields_by_name: BTreeMap<String, Option<String>>,
}

impl Mappings {
    /// Parse a mapping file for one direction. Comments (`#`) and blank lines are skipped; anything
    /// else that does not match the grammar is an error naming the 1-based line.
    pub(crate) fn parse(
        text: &str,
        format: MappingFormat,
        direction: RemapDirection,
    ) -> Result<Self, String> {
        match format {
            MappingFormat::Proguard => Self::parse_proguard(text, direction),
        }
    }

    fn parse_proguard(text: &str, direction: RemapDirection) -> Result<Self, String> {
        // Pass 1: the whole class map. A member's descriptor translation can reference a class
        // declared anywhere in the file, so members only parse after every class is known.
        //
        // Both directions of the class map are built regardless of `direction`: the reverse one is
        // an integrity check (two official classes may not share an obfuscated name) even when it is
        // not the one kept, and the forward one is what descriptor translation reads.
        let mut lines = Vec::new();
        let mut official_to_obf: BTreeMap<String, String> = BTreeMap::new();
        let mut obf_to_official: BTreeMap<String, String> = BTreeMap::new();
        let mut mappings = Self::default();
        for (number, raw) in text.lines().enumerate() {
            let number = number + 1;
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with(char::is_whitespace) {
                lines.push((number, line.trim(), true));
                continue;
            }
            let (official, obf) = Self::split_arrow(line, number)?;
            let obf = obf.strip_suffix(':').ok_or_else(|| {
                format!("mapping line {number} is not a class line (missing `:`)")
            })?;
            let official = Self::internalize(official);
            let obf = Self::internalize(obf);
            if official_to_obf
                .insert(official.clone(), obf.clone())
                .is_some()
            {
                return Err(format!(
                    "mapping line {number} redefines class `{official}`"
                ));
            }
            if obf_to_official
                .insert(obf.clone(), official.clone())
                .is_some()
            {
                return Err(format!(
                    "mapping line {number} reuses obfuscated class name `{obf}`"
                ));
            }
            lines.push((number, line, false));
        }
        mappings.classes = match direction {
            RemapDirection::Deobfuscate => obf_to_official,
            RemapDirection::Reobfuscate => official_to_obf.clone(),
        };

        // Pass 2: member lines attach to the class line that most recently preceded them.
        let mut owner: Option<String> = None;
        for (number, line, is_member) in lines {
            if !is_member {
                // A class line: recover the official name (the class map already validated it).
                let (official, _) = Self::split_arrow(line, number)?;
                owner = Some(Self::internalize(official));
                continue;
            }
            let official_owner = owner
                .clone()
                .ok_or_else(|| format!("mapping line {number} is a member before any class"))?;
            let (left, obf_name) = Self::split_arrow(line, number)?;
            let is_method = left.contains('(');
            // The declared name plus its descriptor, with class names translated through
            // `class_map`. An empty map leaves them in the official namespace.
            let entry = |class_map: &BTreeMap<String, String>| {
                if is_method {
                    Self::method_entry(class_map, left, number)
                } else {
                    Self::field_entry(class_map, left, number)
                }
            };
            let (official_name, obf_desc) = entry(&official_to_obf)?;
            // The member's identity in each namespace. Deobfuscating keys by the obfuscated pair
            // under the official owner; reobfuscating keys by the official pair under the
            // obfuscated one. Both are derived from the same line, so neither direction can index a
            // member the other would have missed.
            let (owner_key, source_name, source_desc, target_name) = match direction {
                RemapDirection::Deobfuscate => {
                    (official_owner, obf_name.to_owned(), obf_desc, official_name)
                }
                RemapDirection::Reobfuscate => {
                    // The official descriptor is the same signature read without translating class
                    // names, so it comes from the same parser with an empty class map.
                    let (_, official_desc) = entry(&BTreeMap::new())?;
                    let obf_owner = official_to_obf
                        .get(&official_owner)
                        .cloned()
                        .ok_or_else(|| format!("mapping line {number} has an unmapped owner"))?;
                    (obf_owner, official_name, official_desc, obf_name.to_owned())
                }
            };
            let members = mappings.members.entry(owner_key).or_default();
            if is_method {
                members.insert_method(source_name, source_desc, target_name);
            } else {
                members.insert_field(source_name, source_desc, target_name);
            }
        }
        Ok(mappings)
    }

    /// Split `left -> right` exactly once; the Mojang format never contains a second arrow.
    fn split_arrow(line: &str, number: usize) -> Result<(&str, &str), String> {
        let (left, right) = line
            .split_once(" -> ")
            .ok_or_else(|| format!("mapping line {number} is missing ` -> `"))?;
        if right.contains(" -> ") {
            return Err(format!("mapping line {number} has more than one ` -> `"));
        }
        Ok((left, right))
    }

    /// Convert a dotted Java binary name (`com.foo.Outer$Inner`) to internal form.
    fn internalize(dotted: &str) -> String {
        dotted.replace('.', "/")
    }

    /// Parse a method member line's left side: `[start:end:]return name(params)`. The obfuscated
    /// descriptor is recomputed from the official signature through the class map.
    fn method_entry(
        class_map: &BTreeMap<String, String>,
        left: &str,
        number: usize,
    ) -> Result<(String, String), String> {
        let mut head = left;
        if head.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            // `start:end:` line-number prefix (always present in Mojang files, optional in others).
            let mut parts = head.splitn(3, ':');
            let (start, end, rest) = (parts.next(), parts.next(), parts.next());
            match (start, end, rest) {
                (Some(a), Some(b), Some(rest))
                    if !a.is_empty()
                        && !b.is_empty()
                        && a.bytes().all(|c| c.is_ascii_digit())
                        && b.bytes().all(|c| c.is_ascii_digit()) =>
                {
                    head = rest;
                }
                _ => {
                    return Err(format!(
                        "mapping line {number} has a bad line-number prefix"
                    ));
                }
            }
        }
        let open = head
            .find('(')
            .ok_or_else(|| format!("mapping line {number} has a malformed method"))?;
        let close = head
            .rfind(')')
            .ok_or_else(|| format!("mapping line {number} has a malformed method"))?;
        if close != head.len() - 1 || close < open {
            return Err(format!("mapping line {number} has a malformed method"));
        }
        let (ret_and_name, params) = (&head[..open], &head[open + 1..close]);
        let mut tokens = ret_and_name.split_whitespace();
        let ret = tokens
            .next()
            .ok_or_else(|| format!("mapping line {number} is missing a return type"))?;
        let name = tokens
            .next()
            .ok_or_else(|| format!("mapping line {number} is missing a method name"))?;
        if tokens.next().is_some() {
            return Err(format!("mapping line {number} has a malformed method"));
        }
        let mut desc = String::from("(");
        if !params.is_empty() {
            for param in params.split(',') {
                desc.push_str(&Self::obf_descriptor_of(class_map, param.trim(), number)?);
            }
        }
        desc.push(')');
        desc.push_str(&Self::obf_descriptor_of(class_map, ret, number)?);
        Ok((name.to_owned(), desc))
    }

    /// Parse a field member line's left side: `type name`.
    fn field_entry(
        class_map: &BTreeMap<String, String>,
        left: &str,
        number: usize,
    ) -> Result<(String, String), String> {
        let mut tokens = left.split_whitespace();
        let ty = tokens
            .next()
            .ok_or_else(|| format!("mapping line {number} is missing a field type"))?;
        let name = tokens
            .next()
            .ok_or_else(|| format!("mapping line {number} is missing a field name"))?;
        if tokens.next().is_some() {
            return Err(format!("mapping line {number} has a malformed field"));
        }
        Ok((
            name.to_owned(),
            Self::obf_descriptor_of(class_map, ty, number)?,
        ))
    }

    /// The descriptor fragment for a Java source type (`int[]`, `com.foo.Bar`), with class names
    /// translated to their obfuscated internal form when the class map covers them.
    fn obf_descriptor_of(
        class_map: &BTreeMap<String, String>,
        java_type: &str,
        number: usize,
    ) -> Result<String, String> {
        let mut dimensions = 0;
        let mut base = java_type.trim();
        while let Some(stripped) = base.strip_suffix("[]") {
            dimensions += 1;
            base = stripped;
        }
        if dimensions > 255 {
            return Err(format!("mapping line {number} has an absurd array depth"));
        }
        let mut out = "[".repeat(dimensions);
        match base {
            "byte" => out.push('B'),
            "char" => out.push('C'),
            "double" => out.push('D'),
            "float" => out.push('F'),
            "int" => out.push('I'),
            "long" => out.push('J'),
            "short" => out.push('S'),
            "boolean" => out.push('Z'),
            "void" => out.push('V'),
            class => {
                let official = Self::internalize(class);
                let internal = class_map.get(&official).unwrap_or(&official);
                out.push('L');
                out.push_str(internal);
                out.push(';');
            }
        }
        Ok(out)
    }

    /// The target internal name for a source internal name, when the class map covers it.
    pub(crate) fn remap_class(&self, source_internal: &str) -> Option<&str> {
        self.classes.get(source_internal).map(String::as_str)
    }

    /// The target name of a method declared by `owner_target` (internal form), looked up by its
    /// source name and descriptor.
    pub(crate) fn remap_method(
        &self,
        owner_target: &str,
        source_name: &str,
        source_desc: &str,
    ) -> Option<&str> {
        self.members
            .get(owner_target)?
            .methods
            .get(&(source_name.to_owned(), source_desc.to_owned()))
            .map(String::as_str)
    }

    /// The target name of a field declared by `owner_target` (internal form), looked up by its
    /// source name and descriptor.
    pub(crate) fn remap_field(
        &self,
        owner_target: &str,
        source_name: &str,
        source_desc: &str,
    ) -> Option<&str> {
        self.members
            .get(owner_target)?
            .fields
            .get(&(source_name.to_owned(), source_desc.to_owned()))
            .map(String::as_str)
    }

    /// The target name of a method when it is the only method carrying `source_name` in the owner
    /// (used for annotation elements, which carry no descriptor).
    pub(crate) fn remap_method_by_name(
        &self,
        owner_target: &str,
        source_name: &str,
    ) -> Option<&str> {
        self.members
            .get(owner_target)?
            .methods_by_name
            .get(source_name)?
            .as_deref()
    }

    /// The target name of a field when it is the only field carrying `source_name` in the owner
    /// (used for enum constants in annotations, which carry no descriptor).
    pub(crate) fn remap_field_by_name(
        &self,
        owner_target: &str,
        source_name: &str,
    ) -> Option<&str> {
        self.members
            .get(owner_target)?
            .fields_by_name
            .get(source_name)?
            .as_deref()
    }
}

impl ClassMembers {
    fn insert_method(&mut self, source_name: String, source_desc: String, target: String) {
        self.methods
            .insert((source_name.clone(), source_desc), target.clone());
        Self::insert_by_name(&mut self.methods_by_name, source_name, target);
    }

    fn insert_field(&mut self, source_name: String, source_desc: String, target: String) {
        self.fields
            .insert((source_name.clone(), source_desc), target.clone());
        Self::insert_by_name(&mut self.fields_by_name, source_name, target);
    }

    /// Keep `source -> target` only while unambiguous: a second distinct target name for the same
    /// source name poisons the entry (`None`), so name-only lookups miss instead of guessing.
    fn insert_by_name(map: &mut BTreeMap<String, Option<String>>, source: String, target: String) {
        match map.get_mut(&source) {
            Some(slot) => {
                if slot.as_ref() != Some(&target) {
                    *slot = None;
                }
            }
            None => {
                map.insert(source, Some(target));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# comment
com.example.Outer -> a:
    int count -> a
    int size() -> b
    1:2:com.example.Outer nest(com.example.Outer) -> c
com.example.Outer$Inner -> a$a:
    java.lang.String name -> a
";

    fn deobfuscating(text: &str) -> Mappings {
        Mappings::parse(text, MappingFormat::Proguard, RemapDirection::Deobfuscate).expect("parses")
    }

    #[test]
    fn parses_classes_fields_and_methods() {
        let text = SAMPLE;
        let map = deobfuscating(text);
        assert_eq!(map.remap_class("a"), Some("com/example/Outer"));
        assert_eq!(map.remap_class("a$a"), Some("com/example/Outer$Inner"));
        assert_eq!(
            map.remap_field("com/example/Outer", "a", "I"),
            Some("count")
        );
        assert_eq!(
            map.remap_method("com/example/Outer", "b", "()I"),
            Some("size")
        );
        // Method whose descriptor mentions a mapped class uses the obfuscated form.
        assert_eq!(
            map.remap_method("com/example/Outer", "c", "(La;)La;"),
            Some("nest")
        );
        assert_eq!(
            map.remap_field_by_name("com/example/Outer$Inner", "a"),
            Some("name")
        );
    }

    #[test]
    fn rejects_member_before_class() {
        assert!(
            Mappings::parse(
                "    int x -> a\n",
                MappingFormat::Proguard,
                RemapDirection::Deobfuscate
            )
            .is_err()
        );
    }

    #[test]
    fn reobfuscating_indexes_the_same_file_the_other_way() {
        // One file, two tables. What each direction has to get right is not just the class map but
        // *which* namespace the member key is written in: reobfuscating looks a member up by the
        // official name and the official descriptor, under the obfuscated owner.
        let map = Mappings::parse(SAMPLE, MappingFormat::Proguard, RemapDirection::Reobfuscate)
            .expect("parses");

        assert_eq!(map.remap_class("com/example/Outer"), Some("a"));
        assert_eq!(map.remap_class("com/example/Outer$Inner"), Some("a$a"));
        assert_eq!(map.remap_field("a", "count", "I"), Some("a"));
        assert_eq!(map.remap_method("a", "size", "()I"), Some("b"));
        // The descriptor is the *official* one here — the same signature the deobfuscating table
        // keys as `(La;)La;`.
        assert_eq!(
            map.remap_method("a", "nest", "(Lcom/example/Outer;)Lcom/example/Outer;"),
            Some("c")
        );
        assert_eq!(map.remap_field_by_name("a$a", "name"), Some("a"));
    }

    #[test]
    fn the_two_directions_are_inverses_on_every_entry() {
        // The property that makes a build's reobfuscation trustworthy: whatever deobfuscation
        // renamed, reobfuscation renames back. Checked over the whole table rather than a sample,
        // so an entry only one direction indexes cannot hide.
        let out = deobfuscating(SAMPLE);
        let back = Mappings::parse(SAMPLE, MappingFormat::Proguard, RemapDirection::Reobfuscate)
            .expect("parses");

        for (source, target) in &out.classes {
            assert_eq!(
                back.remap_class(target),
                Some(source.as_str()),
                "class `{source}` does not round-trip"
            );
        }
        for (owner_target, members) in &out.members {
            let owner_source = out
                .classes
                .iter()
                .find_map(|(s, t)| (t == owner_target).then_some(s.as_str()))
                .expect("every owner is a mapped class");
            for ((source_name, _), target_name) in &members.methods {
                assert!(
                    back.remap_method_by_name(owner_source, target_name)
                        .is_some_and(|back_name| back_name == source_name),
                    "method `{owner_target}.{source_name}` does not round-trip"
                );
            }
            for ((source_name, _), target_name) in &members.fields {
                assert!(
                    back.remap_field_by_name(owner_source, target_name)
                        .is_some_and(|back_name| back_name == source_name),
                    "field `{owner_target}.{source_name}` does not round-trip"
                );
            }
        }
    }
}
