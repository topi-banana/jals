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

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// A parsed mapping file, indexed for deobfuscation (obfuscated → official lookups).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mappings {
    /// Official internal name → obfuscated internal name.
    official_to_obf: BTreeMap<String, String>,
    /// Obfuscated internal name → official internal name.
    obf_to_official: BTreeMap<String, String>,
    /// Official internal owner name → its declared members, keyed for obfuscated lookup.
    members: BTreeMap<String, ClassMembers>,
}

/// The renamed members of one class, keyed by their obfuscated identities.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ClassMembers {
    /// `(obfuscated name, obfuscated descriptor)` → official name.
    methods: BTreeMap<(String, String), String>,
    /// `(obfuscated name, obfuscated descriptor)` → official name.
    fields: BTreeMap<(String, String), String>,
    /// Obfuscated name → official name, only while exactly one method carries the obfuscated name.
    methods_by_name: BTreeMap<String, Option<String>>,
    /// Obfuscated name → official name, only while exactly one field carries the obfuscated name.
    fields_by_name: BTreeMap<String, Option<String>>,
}

impl Mappings {
    /// Parse a Mojang mapping file. Comments (`#`) and blank lines are skipped; anything else that
    /// does not match the grammar is an error naming the 1-based line.
    pub fn parse(text: &str) -> Result<Self, String> {
        // Pass 1: the whole class map. A member's descriptor translation can reference a class
        // declared anywhere in the file, so members only parse after every class is known.
        let mut lines = Vec::new();
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
            if mappings
                .official_to_obf
                .insert(official.clone(), obf.clone())
                .is_some()
            {
                return Err(format!(
                    "mapping line {number} redefines class `{official}`"
                ));
            }
            if mappings
                .obf_to_official
                .insert(obf.clone(), official.clone())
                .is_some()
            {
                return Err(format!(
                    "mapping line {number} reuses obfuscated class name `{obf}`"
                ));
            }
            lines.push((number, line, false));
        }

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
            let class_map = &mappings.official_to_obf;
            let members = mappings.members.entry(official_owner).or_default();
            if left.contains('(') {
                let (name, desc) = Self::method_entry(class_map, left, number)?;
                members.insert_method(obf_name.to_owned(), desc, name);
            } else {
                let (name, desc) = Self::field_entry(class_map, left, number)?;
                members.insert_field(obf_name.to_owned(), desc, name);
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

    /// The official internal name for an obfuscated internal name, when the class map covers it.
    pub fn remap_class(&self, obf_internal: &str) -> Option<&str> {
        self.obf_to_official.get(obf_internal).map(String::as_str)
    }

    /// The obfuscated internal name for an official internal name, when the class map covers it.
    pub fn obfuscate_class(&self, official_internal: &str) -> Option<&str> {
        self.official_to_obf
            .get(official_internal)
            .map(String::as_str)
    }

    /// The official name of a method declared by `owner_official` (internal form), looked up by
    /// its obfuscated name and descriptor.
    pub fn remap_method(
        &self,
        owner_official: &str,
        obf_name: &str,
        obf_desc: &str,
    ) -> Option<&str> {
        self.members
            .get(owner_official)?
            .methods
            .get(&(obf_name.to_owned(), obf_desc.to_owned()))
            .map(String::as_str)
    }

    /// The official name of a field declared by `owner_official` (internal form), looked up by
    /// its obfuscated name and descriptor.
    pub fn remap_field(
        &self,
        owner_official: &str,
        obf_name: &str,
        obf_desc: &str,
    ) -> Option<&str> {
        self.members
            .get(owner_official)?
            .fields
            .get(&(obf_name.to_owned(), obf_desc.to_owned()))
            .map(String::as_str)
    }

    /// The official name of a method when it is the only method carrying `obf_name` in the owner
    /// (used for annotation elements, which carry no descriptor).
    pub fn remap_method_by_name(&self, owner_official: &str, obf_name: &str) -> Option<&str> {
        self.members
            .get(owner_official)?
            .methods_by_name
            .get(obf_name)?
            .as_deref()
    }

    /// The official name of a field when it is the only field carrying `obf_name` in the owner
    /// (used for enum constants in annotations, which carry no descriptor).
    pub fn remap_field_by_name(&self, owner_official: &str, obf_name: &str) -> Option<&str> {
        self.members
            .get(owner_official)?
            .fields_by_name
            .get(obf_name)?
            .as_deref()
    }
}

impl ClassMembers {
    fn insert_method(&mut self, obf_name: String, obf_desc: String, official: String) {
        self.methods
            .insert((obf_name.clone(), obf_desc), official.clone());
        Self::insert_by_name(&mut self.methods_by_name, obf_name, official);
    }

    fn insert_field(&mut self, obf_name: String, obf_desc: String, official: String) {
        self.fields
            .insert((obf_name.clone(), obf_desc), official.clone());
        Self::insert_by_name(&mut self.fields_by_name, obf_name, official);
    }

    /// Keep `obf -> official` only while unambiguous: a second distinct official name for the same
    /// obfuscated name poisons the entry (`None`), so name-only lookups miss instead of guessing.
    fn insert_by_name(map: &mut BTreeMap<String, Option<String>>, obf: String, official: String) {
        match map.get_mut(&obf) {
            Some(slot) => {
                if slot.as_ref() != Some(&official) {
                    *slot = None;
                }
            }
            None => {
                map.insert(obf, Some(official));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_classes_fields_and_methods() {
        let text = "\
# comment
com.example.Outer -> a:
    int count -> a
    int size() -> b
    1:2:com.example.Outer nest(com.example.Outer) -> c
com.example.Outer$Inner -> a$a:
    java.lang.String name -> a
";
        let map = Mappings::parse(text).expect("parses");
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
        assert!(Mappings::parse("    int x -> a\n").is_err());
    }
}
