//! Text-to-text mapping operations for the declarative task graph.
//!
//! Three operations over the *text* a mapping set travels as, each the host half of one task node:
//!
//! - [`MappingText::compose`] joins two published mapping sets into one table. Neither half of a
//!   Fabric-style renaming is published as a pair: Mojang publishes `project name → obfuscated`
//!   (ProGuard-style text) and Fabric publishes `obfuscated → alternative` (tiny v2), so the table
//!   a mod's two remap steps read is a composition of the two, joined on the obfuscated member by
//!   name **and** descriptor. The result is tiny v2 text naming the pair the caller's
//!   [`MappingFormat`] states, with descriptors in the project-side namespace — one file both a
//!   deobfuscating and a reobfuscating remap can read.
//! - [`MappingText::append_copies`] extends a tiny v2 text with sections that re-file existing
//!   entries under additional owners. A member lookup during a remap walks the class hierarchy and
//!   consults the table under each owner it reaches; a class that *declares* a member it does not
//!   *extend* — a Mixin-style shadow, a relocated class — is invisible to that walk until the table
//!   carries a section for it. The copies name what to duplicate, and the operation resolves the
//!   entries itself, so a caller never has to know the target-side names.
//! - [`MappingText::resolve_references`] answers member references written as text: one reference
//!   per line, each with the owner context to resolve an unqualified name against, each answered
//!   with the reference rewritten part by part into the table's other namespace. It is the text
//!   analogue of what a remap does to a constant pool, for the artifacts that carry member names as
//!   strings — a reference map, an access-transformer list — where no class file exists to walk.
//!
//! All three are memoized through the artifact cache, on the digests of their inputs and their own
//! output version: a whole-game composition parses megabytes of text, and a plan that re-runs
//! because a script re-ran must not pay that twice. The memo follows [`JarRemap`](crate::JarRemap)'s
//! shape — an advisory locator index in front of a verified read — and its versions ride
//! [`JarTransforms`](crate::JarTransforms) into every consumer that memoizes around them.
//!
//! The line grammars are deliberately dumb — tab-separated columns, one record per line — because
//! the *content* of a record is the caller's domain: which references a resolver is asked about,
//! and what a passthrough column means, is decided by the script that wrote the request and reads
//! the answer, never here.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jals_classfile::{FieldType, MethodDescriptor};
use jals_progress::Task;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, ProvenanceFold,
};

use crate::mappings::Mappings;
use crate::{MappingFormat, RemapDirection};

/// What [`MappingText::compose`] writes, which is also what the two other operations read: a bump
/// changes the bytes an operation produces for unchanged inputs, so every memo naming one has to
/// move with it.
pub(crate) const COMPOSE_MAPPINGS_OUTPUT_VERSION: u32 = 1;
/// The same, for what [`MappingText::append_copies`] writes.
pub(crate) const COPY_MAPPINGS_OUTPUT_VERSION: u32 = 1;
/// The same, for what [`MappingText::resolve_references`] writes.
pub(crate) const RESOLVE_REFERENCES_OUTPUT_VERSION: u32 = 1;

/// Text-level mapping operations a task plan can name.
pub struct MappingText;

/// One member line of an official mapping text, in both namespaces at once.
///
/// The join a composition performs is on the *obfuscated* identity of a member, while the table it
/// writes is keyed by the *project-side* one, so a line is held in both until the join decides
/// which side answers.
struct OfficialMember<'a> {
    project_owner: String,
    is_method: bool,
    project_desc: String,
    project_name: String,
    obf_owner: String,
    obf_desc: String,
    obf_name: &'a str,
}

/// The obfuscated-side half of a composition: what the alternative table names, keyed the way the
/// join asks for it.
struct AlternativeTable {
    /// Obfuscated internal class name → alternative-namespace name.
    classes: BTreeMap<String, String>,
    /// `(obfuscated owner, is method, obfuscated descriptor, obfuscated name)` → alternative name.
    members: BTreeMap<(String, bool, String, String), String>,
    /// `(is method, obfuscated descriptor, obfuscated name)` — the member identities the table
    /// names *somewhere*, which is how the join tells "declared by a supertype" from "not renamed
    /// at all".
    signatures: BTreeSet<(bool, String, String)>,
}

impl MappingText {
    /// Compose an official mapping text with an alternative tiny v2 text into one tiny v2 table.
    ///
    /// `official` is ProGuard-style text mapping project-side names onto obfuscated ones;
    /// `intermediary` is tiny v2 whose **first** namespace holds the obfuscated names and whose
    /// second is the alternative the composition maps onto. `format` states the namespace pair the
    /// output is written through: the composed text is tiny v2 naming `to` (the project side, whose
    /// descriptors the file carries) and `from` (the alternative side).
    ///
    /// The result covers the whole game rather than any project's slice of it: which names a caller
    /// cares about is not a question a composition can answer, and a table that answers every name
    /// needs no answering.
    ///
    /// # Errors
    /// A message naming what either text, the join, or the namespace pair refused.
    pub async fn compose<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        official: &str,
        intermediary: &str,
        format: &MappingFormat,
        report: &Task,
    ) -> Result<String, String> {
        let MappingFormat::TinyV2 { from, to } = format else {
            return Err(
                "composing mapping sets writes a tiny v2 table, which needs the namespace pair \
                 to write it through: the ProGuard-style grammar names none"
                    .to_owned(),
            );
        };
        Self::memoized(
            cache,
            b"jals.build-task.compose-mappings\0",
            COMPOSE_MAPPINGS_OUTPUT_VERSION,
            |fold| {
                fold.digest(ContentDigest::of(official.as_bytes()))
                    .digest(ContentDigest::of(intermediary.as_bytes()))
                    .bytes(from.as_bytes())
                    .bytes(to.as_bytes());
            },
            || Self::compose_text(official, intermediary, to, from),
            report,
        )
        .await
    }

    /// Extend a tiny v2 text with sections copied from owners it already names.
    ///
    /// `copies` is one request per line, `new-owner<tab>existing-owner<tab>member-name`, in the
    /// text's first namespace. Each request appends, under an identity section for `new-owner`,
    /// every member line of `existing-owner` whose first-namespace name is `member-name` — verbatim,
    /// so the copies carry the descriptors and target names the text already resolved. Requests are
    /// deduplicated and applied in sorted order, so one input is one output.
    ///
    /// Strict on purpose: a request whose existing owner the text does not name, or whose member
    /// has no entry under it, fails the whole operation rather than appending nothing, because the
    /// caller asked for an entry it will look up later and a silent absence is a name that quietly
    /// keeps its old spelling in an otherwise renamed jar.
    ///
    /// # Errors
    /// A message naming the line or owner that the text cannot answer.
    pub async fn append_copies<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        mappings: &str,
        copies: &str,
        report: &Task,
    ) -> Result<String, String> {
        Self::memoized(
            cache,
            b"jals.build-task.copy-mappings\0",
            COPY_MAPPINGS_OUTPUT_VERSION,
            |fold| {
                fold.digest(ContentDigest::of(mappings.as_bytes()))
                    .digest(ContentDigest::of(copies.as_bytes()));
            },
            || Self::append_copies_text(mappings, copies),
            report,
        )
        .await
    }

    /// Resolve textual member references through a mapping set.
    ///
    /// `requests` is one reference per line, `passthrough<tab>context<tab>reference`:
    ///
    /// - **passthrough** travels to the output untouched, as the first column of the answer. It
    ///   exists because a caller assembles something around the answers — a document whose keys are
    ///   the references — and the operation must not have to know its shape.
    /// - **context** is the owner an unqualified reference resolves against, as an internal class
    ///   name; empty when the reference qualifies itself.
    /// - **reference** is `[Lowner;]name[(method-descriptor)|:field-descriptor]` — the shape every
    ///   string-selecting tool writes a JVM member in. Each part is rewritten in place: a selector
    ///   that names no owner keeps naming none, so nothing invents an owner the consumer then has
    ///   to match.
    ///
    /// The mapping set is read in its reobfuscating direction: references are written in the
    /// project-side namespace (`to` for tiny v2, the official side for ProGuard-style text) and
    /// come back in the other. Resolution tries the context owner first and the whole table second,
    /// because the two namespaces disagree by design about where an inherited member is filed; a
    /// name a descriptor does not settle and the table answers more than once is refused rather
    /// than guessed. A constructor keeps its name — `<init>` spells the same in every namespace —
    /// while its owner and descriptor still move.
    ///
    /// Every failing line is collected, and the operation fails once, listing them all: a caller
    /// that sends only references the table should answer wants the whole miss list in one error,
    /// not one error per rebuild.
    ///
    /// # Errors
    /// The failures of every line that did not resolve, joined; or what the mapping text itself
    /// refused to parse as.
    pub async fn resolve_references<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        requests: &str,
        mappings: &str,
        format: &MappingFormat,
        report: &Task,
    ) -> Result<String, String> {
        Self::memoized(
            cache,
            b"jals.build-task.resolve-references\0",
            RESOLVE_REFERENCES_OUTPUT_VERSION,
            |fold| {
                fold.digest(ContentDigest::of(requests.as_bytes()))
                    .digest(ContentDigest::of(mappings.as_bytes()));
                format.fold_into(fold);
            },
            || Self::resolve_references_text(requests, mappings, format),
            report,
        )
        .await
    }

    /// The memo every operation rides: an advisory locator index in front of a verified read, and
    /// a publish behind a miss.
    ///
    /// `produce` runs only on a miss. It is synchronous because all three operations are pure text
    /// transforms — the only I/O in here is the cache's, which this function owns.
    async fn memoized<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        tag: &[u8],
        version: u32,
        fold_inputs: impl FnOnce(&mut ProvenanceFold),
        produce: impl FnOnce() -> Result<String, String>,
        report: &Task,
    ) -> Result<String, String> {
        let mut fold = ProvenanceFold::new(tag);
        fold.version(version);
        fold_inputs(&mut fold);
        let provenance = fold.finish();
        if let Some(key) = cache
            .indexed_key(CacheNamespace::BuildTaskArtifact, provenance)
            .await
            .map_err(|error| format!("mapping-text index lookup failed: {error:?}"))?
            && let Some(bytes) = cache
                .lookup(&key)
                .await
                .map_err(|error| format!("mapping-text cache read failed: {error:?}"))?
        {
            // The memo answered, so `produce` never runs — reported through the caller's unit,
            // which is what turns a silent instant into a `Fresh` line.
            report.fresh();
            return String::from_utf8(bytes)
                .map_err(|error| format!("cached mapping text is not UTF-8: {error}"));
        }
        let text = produce()?;
        let key = CacheKey::new(
            CacheNamespace::BuildTaskArtifact,
            provenance,
            ContentDigest::of(text.as_bytes()),
        );
        cache
            .publish(&key, text.as_bytes())
            .await
            .map_err(|error| format!("mapping-text publish failed: {error:?}"))?;
        cache
            .record_index(&key)
            .await
            .map_err(|error| format!("mapping-text index update failed: {error:?}"))?;
        Ok(text)
    }

    // -----------------------------------------------------------------------
    // Composition
    // -----------------------------------------------------------------------

    fn compose_text(
        official: &str,
        intermediary: &str,
        ns_project: &str,
        ns_alternative: &str,
    ) -> Result<String, String> {
        if ns_project.is_empty() || ns_alternative.is_empty() {
            return Err("a composed mapping text needs two namespace names".to_owned());
        }
        let (project_to_obf, member_lines) = Self::parse_official(official)?;
        let alternative = Self::parse_alternative(intermediary)?;

        // Classes first: every member join below reads the class map, and a class the alternative
        // table does not name keeps its obfuscated name — absent is not an error, it is a type
        // nobody renamed, and writing the obfuscated name is what a remap needs to rename a
        // reference to it into what the runtime jar actually carries.
        let mut classes_out: BTreeMap<String, String> = BTreeMap::new();
        for (project, obf) in &project_to_obf {
            let target = alternative
                .classes
                .get(obf)
                .cloned()
                .unwrap_or_else(|| obf.clone());
            classes_out.insert(project.clone(), target);
        }
        if classes_out.is_empty() {
            return Err("the official mapping text names no class".to_owned());
        }

        // Then the members: the join is on the obfuscated identity, which is the one both texts
        // state.
        let mut members_out: BTreeMap<String, BTreeMap<(bool, String, String), String>> =
            BTreeMap::new();
        let mut joined = 0usize;
        for member in member_lines {
            let key = (
                member.obf_owner.clone(),
                member.is_method,
                member.obf_desc.clone(),
                member.obf_name.to_owned(),
            );
            let target = if let Some(target) = alternative.members.get(&key) {
                joined += 1;
                target.clone()
            } else {
                // The two formats disagree about which type declares a member: the official
                // text repeats an inherited member on every implementor, the alternative table
                // records it once, on the declaring type. So "no entry under this owner" is
                // usually "the entry is under a supertype" — and the declaring type's own line
                // joins fine, so this one is skipped rather than misfiled.
                if alternative.signatures.contains(&(
                    member.is_method,
                    member.obf_desc.clone(),
                    member.obf_name.to_owned(),
                )) {
                    continue;
                }
                // The exception is a member the alternative table names nowhere at all: a
                // synthetic bridge, or something its publisher leaves alone. That one the jar
                // really does carry obfuscated, and the signature is what tells the two apart.
                if member.obf_name == member.project_name {
                    continue;
                }
                member.obf_name.to_owned()
            };
            if target == member.project_name {
                continue;
            }
            members_out.entry(member.project_owner).or_default().insert(
                (member.is_method, member.project_desc, member.project_name),
                target,
            );
        }
        if joined == 0 && !alternative.members.is_empty() {
            return Err(
                "the two mapping texts joined no member: they are not two halves of one release"
                    .to_owned(),
            );
        }

        // Serialize as tiny v2 through the caller's namespace pair, descriptors in the project-side
        // namespace — the file's first one, which is where the grammar wants them. Names are
        // written verbatim: the escape set (`\`, tab, newline, carriage return, NUL) is empty in
        // every JVM identifier and descriptor, so a text that would need `escaped-names` is one
        // whose inputs were not names.
        let mut out = String::with_capacity(official.len() + intermediary.len());
        out.push_str("tiny\t2\t0\t");
        out.push_str(ns_project);
        out.push('\t');
        out.push_str(ns_alternative);
        out.push('\n');
        for (project, alternative_name) in &classes_out {
            out.push_str("c\t");
            out.push_str(project);
            out.push('\t');
            out.push_str(alternative_name);
            out.push('\n');
            for ((is_method, desc, name), target) in members_out.get(project).into_iter().flatten()
            {
                out.push('\t');
                out.push(if *is_method { 'm' } else { 'f' });
                out.push('\t');
                out.push_str(desc);
                out.push('\t');
                out.push_str(name);
                out.push('\t');
                out.push_str(target);
                out.push('\n');
            }
        }
        Ok(out)
    }

    /// The official text as a class map and its member lines in both namespaces.
    ///
    /// Two passes, for the reason the remap-side parser walks twice: a member's obfuscated
    /// descriptor can name a class declared anywhere in the file.
    fn parse_official(
        official: &str,
    ) -> Result<(BTreeMap<String, String>, Vec<OfficialMember<'_>>), String> {
        let mut project_to_obf: BTreeMap<String, String> = BTreeMap::new();
        let mut obf_to_project: BTreeMap<String, String> = BTreeMap::new();
        let mut lines: Vec<(usize, &str, bool)> = Vec::new();
        for (number, raw) in official.lines().enumerate() {
            let number = number + 1;
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with(char::is_whitespace) {
                lines.push((number, line.trim(), true));
                continue;
            }
            let (project, obf) = Mappings::split_arrow(line, number)?;
            let obf = obf.strip_suffix(':').ok_or_else(|| {
                format!("mapping line {number} is not a class line (missing `:`)")
            })?;
            let project = Mappings::internalize(project);
            let obf = Mappings::internalize(obf);
            if project_to_obf
                .insert(project.clone(), obf.clone())
                .is_some()
            {
                return Err(format!("mapping line {number} redefines class `{project}`"));
            }
            if obf_to_project.insert(obf, project.clone()).is_some() {
                return Err(format!(
                    "mapping line {number} reuses an obfuscated class name for `{project}`"
                ));
            }
            lines.push((number, line, false));
        }

        let empty: BTreeMap<String, String> = BTreeMap::new();
        let mut member_lines = Vec::new();
        let mut owner: Option<String> = None;
        for (number, line, is_member) in lines {
            if !is_member {
                let (project, _) = Mappings::split_arrow(line, number)?;
                owner = Some(Mappings::internalize(project));
                continue;
            }
            let Some(project_owner) = owner.clone() else {
                return Err(format!(
                    "mapping line {number} is a member before any class"
                ));
            };
            let (left, obf_name) = Mappings::split_arrow(line, number)?;
            let is_method = left.contains('(');
            let entry = |class_map: &BTreeMap<String, String>| {
                if is_method {
                    Mappings::method_entry(class_map, left, number)
                } else {
                    Mappings::field_entry(class_map, left, number)
                }
            };
            let (project_name, obf_desc) = entry(&project_to_obf)?;
            // The project-side descriptor is the same signature read without translating class
            // names, so it comes from the same parser with an empty class map.
            let (_, project_desc) = entry(&empty)?;
            let Some(obf_owner) = project_to_obf.get(&project_owner).cloned() else {
                return Err(format!("mapping line {number} has an unmapped owner"));
            };
            member_lines.push(OfficialMember {
                project_owner,
                is_method,
                project_desc,
                project_name,
                obf_owner,
                obf_desc,
                obf_name,
            });
        }
        Ok((project_to_obf, member_lines))
    }

    /// The alternative tiny v2 text, read through its first two namespaces as the obfuscated-side
    /// half of a join. Later namespaces, and the sections this crate does not read, are skipped by
    /// the same rules the remap-side parser skips them under.
    fn parse_alternative(intermediary: &str) -> Result<AlternativeTable, String> {
        let mut lines = intermediary
            .lines()
            .enumerate()
            .map(|(index, raw)| (index + 1, raw.strip_suffix('\r').unwrap_or(raw)));
        let (header_number, header) = lines
            .by_ref()
            .find(|(_, line)| !line.is_empty())
            .ok_or_else(|| "alternative mapping text is empty".to_owned())?;
        let namespaces = Mappings::tiny_header(header, header_number)?;

        let mut escaped = false;
        let mut body: Vec<(usize, usize, &str)> = Vec::new();
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
                escaped |= key == "escaped-names";
                continue;
            }
            in_header = false;
            body.push((number, depth, rest));
        }

        let mut table = AlternativeTable {
            classes: BTreeMap::new(),
            members: BTreeMap::new(),
            signatures: BTreeSet::new(),
        };
        let mut owner: Option<String> = None;
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
                    let names = Mappings::tiny_names(columns, namespaces.len(), number, escaped)?;
                    // An empty alternative name means "no name in that namespace", which for a
                    // rename is the identity — recorded as such, exactly as the remap-side parser
                    // records it.
                    let alternative = if names[1].is_empty() {
                        names[0].clone()
                    } else {
                        names[1].clone()
                    };
                    if table
                        .classes
                        .insert(names[0].clone(), alternative)
                        .is_some()
                    {
                        return Err(format!(
                            "mapping line {number} redefines class `{}`",
                            names[0]
                        ));
                    }
                    owner = Some(names[0].clone());
                }
                (1, "f" | "m") => {
                    let is_method = tag == "m";
                    let descriptor = columns.next().ok_or_else(|| {
                        format!("mapping line {number} is a member with no descriptor")
                    })?;
                    let descriptor =
                        Mappings::tiny_descriptor(descriptor, is_method, number, escaped)?;
                    let names = Mappings::tiny_names(columns, namespaces.len(), number, escaped)?;
                    let Some(owner) = owner.clone() else {
                        continue;
                    };
                    let alternative = if names[1].is_empty() {
                        names[0].clone()
                    } else {
                        names[1].clone()
                    };
                    table
                        .signatures
                        .insert((is_method, descriptor.clone(), names[0].clone()));
                    table.members.insert(
                        (owner, is_method, descriptor, names[0].clone()),
                        alternative,
                    );
                }
                (1..=3, "c") => Mappings::tiny_comment(columns, number)?,
                (2, "p") => Mappings::tiny_variable(columns, false, number)?,
                (2, "v") => Mappings::tiny_variable(columns, true, number)?,
                (_, _) => skip_below = Some(depth),
            }
        }
        Ok(table)
    }

    // -----------------------------------------------------------------------
    // Copied sections
    // -----------------------------------------------------------------------

    fn append_copies_text(text: &str, copies: &str) -> Result<String, String> {
        let mut requests: BTreeSet<(String, String, String)> = BTreeSet::new();
        for (number, raw) in copies.lines().enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                continue;
            }
            let columns: Vec<&str> = line.split('\t').collect();
            if columns.len() != 3 || columns.iter().any(|column| column.is_empty()) {
                return Err(format!(
                    "copy line {} is not `new-owner<tab>existing-owner<tab>member-name`",
                    number + 1
                ));
            }
            requests.insert((
                columns[0].to_owned(),
                columns[1].to_owned(),
                columns[2].to_owned(),
            ));
        }
        if requests.is_empty() {
            return Ok(text.to_owned());
        }

        // One walk of the text collects every class section's first-namespace name and the raw
        // member lines under it, verbatim: a copy re-files lines the text already resolved, so
        // nothing here parses a descriptor or a target name.
        let sections = Self::tiny_sections(text)?;

        let mut appended: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (new_owner, existing_owner, member_name) in &requests {
            if sections.contains_key(new_owner) {
                return Err(format!(
                    "copy destination `{new_owner}` is already a class of the mapping text, and \
                     a second section for it would make the file unreadable"
                ));
            }
            let section = sections.get(existing_owner).ok_or_else(|| {
                format!("copy line names `{existing_owner}`, which the mapping text does not")
            })?;
            let mut found = false;
            for (name, line) in section {
                if name == member_name {
                    found = true;
                    let lines = appended.entry(new_owner.clone()).or_default();
                    if !lines.iter().any(|existing| existing == line) {
                        lines.push(line.clone());
                    }
                }
            }
            if !found {
                return Err(format!(
                    "copy line names member `{member_name}` of `{existing_owner}`, which the \
                     mapping text has no entry for"
                ));
            }
        }

        let mut out = String::with_capacity(text.len() + appended.len() * 64);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
        for (new_owner, lines) in &appended {
            out.push_str("c\t");
            out.push_str(new_owner);
            out.push('\t');
            out.push_str(new_owner);
            out.push('\n');
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        Ok(out)
    }

    /// Every class section of a tiny v2 text: first-namespace class name → its member lines as
    /// `(first-namespace member name, raw line)`.
    fn tiny_sections(text: &str) -> Result<BTreeMap<String, Vec<(String, String)>>, String> {
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(index, raw)| (index + 1, raw.strip_suffix('\r').unwrap_or(raw)));
        let (header_number, header) = lines
            .by_ref()
            .find(|(_, line)| !line.is_empty())
            .ok_or_else(|| "mapping text is empty".to_owned())?;
        let namespaces = Mappings::tiny_header(header, header_number)?;
        let mut escaped = false;
        let mut sections: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        let mut owner: Option<String> = None;
        let mut in_header = true;
        let mut skip_below: Option<usize> = None;
        for (number, raw) in lines {
            if raw.is_empty() {
                continue;
            }
            let depth = raw.bytes().take_while(|&byte| byte == b'\t').count();
            let rest = &raw[depth..];
            if in_header && depth == 1 {
                let key = rest.split('\t').next().unwrap_or_default();
                if key.is_empty() {
                    return Err(format!("mapping line {number} is a property with no key"));
                }
                escaped |= key == "escaped-names";
                continue;
            }
            in_header = false;
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
                    let names = Mappings::tiny_names(columns, namespaces.len(), number, escaped)?;
                    owner = Some(names[0].clone());
                    sections.entry(names[0].clone()).or_default();
                }
                (1, "f" | "m") => {
                    let is_method = tag == "m";
                    let descriptor = columns.next().ok_or_else(|| {
                        format!("mapping line {number} is a member with no descriptor")
                    })?;
                    Mappings::tiny_descriptor(descriptor, is_method, number, escaped)?;
                    let names = Mappings::tiny_names(columns, namespaces.len(), number, escaped)?;
                    if let Some(owner) = &owner {
                        sections
                            .entry(owner.clone())
                            .or_default()
                            .push((names[0].clone(), raw.to_owned()));
                    }
                }
                (1..=3, "c") => Mappings::tiny_comment(columns, number)?,
                (2, "p") => Mappings::tiny_variable(columns, false, number)?,
                (2, "v") => Mappings::tiny_variable(columns, true, number)?,
                (_, _) => skip_below = Some(depth),
            }
        }
        Ok(sections)
    }

    // -----------------------------------------------------------------------
    // Reference resolution
    // -----------------------------------------------------------------------

    fn resolve_references_text(
        requests: &str,
        mappings: &str,
        format: &MappingFormat,
    ) -> Result<String, String> {
        let table = Mappings::parse(mappings, format, RemapDirection::Reobfuscate)?;
        // The whole-table fallback index, by member name across every owner. Built once per call:
        // a bare reference the context owner does not file is answered by the table at large, and
        // scanning it per request would be quadratic over exactly the biggest inputs. Methods and
        // fields share one index because a reference that names no descriptor selects on the name
        // alone, and a name both kinds carry is settled by the descriptor filter or not at all.
        let mut global: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
        for members in table.member_table().values() {
            for ((name, desc), target) in members.methods().iter().chain(members.fields()) {
                global
                    .entry(name.as_str())
                    .or_default()
                    .push((desc.as_str(), target.as_str()));
            }
        }

        let mut failures: Vec<String> = Vec::new();
        let mut out = String::new();
        for (number, raw) in requests.lines().enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                continue;
            }
            let columns: Vec<&str> = line.split('\t').collect();
            if columns.len() != 3 {
                return Err(format!(
                    "request line {} is not `passthrough<tab>context<tab>reference`",
                    number + 1
                ));
            }
            let (passthrough, context, reference) = (columns[0], columns[1], columns[2]);
            match Self::rewrite_reference(reference, context, &table, &global) {
                Ok(rewritten) => {
                    out.push_str(passthrough);
                    out.push('\t');
                    out.push_str(&rewritten);
                    out.push('\n');
                }
                Err(reason) => {
                    failures.push(format!("line {}: `{reference}`: {reason}", number + 1));
                }
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "no alternative name for a member reference:\n  {}",
                failures.join("\n  ")
            ));
        }
        Ok(out)
    }

    /// One reference, rewritten part by part, or the reason it could not be.
    fn rewrite_reference<'table>(
        reference: &str,
        context: &str,
        table: &'table Mappings,
        global: &BTreeMap<&'table str, Vec<(&'table str, &'table str)>>,
    ) -> Result<String, String> {
        // `[Lowner;]` — the owner part. Mimicked from the selector grammar rather than a regex:
        // the group is present exactly when what follows the `L` contains a `;`, so a bare name
        // that happens to start with `L` stays a name.
        let mut rest = reference;
        let mut owner: Option<&str> = None;
        if let Some(tail) = reference.strip_prefix('L')
            && let Some(end) = tail.find(';')
        {
            owner = Some(&tail[..end]);
            rest = &tail[end + 1..];
        }

        let name_end = rest.find(['(', ':']).unwrap_or(rest.len());
        let name = &rest[..name_end];
        if name.is_empty() {
            return Err("the reference names no member".to_owned());
        }
        let descriptor = &rest[name_end..];
        let is_field = descriptor.starts_with(':');
        let desc = if is_field {
            &descriptor[1..]
        } else {
            descriptor
        };
        if !desc.is_empty() {
            let valid = if is_field {
                FieldType::parse(desc).is_ok()
            } else {
                MethodDescriptor::parse(desc).is_ok()
            };
            if !valid {
                return Err(format!("`{desc}` is not a descriptor"));
            }
        }

        // The owner a bare name resolves against: the reference's own, else the request's context.
        let resolution_root = owner.unwrap_or(context);
        let owner_translated = match owner {
            // Strict for a written owner: a caller sends a reference whose owner the table should
            // cover, and an owner it does not cover is a name that would silently stay behind in an
            // otherwise translated reference. Classes a *descriptor* mentions are the opposite
            // case and stay as they are — a descriptor names every type it mentions, most of which
            // no mapping set ever covered.
            Some(owner) => Some(
                table
                    .remap_class(owner)
                    .ok_or_else(|| format!("the mapping set does not name the owner `{owner}`"))?,
            ),
            None => None,
        };

        let renamed = if name.starts_with('<') {
            // A constructor spells the same in every namespace; only its owner and descriptor move.
            name.to_owned()
        } else {
            let mut candidates: Vec<(&str, &str)> = Vec::new();
            if !resolution_root.is_empty()
                && let Some(target_owner) = table.remap_class(resolution_root)
                && let Some(members) = table.member_table().get(target_owner)
            {
                candidates.extend(
                    members
                        .methods()
                        .iter()
                        .chain(members.fields())
                        .filter(|((source_name, _), _)| source_name == name)
                        .map(|((_, desc), target)| (desc.as_str(), target.as_str())),
                );
            }
            if candidates.is_empty() {
                candidates.extend(global.get(name).cloned().unwrap_or_default());
            }
            if !desc.is_empty() {
                let exact: Vec<(&str, &str)> = candidates
                    .iter()
                    .filter(|(candidate, _)| *candidate == desc)
                    .copied()
                    .collect();
                if !exact.is_empty() {
                    candidates = exact;
                }
            }
            let mut unique: Option<&str> = None;
            let mut conflicting = false;
            for (_, target) in &candidates {
                match unique {
                    None => unique = Some(target),
                    Some(found) if found == *target => {}
                    Some(_) => conflicting = true,
                }
            }
            match unique {
                Some(target) if !conflicting => target.to_owned(),
                Some(_) => {
                    return Err(format!(
                        "`{name}` resolves to more than one alternative name"
                    ));
                }
                None => return Err(format!("the mapping set names no `{name}`")),
            }
        };

        let mut rewritten = String::with_capacity(reference.len());
        if let Some(owner) = owner_translated {
            rewritten.push('L');
            rewritten.push_str(owner);
            rewritten.push(';');
        }
        rewritten.push_str(&renamed);
        if !desc.is_empty() {
            if is_field {
                rewritten.push(':');
            }
            rewritten.push_str(&table.remap_descriptor(desc));
        }
        Ok(rewritten)
    }
}

#[cfg(test)]
mod tests {
    use jals_exec::block_on_inline;
    use jals_progress::Progress;
    use jals_storage::MemoryCache;

    use super::*;

    /// A two-class official text with the shapes a composition has to get right: a member the
    /// alternative table files elsewhere (inherited), one it names nowhere (left obfuscated), and
    /// an identity class.
    const OFFICIAL: &str = "\
net.minecraft.world.level.Level -> abc:
    void tick(java.util.function.BooleanSupplier) -> a
    net.minecraft.world.entity.player.Player nearestPlayer(double) -> b
    boolean isClientSide() -> c
net.minecraft.server.level.ServerLevel -> xyz:
    void tick(java.util.function.BooleanSupplier) -> a
net.minecraft.client.Main -> net.minecraft.client.Main:
";

    /// The alternative half: tiny v2, obfuscated first. `ServerLevel.tick` is absent — the table
    /// files it once, under `Level` — and `isClientSide` is absent everywhere.
    const ALTERNATIVE: &str = "\
tiny\t2\t0\tofficial\tintermediary
c\tabc\tclass_1937
\tm\t(Ljava/util/function/BooleanSupplier;)V\ta\tmethod_18765
\tm\t(D)Lpl;\tb\tmethod_18459
c\txyz\tclass_3218
c\tpl\tclass_1657
";

    /// The `Player` class the descriptor above needs, with a member the alternative names nowhere.
    const OFFICIAL_WITH_PLAYER: &str = "\
net.minecraft.world.entity.player.Player -> pl:
    java.lang.String getName() -> d
";

    /// The composed table both fixture halves produce, as the serializer writes it: classes in
    /// order, members under each by `(kind, descriptor, name)`.
    const COMPOSED: &str = "\
tiny\t2\t0\tmojang\tintermediary
c\tnet/minecraft/client/Main\tnet/minecraft/client/Main
c\tnet/minecraft/server/level/ServerLevel\tclass_3218
c\tnet/minecraft/world/entity/player/Player\tclass_1657
\tm\t()Ljava/lang/String;\tgetName\td
c\tnet/minecraft/world/level/Level\tclass_1937
\tm\t()Z\tisClientSide\tc
\tm\t(D)Lnet/minecraft/world/entity/player/Player;\tnearestPlayer\tmethod_18459
\tm\t(Ljava/util/function/BooleanSupplier;)V\ttick\tmethod_18765
";

    fn fixture() -> String {
        format!("{OFFICIAL}{OFFICIAL_WITH_PLAYER}")
    }

    fn pair(from: &str, to: &str) -> MappingFormat {
        MappingFormat::TinyV2 {
            from: from.to_owned(),
            to: to.to_owned(),
        }
    }

    #[test]
    fn compose_joins_through_the_obfuscated_member() {
        let composed = MappingText::compose_text(&fixture(), ALTERNATIVE, "mojang", "intermediary")
            .expect("composes");
        assert_eq!(composed, COMPOSED);
    }

    #[test]
    fn compose_needs_a_namespace_pair() {
        // The async gate: a ProGuard-style output pair has no namespaces to write a tiny header
        // from, and the refusal names why rather than writing a header of empty names.
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let report = jals_progress::Task::silent();
        let error = block_on_inline(MappingText::compose(
            &mut cache,
            &fixture(),
            ALTERNATIVE,
            &MappingFormat::Proguard,
            &report,
        ))
        .unwrap_err();
        assert!(error.contains("namespace pair"), "{error}");
    }

    #[test]
    fn compose_refuses_two_halves_of_different_releases() {
        let unrelated =
            "tiny\t2\t0\tofficial\tintermediary\nc\tqqq\tclass_9999\n\tf\tI\tq\tfield_9999\n";
        let error =
            MappingText::compose_text(&fixture(), unrelated, "mojang", "intermediary").unwrap_err();
        assert!(error.contains("joined no member"), "{error}");
    }

    #[test]
    fn compose_is_memoized_on_its_inputs() {
        use std::sync::{Arc, Mutex};

        use jals_progress::{Activity, Event, Outcome, Sink};

        /// Every event one run emits, in order.
        struct Capture(Mutex<Vec<Event>>);

        impl Sink for Capture {
            fn emit(&self, event: &Event) {
                self.0
                    .lock()
                    .expect("capture is not poisoned")
                    .push(event.clone());
            }
        }

        let capture = Arc::new(Capture(Mutex::new(Vec::new())));
        let progress = Progress::to(Arc::clone(&capture) as Arc<dyn Sink>);
        let mut cache = ArtifactCache::new(MemoryCache::default());
        let format = pair("intermediary", "mojang");

        let first = progress.begin(Activity::Compose, "");
        let composed = block_on_inline(MappingText::compose(
            &mut cache,
            &fixture(),
            ALTERNATIVE,
            &format,
            &first,
        ))
        .expect("composes");
        first.finish(Outcome::Completed);

        let second = progress.begin(Activity::Compose, "");
        let again = block_on_inline(MappingText::compose(
            &mut cache,
            &fixture(),
            ALTERNATIVE,
            &format,
            &second,
        ))
        .expect("memo hit");
        second.finish(Outcome::Completed);

        assert_eq!(composed, COMPOSED);
        assert_eq!(again, composed);
        // The second run answered from the memo, and said so through the caller's unit rather
        // than recomposing megabytes of text silently.
        let outcomes: Vec<_> = capture
            .0
            .lock()
            .expect("capture is not poisoned")
            .iter()
            .filter_map(|event| match event {
                Event::Finished { outcome, .. } => Some(*outcome),
                _ => None,
            })
            .collect();
        assert!(outcomes.contains(&Outcome::Fresh), "{outcomes:?}");
    }

    #[test]
    fn the_composed_text_parses_in_both_directions() {
        let format = pair("intermediary", "mojang");
        let out = Mappings::parse(COMPOSED, &format, RemapDirection::Reobfuscate)
            .expect("reobfuscating parse");
        assert_eq!(
            out.remap_class("net/minecraft/world/level/Level"),
            Some("class_1937")
        );
        assert_eq!(
            out.remap_method(
                "class_1937",
                "nearestPlayer",
                "(D)Lnet/minecraft/world/entity/player/Player;"
            ),
            Some("method_18459")
        );
        let back = Mappings::parse(COMPOSED, &format, RemapDirection::Deobfuscate)
            .expect("deobfuscating parse");
        assert_eq!(
            back.remap_class("class_1937"),
            Some("net/minecraft/world/level/Level")
        );
        assert_eq!(
            back.remap_method(
                "net/minecraft/world/level/Level",
                "method_18459",
                "(D)Lclass_1657;"
            ),
            Some("nearestPlayer")
        );
    }

    #[test]
    fn copies_re_file_an_entry_under_a_second_owner() {
        let extended = MappingText::append_copies_text(
            COMPOSED,
            "me/mod/LevelMixin\tnet/minecraft/world/level/Level\ttick",
        )
        .expect("copies");
        assert!(extended.ends_with(
            "c\tme/mod/LevelMixin\tme/mod/LevelMixin\n\
             \tm\t(Ljava/util/function/BooleanSupplier;)V\ttick\tmethod_18765\n"
        ));
        // The copy is what a reobfuscating remap of the mixin class can now resolve: the section
        // is an identity, so the member is keyed under the mixin's own name.
        let table = Mappings::parse(
            &extended,
            &pair("intermediary", "mojang"),
            RemapDirection::Reobfuscate,
        )
        .expect("parses");
        assert_eq!(
            table.remap_class("me/mod/LevelMixin"),
            Some("me/mod/LevelMixin")
        );
        assert_eq!(
            table.remap_method(
                "me/mod/LevelMixin",
                "tick",
                "(Ljava/util/function/BooleanSupplier;)V"
            ),
            Some("method_18765")
        );
    }

    #[test]
    fn copies_are_strict_about_what_the_text_names() {
        let missing_owner =
            MappingText::append_copies_text(COMPOSED, "me/M\tno/Such\ttick").unwrap_err();
        assert!(missing_owner.contains("does not"), "{missing_owner}");
        // `tick` is filed under Level alone; ServerLevel's section has no member lines.
        let missing_member = MappingText::append_copies_text(
            COMPOSED,
            "me/M\tnet/minecraft/server/level/ServerLevel\ttick",
        )
        .unwrap_err();
        assert!(missing_member.contains("no entry"), "{missing_member}");
        let taken =
            MappingText::append_copies_text(COMPOSED, "net/minecraft/world/level/Level\tabc\ttick")
                .unwrap_err();
        assert!(taken.contains("already a class"), "{taken}");
        let malformed = MappingText::append_copies_text(COMPOSED, "me/M\tonly-two").unwrap_err();
        assert!(malformed.contains("new-owner"), "{malformed}");
    }

    #[test]
    fn an_empty_copy_list_leaves_the_text_alone() {
        assert_eq!(
            MappingText::append_copies_text(COMPOSED, ""),
            Ok(COMPOSED.to_owned())
        );
    }

    #[test]
    fn references_resolve_by_context_then_by_the_whole_table() {
        let format = pair("intermediary", "mojang");
        let requests = "\
mappings\u{1}me/LevelMixin\u{1}tick\tnet/minecraft/server/level/ServerLevel\ttick\n\
mappings\u{1}me/LevelMixin\u{1}Lnet/minecraft/world/level/Level;nearestPlayer(D)Lnet/minecraft/world/entity/player/Player;\t\tLnet/minecraft/world/level/Level;nearestPlayer(D)Lnet/minecraft/world/entity/player/Player;\n\
ctor\t\t<init>(Lnet/minecraft/world/level/Level;)V\n";
        let resolved =
            MappingText::resolve_references_text(requests, COMPOSED, &format).expect("resolves");
        assert_eq!(
            resolved,
            "mappings\u{1}me/LevelMixin\u{1}tick\tmethod_18765\n\
             mappings\u{1}me/LevelMixin\u{1}Lnet/minecraft/world/level/Level;nearestPlayer(D)Lnet/minecraft/world/entity/player/Player;\tLclass_1937;method_18459(D)Lclass_1657;\n\
             ctor\t<init>(Lclass_1937;)V\n"
        );
    }

    #[test]
    fn a_field_reference_keeps_its_colon() {
        let format = pair("intermediary", "mojang");
        let resolved = MappingText::resolve_references_text(
            "f\t\tgetName:Ljava/lang/String;",
            COMPOSED,
            &format,
        )
        .expect("resolves");
        // The fixture files `getName` as a method, so the field descriptor settles nothing and the
        // unique name answers; the colon form is what comes back out.
        assert_eq!(resolved, "f\td:Ljava/lang/String;\n");
    }

    #[test]
    fn references_report_every_line_that_does_not_resolve() {
        let format = pair("intermediary", "mojang");
        let error = MappingText::resolve_references_text(
            "a\t\tnoSuchMember\nb\tnet/minecraft/world/level/Level\tnoSuchEither",
            COMPOSED,
            &format,
        )
        .unwrap_err();
        assert!(error.contains("line 1: `noSuchMember`"), "{error}");
        assert!(error.contains("line 2: `noSuchEither`"), "{error}");
        // An owner the table does not cover fails the line rather than passing through.
        let error = MappingText::resolve_references_text(
            "a\t\tLcarpet/Helpers;doExplosionA",
            COMPOSED,
            &format,
        )
        .unwrap_err();
        assert!(error.contains("does not name the owner"), "{error}");
        // A request line of the wrong shape fails the whole call: it is a caller bug, not a
        // reference the table cannot answer.
        let error =
            MappingText::resolve_references_text("one-column", COMPOSED, &format).unwrap_err();
        assert!(error.contains("passthrough"), "{error}");
    }

    #[test]
    fn an_ambiguous_bare_name_is_refused_rather_than_guessed() {
        // Two owners, one member name, two alternative names: with no context and no descriptor
        // there is nothing to choose between them.
        let text = "\
tiny\t2\t0\tmojang\tintermediary
c\ta/A\tclass_1
\tm\t()V\tshared\tmethod_1
c\tb/B\tclass_2
\tm\t()V\tshared\tmethod_2
";
        let format = pair("intermediary", "mojang");
        let error = MappingText::resolve_references_text("x\t\tshared", text, &format).unwrap_err();
        assert!(error.contains("more than one"), "{error}");
        // A descriptor settles it.
        let resolved =
            MappingText::resolve_references_text("x\t\tshared()V", text, &format).unwrap_err();
        // ...unless both carry the same descriptor, as here: the ambiguity is real.
        assert!(resolved.contains("more than one"), "{resolved}");
        // A context owner settles it too.
        let resolved = MappingText::resolve_references_text("x\ta/A\tshared", text, &format)
            .expect("the context settles it");
        assert_eq!(resolved, "x\tmethod_1\n");
    }
}
