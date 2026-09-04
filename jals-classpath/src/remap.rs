//! Jar remapping with Mojang official mappings, plus registered jar merge.
//!
//! [`JarRemap::remap`] turns an obfuscated jar + mapping text into a deobfuscated jar published
//! under `BuildTaskArtifact`. The transform is append-only on every class pool (new Utf8 /
//! `NameAndType` / Class entries are added, refs are rewritten in place) so every external index
//! stays stable while rates of hierarchy-aware member renaming and descriptor/signature
//! rewriting proceed. Non-class members pass through verbatim with two exceptions, both of which
//! exist because a remapped jar has to *run*: a jar signature block (`META-INF/*.{SF,DSA,RSA,EC}`)
//! is dropped along with the manifest's per-entry digests, since rewriting every class leaves them
//! describing bytes that no longer exist and a JVM refuses such an archive; and
//! `META-INF/MANIFEST.MF`'s `Main-Class` is rewritten when present.
//!
//! [`JarMerge::merge`] unions two jars by member path: the overlay wins on conflicts, the base
//! keeps everything else, both in deterministic input order.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use jals_classfile::{ClassFile, ConstantPool, ConstantPoolEntry};
use jals_exec::Exec;
use jals_progress::Task;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, ProvenanceFold,
};

use crate::jar::JarPackage;
use crate::load::{Archive, SourceTreeLimits};
use crate::manifest::{Manifest, MetaInf};
use crate::mappings::Mappings;
use crate::zip::WriteMember;
use crate::{MappingFormat, RemapDirection};

/// Hardcoded size budget for a remapped / merged jar input. Matches the task-side
/// `ExtractJava` total (1 GiB) so a Minecraft client/server jar always fits with headroom.
const JAR_LIMITS: SourceTreeLimits = SourceTreeLimits {
    max_files: 200_000,
    max_file_bytes: 64 * 1_048_576,
    max_total_bytes: 1_024 * 1_048_576,
};

/// Version of what a remap *writes*, folded into every remap provenance.
///
/// The provenance's other inputs — the source jar, the mapping bytes, the direction, the class
/// hierarchy — say what went in and nothing about what this crate does with it, so a change to the
/// transform itself would be served the previous transform's jar out of the cache. Bump this
/// whenever the bytes written for an otherwise unchanged input change; [`JarTransforms`] carries it
/// to the consumers that memoize around this one.
///
/// 2: everything the `META-INF/` pass has done to a remapped jar. The signature block goes and the
/// manifest's per-entry digests go with it — which is the change this constant should have moved
/// for when that landed, and did not.
///
/// 3: `META-INF/` is matched however the archive spells it, as the JVM matches it, so a
/// `meta-inf/`-spelled signature block goes and a `meta-inf/`-spelled manifest is stripped instead
/// of the pass leaving half a claim standing. And a `Main-Class` folded onto continuation lines —
/// which is every entry point whose name runs past the manifest's 72-byte cap, `jar.rs`'s own
/// output included — is read as one attribute and written back folded, rather than being missed
/// and left naming a class the remap has since renamed.
///
/// 4: the manifest is edited rather than re-rendered, so a signed manifest that mixed its line
/// terminators or left its last section unclosed keeps what its author wrote instead of being
/// normalized; `Main-Class` is read and written as the main attribute it is, never from an
/// individual section, where it says nothing a JVM reads; and the output leads with its manifest,
/// which `JarPackage::write_members` now imposes on every jar this crate emits rather than on the
/// merged ones alone.
const REMAP_OUTPUT_VERSION: u32 = 4;

/// The same, for what a merge writes.
///
/// 2: a merged manifest carries `Multi-Release` when either input's did.
///
/// 3: the two sides' manifests are one conflict however either spells the name, the survivor is
/// written first, and the digest strip leaves a manifest that has no digests in it alone.
///
/// 4: the `META-INF/` component is matched case-insensitively, so a `meta-inf/`-spelled manifest is
/// the manifest for the conflict, the `Multi-Release` read and the digest strip alike.
///
/// 5: the manifest edits are [`crate::manifest`]'s, and are byte-identity wherever they change
/// nothing — a union whose surviving manifest already said `Multi-Release: true` now keeps that
/// manifest's own bytes rather than a re-rendering of them.
const MERGE_OUTPUT_VERSION: u32 = 5;

/// The output versions of every jar transform this crate performs.
///
/// Published because the versions above are not the whole rule. A consumer that memoizes *around*
/// one of these transforms — `jals-project` records what a build task produced and replays it
/// without re-running the task at all — names the transform's inputs in its own key and nothing
/// about the transform, so a bump here would leave that consumer serving the previous transform's
/// bytes out of a warm cache. That has happened twice: a remapped jar kept its signature block, and
/// a merged jar kept saying `Multi-Release: false`, both after the fix had shipped.
///
/// It is a fold rather than a number a consumer copies, so the rule holds without anyone
/// remembering it: a consumer folds this into its key once, and a transform added or bumped here
/// moves every such key with no edit on the consumer's side.
pub struct JarTransforms;

impl JarTransforms {
    /// Every transform's name and output version, in a fixed order.
    ///
    /// The name is folded beside the number so that two transforms swapping versions is not the
    /// same fold, and so that adding one shifts nothing that came before it.
    const VERSIONS: &'static [(&'static str, u32)] = &[
        ("remap", REMAP_OUTPUT_VERSION),
        ("merge", MERGE_OUTPUT_VERSION),
    ];

    /// Fold every transform's output version into `fold`.
    pub fn fold(fold: &mut ProvenanceFold) {
        for (name, version) in Self::VERSIONS {
            fold.bytes(name.as_bytes()).version(*version);
        }
    }
}

/// Obfuscated class-hierarchy index used to walk supers/interfaces for inherited member lookups.
#[derive(Debug, Default)]
struct ClassIndex {
    /// Obfuscated internal name → (optional super, interfaces), in obfuscated internal form.
    supers: BTreeMap<String, (Option<String>, Vec<String>)>,
}

impl ClassIndex {
    fn insert(&mut self, this: String, super_name: Option<String>, interfaces: Vec<String>) {
        self.supers.insert(this, (super_name, interfaces));
    }

    /// `owner` and its supertypes (obfuscated internal names), in the order the JVM resolves a
    /// member: the class itself, then its superclass chain, then its superinterfaces breadth-first
    /// in declaration order. Each type is yielded at most once.
    ///
    /// The order decides which mapping a member reference adopts when more than one supertype
    /// declares the same name and descriptor. Searching interfaces before the superclass — or
    /// interfaces in reverse declaration order — picks a different mapping than the JVM picks at
    /// run time, which silently rewires a call to a different method.
    fn walk_hierarchy<'a>(&'a self, owner: &'a str) -> Vec<&'a str> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        let mut interfaces = Vec::new();

        // The class itself, then up the superclass chain.
        let mut current = Some(owner);
        while let Some(class) = current {
            if !seen.insert(class) {
                break;
            }
            out.push(class);
            let Some((super_name, declared)) = self.supers.get(class) else {
                break;
            };
            interfaces.extend(declared.iter().map(String::as_str));
            current = super_name.as_deref();
        }

        // Then interfaces, breadth-first, keeping declaration order within each level.
        let mut next = 0;
        while next < interfaces.len() {
            let interface = interfaces[next];
            next += 1;
            if !seen.insert(interface) {
                continue;
            }
            out.push(interface);
            if let Some((_, declared)) = self.supers.get(interface) {
                interfaces.extend(declared.iter().map(String::as_str));
            }
        }
        out
    }
}

/// Nested-jar extraction namespace.
pub struct NestedJar;

impl NestedJar {
    /// Extract a single nested `-jar` member from `parent` and publish it as a build-task artifact.
    pub async fn extract<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        parent: &CacheKey,
        member: &str,
    ) -> Result<CacheKey, String> {
        let reader = cache
            .open_verified(parent)
            .await
            .map_err(|error| format!("parent jar is invalid: {error:?}"))?
            .ok_or_else(|| "parent jar is not cached".to_owned())?;
        let members = Archive::decode_all_bounded(exec, reader, JAR_LIMITS).await?;
        let bytes = members
            .into_iter()
            .find(|(name, _)| name == member)
            .ok_or_else(|| format!("nested jar member `{member}` is missing"))?
            .1
            .map_err(|error| format!("failed to read nested jar `{member}`: {error}"))?;
        Self::publish_nested(cache, parent, member, &bytes).await
    }

    /// Extract every nested `-jar` member of `parent` (in archive order) and publish each as a
    /// build-task artifact. Used to flatten library bundlers onto the compile classpath.
    pub async fn extract_all<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        parent: &CacheKey,
    ) -> Result<Vec<CacheKey>, String> {
        let reader = cache
            .open_verified(parent)
            .await
            .map_err(|error| format!("parent jar is invalid: {error:?}"))?
            .ok_or_else(|| "parent jar is not cached".to_owned())?;
        let members = Archive::decode_all_bounded(exec, reader, JAR_LIMITS).await?;
        let mut out = Vec::new();
        for (name, outcome) in members {
            if !helpers::has_extension(&name, "jar") {
                continue;
            }
            let bytes =
                outcome.map_err(|error| format!("failed to read nested jar `{name}`: {error}"))?;
            // A `.jar`-named member that is not actually an archive is skipped rather than fatal;
            // `extract`, which names one member explicitly, rejects it instead.
            if !helpers::looks_like_zip(&bytes) {
                continue;
            }
            out.push(Self::publish_nested(cache, parent, &name, &bytes).await?);
        }
        Ok(out)
    }

    async fn publish_nested<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        parent: &CacheKey,
        member: &str,
        bytes: &[u8],
    ) -> Result<CacheKey, String> {
        if !helpers::looks_like_zip(bytes) {
            return Err(format!("nested member `{member}` is not a zip archive"));
        }
        let mut fold = ProvenanceFold::new(b"nested-jar\0");
        fold.parent(parent).bytes(member.as_bytes());
        let key = CacheKey::new(
            CacheNamespace::BuildTaskArtifact,
            fold.finish(),
            ContentDigest::of(bytes),
        );
        cache
            .publish(&key, bytes)
            .await
            .map_err(|error| format!("nested jar publish failed: {error:?}"))?;
        Ok(key)
    }
}

/// Jar remapping namespace.
pub struct JarRemap;

impl JarRemap {
    /// Remap every `.class` member of `jar` per `request`, publishing the resulting jar under
    /// `BuildTaskArtifact`.
    ///
    /// Provenance covers the source jar key, the mapping *digest* (not its text — the rule keys on
    /// mapping identity), the direction, the format, and every hierarchy jar, so re-runs are
    /// content-addressed and two directions of one file never collide.
    ///
    /// The whole thing is memoized through the cache's advisory locator index: the work here is
    /// proportional to the size of a game jar, and the callers that are not the task graph — a
    /// `[dependencies]` entry resolved on every editor reload — have no plan-level memo above them.
    /// A stale index entry costs a miss, never wrong bytes, because the artifact still comes back
    /// through a verified read.
    /// One class remapped, named by what it became.
    ///
    /// Split out of the fan-out closure so the closure is the two lines that matter — the work, and
    /// the tick that says it happened.
    fn remap_one(
        position: usize,
        cf: &mut ClassFile,
        mappings: &Mappings,
        index: &ClassIndex,
    ) -> Result<(usize, String, Vec<u8>), (usize, String)> {
        helpers::remap_class(cf, mappings, index)
            .map(|()| {
                let this = cf.constant_pool.class_name(cf.this_class).map_or_else(
                    || format!("unknown{position}"),
                    alloc::borrow::Cow::into_owned,
                );
                let member_name = format!("{this}.class");
                (position, member_name, cf.write())
            })
            .map_err(|error| (position, error))
    }

    /// `report` is the caller's unit of work. A remap of a whole game jar is tens of thousands of
    /// classes over three passes, and the fan-out in the middle counts through a
    /// [`Ticker`](jals_progress::Ticker) — which is the whole reason that type exists.
    pub async fn remap<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
        request: &RemapRequest<'_>,
        report: &Task,
    ) -> Result<CacheKey, String> {
        let provenance = request.provenance(jar);
        if let Some(key) = cache
            .indexed_key(CacheNamespace::BuildTaskArtifact, provenance)
            .await
            .map_err(|error| format!("remap index lookup failed: {error:?}"))?
            && cache
                .open_verified(&key)
                .await
                .map_err(|error| format!("remapped jar is invalid: {error:?}"))?
                .is_some()
        {
            // The memo answered, so nothing below runs. Reported through the caller's unit, which
            // is what turns a silent instant into a `Fresh` line.
            report.fresh();
            return Ok(key);
        }

        let mappings = Mappings::parse(request.mappings, &request.format, request.direction)
            .map_err(|error| format!("mappings parse failed: {error}"))?;
        let mappings = Arc::new(mappings);

        let reader = cache
            .open_verified(jar)
            .await
            .map_err(|error| format!("remap jar is invalid: {error:?}"))?
            .ok_or_else(|| "remap jar is not cached".to_owned())?;
        let members = Archive::decode_all_bounded(exec, reader, JAR_LIMITS).await?;

        // Pass 1: parse every class file and build the source-namespace class hierarchy.
        let mut parsed: Vec<(usize, ClassFile)> = Vec::new();
        let mut index = ClassIndex::default();
        for (position, (name, outcome)) in members.iter().enumerate() {
            if !helpers::has_extension(name, "class") {
                continue;
            }
            let bytes = outcome
                .as_ref()
                .map_err(|error| format!("failed to read archive member `{name}`: {error}"))?;
            let cf = ClassFile::read(bytes.as_slice())
                .await
                .map_err(|error| format!("failed to parse archive member `{name}`: {error}"))?;
            Self::index_class(&mut index, name, &cf)?;
            parsed.push((position, cf));
        }
        Self::index_hierarchy(exec, cache, request.hierarchy, &mut index).await?;
        let index = Arc::new(index);

        // Pass 2: remap each class (CPU-bound; fan-out keeps input order).
        let inputs: Vec<_> = parsed
            .into_iter()
            .map(|(position, cf)| (position, cf, Arc::clone(&mappings), Arc::clone(&index)))
            .collect();
        report.set_total(inputs.len() as u64);
        let ticker = report.ticker();
        let outcomes = exec
            .fan_out(inputs, move |(position, mut cf, mappings, index)| {
                let ticker = ticker.clone();
                async move {
                    let outcome = Self::remap_one(position, &mut cf, &mappings, &index);
                    ticker.tick();
                    outcome
                }
            })
            .await;

        let mut remapped: BTreeMap<usize, (String, Vec<u8>)> = BTreeMap::new();
        for outcome in outcomes {
            match outcome {
                Ok((position, member_name, bytes)) => {
                    remapped.insert(position, (member_name, bytes));
                }
                Err((position, error)) => {
                    let name = members
                        .get(position)
                        .map_or("<unknown>", |(n, _)| n.as_str());
                    return Err(format!("failed to remap `{name}`: {error}"));
                }
            }
        }

        // Pass 3: rebuild the archive in original member order, but with class paths matching
        // the official this_class name so JVM/javac loaders resolve members by path.
        let mut out_members = Vec::with_capacity(members.len());
        let mut used_names = BTreeSet::new();
        for (position, (name, outcome)) in members.into_iter().enumerate() {
            // A signature block describes bytes that no longer exist. Every class in this jar was
            // rewritten, so the digests in `META-INF/*.SF` match nothing and a JVM refuses the
            // whole archive with `SecurityException: signer information does not match` — which is
            // why a remapped Minecraft jar compiles against but never *runs*. The block goes, and
            // the manifest's per-entry digests go with it below: they are one claim in two halves,
            // and keeping half is worse than keeping neither.
            if MetaInf::is_signature(&name) {
                continue;
            }
            let (name, bytes) = if let Some((member_name, remapped_bytes)) =
                remapped.remove(&position)
            {
                // A multi-release jar stores the same class twice, once under
                // `META-INF/versions/<n>/`. Both have the same `this_class`, so naming the output
                // purely from it collides and fails the whole remap. Keep the versioned prefix.
                let prefix = MetaInf::multi_release_prefix(&name);
                (format!("{prefix}{member_name}"), remapped_bytes)
            } else {
                let bytes = outcome
                    .map_err(|error| format!("failed to read archive member `{name}`: {error}"))?;
                let bytes = if MetaInf::is_manifest(&name) {
                    helpers::remap_manifest(&bytes, &mappings)
                } else {
                    bytes
                };
                (name, bytes)
            };
            if !used_names.insert(name.clone()) {
                return Err(format!("duplicate remapped archive member `{name}`"));
            }
            out_members.push(WriteMember { name, bytes });
        }
        let jar_bytes = JarPackage::write_members(out_members)?;

        let key = CacheKey::new(
            CacheNamespace::BuildTaskArtifact,
            provenance,
            ContentDigest::of(&jar_bytes),
        );
        cache
            .publish(&key, &jar_bytes)
            .await
            .map_err(|error| format!("remapped jar publish failed: {error:?}"))?;
        cache
            .record_index(&key)
            .await
            .map_err(|error| format!("remapped jar index failed: {error:?}"))?;
        Ok(key)
    }

    /// Add every class of `hierarchy` to `index` without remapping any of them.
    ///
    /// The jar being remapped rarely closes its own hierarchy. Reobfuscating a mod is the clear
    /// case — its classes extend types that live in the game jar — but a library split across
    /// archives has the same shape. Without these, an inherited member is looked up against a
    /// supertype nobody declared, misses, and keeps its source name in an otherwise remapped jar: a
    /// silent wrong answer rather than a failure.
    async fn index_hierarchy<C: CacheBackend>(
        exec: &Exec,
        cache: &ArtifactCache<C>,
        hierarchy: &[CacheKey],
        index: &mut ClassIndex,
    ) -> Result<(), String> {
        for extra in hierarchy {
            let reader = cache
                .open_verified(extra)
                .await
                .map_err(|error| format!("hierarchy jar is invalid: {error:?}"))?
                .ok_or_else(|| "hierarchy jar is not cached".to_owned())?;
            for (name, outcome) in Archive::decode_all_bounded(exec, reader, JAR_LIMITS).await? {
                if !helpers::has_extension(&name, "class") {
                    continue;
                }
                let bytes = outcome.map_err(|error| {
                    format!("failed to read hierarchy member `{name}`: {error}")
                })?;
                let cf = ClassFile::read(bytes.as_slice()).await.map_err(|error| {
                    format!("failed to parse hierarchy member `{name}`: {error}")
                })?;
                Self::index_class(index, &name, &cf)?;
            }
        }
        Ok(())
    }

    /// Record one class's `this` / `super` / `interfaces` edges into the hierarchy index.
    fn index_class(index: &mut ClassIndex, name: &str, cf: &ClassFile) -> Result<(), String> {
        let this = cf
            .constant_pool
            .class_name(cf.this_class)
            .ok_or_else(|| format!("class `{name}` has no this_class name"))?
            .into_owned();
        let super_name = if cf.super_class == 0 {
            None
        } else {
            Some(
                cf.constant_pool
                    .class_name(cf.super_class)
                    .ok_or_else(|| format!("class `{name}` has no super_class name"))?
                    .into_owned(),
            )
        };
        let interfaces = cf
            .interfaces
            .iter()
            .map(|&i| {
                cf.constant_pool
                    .class_name(i)
                    .map(alloc::borrow::Cow::into_owned)
                    .ok_or_else(|| format!("class `{name}` has a broken interfaces entry"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        index.insert(this, super_name, interfaces);
        Ok(())
    }
}

/// What one [`JarRemap::remap`] applies: the mapping text, how to read it, which way to apply it,
/// and the archives that close the hierarchy it needs.
///
/// Not `Copy`: a [`MappingFormat`] that names a namespace pair owns those names, and holding them
/// by reference would put the format's lifetime into every type that carries one, including the
/// owned [`crate::MappingSpec`] a resolver hands back.
#[derive(Debug, Clone)]
pub struct RemapRequest<'a> {
    /// The mapping text, already fetched and verified by whoever produced it.
    pub mappings: &'a str,
    /// Which grammar `mappings` is written in.
    pub format: MappingFormat,
    /// Which namespace the jar being remapped is written in.
    pub direction: RemapDirection,
    /// Cached jars read for their class hierarchy only — never remapped, never in the output.
    ///
    /// A `[dependencies]` deobfuscation usually needs none (a game jar closes over itself); a
    /// `[build] remap` needs the resolved compile classpath, because that is where the supertypes of
    /// the classes being reobfuscated live.
    pub hierarchy: &'a [CacheKey],
}

impl RemapRequest<'_> {
    /// The provenance of the artifact this request produces from `jar`.
    ///
    /// Every input that changes the output is folded, and the mapping text folds as its digest
    /// rather than its bytes so identity — not position in some file — is what the key rests on.
    fn provenance(&self, jar: &CacheKey) -> ContentDigest {
        let mut fold = ProvenanceFold::new(b"remap-jar\0");
        fold.version(REMAP_OUTPUT_VERSION)
            .parent(jar)
            .digest(ContentDigest::of(self.mappings.as_bytes()));
        // Through the format itself, so the match over its variants is exhaustive: a format that
        // selects a renaming from more than its tag — tiny v2's namespace pair — has to reach the
        // key, or two different remaps of one file would be served each other's jar.
        self.format.fold_into(&mut fold);
        fold.bytes(self.direction.tag_name().as_bytes());
        for extra in self.hierarchy {
            fold.parent(extra);
        }
        fold.finish()
    }
}

/// Jar merge namespace.
pub struct JarMerge;

impl JarMerge {
    /// Merge two cached jars. Members of `overlay` win on path conflicts; everything else comes
    /// from `base` in its original order, followed by any `overlay`-only members in overlay order.
    ///
    /// A signature block is dropped from both sides, as it is by a remap and for the same reason: a
    /// union carries members the signer never saw, and a JVM reading a signed archive that mixes
    /// signed and unsigned classes in one package refuses it. The half of the claim that lives in
    /// the manifest goes with it.
    ///
    /// The two sides' manifests are **one** conflict however either spells the name — the path
    /// collision a case-insensitive predicate recognises is not one an exact-keyed map would.
    /// Whichever survives is written first, but that is not decided here: this hands its union to
    /// [`JarPackage::write_members`], which is where "the manifest leads" is written down for every
    /// jar this crate emits. The conflict claim reaches no further than the two sides — a single
    /// input carrying two manifests of its own is two members here as it was there, since
    /// deduplicating *within* a side would be this function inventing a conflict its inputs did not
    /// have.
    /// `report` is the caller's unit of work; the two member loops count into it.
    pub async fn merge<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        base: &CacheKey,
        overlay: &CacheKey,
        report: &Task,
    ) -> Result<CacheKey, String> {
        let base_reader = cache
            .open_verified(base)
            .await
            .map_err(|error| format!("merge base jar is invalid: {error:?}"))?
            .ok_or_else(|| "merge base jar is not cached".to_owned())?;
        let overlay_reader = cache
            .open_verified(overlay)
            .await
            .map_err(|error| format!("merge overlay jar is invalid: {error:?}"))?
            .ok_or_else(|| "merge overlay jar is not cached".to_owned())?;
        let base_members = Archive::decode_all_bounded(exec, base_reader, JAR_LIMITS).await?;
        let overlay_members = Archive::decode_all_bounded(exec, overlay_reader, JAR_LIMITS).await?;
        report.set_total((base_members.len() + overlay_members.len()) as u64);

        // Whether either input is a multi-release archive. Only one manifest survives a merge —
        // the overlay's, like every other conflict — but `Multi-Release` is not a claim about the
        // manifest's own side. It says the archive's `META-INF/versions/<n>/` entries are live, and
        // a union carries both sides' entries, so dropping it with the losing manifest leaves those
        // entries in the jar and invisible to the JVM. That is not academic: 1.17's flat server jar
        // bundles log4j-api, whose `StackLocator` has a Java 8 body at the root and a Java 9 body
        // under `versions/9/`; without the attribute the client loads the Java 8 one and dies in
        // the first `LogManager.getLogger()` asking for a method Java 9 removed.
        let mut multi_release = false;
        let mut overlay_map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut overlay_order: Vec<String> = Vec::new();
        // Which member the overlay's manifest is, by name. Remembered rather than looked up again
        // by the base's spelling: `MetaInf::is_manifest` matches case-insensitively on purpose, so
        // a base `META-INF/MANIFEST.MF` and an overlay `META-INF/manifest.mf` would never collide
        // in a map keyed by the exact name, and the union would carry two manifests — with the
        // base's winning, which is the documented conflict rule backwards.
        let mut overlay_manifest: Option<String> = None;
        for (name, outcome) in overlay_members {
            report.advance(1);
            if MetaInf::is_signature(&name) {
                continue;
            }
            let mut bytes = outcome
                .map_err(|error| format!("failed to read overlay member `{name}`: {error}"))?;
            if MetaInf::is_manifest(&name) {
                bytes = Manifest::write_without_digests(&bytes);
                multi_release |= Manifest::read_multi_release(&bytes);
                overlay_manifest.get_or_insert_with(|| name.clone());
            }
            if overlay_map.insert(name.clone(), bytes).is_none() {
                overlay_order.push(name);
            }
        }

        // Walking the base consumes every overlay member that shadows one, so whatever is still in
        // `overlay_map` afterwards is exactly the overlay-only set.
        let mut out_members = Vec::new();
        for (name, outcome) in base_members {
            report.advance(1);
            if MetaInf::is_signature(&name) {
                continue;
            }
            let is_manifest = MetaInf::is_manifest(&name);
            // The manifest is matched to the overlay's manifest however either side spells it;
            // every other member is matched by exact path, as a zip's own identity is.
            let shadowing = if is_manifest {
                overlay_manifest
                    .as_ref()
                    .and_then(|manifest| overlay_map.remove(manifest))
            } else {
                overlay_map.remove(&name)
            };
            let bytes = if let Some(overlay_bytes) = shadowing {
                if is_manifest {
                    // Read even though the overlay's copy is the one that survives: the base
                    // manifest is the only place the base side can say `Multi-Release`, and a
                    // member that could not be read is not a member that said no. Fatal for the
                    // same reason it is fatal in the arm below — an I/O failure is not missing
                    // data, and answering it as "no" here is the wrong-variant-at-run-time bug
                    // this whole block exists to prevent.
                    let shadowed = outcome
                        .map_err(|error| format!("failed to read base member `{name}`: {error}"))?;
                    multi_release |= Manifest::read_multi_release(&shadowed);
                }
                overlay_bytes
            } else {
                let bytes = outcome
                    .map_err(|error| format!("failed to read base member `{name}`: {error}"))?;
                if is_manifest {
                    Manifest::write_without_digests(&bytes)
                } else {
                    bytes
                }
            };
            out_members.push(WriteMember { name, bytes });
        }
        for name in overlay_order {
            if let Some(bytes) = overlay_map.remove(&name) {
                out_members.push(WriteMember { name, bytes });
            }
        }
        // Applied after the union is assembled rather than while it is: which manifest survives is
        // decided by the walk above, and this has to reach whichever one did. The *first* one, for
        // the same reason `JarPackage::write_members` hoists that one — it is the manifest a
        // streaming reader gets, and `Multi-Release` is the one attribute this merge adds.
        if multi_release
            && let Some(manifest) = out_members
                .iter_mut()
                .find(|member| MetaInf::is_manifest(&member.name))
        {
            manifest.bytes = Manifest::write_multi_release(&manifest.bytes);
        }

        let jar_bytes = JarPackage::write_members(out_members)?;
        let mut fold = ProvenanceFold::new(b"merge-jars\0");
        fold.version(MERGE_OUTPUT_VERSION)
            .parent(base)
            .parent(overlay);
        let key = CacheKey::new(
            CacheNamespace::BuildTaskArtifact,
            fold.finish(),
            ContentDigest::of(&jar_bytes),
        );
        cache
            .publish(&key, &jar_bytes)
            .await
            .map_err(|error| format!("merged jar publish failed: {error:?}"))?;
        Ok(key)
    }
}

mod helpers {
    // Named rather than globbed. The list is long because this module does the class-file work, but
    // a glob here reaches through `super` for everything the *file* imports, which is how a helper
    // silently acquires a dependency the module above it took on for another reason.
    use alloc::borrow::ToOwned;
    use alloc::format;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use jals_classfile::{
        Annotation, Attribute, AttributeBody, ClassFile, ClassSignature, ClassTypeSignature,
        ConstantPool, ConstantPoolEntry, ElementValue, FieldInfo, FieldType, InnerClassEntry,
        MethodAccessFlags, MethodInfo, MethodSignature, RecordComponentInfo,
        SimpleClassTypeSignature, ThrowsSignature, TypeAnnotation, TypeArgument, TypeParameter,
        TypeSignature,
    };

    use super::{ClassIndex, PoolInterner};
    use crate::manifest::Manifest;
    use crate::mappings::Mappings;

    /// Whether archive member `name` carries `extension`, compared case-insensitively. Directory
    /// entries end in `/`, so they never match.
    pub(super) fn has_extension(name: &str, extension: &str) -> bool {
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(extension))
    }

    /// Whether `bytes` opens with a local-file-header / central-directory zip signature.
    pub(super) fn looks_like_zip(bytes: &[u8]) -> bool {
        bytes.len() >= 4 && bytes.starts_with(b"PK")
    }

    /// A manifest member's bytes as a remap leaves them: the entry point renamed, the digests
    /// gone.
    ///
    /// The two edits are [`crate::manifest`]'s and the decision between them is this module's.
    /// What a `Main-Class` maps to is a mapping question — the only one a manifest raises — and how
    /// a manifest spells the answer, folded onto continuation lines within the 72-byte cap and
    /// terminated the way the archive terminates its other lines, is not.
    ///
    /// Both edits are the identity when there is nothing to do, so an unsigned jar with no entry
    /// point comes back byte for byte.
    pub(super) fn remap_manifest(bytes: &[u8], mappings: &Mappings) -> Vec<u8> {
        let renamed = Manifest::read_main_class(bytes)
            .and_then(|dotted| mappings.remap_class(&dotted.replace('.', "/")))
            .map(|official| Manifest::write_main_class(bytes, &official.replace('/', ".")));
        Manifest::write_without_digests(renamed.as_deref().unwrap_or(bytes))
    }

    /// Kind of Signature attribute at a given attribute site.
    #[derive(Clone, Copy)]
    enum SignatureKind {
        Class,
        Field,
        Method,
    }

    /// Transform one class file in place. Returns an error string only on unrecoverable pool growth.
    pub(super) fn remap_class(
        cf: &mut ClassFile,
        mappings: &Mappings,
        index: &ClassIndex,
    ) -> Result<(), String> {
        // Snapshot this class's pre-remap identity (obfuscated internal name).
        let this_obf = cf
            .constant_pool
            .class_name(cf.this_class)
            .ok_or_else(|| "class has no this_class name".to_owned())?
            .into_owned();

        // Phase A: rewrite member refs / MethodType / indy / dynamic NaT entries using the
        // original (obfuscated) Class entry names still present in the pool.
        remap_pool_member_refs(cf, mappings, index)?;

        // Phase B: rename Class entries (in-place Class.name_index rebuild).
        remap_pool_class_entries(cf, mappings)?;

        // Phase C: declaration-side field/method name+descriptor rewrites.
        remap_declarations(cf, mappings, &this_obf, index)?;

        // Phase D: attributes (signatures, SourceFile, annotations, LVT, InnerClasses…).
        // Code-nested attributes are walked recursively inside the helper.
        let mut pool = PoolInterner::new(&mut cf.constant_pool);
        remap_attributes_with_pool(
            &mut pool,
            &mut cf.attributes,
            mappings,
            &this_obf,
            index,
            SignatureKind::Class,
        )?;
        for field in &mut cf.fields {
            remap_attributes_with_pool(
                &mut pool,
                &mut field.attributes,
                mappings,
                &this_obf,
                index,
                SignatureKind::Field,
            )?;
        }
        for method in &mut cf.methods {
            remap_attributes_with_pool(
                &mut pool,
                &mut method.attributes,
                mappings,
                &this_obf,
                index,
                SignatureKind::Method,
            )?;
        }
        Ok(())
    }

    fn remap_pool_member_refs(
        cf: &mut ClassFile,
        mappings: &Mappings,
        index: &ClassIndex,
    ) -> Result<(), String> {
        let mut interner = PoolInterner::new(&mut cf.constant_pool);
        let pool = &mut interner;
        let end = pool.next_index();
        for i in 1..end {
            let Some(entry) = pool.get(i).cloned() else {
                continue;
            };
            match entry {
                ConstantPoolEntry::FieldRef {
                    class_index,
                    name_and_type_index,
                } => {
                    if let Some(nat) = remap_member_nat(
                        pool,
                        mappings,
                        index,
                        class_index,
                        name_and_type_index,
                        true,
                    )? {
                        pool.replace(
                            i,
                            ConstantPoolEntry::FieldRef {
                                class_index,
                                name_and_type_index: nat,
                            },
                        );
                    }
                }
                ConstantPoolEntry::MethodRef {
                    class_index,
                    name_and_type_index,
                } => {
                    if let Some(nat) = remap_member_nat(
                        pool,
                        mappings,
                        index,
                        class_index,
                        name_and_type_index,
                        false,
                    )? {
                        pool.replace(
                            i,
                            ConstantPoolEntry::MethodRef {
                                class_index,
                                name_and_type_index: nat,
                            },
                        );
                    }
                }
                ConstantPoolEntry::InterfaceMethodRef {
                    class_index,
                    name_and_type_index,
                } => {
                    if let Some(nat) = remap_member_nat(
                        pool,
                        mappings,
                        index,
                        class_index,
                        name_and_type_index,
                        false,
                    )? {
                        pool.replace(
                            i,
                            ConstantPoolEntry::InterfaceMethodRef {
                                class_index,
                                name_and_type_index: nat,
                            },
                        );
                    }
                }
                ConstantPoolEntry::InvokeDynamic {
                    bootstrap_method_attr_index,
                    name_and_type_index,
                } => {
                    if let Some(nat) = remap_dynamic_nat(pool, mappings, name_and_type_index)? {
                        pool.replace(
                            i,
                            ConstantPoolEntry::InvokeDynamic {
                                bootstrap_method_attr_index,
                                name_and_type_index: nat,
                            },
                        );
                    }
                }
                ConstantPoolEntry::Dynamic {
                    bootstrap_method_attr_index,
                    name_and_type_index,
                } => {
                    if let Some(nat) = remap_dynamic_nat(pool, mappings, name_and_type_index)? {
                        pool.replace(
                            i,
                            ConstantPoolEntry::Dynamic {
                                bootstrap_method_attr_index,
                                name_and_type_index: nat,
                            },
                        );
                    }
                }
                ConstantPoolEntry::MethodType { descriptor_index } => {
                    let Some(desc) = utf8_owned(pool, descriptor_index) else {
                        continue;
                    };
                    let new_desc = mappings.remap_descriptor(&desc);
                    if new_desc != desc {
                        let idx = intern_utf8(pool, &new_desc)?;
                        pool.replace(
                            i,
                            ConstantPoolEntry::MethodType {
                                descriptor_index: idx,
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Remap a FieldRef/MethodRef `NameAndType`, returning a new `NaT` index when anything changes.
    fn remap_member_nat(
        pool: &mut PoolInterner<'_>,
        mappings: &Mappings,
        index: &ClassIndex,
        class_index: u16,
        nat_index: u16,
        is_field: bool,
    ) -> Result<Option<u16>, String> {
        let Some(owner_obf) = pool
            .class_name(class_index)
            .map(alloc::borrow::Cow::into_owned)
        else {
            return Ok(None);
        };
        let Some(ConstantPoolEntry::NameAndType {
            name_index,
            descriptor_index,
        }) = pool.get(nat_index).cloned()
        else {
            return Ok(None);
        };
        let Some(name_obf) = utf8_owned(pool, name_index) else {
            return Ok(None);
        };
        let Some(desc_obf) = utf8_owned(pool, descriptor_index) else {
            return Ok(None);
        };

        // Hierarchy walk for the member name.
        let new_name = lookup_member(mappings, index, &owner_obf, &name_obf, &desc_obf, is_field)
            .unwrap_or_else(|| name_obf.clone());
        let new_desc = mappings.remap_descriptor(&desc_obf);
        if new_name == name_obf && new_desc == desc_obf {
            return Ok(None);
        }
        let name_i = intern_utf8(pool, &new_name)?;
        let desc_i = intern_utf8(pool, &new_desc)?;
        let nat = pool
            .add(ConstantPoolEntry::NameAndType {
                name_index: name_i,
                descriptor_index: desc_i,
            })
            .ok_or_else(|| "constant pool is full".to_owned())?;
        Ok(Some(nat))
    }

    /// Remap only the descriptor half of a Dynamic/InvokeDynamic `NameAndType` (call-site names stay).
    fn remap_dynamic_nat(
        pool: &mut PoolInterner<'_>,
        mappings: &Mappings,
        nat_index: u16,
    ) -> Result<Option<u16>, String> {
        let Some(ConstantPoolEntry::NameAndType {
            name_index,
            descriptor_index,
        }) = pool.get(nat_index).cloned()
        else {
            return Ok(None);
        };
        let Some(desc_obf) = utf8_owned(pool, descriptor_index) else {
            return Ok(None);
        };
        let new_desc = mappings.remap_descriptor(&desc_obf);
        if new_desc == desc_obf {
            return Ok(None);
        }
        let desc_i = intern_utf8(pool, &new_desc)?;
        let nat = pool
            .add(ConstantPoolEntry::NameAndType {
                name_index,
                descriptor_index: desc_i,
            })
            .ok_or_else(|| "constant pool is full".to_owned())?;
        Ok(Some(nat))
    }

    fn lookup_member(
        mappings: &Mappings,
        index: &ClassIndex,
        owner_obf: &str,
        name_obf: &str,
        desc_obf: &str,
        is_field: bool,
    ) -> Option<String> {
        // Array owners (`[L…;` / `[I`) never declare members; walk Object via the mapping for Java libs.
        let owners: Vec<&str> = if owner_obf.starts_with('[') {
            vec!["java/lang/Object"]
        } else {
            index.walk_hierarchy(owner_obf)
        };
        for owner in owners {
            let Some(official_owner) = mappings.remap_class(owner) else {
                continue;
            };
            let hit = if is_field {
                mappings.remap_field(official_owner, name_obf, desc_obf)
            } else {
                mappings.remap_method(official_owner, name_obf, desc_obf)
            };
            if let Some(name) = hit {
                return Some(name.to_owned());
            }
        }
        None
    }

    /// Rename every `Class` entry in place. `Package` entries are deliberately left alone:
    /// recovering a package rename would mean walking the whole class map by common prefix, which
    /// is not worth the cost.
    fn remap_pool_class_entries(cf: &mut ClassFile, mappings: &Mappings) -> Result<(), String> {
        let mut interner = PoolInterner::new(&mut cf.constant_pool);
        let pool = &mut interner;
        let end = pool.next_index();
        for i in 1..end {
            let Some(ConstantPoolEntry::Class { name_index }) = pool.get(i).cloned() else {
                continue;
            };
            let Some(raw) = utf8_owned(pool, name_index) else {
                continue;
            };
            let new = remap_class_constant(&raw, mappings);
            if new != raw {
                let idx = intern_utf8(pool, &new)?;
                pool.replace(i, ConstantPoolEntry::Class { name_index: idx });
            }
        }
        Ok(())
    }

    /// Remap a Class-entry Utf8: either an internal binary name or an array descriptor.
    fn remap_class_constant(raw: &str, mappings: &Mappings) -> String {
        if raw.starts_with('[') {
            return mappings.remap_descriptor(raw);
        }
        mappings
            .remap_class(raw)
            .map_or_else(|| raw.to_owned(), str::to_owned)
    }

    fn remap_declarations(
        cf: &mut ClassFile,
        mappings: &Mappings,
        this_obf: &str,
        index: &ClassIndex,
    ) -> Result<(), String> {
        let official_owner = mappings
            .remap_class(this_obf)
            .map_or_else(|| this_obf.to_owned(), str::to_owned);

        let mut pool = PoolInterner::new(&mut cf.constant_pool);
        for field in &mut cf.fields {
            remap_field_decl(&mut pool, field, mappings, &official_owner)?;
        }
        for method in &mut cf.methods {
            remap_method_decl(
                &mut pool,
                method,
                mappings,
                &official_owner,
                this_obf,
                index,
            )?;
        }
        Ok(())
    }

    fn remap_field_decl(
        pool: &mut PoolInterner<'_>,
        field: &mut FieldInfo,
        mappings: &Mappings,
        official_owner: &str,
    ) -> Result<(), String> {
        let Some(name_obf) = utf8_owned(pool, field.name_index) else {
            return Ok(());
        };
        let Some(desc_obf) = utf8_owned(pool, field.descriptor_index) else {
            return Ok(());
        };
        // A field declaration takes *this* class's mapping and no other. Fields are never
        // overridden — a same-named field in a subclass hides the super's — so walking the
        // hierarchy here would rename a synthetic like `this$0` or `$VALUES` to whatever a
        // supertype happens to call a field with the same name and descriptor.
        let new_name = mappings
            .remap_field(official_owner, &name_obf, &desc_obf)
            .map_or_else(|| name_obf.clone(), str::to_owned);
        let new_desc = mappings.remap_descriptor(&desc_obf);
        if new_name != name_obf {
            field.name_index = intern_utf8(pool, &new_name)?;
        }
        if new_desc != desc_obf {
            field.descriptor_index = intern_utf8(pool, &new_desc)?;
        }
        Ok(())
    }

    fn remap_method_decl(
        pool: &mut PoolInterner<'_>,
        method: &mut MethodInfo,
        mappings: &Mappings,
        official_owner: &str,
        this_obf: &str,
        index: &ClassIndex,
    ) -> Result<(), String> {
        let Some(name_obf) = utf8_owned(pool, method.name_index) else {
            return Ok(());
        };
        let Some(desc_obf) = utf8_owned(pool, method.descriptor_index) else {
            return Ok(());
        };
        // <init> / <clinit> names never rename; descriptor still remaps.
        let new_name = if name_obf.starts_with('<') {
            name_obf.clone()
        } else {
            // This class's own mapping wins. Only fall back to a supertype's when the method could
            // actually be an override that the mappings left out — a `private` or `static` method
            // never overrides anything, so inheriting a supertype's name for one is always wrong
            // (and would collide with the member it borrowed the name from).
            let overridable = !method.access_flags.contains(MethodAccessFlags::PRIVATE)
                && !method.access_flags.is_static();
            mappings
                .remap_method(official_owner, &name_obf, &desc_obf)
                .map(str::to_owned)
                .or_else(|| {
                    overridable
                        .then(|| {
                            lookup_member(mappings, index, this_obf, &name_obf, &desc_obf, false)
                        })
                        .flatten()
                })
                .unwrap_or_else(|| name_obf.clone())
        };
        let new_desc = mappings.remap_descriptor(&desc_obf);
        if new_name != name_obf {
            method.name_index = intern_utf8(pool, &new_name)?;
        }
        if new_desc != desc_obf {
            method.descriptor_index = intern_utf8(pool, &new_desc)?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Descriptor / signature rewriting
    // ---------------------------------------------------------------------------

    // Descriptor rewriting itself lives on `Mappings`: the tiny v2 parser needs the same rewrite to
    // translate a member descriptor out of the file's first namespace before it can key an entry,
    // and one derivation with two implementations is one derivation that will eventually give two
    // answers. Signature rewriting stays here — a `Signature` attribute is a class-file structure,
    // not something a mapping table has an opinion about.

    fn remap_type_signature(ts: TypeSignature, mappings: &Mappings) -> TypeSignature {
        match ts {
            TypeSignature::Base(b) => TypeSignature::Base(b),
            TypeSignature::TypeVariable(v) => TypeSignature::TypeVariable(v),
            TypeSignature::Array(inner) => TypeSignature::Array(alloc::boxed::Box::new(
                remap_type_signature(*inner, mappings),
            )),
            TypeSignature::Class(c) => TypeSignature::Class(remap_class_type_sig(c, mappings)),
        }
    }

    fn remap_class_type_sig(mut c: ClassTypeSignature, mappings: &Mappings) -> ClassTypeSignature {
        // Reconstruct the full nested binary name (Outer$Inner$Deep), map it, then split.
        let mut full = c.name.clone();
        for s in &c.suffixes {
            full.push('$');
            full.push_str(&s.name);
        }
        if let Some(mapped) = mappings.remap_class(&full) {
            let mut parts = mapped.split('$');
            if let Some(outer) = parts.next() {
                c.name.clear();
                c.name.push_str(outer);
                let new_suffixes: Vec<String> = parts.map(str::to_owned).collect();
                // Preserve type-arg structure; rebind suffix simple names when counts match.
                if new_suffixes.len() == c.suffixes.len() {
                    for (suffix, name) in c.suffixes.iter_mut().zip(new_suffixes) {
                        suffix.name = name;
                    }
                } else if new_suffixes.is_empty() {
                    c.suffixes.clear();
                } else {
                    // Nesting depth changed: keep outer name + rebuild suffixes without type args.
                    c.suffixes = new_suffixes
                        .into_iter()
                        .map(|name| SimpleClassTypeSignature {
                            name,
                            type_arguments: Vec::new(),
                        })
                        .collect();
                }
            }
        } else if let Some(mapped) = mappings.remap_class(&c.name) {
            c.name.clear();
            c.name.push_str(mapped);
        }
        c.type_arguments = c
            .type_arguments
            .into_iter()
            .map(|a| remap_type_arg(a, mappings))
            .collect();
        for suffix in &mut c.suffixes {
            suffix.type_arguments = suffix
                .type_arguments
                .drain(..)
                .map(|a| remap_type_arg(a, mappings))
                .collect();
        }
        c
    }

    fn remap_type_arg(arg: TypeArgument, mappings: &Mappings) -> TypeArgument {
        match arg {
            TypeArgument::Any => TypeArgument::Any,
            TypeArgument::Exact(t) => TypeArgument::Exact(remap_type_signature(t, mappings)),
            TypeArgument::Extends(t) => TypeArgument::Extends(remap_type_signature(t, mappings)),
            TypeArgument::Super(t) => TypeArgument::Super(remap_type_signature(t, mappings)),
        }
    }

    fn remap_type_parameter(mut p: TypeParameter, mappings: &Mappings) -> TypeParameter {
        p.class_bound = p.class_bound.map(|b| remap_type_signature(b, mappings));
        p.interface_bounds = p
            .interface_bounds
            .into_iter()
            .map(|b| remap_type_signature(b, mappings))
            .collect();
        p
    }

    fn remap_class_signature(mut s: ClassSignature, mappings: &Mappings) -> ClassSignature {
        s.type_parameters = s
            .type_parameters
            .into_iter()
            .map(|p| remap_type_parameter(p, mappings))
            .collect();
        s.superclass = remap_class_type_sig(s.superclass, mappings);
        s.superinterfaces = s
            .superinterfaces
            .into_iter()
            .map(|i| remap_class_type_sig(i, mappings))
            .collect();
        s
    }

    fn remap_method_signature(mut s: MethodSignature, mappings: &Mappings) -> MethodSignature {
        s.type_parameters = s
            .type_parameters
            .into_iter()
            .map(|p| remap_type_parameter(p, mappings))
            .collect();
        s.parameters = s
            .parameters
            .into_iter()
            .map(|p| remap_type_signature(p, mappings))
            .collect();
        s.result = match s.result {
            jals_classfile::ResultSignature::Void => jals_classfile::ResultSignature::Void,
            jals_classfile::ResultSignature::Type(t) => {
                jals_classfile::ResultSignature::Type(remap_type_signature(t, mappings))
            }
        };
        s.throws = s
            .throws
            .into_iter()
            .map(|t| match t {
                ThrowsSignature::Class(c) => {
                    ThrowsSignature::Class(remap_class_type_sig(c, mappings))
                }
                ThrowsSignature::TypeVariable(v) => ThrowsSignature::TypeVariable(v),
            })
            .collect();
        s
    }

    // ---------------------------------------------------------------------------
    // Attribute rewriting (needs the pool)
    // ---------------------------------------------------------------------------

    /// Remap class/field/method-level attributes that carry snowflake strings or pool indices that
    /// are not Class-entry-stable: Signature, `SourceFile`, annotations, LVT, `InnerClasses` names,
    /// `EnclosingMethod` `NaT`, `MethodParameters` names, Record components.
    fn remap_attributes_with_pool(
        pool: &mut PoolInterner<'_>,
        attrs: &mut [Attribute],
        mappings: &Mappings,
        this_obf: &str,
        index: &ClassIndex,
        kind: SignatureKind,
    ) -> Result<(), String> {
        for attr in attrs {
            match &mut attr.body {
                AttributeBody::Signature { signature_index } => {
                    let Some(sig) = utf8_owned(pool, *signature_index) else {
                        continue;
                    };
                    let new = match kind {
                        SignatureKind::Class => ClassSignature::parse(&sig).map_or_else(
                            |_| sig.clone(),
                            |s| remap_class_signature(s, mappings).to_string(),
                        ),
                        SignatureKind::Field => TypeSignature::parse(&sig).map_or_else(
                            |_| sig.clone(),
                            |s| remap_type_signature(s, mappings).to_string(),
                        ),
                        SignatureKind::Method => MethodSignature::parse(&sig).map_or_else(
                            |_| sig.clone(),
                            |s| remap_method_signature(s, mappings).to_string(),
                        ),
                    };
                    if new != sig {
                        *signature_index = intern_utf8(pool, &new)?;
                    }
                }
                AttributeBody::SourceFile { sourcefile_index } => {
                    // `SourceFile` names the file the *outermost* class was declared in, so
                    // `com/example/Outer$Inner` is `Outer.java`, not `Inner.java`. Strip the
                    // package first, then take the segment before the first `$` — splitting on
                    // both at once and taking the last segment yields the innermost name.
                    if let Some(official) = mappings.remap_class(this_obf) {
                        let simple = official.rsplit('/').next().unwrap_or(official);
                        let outermost = simple.split('$').next().unwrap_or(simple);
                        let file = format!("{outermost}.java");
                        *sourcefile_index = intern_utf8(pool, &file)?;
                    }
                }
                AttributeBody::InnerClasses(entries) => {
                    for entry in entries.iter_mut() {
                        remap_inner_class_entry(pool, entry)?;
                    }
                }
                AttributeBody::EnclosingMethod {
                    class_index: _,
                    method_index,
                } => {
                    if *method_index != 0 {
                        // class_index already remapped by Class entry pass; NaT needs name+desc remap
                        // by hierarchy of the enclosing class. Look it up via the Class index.
                        // We don't have the enclosing owner here readily as obfuscated name after
                        // Class rename — so only remap the descriptor half via Dynamic Nat.
                        if let Some(nat) = remap_dynamic_nat(pool, mappings, *method_index)? {
                            *method_index = nat;
                        }
                        // Note: the method NAME inside the enclosing NaT is intentionally left alone
                        // when mapping misses; a full owner-aware pass would need the pre-rename
                        // Class name which is already lost. Acceptable for this pass.
                    }
                }
                AttributeBody::LocalVariableTable(entries) => {
                    for entry in entries.iter_mut() {
                        let Some(desc) = utf8_owned(pool, entry.descriptor_index) else {
                            continue;
                        };
                        let new = mappings.remap_descriptor(&desc);
                        if new != desc {
                            entry.descriptor_index = intern_utf8(pool, &new)?;
                        }
                    }
                }
                AttributeBody::LocalVariableTypeTable(entries) => {
                    for entry in entries.iter_mut() {
                        let Some(sig) = utf8_owned(pool, entry.signature_index) else {
                            continue;
                        };
                        let new = TypeSignature::parse(&sig).map_or_else(
                            |_| sig.clone(),
                            |s| remap_type_signature(s, mappings).to_string(),
                        );
                        if new != sig {
                            entry.signature_index = intern_utf8(pool, &new)?;
                        }
                    }
                }
                AttributeBody::RuntimeVisibleAnnotations(annos)
                | AttributeBody::RuntimeInvisibleAnnotations(annos) => {
                    for a in annos.iter_mut() {
                        remap_annotation(pool, a, mappings, index)?;
                    }
                }
                AttributeBody::RuntimeVisibleParameterAnnotations(params)
                | AttributeBody::RuntimeInvisibleParameterAnnotations(params) => {
                    for list in params.iter_mut() {
                        for a in list.iter_mut() {
                            remap_annotation(pool, a, mappings, index)?;
                        }
                    }
                }
                AttributeBody::RuntimeVisibleTypeAnnotations(annos)
                | AttributeBody::RuntimeInvisibleTypeAnnotations(annos) => {
                    for a in annos.iter_mut() {
                        remap_type_annotation(pool, a, mappings, index)?;
                    }
                }
                AttributeBody::AnnotationDefault(value) => {
                    remap_element_value(pool, value, mappings, index)?;
                }
                AttributeBody::Record(components) => {
                    for c in components.iter_mut() {
                        remap_record_component(pool, c, mappings, this_obf, index)?;
                    }
                }
                AttributeBody::Code(code) => {
                    remap_attributes_with_pool(
                        pool,
                        &mut code.attributes,
                        mappings,
                        this_obf,
                        index,
                        SignatureKind::Field,
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Resync an `InnerClasses` entry's simple name with its Class entry, which the Class-entry
    /// pass has already renamed. No `Mappings` lookup is needed: the official name is the one the
    /// pool now holds (or the unchanged obfuscated name on a mapping miss).
    fn remap_inner_class_entry(
        pool: &mut PoolInterner<'_>,
        entry: &mut InnerClassEntry,
    ) -> Result<(), String> {
        if entry.inner_name_index == 0 {
            return Ok(());
        }
        // The simple name of the inner class is the last '$' segment of its binary name.
        let Some(simple) = pool
            .class_name(entry.inner_class_info_index)
            .map(|name| name.rsplit(['/', '$']).next().unwrap_or(&name).to_owned())
        else {
            return Ok(());
        };
        let Some(current) = utf8_owned(pool, entry.inner_name_index) else {
            return Ok(());
        };
        if current != simple {
            entry.inner_name_index = intern_utf8(pool, &simple)?;
        }
        Ok(())
    }

    fn remap_annotation(
        pool: &mut PoolInterner<'_>,
        anno: &mut Annotation,
        mappings: &Mappings,
        index: &ClassIndex,
    ) -> Result<(), String> {
        // type_index holds a field descriptor of the annotation type.
        let Some(desc) = utf8_owned(pool, anno.type_index) else {
            return Ok(());
        };
        let new_desc = mappings.remap_descriptor(&desc);
        if new_desc != desc {
            anno.type_index = intern_utf8(pool, &new_desc)?;
        }
        let owner_official = FieldType::parse(&new_desc).ok().and_then(|ft| match ft {
            FieldType::Object(n) => Some(n),
            _ => None,
        });
        for pair in &mut anno.element_value_pairs {
            if let Some(owner) = owner_official.as_deref()
                && let Some(name_obf) = utf8_owned(pool, pair.element_name_index)
                && let Some(official) = mappings.remap_method_by_name(owner, &name_obf)
                && official != name_obf
            {
                pair.element_name_index = intern_utf8(pool, official)?;
            }
            remap_element_value(pool, &mut pair.value, mappings, index)?;
        }
        Ok(())
    }

    fn remap_type_annotation(
        pool: &mut PoolInterner<'_>,
        anno: &mut TypeAnnotation,
        mappings: &Mappings,
        index: &ClassIndex,
    ) -> Result<(), String> {
        // Same shape as Annotation for the type/elements half.
        let mut plain = Annotation {
            type_index: anno.type_index,
            element_value_pairs: core::mem::take(&mut anno.element_value_pairs),
        };
        remap_annotation(pool, &mut plain, mappings, index)?;
        anno.type_index = plain.type_index;
        anno.element_value_pairs = plain.element_value_pairs;
        Ok(())
    }

    fn remap_element_value(
        pool: &mut PoolInterner<'_>,
        value: &mut ElementValue,
        mappings: &Mappings,
        index: &ClassIndex,
    ) -> Result<(), String> {
        match value {
            ElementValue::Const { .. } => {}
            ElementValue::Enum {
                type_name_index,
                const_name_index,
            } => {
                let Some(desc) = utf8_owned(pool, *type_name_index) else {
                    return Ok(());
                };
                let new_desc = mappings.remap_descriptor(&desc);
                if new_desc != desc {
                    *type_name_index = intern_utf8(pool, &new_desc)?;
                }
                if let Ok(FieldType::Object(owner)) = FieldType::parse(&new_desc)
                    && let Some(name_obf) = utf8_owned(pool, *const_name_index)
                    && let Some(official) = mappings.remap_field_by_name(&owner, &name_obf)
                    && official != name_obf
                {
                    *const_name_index = intern_utf8(pool, official)?;
                }
            }
            ElementValue::Class { class_info_index } => {
                let Some(desc) = utf8_owned(pool, *class_info_index) else {
                    return Ok(());
                };
                // Return-descriptor form (`Ljava/lang/String;` or `V` etc.).
                let new = if desc == "V" {
                    desc.clone()
                } else {
                    mappings.remap_descriptor(&desc)
                };
                if new != desc {
                    *class_info_index = intern_utf8(pool, &new)?;
                }
            }
            ElementValue::Annotation(a) => remap_annotation(pool, a, mappings, index)?,
            ElementValue::Array(items) => {
                for item in items.iter_mut() {
                    remap_element_value(pool, item, mappings, index)?;
                }
            }
        }
        Ok(())
    }

    fn remap_record_component(
        pool: &mut PoolInterner<'_>,
        component: &mut RecordComponentInfo,
        mappings: &Mappings,
        this_obf: &str,
        index: &ClassIndex,
    ) -> Result<(), String> {
        let Some(name_obf) = utf8_owned(pool, component.name_index) else {
            return Ok(());
        };
        let Some(desc_obf) = utf8_owned(pool, component.descriptor_index) else {
            return Ok(());
        };
        let new_name = lookup_member(mappings, index, this_obf, &name_obf, &desc_obf, true)
            .unwrap_or_else(|| name_obf.clone());
        let new_desc = mappings.remap_descriptor(&desc_obf);
        if new_name != name_obf {
            component.name_index = intern_utf8(pool, &new_name)?;
        }
        if new_desc != desc_obf {
            component.descriptor_index = intern_utf8(pool, &new_desc)?;
        }
        remap_attributes_with_pool(
            pool,
            &mut component.attributes,
            mappings,
            this_obf,
            index,
            SignatureKind::Field,
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // Interning
    // ---------------------------------------------------------------------------

    /// The owned text of the `Utf8` entry at `index`, or `None` when it is absent or not a `Utf8`.
    /// Every caller here needs an owned copy so the pool can be mutated while the text is in hand.
    fn utf8_owned(pool: &ConstantPool, index: u16) -> Option<String> {
        pool.utf8(index).map(alloc::borrow::Cow::into_owned)
    }

    /// Intern `s` as a `Utf8` entry, reusing an existing one when the text already appears.
    ///
    /// Two things this must not do naively. It must encode *modified* UTF-8: writing standard
    /// UTF-8 corrupts any name containing NUL or a supplementary character, because the decoder on
    /// the other side reads the six-byte surrogate form. And it must deduplicate: remapping
    /// interns a name and a descriptor for every renamed reference, so blindly appending grew the
    /// pool by roughly three slots per changed member reference — recreating the same shared
    /// `NameAndType` once per referrer. The pool caps at 65535 slots, so on a class with a few
    /// thousand member references that growth made remapping fail outright.
    fn intern_utf8(pool: &mut PoolInterner<'_>, s: &str) -> Result<u16, String> {
        pool.intern(s)
    }
}

/// A class's constant pool plus an index of its `Utf8` entries, so interning reuses them.
struct PoolInterner<'a> {
    pool: &'a mut ConstantPool,
    utf8: BTreeMap<Vec<u8>, u16>,
}

impl<'a> PoolInterner<'a> {
    fn new(pool: &'a mut ConstantPool) -> Self {
        let mut utf8 = BTreeMap::new();
        for index in 0..pool.next_index() {
            if let Some(ConstantPoolEntry::Utf8(bytes)) = pool.get(index) {
                // First wins: earlier indices are the ones existing references already use.
                utf8.entry(bytes.clone()).or_insert(index);
            }
        }
        Self { pool, utf8 }
    }

    fn intern(&mut self, text: &str) -> Result<u16, String> {
        let bytes = ConstantPool::encode_modified_utf8(text);
        if let Some(&index) = self.utf8.get(&bytes) {
            return Ok(index);
        }
        let index = self
            .pool
            .add(ConstantPoolEntry::Utf8(bytes.clone()))
            .ok_or_else(|| {
                format!(
                    "constant pool is full; cannot intern a {}-byte name",
                    bytes.len()
                )
            })?;
        self.utf8.insert(bytes, index);
        Ok(index)
    }
}

impl core::ops::Deref for PoolInterner<'_> {
    type Target = ConstantPool;

    fn deref(&self) -> &Self::Target {
        self.pool
    }
}

impl core::ops::DerefMut for PoolInterner<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.pool
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::helpers::remap_manifest;
    use crate::mappings::Mappings;
    use crate::{MappingFormat, RemapDirection};

    fn mappings(text: &str) -> Mappings {
        Mappings::parse(text, &MappingFormat::Proguard, RemapDirection::Deobfuscate)
            .expect("parses")
    }

    /// What a remap does to a manifest is one mapping decision and two edits, and the decision is
    /// the only half this module owns.
    ///
    /// The entry point is renamed because every class in the jar was, and the per-entry digests go
    /// because they describe bytes that no longer exist — a JVM refuses an archive whose signature
    /// claims do not check out, which is why a remapped Minecraft jar used to compile against but
    /// never *run*.
    #[test]
    fn a_remapped_manifest_is_renamed_and_unsigned() {
        let manifest = "Manifest-Version: 1.0\r\n\
             Main-Class: a.b.C\r\n\
             \r\n\
             Name: a/b/C.class\r\n\
             SHA-256-Digest: Zm9v\r\n\
             \r\n";
        let renamed = remap_manifest(
            manifest.as_bytes(),
            &mappings("com.example.Main -> a.b.C:\n"),
        );
        assert_eq!(
            String::from_utf8(renamed).unwrap(),
            "Manifest-Version: 1.0\r\nMain-Class: com.example.Main\r\n\r\n"
        );
    }

    /// A mapping that says nothing about the entry point leaves it alone, and an unsigned manifest
    /// with no entry point at all comes back byte for byte: a remap edits a manifest, it does not
    /// rewrite one.
    #[test]
    fn a_manifest_with_nothing_to_rename_or_strip_passes_through() {
        let unrelated = mappings("com.example.Main -> x.Y:\n");
        for manifest in [
            "Manifest-Version: 1.0\r\nMain-Class: a.b.C\r\n\r\n",
            "Manifest-Version: 1.0\r\n\r\n",
            // Not text at all: a member this crate cannot read is one it must not replace with an
            // empty one.
            "\u{0}\u{1}not a manifest",
        ] {
            assert_eq!(
                remap_manifest(manifest.as_bytes(), &unrelated),
                manifest.as_bytes()
            );
        }
    }
}
