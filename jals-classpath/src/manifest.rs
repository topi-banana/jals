//! The jar manifest, and the `META-INF/` members whose claim it is one half of.
//!
//! Two places in this crate produce a `META-INF/MANIFEST.MF`: [`crate::jar::JarPackage`] writes a
//! fresh one beside compiled classes, and [`crate::remap`] edits one somebody else wrote as it
//! remaps or merges an archive. They used to agree about the format by writing it down twice — the
//! 72-byte physical line cap and its fold rule in both, the `META-INF/` name matching in one and
//! the member ordering in the other — which is two copies of a specification and one of them
//! always a release behind. Both now ask here, and this module is the only place in the crate that
//! knows what a manifest is.
//!
//! Three rules earn the module rather than a shared helper:
//!
//! - **A manifest is edited, not re-rendered.** Every transform below returns the bytes it was
//!   given when it changes nothing, and writes back the archive's own spelling — its terminators,
//!   its fold points, its attribute order — for every attribute it did not touch. A jar this crate
//!   remaps was written by somebody else, and normalizing a manifest nobody asked about is a diff
//!   in an artifact whose determinism is a stated invariant.
//! - **A main attribute is not an individual one.** `Multi-Release` and `Main-Class` mean something
//!   in the main section and nothing in the per-member sections a signer writes, so both are read
//!   and written there and nowhere else. The digests are the mirror case and are stripped from
//!   every section alike.
//! - **A member name is matched the way the JVM matches it.** `JarFile::getManEntry` falls back to
//!   an `equalsIgnoreCase` sweep of the `META-INF/` names and
//!   `SignatureFileVerifier::isSigningRelated` upper-cases the whole entry name before testing it,
//!   so an archive spelling the directory `meta-inf/` still has its manifest read and its block
//!   verified. Matching one of the two loosely and the other exactly is what leaves half a claim
//!   standing — a jar whose `.RSA` survived a remap that had already stripped the digests it
//!   covers.
//!
//! Portable like the rest of `archive`: an in-browser compile packages its own output with no host
//! archiver involved.

use alloc::borrow::{Cow, ToOwned};
use alloc::string::String;
use alloc::vec::Vec;

/// The manifest's physical line cap, in bytes, counted *including* the line terminator.
///
/// The jar specification caps a line at 72 bytes and is read both ways on whether the terminator
/// counts against it. Counting it is the strict reading, so a manifest written here is legal under
/// either.
const MAX_LINE: usize = 72;

/// Manifest lines end with CRLF, not the host's line ending — the format is not text-mode.
///
/// What a *fresh* manifest is written with. An edited one keeps whatever its author used; see
/// [`Manifest::terminator`].
const CRLF: &str = "\r\n";

/// The `META-INF/` members a JVM reads by name, and how it matches them.
pub(crate) struct MetaInf;

impl MetaInf {
    /// The manifest member every jar carries, and the first member a jar writer emits.
    pub(crate) const MANIFEST_PATH: &'static str = "META-INF/MANIFEST.MF";

    /// What `name` spells directly under `META-INF/`, or `None` when it is not under it at all.
    ///
    /// The directory component is compared case-insensitively, exactly as the basenames below are
    /// and for the reason the module doc gives: the JVM matches it that way.
    fn under(name: &str) -> Option<&str> {
        const META_INF: &str = "META-INF/";
        let (prefix, base) = name.split_at_checked(META_INF.len())?;
        prefix.eq_ignore_ascii_case(META_INF).then_some(base)
    }

    /// Whether archive member `name` carries `extension`, compared case-insensitively. Directory
    /// entries end in `/`, so they never match.
    fn has_extension(name: &str, extension: &str) -> bool {
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
    }

    /// Whether `name` is the jar manifest: `META-INF/MANIFEST.MF`, at that one depth.
    pub(crate) fn is_manifest(name: &str) -> bool {
        Self::under(name).is_some_and(|base| base.eq_ignore_ascii_case("MANIFEST.MF"))
    }

    /// Whether `name` is a jar signature block: `META-INF/<base>.{SF,DSA,RSA,EC}`, or the
    /// `META-INF/SIG-*` spelling.
    ///
    /// Directly under `META-INF/` and nowhere else — the JAR specification only reads the block at
    /// that one depth, so a `META-INF/services/x.sf` is an ordinary member and stays.
    ///
    /// The `SIG-` prefix is the second form the JDK's own predicate
    /// (`sun.security.util.SignatureFileVerifier::isSigningRelated`) recognises, with or without an
    /// extension. Matching what the JVM matches is the whole point, and it cuts both ways: a block
    /// this misses keeps the archive "signed" while the manifest half of the claim has already been
    /// stripped — the half-a-claim state the module doc calls worse than keeping neither — and a
    /// member this matches that the JVM would not is an ordinary resource deleted from a jar that
    /// still needs it. So the `SIG-` extension rule is the JDK's: absent, or one to three ASCII
    /// alphanumerics. `META-INF/SIG-config.json` is a resource, and `META-INF/SIG-Foo.class` is a
    /// class — which this predicate is asked about *before* a remap collects its rewritten bytes,
    /// so matching it would have thrown away a class the pass had already rewritten.
    pub(crate) fn is_signature(name: &str) -> bool {
        let Some(base) = Self::under(name) else {
            return false;
        };
        if base.contains('/') {
            return false;
        }
        // Over the bytes, not a `&str` slice: a member name is whatever the archive says, and
        // `&base[..4]` panics when byte 4 falls inside a multi-byte character.
        if base
            .as_bytes()
            .first_chunk::<4>()
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"SIG-"))
        {
            return base.rsplit_once('.').is_none_or(|(_, extension)| {
                (1..=3).contains(&extension.len())
                    && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
        }
        ["sf", "dsa", "rsa", "ec"]
            .iter()
            .any(|extension| Self::has_extension(base, extension))
    }

    /// The `META-INF/versions/<n>/` prefix `name` carries, or `""` when it carries none.
    ///
    /// Beside the `Multi-Release` attribute rather than with the remap that consumes it: the
    /// attribute is what makes these entries live, and a versioned entry is what makes the
    /// attribute mean anything. A multi-release jar stores the same class twice — once at its plain
    /// path and once under the prefix — and both copies share a `this_class`, so a remap naming its
    /// output purely from that would collide the two and fail the whole archive.
    pub(crate) fn multi_release_prefix(name: &str) -> &str {
        const ROOT: &str = "META-INF/versions/";
        let Some(rest) = name.strip_prefix(ROOT) else {
            return "";
        };
        rest.find('/')
            .map_or("", |end| &name[..=(ROOT.len() + end)])
    }
}

/// One line-group of a manifest, in the order the archive spells them.
enum Item<'a> {
    /// An attribute: the physical lines it occupies, terminators included and contiguous, together
    /// with the logical `name: value` those lines fold up to.
    Attribute {
        verbatim: &'a str,
        logical: Cow<'a, str>,
    },
    /// The blank line that closes a section.
    Break(&'a str),
}

/// A jar manifest, read as the attributes and section breaks the archive spells, each remembering
/// the bytes it came from.
///
/// Not a model the file is re-rendered from: an edit writes every untouched attribute back
/// verbatim, so the transforms below are byte-identity wherever they decide nothing needs to
/// change. That is what lets a remap edit `Main-Class` in a manifest it otherwise has no business
/// reformatting.
pub(crate) struct Manifest<'a> {
    /// The whole manifest, so an edit that decides to change nothing can hand it straight back.
    text: &'a str,
    items: Vec<Item<'a>>,
    /// One past the main section's last item — where an appended main attribute goes, and the end
    /// of the range a main-attribute query reads.
    main_end: usize,
    /// The terminator the archive itself used, for the lines an edit writes.
    terminator: &'a str,
}

impl Manifest<'_> {
    // --- What the rest of the crate sees: an archive member's bytes in, and out ------------------
    //
    // A `Manifest` never leaves this module. Handing one out would publish the parse — and with it
    // the rule that a member which is not UTF-8 is left exactly as it was found, which each caller
    // would then be restating. Every entry point below states it once by going through
    // [`Self::parse`].

    /// Whether the manifest in `bytes` declares `Multi-Release: true` in its main section.
    ///
    /// A member that is not a readable manifest declares nothing — which is a different answer from
    /// a member that could not be *read*, and callers that must not confuse the two check for that
    /// before asking.
    pub(crate) fn read_multi_release(bytes: &[u8]) -> bool {
        Manifest::parse(bytes).is_some_and(|manifest| manifest.declares_multi_release())
    }

    /// The manifest in `bytes` with `Multi-Release: true` in its main section.
    pub(crate) fn write_multi_release(bytes: &[u8]) -> Vec<u8> {
        Manifest::parse(bytes)
            .map_or_else(|| bytes.to_vec(), |manifest| manifest.with_multi_release())
    }

    /// The manifest in `bytes` without the per-entry digests a signer wrote into it.
    pub(crate) fn write_without_digests(bytes: &[u8]) -> Vec<u8> {
        Manifest::parse(bytes).map_or_else(|| bytes.to_vec(), |manifest| manifest.without_digests())
    }

    /// The entry point the manifest in `bytes` declares, dotted as a manifest spells it.
    pub(crate) fn read_main_class(bytes: &[u8]) -> Option<String> {
        Manifest::parse(bytes)?.main_class().map(ToOwned::to_owned)
    }

    /// The manifest in `bytes` with its main `Main-Class` replaced by `value`.
    pub(crate) fn write_main_class(bytes: &[u8], value: &str) -> Vec<u8> {
        Manifest::parse(bytes).map_or_else(
            || bytes.to_vec(),
            |manifest| manifest.with_main_class(value),
        )
    }
}

impl<'a> Manifest<'a> {
    /// Read `bytes` as a manifest, or `None` when they are not UTF-8.
    ///
    /// Every caller answers `None` by leaving the member exactly as it found it: a manifest this
    /// crate cannot read is one it must not rewrite, because the alternative is replacing a member
    /// it does not understand with an empty one.
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        let text = core::str::from_utf8(bytes).ok()?;
        // A manifest is written with CRLF. Detected rather than assumed, so a hand-written one that
        // is not stays the way its author left it — and CRLF again when the text carries no
        // terminator to go on, because that is the format's own and an unterminated manifest gives
        // no evidence for anything else.
        let terminator = match (text.contains("\r\n"), text.contains('\n')) {
            (false, true) => "\n",
            _ => CRLF,
        };
        let mut items: Vec<Item<'a>> = Vec::new();
        let mut main_end = None;
        let mut offset = 0;
        while offset < text.len() {
            let start = offset;
            let mut end = Self::line_end(text, offset);
            if Self::body(&text[start..end]).is_empty() {
                main_end.get_or_insert(items.len());
                items.push(Item::Break(&text[start..end]));
                offset = end;
                continue;
            }
            let mut logical = Cow::Borrowed(Self::body(&text[start..end]));
            offset = end;
            // A continuation line opens with exactly one space and belongs to the attribute above
            // it. Folded here once so that no reader below has to know the rule — matching an
            // attribute on the physical line is what left a wrapped `Main-Class` unrewritten and a
            // wrapped digest's tail behind as syntax nothing can parse.
            while offset < text.len() && text[offset..].starts_with(' ') {
                end = Self::line_end(text, offset);
                // The one space that marks a continuation is syntax, not value. It is ASCII, so
                // byte 1 is always a character boundary.
                logical
                    .to_mut()
                    .push_str(&Self::body(&text[offset..end])[1..]);
                offset = end;
            }
            items.push(Item::Attribute {
                verbatim: &text[start..end],
                logical,
            });
        }
        let main_end = main_end.unwrap_or(items.len());
        Some(Self {
            text,
            items,
            main_end,
            terminator,
        })
    }

    /// The main section of a fresh jar manifest, packaging `main_class` when there is one.
    ///
    /// `Manifest-Version` comes first because the specification requires the version to be the main
    /// section's first attribute; a blank line terminates the section. Without a `Main-Class` the
    /// result describes a library jar, which is the honest output for a project that declares no
    /// entry point.
    ///
    /// No `Created-By`: naming the writing tool's version would change the bytes on every release,
    /// and the archive is otherwise reproducible from its inputs alone.
    pub(crate) fn packaged(main_class: Option<&str>) -> Vec<u8> {
        let mut out = String::new();
        Self::write_attribute(&mut out, "Manifest-Version", "1.0", CRLF, CRLF);
        if let Some(main_class) = main_class {
            Self::write_attribute(&mut out, "Main-Class", main_class, CRLF, CRLF);
        }
        out.push_str(CRLF);
        out.into_bytes()
    }

    /// The value of main attribute `name`, trimmed, or `None` when the main section does not
    /// declare it.
    ///
    /// The main section only, because that is the only place the attributes this crate asks about
    /// mean anything: the JVM reads `Main-Class` and `Multi-Release` from the main attributes, and
    /// an individual section declaring one says something about a single member instead.
    fn main_attribute(&self, name: &str) -> Option<&str> {
        self.items[..self.main_end]
            .iter()
            .find_map(|item| match item {
                Item::Attribute { logical, .. } => Self::value_of(logical, name),
                Item::Break(_) => None,
            })
    }

    /// Whether the main section declares `Multi-Release: true`.
    fn declares_multi_release(&self) -> bool {
        self.main_attribute("Multi-Release")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }

    /// The main section's `Main-Class`, as the manifest spells it — dotted, not internal.
    fn main_class(&self) -> Option<&str> {
        self.main_attribute("Main-Class")
    }

    /// The same manifest with `Multi-Release: true` in its main section.
    ///
    /// Idempotent by returning the input untouched when the attribute is already there, and a
    /// `Multi-Release: false` is *replaced* rather than joined by a contradicting second line.
    /// Order within a section carries no meaning beyond `Manifest-Version` coming first, which the
    /// append never displaces.
    fn with_multi_release(&self) -> Vec<u8> {
        if self.declares_multi_release() {
            return self.text.as_bytes().to_vec();
        }
        let mut out = String::with_capacity(self.text.len() + 32);
        for item in &self.items[..self.main_end] {
            match item {
                Item::Attribute { logical, .. }
                    if Self::value_of(logical, "Multi-Release").is_some() => {}
                Item::Attribute { verbatim, .. } => self.push_terminated(&mut out, verbatim),
                // Unreachable: `main_end` is the index of the first break.
                Item::Break(raw) => out.push_str(raw),
            }
        }
        Self::write_attribute(
            &mut out,
            "Multi-Release",
            "true",
            self.terminator,
            self.terminator,
        );
        if self.main_end == self.items.len() {
            // A manifest whose main section was never closed. Close it, rather than leaving the
            // attribute in a section the JVM would read as being about a member.
            out.push_str(self.terminator);
        }
        for item in &self.items[self.main_end..] {
            match item {
                Item::Attribute { verbatim, .. } => out.push_str(verbatim),
                Item::Break(raw) => out.push_str(raw),
            }
        }
        out.into_bytes()
    }

    /// The same manifest with its main `Main-Class` replaced by `value`.
    ///
    /// The manifest is handed back untouched when its main section declares none: this rewrites an
    /// entry point, it does not introduce one, and a jar that declares no `Main-Class` is a library
    /// jar whatever a mapping has to say about class names.
    fn with_main_class(&self, value: &str) -> Vec<u8> {
        let mut out = String::with_capacity(self.text.len() + value.len());
        let mut replaced = false;
        for (index, item) in self.items.iter().enumerate() {
            match item {
                Item::Attribute { verbatim, logical }
                    if index < self.main_end && Self::value_of(logical, "Main-Class").is_some() =>
                {
                    // The manifest's own terminators, not this crate's CRLF: it edits an archive
                    // somebody else may have written, and putting one CRLF line into an LF manifest
                    // leaves a file whose terminators disagree. The attribute's *first* physical
                    // line says how a fold is terminated and its last says how the attribute ends,
                    // so a final attribute the archive left unterminated stays that way.
                    let fold = match Self::first_line_terminator(verbatim) {
                        "" => self.terminator,
                        terminator => terminator,
                    };
                    Self::write_attribute(
                        &mut out,
                        "Main-Class",
                        value,
                        fold,
                        Self::terminator_of(verbatim),
                    );
                    replaced = true;
                }
                Item::Attribute { verbatim, .. } => out.push_str(verbatim),
                Item::Break(raw) => out.push_str(raw),
            }
        }
        if replaced {
            out.into_bytes()
        } else {
            self.text.as_bytes().to_vec()
        }
    }

    /// The same manifest without the per-entry digests a signer wrote into it.
    ///
    /// Removing `META-INF/*.SF` alone is not enough. A signed jar states a digest for every member
    /// in an individual section of `MANIFEST.MF` and the signature file states a digest of those
    /// sections in turn, so a remapped jar that keeps the manifest half carries megabytes of claims
    /// about bytes that no longer exist — and hands them to whoever signs the jar next.
    ///
    /// Only the digests go. An individual section that says something else about its member keeps
    /// saying it, and a section left with nothing but its `Name:` goes entirely rather than
    /// surviving as an empty claim. The rule is applied to every section alike rather than to all
    /// but the first, so a main-section attribute that did name a digest would go too — but nothing
    /// a signer writes there does, which is why the main section survives whole in practice.
    fn without_digests(&self) -> Vec<u8> {
        // Nothing to strip is the common case — every unsigned jar, and the jars this crate writes
        // itself. Answered over the same predicate the pass below uses, so the two cannot disagree
        // about what a digest is.
        if !self
            .items
            .iter()
            .any(|item| matches!(item, Item::Attribute { logical, .. } if Self::is_digest(logical)))
        {
            return self.text.as_bytes().to_vec();
        }
        let mut out = String::with_capacity(self.text.len());
        let mut section: Vec<&Item<'a>> = Vec::new();
        for item in &self.items {
            match item {
                Item::Attribute { .. } => section.push(item),
                Item::Break(raw) => {
                    self.push_section(&mut out, &section, raw);
                    section.clear();
                }
            }
        }
        // A file that ended without closing its last section keeps ending that way; this pass
        // removes claims, it does not tidy.
        self.push_section(&mut out, &section, "");
        out.into_bytes()
    }

    /// Append one section's kept attributes and its closing blank line to `out`, or nothing at all
    /// when the digests were all it had to say.
    fn push_section(&self, out: &mut String, section: &[&Item<'a>], close: &str) {
        let mut kept: Vec<&str> = Vec::new();
        let mut names_only = true;
        for item in section {
            let Item::Attribute { verbatim, logical } = item else {
                continue;
            };
            if Self::is_digest(logical) {
                continue;
            }
            // A `Name:` alone identifies a member and claims nothing about it, so a section holding
            // only names is residue of the digests just removed.
            names_only &= Self::value_of(logical, "Name").is_some();
            kept.push(verbatim);
        }
        if kept.is_empty() || names_only {
            return;
        }
        for verbatim in kept {
            self.push_terminated(out, verbatim);
        }
        out.push_str(close);
    }

    /// Append `verbatim` to `out`, terminating it when the archive did not — which only the file's
    /// final line can be, and only until something is written after it.
    fn push_terminated(&self, out: &mut String, verbatim: &str) {
        out.push_str(verbatim);
        if Self::terminator_of(verbatim).is_empty() {
            out.push_str(self.terminator);
        }
    }

    /// Append `name: value` to `out` as one or more physical manifest lines.
    ///
    /// A value too long for one line continues on following lines that each begin with exactly one
    /// space. Every physical line, terminator included, stays within [`MAX_LINE`] bytes, and the
    /// split never lands inside a UTF-8 sequence — `str::floor_char_boundary` is unstable and this
    /// crate is `no_std`, so the boundary is walked back by hand. Only a deeply nested `Main-Class`
    /// ever reaches the wrapping path, but an unwrapped over-long line is an invalid manifest, not
    /// a cosmetic issue: it is a manifest this crate itself could not read back.
    ///
    /// `fold` terminates the intermediate lines and `end` closes the last one. They are the same
    /// for a manifest written from scratch and differ only where an edit has to preserve what the
    /// archive already spelled.
    fn write_attribute(out: &mut String, name: &str, value: &str, fold: &str, end: &str) {
        let mut line = String::with_capacity(name.len() + 2 + value.len());
        line.push_str(name);
        line.push_str(": ");
        line.push_str(value);

        let mut rest = line.as_str();
        // The first physical line spends its whole budget on content; a continuation gives one byte
        // back to the leading space that marks it as one. Budgeted against `fold` rather than `end`
        // because only a line the fold terminates can be over-long: the last one already fits.
        let mut budget = MAX_LINE.saturating_sub(fold.len());
        loop {
            if rest.len() <= budget {
                out.push_str(rest);
                out.push_str(end);
                return;
            }
            let mut take = budget;
            while take > 0 && !rest.is_char_boundary(take) {
                take -= 1;
            }
            if take == 0 {
                // Unreachable while the budget exceeds four bytes, but emitting one whole character
                // keeps the loop total rather than spinning on a zero-length split.
                take = rest.chars().next().map_or(rest.len(), char::len_utf8);
            }
            let (head, tail) = rest.split_at(take);
            out.push_str(head);
            out.push_str(fold);
            out.push(' ');
            rest = tail;
            budget = MAX_LINE.saturating_sub(fold.len() + 1);
        }
    }

    /// The value `logical` gives attribute `name`, trimmed, or `None` when it declares another.
    fn value_of<'l>(logical: &'l str, name: &str) -> Option<&'l str> {
        logical
            .split_once(':')
            .filter(|(declared, _)| declared.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    }

    /// Whether a logical attribute declares a digest of something.
    ///
    /// Matched on the substring rather than on a fixed list, because the algorithm is part of the
    /// name (`SHA-256-Digest`, `SHA1-Digest`) and a signer may add `-Digest-Manifest` spellings of
    /// its own. On `digest` rather than on `-digest`, because the specification's legacy
    /// `Digest-Algorithms` puts the word first: matching only the hyphenated form would leave a
    /// section saying which algorithms were used and carrying none of them — and, since that line
    /// is not a `Name:`, would keep the whole section alive around it.
    fn is_digest(logical: &str) -> bool {
        const DIGEST: &[u8] = b"digest";
        logical.split_once(':').is_some_and(|(name, _)| {
            // Matched without allocating: a signed client jar's manifest holds one section per
            // member, so this runs once per attribute over megabytes of text.
            name.trim()
                .as_bytes()
                .windows(DIGEST.len())
                .any(|window| window.eq_ignore_ascii_case(DIGEST))
        })
    }

    /// The index just past the line starting at `offset`, terminator included.
    fn line_end(text: &str, offset: usize) -> usize {
        text[offset..]
            .find('\n')
            .map_or(text.len(), |index| offset + index + 1)
    }

    /// One or more physical lines without the trailing terminator.
    fn body(line: &str) -> &str {
        line.trim_end_matches(['\r', '\n'])
    }

    /// The terminator a run of physical lines ends with — empty for a final line the archive left
    /// unterminated.
    fn terminator_of(line: &str) -> &str {
        &line[Self::body(line).len()..]
    }

    /// The terminator the *first* physical line of a run ends with.
    fn first_line_terminator(verbatim: &str) -> &str {
        Self::terminator_of(verbatim.split_inclusive('\n').next().unwrap_or(verbatim))
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;

    use super::*;

    /// The manifest in `bytes`, which every test here writes as valid UTF-8.
    fn parse(bytes: &[u8]) -> Manifest<'_> {
        Manifest::parse(bytes).expect("the fixture is a readable manifest")
    }

    /// A manifest's logical attributes, each paired with the widest physical line it was written
    /// across — the two things a reader of the bytes has to agree with the writer about.
    fn attributes(manifest: &[u8]) -> Vec<(String, usize)> {
        let text = core::str::from_utf8(manifest).expect("the rewritten manifest stays UTF-8");
        let mut folded: Vec<(String, usize)> = Vec::new();
        for line in text.split_inclusive('\n') {
            let body = line.trim_end_matches(['\r', '\n']);
            match (body.strip_prefix(' '), folded.last_mut()) {
                (Some(rest), Some((attribute, widest))) => {
                    attribute.push_str(rest);
                    *widest = (*widest).max(line.len());
                }
                _ => folded.push((String::from(body), line.len())),
            }
        }
        folded
    }

    /// The manifest is found the way the signature block is, or a jar spelling it in lower case
    /// would lose the block and keep the digests — half a claim, which the module doc says is worse
    /// than none.
    #[test]
    fn the_manifest_is_recognised_the_way_a_signature_block_is() {
        assert!(MetaInf::is_manifest("META-INF/MANIFEST.MF"));
        assert!(MetaInf::is_manifest("META-INF/manifest.mf"));
        // Both components, exactly as `is_signature` reads them and as `JarFile::getManEntry`'s
        // `equalsIgnoreCase` sweep finds the manifest.
        assert!(MetaInf::is_manifest("meta-inf/MANIFEST.MF"));
        assert!(MetaInf::is_manifest("Meta-Inf/Manifest.mf"));

        assert!(!MetaInf::is_manifest("META-INF/versions/9/MANIFEST.MF"));
        assert!(!MetaInf::is_manifest("MANIFEST.MF"));
        assert!(!MetaInf::is_manifest("META-INF/SIGNER.SF"));
    }

    /// The signature block is matched as the JDK matches it — no wider, because a member this
    /// deletes that the JVM would have kept is a resource taken out of a jar that still needs it.
    #[test]
    fn a_signature_block_is_matched_as_the_jdk_matches_it() {
        assert!(MetaInf::is_signature("META-INF/SIGNER.SF"));
        assert!(MetaInf::is_signature("META-INF/signer.rsa"));
        assert!(MetaInf::is_signature("META-INF/SIGNER.DSA"));
        assert!(MetaInf::is_signature("META-INF/SIGNER.EC"));
        assert!(MetaInf::is_signature("META-INF/SIG-BC"));
        assert!(MetaInf::is_signature("META-INF/sig-bc.rsa"));

        // …but only with the extension the JDK accepts: absent, or one to three alphanumerics. For
        // a `.class` this would delete one a remap had already rewritten, since the predicate is
        // asked before the remapped bytes are collected.
        assert!(!MetaInf::is_signature("META-INF/SIG-config.json"));
        assert!(!MetaInf::is_signature("META-INF/SIG-Foo.class"));
        assert!(!MetaInf::is_signature("META-INF/SIG-x.a_b"));

        assert!(!MetaInf::is_signature("META-INF/MANIFEST.MF"));
        assert!(!MetaInf::is_signature("META-INF/SIGNATURES.TXT"));
        // A member name is whatever the archive says; a prefix test over bytes must not panic on
        // one whose fourth byte falls inside a character.
        assert!(!MetaInf::is_signature("META-INF/日本語.txt"));
        assert!(!MetaInf::is_signature("META-INF/ыыы"));
        // Only at that one depth: a service descriptor is an ordinary member.
        assert!(!MetaInf::is_signature("META-INF/services/provider.sf"));
        assert!(!MetaInf::is_signature("net/minecraft/Client.sf"));
        assert!(!MetaInf::is_signature("META-INF/"));

        // The directory component too, because the JDK upper-cases the whole entry name before
        // testing it. A block this spelling hid would ride through a remap that had already
        // stripped the manifest half of its claim.
        assert!(MetaInf::is_signature("meta-inf/SIGNER.RSA"));
        assert!(MetaInf::is_signature("Meta-Inf/signer.sf"));
        assert!(!MetaInf::is_signature("meta-inf/services/provider.sf"));
    }

    /// A versioned member keeps the prefix that distinguishes it from the class it shadows.
    #[test]
    fn a_versioned_member_keeps_its_prefix() {
        assert_eq!(
            MetaInf::multi_release_prefix("META-INF/versions/9/a/B.class"),
            "META-INF/versions/9/"
        );
        assert_eq!(MetaInf::multi_release_prefix("a/B.class"), "");
        // No member under it, so nothing to keep.
        assert_eq!(MetaInf::multi_release_prefix("META-INF/versions/9"), "");
    }

    /// A fresh manifest leads with its version and closes its section.
    #[test]
    fn a_packaged_manifest_is_the_shape_the_specification_asks_for() {
        assert_eq!(
            Manifest::packaged(Some("com.example.Main")),
            b"Manifest-Version: 1.0\r\nMain-Class: com.example.Main\r\n\r\n"
        );
        // A project with no entry point still packages — as a library jar.
        assert_eq!(Manifest::packaged(None), b"Manifest-Version: 1.0\r\n\r\n");
    }

    /// The wrap walks back to a character boundary, so a multibyte value cannot be split into
    /// invalid UTF-8 — the manifest is read as text on the other side.
    #[test]
    fn a_wrapped_value_never_splits_a_utf8_sequence() {
        // Three-byte characters land astride every candidate split point.
        let value: String = core::iter::repeat_n('あ', 60).collect();
        let packaged = Manifest::packaged(Some(&value));
        let text = core::str::from_utf8(&packaged).expect("the wrap keeps the manifest UTF-8");

        // Everything after `Manifest-Version` and before the blank line that closes the section is
        // the one wrapped attribute: the first of those lines starts it, each later one continues
        // it with exactly one leading space.
        let body = text
            .strip_suffix(CRLF)
            .expect("the main section is terminated");
        let lines: Vec<&str> = body.split_terminator(CRLF).skip(1).collect();
        let mut joined = String::from(lines[0]);
        for line in &lines[1..] {
            assert!(
                line.starts_with(' ') && !line[1..].starts_with(' '),
                "continuation must start with exactly one space: {line:?}"
            );
            joined.push_str(&line[1..]);
        }
        assert_eq!(joined, format!("Main-Class: {value}"));
        for line in &lines {
            assert!(
                line.len() + CRLF.len() <= MAX_LINE,
                "physical line exceeds the budget: {line:?}"
            );
        }
    }

    /// `Multi-Release` is read from the main section only, and only as `true`.
    #[test]
    fn multi_release_is_a_main_attribute() {
        assert!(
            parse(b"Manifest-Version: 1.0\r\nMulti-Release: true\r\n\r\n").declares_multi_release()
        );
        assert!(
            parse(b"Manifest-Version: 1.0\r\nmulti-release: TRUE\r\n\r\n").declares_multi_release()
        );
        assert!(
            !parse(b"Manifest-Version: 1.0\r\nMulti-Release: false\r\n\r\n")
                .declares_multi_release()
        );
        assert!(!parse(b"Manifest-Version: 1.0\r\n\r\n").declares_multi_release());
        // An individual section says it about one member, which is not what the JVM reads.
        assert!(
            !parse(b"Manifest-Version: 1.0\r\n\r\nName: a/B.class\r\nMulti-Release: true\r\n\r\n")
                .declares_multi_release()
        );
    }

    /// Setting it is idempotent, replaces a `false`, and never lands in an individual section.
    #[test]
    fn multi_release_is_written_into_the_main_section() {
        let plain =
            b"Manifest-Version: 1.0\r\nCreated-By: x\r\n\r\nName: a/B.class\r\nFoo: 1\r\n\r\n";
        let out = parse(plain).with_multi_release();
        assert_eq!(
            core::str::from_utf8(&out).expect("utf-8"),
            concat!(
                "Manifest-Version: 1.0\r\nCreated-By: x\r\nMulti-Release: true\r\n\r\n",
                "Name: a/B.class\r\nFoo: 1\r\n\r\n"
            )
        );
        assert!(parse(&out).declares_multi_release());
        // Applying it again is the identity, byte for byte, rather than a re-render that happens to
        // agree: an archive whose manifest is already right is one this pass must not touch.
        assert_eq!(parse(&out).with_multi_release(), out);

        // A `false` becomes a `true` rather than standing beside one.
        let fixed =
            parse(b"Manifest-Version: 1.0\r\nMulti-Release: false\r\n\r\n").with_multi_release();
        assert_eq!(
            core::str::from_utf8(&fixed).expect("utf-8"),
            "Manifest-Version: 1.0\r\nMulti-Release: true\r\n\r\n"
        );
    }

    /// A manifest whose main section the archive never closed gets closed, or the attribute would
    /// land in a section the JVM reads as being about a member.
    #[test]
    fn an_unclosed_main_section_is_closed_around_the_attribute() {
        let out = parse(b"Manifest-Version: 1.0\r\n").with_multi_release();
        assert_eq!(
            core::str::from_utf8(&out).expect("utf-8"),
            "Manifest-Version: 1.0\r\nMulti-Release: true\r\n\r\n"
        );
        // Even when the final line carried no terminator at all.
        let out = parse(b"Manifest-Version: 1.0").with_multi_release();
        assert_eq!(
            core::str::from_utf8(&out).expect("utf-8"),
            "Manifest-Version: 1.0\r\nMulti-Release: true\r\n\r\n"
        );
    }

    /// A manifest that is not CRLF keeps its own terminator, including for the line added to it.
    #[test]
    fn an_edit_writes_the_terminator_the_archive_used() {
        let out = parse(b"Manifest-Version: 1.0\n\n").with_multi_release();
        assert_eq!(
            core::str::from_utf8(&out).expect("utf-8"),
            "Manifest-Version: 1.0\nMulti-Release: true\n\n"
        );
    }

    /// A `Main-Class` too long for one manifest line is one attribute in both directions.
    ///
    /// [`Manifest::write_attribute`] caps a physical line at 72 bytes including its terminator, so
    /// every entry point whose name runs past 58 characters reaches a remap already folded — and
    /// this crate is what wrote it that way. A pass that matched physical lines read no
    /// `Main-Class` at all there, and the reobfuscated jar shipped naming a class the remap had
    /// just renamed away.
    #[test]
    fn a_main_class_too_long_for_one_line_is_read_and_written_as_one_attribute() {
        const LONG: &str = "com.example.application.launcher.VeryLongApplicationEntryPoint";
        assert!(
            "Main-Class: ".len() + LONG.len() > MAX_LINE,
            "the fixture has to be long enough to need folding"
        );

        // Writing: a short name becomes a long one, folded rather than emitted as one over-long
        // line, which is not a legal manifest.
        let written =
            parse(b"Manifest-Version: 1.0\r\nMain-Class: a\r\n\r\n").with_main_class(LONG);
        let out = attributes(&written);
        assert!(
            out.contains(&(format!("Main-Class: {LONG}"), MAX_LINE)),
            "the long name is one attribute folded onto 72-byte lines: {out:?}"
        );
        assert!(
            out.iter().all(|(_, widest)| *widest <= MAX_LINE),
            "no physical line may exceed the cap: {out:?}"
        );

        // Reading: the folded form this crate writes is what a reobfuscating remap is handed back.
        assert_eq!(parse(&written).main_class(), Some(LONG));
        let read = parse(&written).with_main_class("com.example.Short");
        assert!(
            attributes(&read).contains(&("Main-Class: com.example.Short".to_owned(), 31)),
            "a folded `Main-Class` is the attribute the replacement lands on"
        );
        // Everything else survives, terminators included.
        assert!(
            core::str::from_utf8(&read)
                .expect("utf-8")
                .starts_with("Manifest-Version: 1.0\r\n"),
            "the rest of the manifest is untouched"
        );
    }

    /// A `Main-Class` in an individual section says something about one member and is not the
    /// archive's entry point, so neither reading nor writing it reaches there.
    #[test]
    fn main_class_is_a_main_attribute_too() {
        let manifest = b"Manifest-Version: 1.0\r\n\r\nName: a/B.class\r\nMain-Class: a.B\r\n\r\n";
        assert_eq!(parse(manifest).main_class(), None);
        // Nothing to replace, so the bytes come back exactly as they went in.
        assert_eq!(parse(manifest).with_main_class("x.Y"), manifest);
    }

    /// A remapped jar that keeps its per-entry digests is refused by a JVM exactly as one that
    /// keeps its signature block, so both halves of the claim go.
    #[test]
    fn the_manifest_loses_its_per_entry_digests() {
        let manifest = "Manifest-Version: 1.0\r\n\
             Main-Class: net.minecraft.client.main.Main\r\n\
             \r\n\
             Name: net/minecraft/client/Minecraft.class\r\n\
             SHA-256-Digest: Zm9vYmFyYmF6\r\n\
             \r\n\
             Name: assets/minecraft/lang/en_us.json\r\n\
             SHA-256-Digest: cXV1eA==\r\n\
             \r\n";
        assert_eq!(
            String::from_utf8(parse(manifest.as_bytes()).without_digests()).unwrap(),
            "Manifest-Version: 1.0\r\nMain-Class: net.minecraft.client.main.Main\r\n\r\n"
        );
    }

    /// A digest attribute whose name opens with the word — the specification's legacy
    /// `Digest-Algorithms` — is a digest like any other. Left behind it would both survive as
    /// residue and, not being a `Name:`, keep its whole section alive around it.
    #[test]
    fn a_legacy_digest_algorithms_line_goes_with_the_digests_it_names() {
        let manifest = "Manifest-Version: 1.0\r\n\
             \r\n\
             Name: a/B.class\r\n\
             Digest-Algorithms: SHA MD5\r\n\
             SHA-Digest: Zm9v\r\n\
             MD5-Digest: YmFy\r\n\
             \r\n";
        assert_eq!(
            String::from_utf8(parse(manifest.as_bytes()).without_digests()).unwrap(),
            "Manifest-Version: 1.0\r\n\r\n"
        );
    }

    /// The digests are what is stale; whatever else a section says about its member is not.
    #[test]
    fn a_section_saying_more_than_a_digest_keeps_the_rest() {
        let manifest = "Manifest-Version: 1.0\r\n\
             \r\n\
             Name: com/example/\r\n\
             Sealed: true\r\n\
             SHA-256-Digest: Zm9v\r\n\
             \r\n";
        assert_eq!(
            String::from_utf8(parse(manifest.as_bytes()).without_digests()).unwrap(),
            "Manifest-Version: 1.0\r\n\r\nName: com/example/\r\nSealed: true\r\n\r\n"
        );
    }

    /// A manifest wraps at 72 bytes, so a digest is routinely two lines. Dropping the first and
    /// leaving the second behind would produce a manifest nothing can parse.
    #[test]
    fn a_wrapped_digest_loses_its_continuation_lines_too() {
        let manifest = "Manifest-Version: 1.0\r\n\
             \r\n\
             Name: a/B.class\r\n\
             SHA-256-Digest: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\r\n\
             \x20AAAAAAAAAAAA=\r\n\
             Sealed: true\r\n\
             \r\n";
        assert_eq!(
            String::from_utf8(parse(manifest.as_bytes()).without_digests()).unwrap(),
            "Manifest-Version: 1.0\r\n\r\nName: a/B.class\r\nSealed: true\r\n\r\n"
        );
    }

    /// The main section is never a digest, and a manifest with nothing else in it comes out as it
    /// went in — including its line terminator, which a hand-written one need not spell CRLF.
    #[test]
    fn an_unsigned_manifest_is_left_alone() {
        for manifest in [
            "Manifest-Version: 1.0\r\nMulti-Release: true\r\n\r\n",
            "Manifest-Version: 1.0\n\n",
            // Mixed terminators, which a re-render rather than an edit would normalize away.
            "Manifest-Version: 1.0\r\nCreated-By: x\n\r\n",
        ] {
            assert_eq!(
                String::from_utf8(parse(manifest.as_bytes()).without_digests()).unwrap(),
                manifest
            );
        }
    }

    /// Not text at all: a manifest this crate cannot read is one it must not rewrite, because the
    /// alternative is replacing a member it does not understand with an empty one.
    #[test]
    fn a_manifest_that_is_not_utf8_is_not_a_manifest_this_crate_edits() {
        let bytes: Vec<u8> = vec![0xff, 0xfe, b'M', b'Z'];
        assert!(Manifest::parse(&bytes).is_none());
    }
}
