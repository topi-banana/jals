//! Mapping files, parsed into the one index a remapper reads.
//!
//! Two grammars, one output. **ProGuard-style** is what Mojang publishes for each Minecraft release
//! (`server_mappings` / `client_mappings` in the version metadata): line-oriented, a class line
//! `official.Name -> obfuscated:` introducing indented member lines that map fields
//! (`type name -> obfuscated`) and methods (`start:end:return name(params) -> obfuscated`), all in
//! *dotted* Java names. **Tiny v2** is the tab-separated format Fabric publishes: a header naming
//! two or more namespaces, then `c` / `f` / `m` sections carrying one name per namespace, in
//! internal (`/`-separated) form with JVM descriptors.
//!
//! Both are converted to the internal form class files use, with the member descriptors precomputed
//! in the *source* namespace so a remapper can look members up by `(owner, source name, source
//! descriptor)`.
//!
//! The parser is strict: a malformed line fails the whole file, because silently dropping rename
//! information would produce an inconsistent jar. Strict means "reject a line that does not match
//! the grammar", not "reject a line whose content this crate ignores" — tiny v2's parameter, local
//! variable, and javadoc-comment sections are checked for shape and skipped, and its spec requires
//! that an unknown section type be skipped rather than rejected, so a mapping file written against
//! a later revision of the format still loads.
//!
//! One parse describes one *pair* of namespaces, so it serves both directions: deobfuscating a
//! library into the names a project is written against, and reobfuscating that project's own output
//! back into the names its runtime loads. [`RemapDirection`] chooses which way the indices are built,
//! and everything downstream — the hierarchy walk, the descriptor rewrite, the member lookup — is
//! written against "source" and "target" rather than against either namespace by name.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use jals_classfile::{FieldType, MethodDescriptor, ReturnType};

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
    /// Parse a mapping file for one direction. Blank lines are skipped (and, in ProGuard-style text, `#`
    /// comments); anything else that does not match the grammar is an error naming the 1-based line.
    pub(crate) fn parse(
        text: &str,
        format: &MappingFormat,
        direction: RemapDirection,
    ) -> Result<Self, String> {
        match format {
            MappingFormat::Proguard => Self::parse_proguard(text, direction),
            MappingFormat::TinyV2 { from, to } => Self::parse_tiny_v2(text, from, to, direction),
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

    /// Parse tiny v2 text, reading the `from` → `to` namespace pair in one [`RemapDirection`].
    ///
    /// The file names its namespaces in a header and writes one name column per namespace, so this
    /// projects an N-namespace table onto the pair the caller selected before anything downstream
    /// sees it. Descriptors are written in the *first* namespace whatever pair is read, so they are
    /// translated into the source namespace here, which is the tiny analogue of the two-class-map
    /// dance `parse_proguard` performs above.
    fn parse_tiny_v2(
        text: &str,
        from: &str,
        to: &str,
        direction: RemapDirection,
    ) -> Result<Self, String> {
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(index, raw)| (index + 1, raw.strip_suffix('\r').unwrap_or(raw)));

        // The header names every namespace in the file, so it is what decides whether the requested
        // pair can be read at all.
        let (header_number, header) = lines
            .by_ref()
            .find(|(_, line)| !line.is_empty())
            .ok_or_else(|| "mapping file is empty".to_owned())?;
        let namespaces = Self::tiny_header(header, header_number)?;
        let position = |wanted: &str| {
            namespaces
                .iter()
                .position(|declared| *declared == wanted)
                .ok_or_else(|| {
                    format!(
                        "mapping file names no namespace `{wanted}` (it declares `{}`)",
                        namespaces.join("`, `")
                    )
                })
        };
        let from_index = position(from)?;
        let to_index = position(to)?;
        if from_index == to_index {
            return Err(format!(
                "mapping namespaces `{from}` and `{to}` are the same namespace"
            ));
        }
        let (source_index, target_index) = match direction {
            RemapDirection::Deobfuscate => (from_index, to_index),
            RemapDirection::Reobfuscate => (to_index, from_index),
        };

        // Properties sit between the header and the first class section, and only one of them
        // changes how a name is read.
        let mut escaped = false;
        let mut body = Vec::new();
        let mut in_header = true;
        for (number, line) in lines {
            if line.is_empty() {
                continue;
            }
            let depth = line.bytes().take_while(|&byte| byte == b'\t').count();
            let rest = &line[depth..];
            if in_header && depth == 1 {
                let key = rest.split('\t').next().unwrap_or_default();
                if key.is_empty() {
                    return Err(format!("mapping line {number} is a property with no key"));
                }
                // Every other property is a section this crate does not read; the spec requires an
                // unknown key be skipped rather than rejected.
                escaped |= key == "escaped-names";
                continue;
            }
            in_header = false;
            body.push((number, depth, rest));
        }

        // Pass 1: the class table. A member's descriptor can name any class in the file, so members
        // only parse once every class is known — the same reason `parse_proguard` walks twice.
        let mut classes: BTreeMap<String, String> = BTreeMap::new();
        let mut targets: BTreeMap<String, String> = BTreeMap::new();
        let mut first_to_source = Self::default();
        for &(number, depth, rest) in &body {
            if depth != 0 {
                continue;
            }
            let mut columns = rest.split('\t');
            if columns.next() != Some("c") {
                continue;
            }
            let names = Self::tiny_names(columns, namespaces.len(), number, escaped)?;
            let source = &names[source_index];
            if source.is_empty() {
                // Not present in the namespace being read, so nothing here can be keyed by it.
                continue;
            }
            // An empty target name means "no name in that namespace", which for a rename is the
            // identity. Recording it as such rather than omitting it is what keeps the class
            // reachable: a member lookup walks the hierarchy through `remap_class` and skips any
            // owner the class table misses, so an omitted identity would let a member inherit a
            // supertype's rename instead of keeping its own name.
            let target = if names[target_index].is_empty() {
                source.clone()
            } else {
                names[target_index].clone()
            };
            if classes.insert(source.clone(), target.clone()).is_some() {
                return Err(format!("mapping line {number} redefines class `{source}`"));
            }
            if let Some(previous) = targets.insert(target.clone(), source.clone()) {
                return Err(format!(
                    "mapping line {number} maps class `{source}` onto `{target}`, which \
                     `{previous}` already maps onto"
                ));
            }
            // The first namespace's own map, which descriptor translation reads. Its uniqueness is
            // checked even when the pair being read makes it neither side of `classes` — the same
            // integrity check `parse_proguard` keeps on the direction it discards, and here it is
            // load-bearing: two classes sharing a first-namespace name would silently give every
            // descriptor mentioning it whichever translation came last.
            if first_to_source
                .classes
                .insert(names[0].clone(), source.clone())
                .is_some()
            {
                return Err(format!(
                    "mapping line {number} reuses `{}` in the file's first namespace",
                    names[0]
                ));
            }
        }
        let mut mappings = Self {
            classes,
            members: BTreeMap::new(),
        };

        // Pass 2: members attach to the class section above them. Sections this crate does not read
        // are shape-checked and skipped, and an unknown one takes its whole subtree with it.
        let mut owner = TinyOwner::Uncovered;
        let mut skip_below: Option<usize> = None;
        for &(number, depth, rest) in &body {
            if let Some(level) = skip_below {
                if depth > level {
                    continue;
                }
                skip_below = None;
            }
            let mut columns = rest.split('\t');
            let tag = columns.next().unwrap_or_default();
            match (depth, tag) {
                (0, "c") => {
                    let names = Self::tiny_names(columns, namespaces.len(), number, escaped)?;
                    owner = mappings
                        .classes
                        .get(&names[source_index])
                        .map_or(TinyOwner::Uncovered, |target| {
                            TinyOwner::Named(target.clone())
                        });
                }
                (1, "f" | "m") => {
                    let is_method = tag == "m";
                    let descriptor = columns.next().ok_or_else(|| {
                        format!("mapping line {number} is a member with no descriptor")
                    })?;
                    let descriptor = Self::tiny_descriptor(descriptor, is_method, number, escaped)?;
                    let names = Self::tiny_names(columns, namespaces.len(), number, escaped)?;
                    let TinyOwner::Named(owner_key) = &owner else {
                        continue;
                    };
                    let owner_key = owner_key.clone();
                    let source_name = &names[source_index];
                    if source_name.is_empty() {
                        continue;
                    }
                    // Written in the file's first namespace whichever pair is being read, so it
                    // becomes a key only once translated into the source one.
                    let source_desc = first_to_source.remap_descriptor(&descriptor);
                    let target_name = if names[target_index].is_empty() {
                        source_name.clone()
                    } else {
                        names[target_index].clone()
                    };
                    let members = mappings.members.entry(owner_key).or_default();
                    if is_method {
                        members.insert_method(source_name.clone(), source_desc, target_name);
                    } else {
                        members.insert_field(source_name.clone(), source_desc, target_name);
                    }
                }
                // Javadoc, at each of the three levels the grammar puts it.
                (1..=3, "c") => Self::tiny_comment(columns, number)?,
                // Parameters and local variables: named, but not renamed by this crate.
                (2, "p") => Self::tiny_variable(columns, false, number)?,
                (2, "v") => Self::tiny_variable(columns, true, number)?,
                (_, _) => skip_below = Some(depth),
            }
        }
        Ok(mappings)
    }

    /// Parse the `tiny<tab>2<tab><minor><tab><ns>…` header into its namespace list.
    fn tiny_header(line: &str, number: usize) -> Result<Vec<&str>, String> {
        let mut columns = line.split('\t');
        if columns.next() != Some("tiny") {
            return Err(format!("mapping line {number} is not a tiny header"));
        }
        match columns.next() {
            Some("2") => {}
            Some(other) => {
                return Err(format!(
                    "mapping line {number} declares tiny major version `{other}`, not 2"
                ));
            }
            None => return Err(format!("mapping line {number} has no tiny version")),
        }
        // The minor version is checked for shape and ignored: every revision so far only adds
        // sections, and this parser skips the ones it does not read.
        match columns.next() {
            Some(minor) if !minor.is_empty() && minor.bytes().all(|c| c.is_ascii_digit()) => {}
            _ => {
                return Err(format!(
                    "mapping line {number} has a malformed tiny minor version"
                ));
            }
        }
        let namespaces: Vec<&str> = columns.collect();
        if namespaces.len() < 2 {
            return Err(format!(
                "mapping line {number} declares fewer than two namespaces"
            ));
        }
        for (index, namespace) in namespaces.iter().enumerate() {
            if namespace.is_empty() {
                return Err(format!("mapping line {number} has an empty namespace name"));
            }
            if namespaces[..index].contains(namespace) {
                return Err(format!(
                    "mapping line {number} declares namespace `{namespace}` twice"
                ));
            }
        }
        Ok(namespaces)
    }

    /// The per-namespace name columns of a class or member section, one entry per declared
    /// namespace.
    ///
    /// The grammar lets trailing namespaces be omitted entirely, and a present-but-empty column
    /// means the same thing — no name in that namespace — so a short line is padded rather than
    /// rejected. A long one is an error: it describes a namespace the header never declared.
    ///
    /// The **first** namespace's name is the exception the grammar makes mandatory
    /// (`<class-name-a> ::= <class-name>`, never the `<optional-…>` form the rest take), and it is
    /// mandatory for a reason this parser depends on: descriptors are written in that namespace, so
    /// a first column that named nothing would be a class the descriptor translation below could
    /// key on nothing at all.
    fn tiny_names<'a>(
        columns: impl Iterator<Item = &'a str>,
        count: usize,
        number: usize,
        escaped: bool,
    ) -> Result<Vec<String>, String> {
        let mut names = Vec::with_capacity(count);
        for column in columns {
            if names.len() == count {
                return Err(format!(
                    "mapping line {number} has more names than the header declares namespaces"
                ));
            }
            names.push(Self::tiny_string(column, number, escaped)?);
        }
        if names.first().is_none_or(String::is_empty) {
            return Err(format!(
                "mapping line {number} has no name in the file's first namespace"
            ));
        }
        names.resize(count, String::new());
        Ok(names)
    }

    /// A member section's descriptor column, validated against the kind of member that declared it.
    ///
    /// Validated rather than carried through verbatim because a descriptor is what a member lookup
    /// is keyed by: an unparsable one would silently key an entry nothing can ever match, which is
    /// a member that quietly keeps its obfuscated name in an otherwise remapped jar.
    fn tiny_descriptor(
        raw: &str,
        is_method: bool,
        number: usize,
        escaped: bool,
    ) -> Result<String, String> {
        let descriptor = Self::tiny_string(raw, number, escaped)?;
        let valid = if is_method {
            MethodDescriptor::parse(&descriptor).is_ok()
        } else {
            FieldType::parse(&descriptor).is_ok()
        };
        if !valid {
            return Err(format!(
                "mapping line {number} has a malformed descriptor `{descriptor}`"
            ));
        }
        Ok(descriptor)
    }

    /// A javadoc section: exactly one column, which this crate does not keep.
    fn tiny_comment<'a>(
        mut columns: impl Iterator<Item = &'a str>,
        number: usize,
    ) -> Result<(), String> {
        if columns.next().is_none() {
            return Err(format!("mapping line {number} is a comment with no text"));
        }
        if columns.next().is_some() {
            return Err(format!("mapping line {number} has a malformed comment"));
        }
        Ok(())
    }

    /// A parameter (`p`) or local-variable (`v`) section. Neither is renamed by this crate — a jar
    /// carries parameter names only as debug attributes — but a malformed one still fails the file,
    /// because a line this parser cannot account for is a line it may be misreading the shape of.
    fn tiny_variable<'a>(
        mut columns: impl Iterator<Item = &'a str>,
        is_local: bool,
        number: usize,
    ) -> Result<(), String> {
        let mut integer = |allow_absent: bool| match columns.next() {
            Some("-1") if allow_absent => Ok(()),
            Some(value) if !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()) => Ok(()),
            _ => Err(format!("mapping line {number} has a malformed index")),
        };
        integer(false)?;
        if is_local {
            // `v` adds a start offset and an LVT index, where `-1` says the entry has none.
            integer(false)?;
            integer(true)?;
        }
        Ok(())
    }

    /// One `<conf-safe-string>` column, unescaped when the file declared `escaped-names`.
    ///
    /// The escapes are the characters the format's tab/newline framing would otherwise swallow, so
    /// an unknown one is rejected: a name whose escaping this parser does not understand is a name
    /// it would write into a class file wrong.
    fn tiny_string(raw: &str, number: usize, escaped: bool) -> Result<String, String> {
        if !escaped || !raw.contains('\\') {
            return Ok(raw.to_owned());
        }
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some(other) => {
                    return Err(format!(
                        "mapping line {number} has an unknown escape `\\{other}`"
                    ));
                }
                None => {
                    return Err(format!("mapping line {number} ends in a trailing escape"));
                }
            }
        }
        Ok(out)
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

    /// Rewrite every class name inside a field or method descriptor through the class table,
    /// leaving a descriptor this table says nothing about untouched.
    ///
    /// It lives with the table rather than with the remapper because it is not only the remapper's:
    /// tiny v2 writes its member descriptors in the file's first namespace, so parsing one already
    /// needs this translation before an entry can be keyed. A second implementation next to the
    /// parser is the kind of split derivation that ends in two answers, so there is one.
    pub(crate) fn remap_descriptor(&self, descriptor: &str) -> String {
        if let Ok(method) = MethodDescriptor::parse(descriptor) {
            let params = method
                .params
                .into_iter()
                .map(|param| self.remap_field_type(param))
                .collect();
            let return_type = match method.return_type {
                ReturnType::Void => ReturnType::Void,
                ReturnType::Type(ty) => ReturnType::Type(self.remap_field_type(ty)),
            };
            return MethodDescriptor {
                params,
                return_type,
            }
            .to_string();
        }
        if let Ok(field) = FieldType::parse(descriptor) {
            return self.remap_field_type(field).to_string();
        }
        descriptor.to_owned()
    }

    /// [`remap_descriptor`](Self::remap_descriptor) for one already-parsed type.
    fn remap_field_type(&self, ty: FieldType) -> FieldType {
        match ty {
            FieldType::Base(base) => FieldType::Base(base),
            FieldType::Object(name) => {
                FieldType::Object(self.remap_class(&name).map(str::to_owned).unwrap_or(name))
            }
            FieldType::Array(inner) => FieldType::Array(Box::new(self.remap_field_type(*inner))),
        }
    }
}

/// Which class section a tiny v2 member line attaches to.
///
/// "Before any class" is deliberately not a state here, unlike the ProGuard-style parser's explicit
/// rejection of one: the grammar indents a property exactly as far as a member line, so a `\tm`
/// ahead of the first class section *is* a property, and a member under an unknown top-level
/// section is skipped along with its parent. What is left is the one distinction that decides
/// whether a member can be indexed at all.
enum TinyOwner {
    /// No class section is open, or the open one has no name in the source namespace — so nothing
    /// under it can be keyed by that namespace.
    Uncovered,
    /// The open class, by its target name — the key members are indexed under.
    Named(String),
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

    /// The same two classes and members as [`SAMPLE`], written as tiny v2 over three namespaces.
    ///
    /// Deliberately the same shape as the ProGuard-style fixture, so the two grammars can be asserted to
    /// produce the same table: the format is what differs between them, and nothing else should.
    /// The third namespace is what a ProGuard-style file cannot have, and it is why the pair is selected
    /// rather than assumed.
    const SAMPLE_TINY: &str = "\
tiny\t2\t0\tofficial\tintermediary\tnamed
\tescaped-names
c\ta\tclass_1\tcom/example/Outer
\tc\tThe outer class.
\tf\tI\ta\tfield_1\tcount
\tm\t()I\tb\tmethod_1\tsize
\tm\t(La;)La;\tc\tmethod_2\tnest
\t\tp\t1\t\targ\tother
\t\tc\tNests one.
c\ta$a\tclass_1$class_2\tcom/example/Outer$Inner
\tf\tLjava/lang/String;\ta\tfield_2\tname
";

    fn deobfuscating(text: &str) -> Mappings {
        Mappings::parse(text, &MappingFormat::Proguard, RemapDirection::Deobfuscate)
            .expect("parses")
    }

    fn tiny(text: &str, from: &str, to: &str, direction: RemapDirection) -> Mappings {
        let format = MappingFormat::TinyV2 {
            from: from.to_owned(),
            to: to.to_owned(),
        };
        Mappings::parse(text, &format, direction).expect("parses")
    }

    /// Whatever `out` renamed, `back` renames back — over the whole table rather than a sample, so
    /// an entry only one direction indexes cannot hide.
    fn assert_inverses(out: &Mappings, back: &Mappings) {
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
                &MappingFormat::Proguard,
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
        let map = Mappings::parse(
            SAMPLE,
            &MappingFormat::Proguard,
            RemapDirection::Reobfuscate,
        )
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
        // renamed, reobfuscation renames back.
        let out = deobfuscating(SAMPLE);
        let back = Mappings::parse(
            SAMPLE,
            &MappingFormat::Proguard,
            RemapDirection::Reobfuscate,
        )
        .expect("parses");
        assert_inverses(&out, &back);
    }

    #[test]
    fn tiny_v2_produces_the_same_table_as_the_equivalent_proguard_file() {
        // The whole point of the format enum: two grammars, one index. Compared as whole tables
        // rather than by sampled lookups, so a member either grammar indexes alone shows up here.
        let proguard = deobfuscating(SAMPLE);
        let tiny = tiny(
            SAMPLE_TINY,
            "official",
            "named",
            RemapDirection::Deobfuscate,
        );
        assert_eq!(tiny, proguard);
    }

    #[test]
    fn tiny_v2_reads_the_namespace_pair_it_is_asked_for() {
        // The file names three namespaces, so the pair is a selection and not a property of the
        // text. Each pair is a different table over the same lines.
        let named = tiny(
            SAMPLE_TINY,
            "official",
            "named",
            RemapDirection::Deobfuscate,
        );
        assert_eq!(named.remap_class("a"), Some("com/example/Outer"));

        let intermediary = tiny(
            SAMPLE_TINY,
            "official",
            "intermediary",
            RemapDirection::Deobfuscate,
        );
        assert_eq!(intermediary.remap_class("a"), Some("class_1"));
        assert_eq!(
            intermediary.remap_method("class_1", "b", "()I"),
            Some("method_1")
        );
    }

    #[test]
    fn tiny_v2_keys_members_by_the_source_namespace_descriptor() {
        // Descriptors are written in the file's *first* namespace whichever pair is read, so a pair
        // that excludes it has to translate them before they can be keys. Reading
        // `intermediary → named` is what a Fabric mod's own classpath needs, and the `official`
        // descriptor `(La;)La;` is a key nothing in that jar would ever look up.
        let map = tiny(
            SAMPLE_TINY,
            "intermediary",
            "named",
            RemapDirection::Deobfuscate,
        );
        assert_eq!(
            map.remap_method("com/example/Outer", "method_2", "(Lclass_1;)Lclass_1;"),
            Some("nest")
        );
        assert_eq!(
            map.remap_method("com/example/Outer", "method_2", "(La;)La;"),
            None
        );
    }

    #[test]
    fn tiny_v2_directions_are_inverses_on_every_entry() {
        let out = tiny(
            SAMPLE_TINY,
            "official",
            "named",
            RemapDirection::Deobfuscate,
        );
        let back = tiny(
            SAMPLE_TINY,
            "official",
            "named",
            RemapDirection::Reobfuscate,
        );
        assert_inverses(&out, &back);
        // Reobfuscating keys by the *named* descriptor, translated out of the official one the file
        // writes — the tiny analogue of what the ProGuard test above asserts.
        assert_eq!(
            back.remap_method("a", "nest", "(Lcom/example/Outer;)Lcom/example/Outer;"),
            Some("c")
        );
    }

    #[test]
    fn tiny_v2_treats_an_absent_target_name_as_the_identity() {
        // A class with no name in the target namespace is not renamed — but it must still be in the
        // class table, because a member lookup walks the hierarchy through `remap_class` and skips
        // any owner the table misses. Omitting it would let `keep` inherit a supertype's rename.
        let text = "\
tiny\t2\t0\tofficial\tnamed
c\ta\tcom/example/Named
\tm\t()V\tkeep\t
c\tb
\tm\t()V\tx\trenamed
";
        let map = tiny(text, "official", "named", RemapDirection::Deobfuscate);
        assert_eq!(map.remap_class("a"), Some("com/example/Named"));
        assert_eq!(map.remap_class("b"), Some("b"));
        assert_eq!(
            map.remap_method("com/example/Named", "keep", "()V"),
            Some("keep")
        );
        assert_eq!(map.remap_method("b", "x", "()V"), Some("renamed"));
    }

    #[test]
    fn tiny_v2_reads_a_line_before_the_first_class_as_a_property() {
        // The grammar indents a property exactly as far as a member line, so what a line at that
        // depth *means* is decided by whether a class section has been opened — not by its tag.
        // This is why the parser has no "member before any class" rejection to mirror ProGuard's:
        // there is no such line to reject.
        let text = "\
tiny\t2\t0\tofficial\tnamed
\tm\tlooks like a member, is a property
c\ta\tcom/example/Named
";
        let map = tiny(text, "official", "named", RemapDirection::Deobfuscate);
        assert_eq!(map.remap_class("a"), Some("com/example/Named"));
        assert!(map.members.is_empty());
    }

    #[test]
    fn tiny_v2_leaves_a_descriptor_class_alone_when_the_source_namespace_does_not_name_it() {
        // The asymmetry worth pinning down. Reading `intermediary → named` makes the source
        // namespace something *other* than the one descriptors are written in, so a class the
        // source namespace does not name has no translation to offer — and a jar in that namespace
        // carries such a class under its first-namespace name, because that is what "no mapping"
        // means to every remapper that writes one. So the absent entry and the identity are the
        // same answer here, which is why the class table's identity fallback has no counterpart in
        // the descriptor table: `remap_class` missing an owner makes a member lookup walk *past*
        // it, while a descriptor that misses simply keeps the name it had.
        let text = "\
tiny\t2\t0\tofficial\tintermediary\tnamed
c\ta\tclass_1\tcom/example/Named
\tm\t(Lb;)V\tx\tinter_x\trenamed
c\tb\t\tcom/example/Other
";
        let map = tiny(text, "intermediary", "named", RemapDirection::Deobfuscate);
        // `b` is named in neither the source namespace nor, therefore, the class table.
        assert_eq!(map.remap_class("b"), None);
        // And the member still matches, keyed by the descriptor a jar in that namespace writes.
        assert_eq!(
            map.remap_method("com/example/Named", "inter_x", "(Lb;)V"),
            Some("renamed")
        );
    }

    #[test]
    fn tiny_v2_skips_the_sections_it_does_not_read() {
        // The spec requires an unknown section type be skipped rather than rejected, and it takes
        // its whole subtree with it. Rejecting one would mean a file written against a later
        // revision of the format stops loading — including, today, every real file's javadoc.
        let text = "\
tiny\t2\t0\tofficial\tnamed
\tsome-property\tsome value
c\ta\tcom/example/Named
\tc\tClass javadoc.
\tz\tan unknown member section
\t\tz\tits unknown child
\tm\t()V\tx\trenamed
\t\tv\t2\t7\t-1\t\tlocal
\t\t\tc\tVariable javadoc.
z\tan unknown top-level section
\tc\tskipped with its parent
";
        let map = tiny(text, "official", "named", RemapDirection::Deobfuscate);
        assert_eq!(
            map.remap_method("com/example/Named", "x", "()V"),
            Some("renamed")
        );
    }

    #[test]
    fn tiny_v2_unescapes_names_only_when_the_file_says_so() {
        let escaped = "\
tiny\t2\t0\tofficial\tnamed
\tescaped-names
c\ta\tcom/example/N\\u0041med
";
        // `\\u` is not one of the format's escapes, and a name this parser cannot read exactly is a
        // name it would write into a class file wrong.
        assert!(
            Mappings::parse(
                escaped,
                &MappingFormat::TinyV2 {
                    from: "official".to_owned(),
                    to: "named".to_owned(),
                },
                RemapDirection::Deobfuscate,
            )
            .is_err()
        );

        // Without the property, a backslash is just a character — a legal one in a JVM name.
        let literal = "\
tiny\t2\t0\tofficial\tnamed
c\ta\tcom/example/N\\Amed
";
        let map = tiny(literal, "official", "named", RemapDirection::Deobfuscate);
        assert_eq!(map.remap_class("a"), Some("com/example/N\\Amed"));
    }

    #[test]
    fn tiny_v2_rejects_what_it_cannot_read() {
        let reject = |text: &str| {
            Mappings::parse(
                text,
                &MappingFormat::TinyV2 {
                    from: "official".to_owned(),
                    to: "named".to_owned(),
                },
                RemapDirection::Deobfuscate,
            )
            .is_err()
        };

        assert!(reject(""), "an empty file has no header");
        assert!(
            reject("tiny\t1\t0\tofficial\tnamed\n"),
            "tiny v1 is a different grammar, not a tiny v2 file"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tintermediary\n"),
            "a pair the header does not declare cannot be read"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tnamed\nc\ta\tX\n\tm\tnot-a-descriptor\tx\ty\n"),
            "an unparsable descriptor keys an entry nothing can match"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tnamed\nc\ta\tX\nc\tb\tX\n"),
            "two classes cannot map onto one name"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tnamed\nc\ta\tX\ta\tb\n"),
            "more names than the header declares namespaces"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tnamed\nc\t\tX\n"),
            "the first namespace's name is the one the grammar makes mandatory"
        );
        assert!(
            Mappings::parse(
                "tiny\t2\t0\tofficial\tintermediary\tnamed\nc\ta\tp\tX\nc\ta\tq\tY\n",
                &MappingFormat::TinyV2 {
                    from: "intermediary".to_owned(),
                    to: "named".to_owned(),
                },
                RemapDirection::Deobfuscate,
            )
            .is_err(),
            "a first-namespace name reused by two classes makes descriptor translation ambiguous, \
             even when neither side of the pair being read collides"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tnamed\nc\ta\tX\n\tm\t()V\t\ty\n"),
            "a member is named in the first namespace too, or its descriptor keys nothing"
        );
        assert!(
            reject("tiny\t2\t0\tofficial\tnamed\nc\ta\tX\n\t\tp\tnope\tq\n"),
            "a parameter index is an integer even though nothing here renames one"
        );
    }
}
