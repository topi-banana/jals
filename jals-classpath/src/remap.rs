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

use alloc::borrow::{Cow, ToOwned};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use jals_classfile::{
    Annotation, Attribute, AttributeBody, ClassFile, ClassSignature, ClassTypeSignature,
    ConstantPool, ConstantPoolEntry, ElementValue, FieldInfo, FieldType, InnerClassEntry,
    MethodAccessFlags, MethodInfo, MethodSignature, RecordComponentInfo, SimpleClassTypeSignature,
    ThrowsSignature, TypeAnnotation, TypeArgument, TypeParameter, TypeSignature,
};
use jals_exec::Exec;
use jals_storage::{
    ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest, ProvenanceFold,
};

use crate::load::{Archive, SourceTreeLimits};
use crate::mappings::Mappings;
use crate::zip::{StoredZip, WriteMember};
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
/// The inputs a remap already folds — the jar, the mapping text, the format, the direction, the
/// hierarchy — say what went in and nothing about what this crate does with it, so a change to the
/// transform itself would be served the previous transform's jar out of the cache. Bump this
/// whenever the bytes written for an otherwise unchanged input change.
///
/// 2: everything the `META-INF/` pass has done to a remapped jar. The signature block goes and the
/// manifest's per-entry digests go with it — which is the change this constant should have moved
/// for when that landed, and did not: `jals-project`'s `TASK_EXECUTION_VERSION` gates the task
/// above this and not the lookup below, so a warm cache kept handing back the signed jar a JVM
/// refuses. And, with it: a `META-INF/SIG-*` member whose extension is not the JDK's one to three
/// alphanumerics is a resource and is kept, a rewritten `Main-Class` carries its own line's
/// terminator rather than an LF, and a manifest with no digest in it comes back byte-identical
/// rather than re-terminated and re-closed.
///
/// 3: `META-INF/` is matched however the archive spells it, as the JVM matches it, so a
/// `meta-inf/`-spelled signature block goes and a `meta-inf/`-spelled manifest is stripped instead
/// of the pass leaving half a claim standing. And a `Main-Class` folded onto continuation lines —
/// which is every entry point whose name runs past the manifest's 72-byte cap, `jar.rs`'s own
/// output included — is read as one attribute and written back folded, rather than being missed
/// and left naming a class the remap has since renamed.
const REMAP_OUTPUT_VERSION: u32 = 3;

/// The same, for what a merge writes.
///
/// 2: a merged manifest carries `Multi-Release` when either input's did.
///
/// 3: the two sides' manifests are one conflict however either spells the name, the survivor is
/// written first, and the digest strip leaves a manifest that has no digests in it alone.
///
/// 4: the `META-INF/` component is matched case-insensitively, so a `meta-inf/`-spelled manifest is
/// the manifest for the conflict, the `Multi-Release` read and the digest strip alike.
const MERGE_OUTPUT_VERSION: u32 = 4;

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
    pub async fn remap<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
        request: &RemapRequest<'_>,
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
        let outcomes = exec
            .fan_out(inputs, |(position, mut cf, mappings, index)| async move {
                helpers::remap_class(&mut cf, &mappings, &index)
                    .map(|()| {
                        let this = cf.constant_pool.class_name(cf.this_class).map_or_else(
                            || format!("unknown{position}"),
                            alloc::borrow::Cow::into_owned,
                        );
                        let member_name = format!("{this}.class");
                        (position, member_name, cf.write())
                    })
                    .map_err(|error| (position, error))
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
            if helpers::is_signature_member(&name) {
                continue;
            }
            let (name, bytes) = if let Some((member_name, remapped_bytes)) =
                remapped.remove(&position)
            {
                // A multi-release jar stores the same class twice, once under
                // `META-INF/versions/<n>/`. Both have the same `this_class`, so naming the output
                // purely from it collides and fails the whole remap. Keep the versioned prefix.
                let prefix = helpers::multi_release_prefix(&name);
                (format!("{prefix}{member_name}"), remapped_bytes)
            } else {
                let mut bytes = outcome
                    .map_err(|error| format!("failed to read archive member `{name}`: {error}"))?;
                if helpers::is_manifest_member(&name) {
                    bytes = helpers::rewrite_manifest_main_class(&bytes, &mappings);
                    bytes = helpers::strip_manifest_digests(&bytes);
                }
                (name, bytes)
            };
            if !used_names.insert(name.clone()) {
                return Err(format!("duplicate remapped archive member `{name}`"));
            }
            out_members.push(WriteMember { name, bytes });
        }
        let jar_bytes = StoredZip::write(&out_members)?;

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
    /// collision a case-insensitive predicate recognises is not one an exact-keyed map would — and
    /// whichever survives is written **first**, the rule `jar.rs` states for the jars it writes and
    /// that this second writer over the same zip writer has to keep too. Neither claim reaches
    /// further than that: a single input carrying two manifests of its own is two members here as
    /// it was there, since deduplicating *within* a side would be this function inventing a
    /// conflict its inputs did not have.
    pub async fn merge<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        base: &CacheKey,
        overlay: &CacheKey,
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
        // by the base's spelling: `is_manifest_member` matches case-insensitively on purpose, so a
        // base `META-INF/MANIFEST.MF` and an overlay `META-INF/manifest.mf` would never collide in
        // a map keyed by the exact name, and the union would carry two manifests — with the base's
        // winning, which is the documented conflict rule backwards.
        let mut overlay_manifest: Option<String> = None;
        for (name, outcome) in overlay_members {
            if helpers::is_signature_member(&name) {
                continue;
            }
            let mut bytes = outcome
                .map_err(|error| format!("failed to read overlay member `{name}`: {error}"))?;
            if helpers::is_manifest_member(&name) {
                bytes = helpers::strip_manifest_digests(&bytes);
                multi_release |= helpers::declares_multi_release(&bytes);
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
            if helpers::is_signature_member(&name) {
                continue;
            }
            let is_manifest = helpers::is_manifest_member(&name);
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
                    multi_release |= helpers::declares_multi_release(&shadowed);
                }
                overlay_bytes
            } else {
                let bytes = outcome
                    .map_err(|error| format!("failed to read base member `{name}`: {error}"))?;
                if is_manifest {
                    helpers::strip_manifest_digests(&bytes)
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
        // The manifest first, whatever order the walk left it in — the rule `jar.rs` states for
        // the jars it writes, and the second writer over `StoredZip` has to keep it too. A base
        // with no manifest takes the overlay's out of the tail loop above, which would put it
        // last, and `JarInputStream::getManifest` reads none but the first: a streaming reader
        // would then see no `Multi-Release`, which is the one attribute this merge adds.
        if let Some(position) = out_members
            .iter()
            .position(|member| helpers::is_manifest_member(&member.name))
            && position != 0
        {
            out_members[..=position].rotate_right(1);
        }
        // Applied after the union is assembled rather than while it is: which manifest survives is
        // decided by the walk above, and this has to reach whichever one did — which the hoist has
        // just put first, so there is no second scan and no second manifest to disagree with.
        if multi_release
            && let Some(manifest) = out_members
                .first_mut()
                .filter(|member| helpers::is_manifest_member(&member.name))
        {
            manifest.bytes = helpers::with_multi_release(&manifest.bytes);
        }

        let jar_bytes = StoredZip::write(&out_members)?;
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
    use super::*;

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

    /// What `name` spells directly under `META-INF/`, or `None` when it is not under it at all.
    ///
    /// The directory component is compared case-insensitively, exactly as the basenames below are
    /// and for the same reason: the JVM matches it that way. `JarFile::getManEntry` falls back to
    /// an `equalsIgnoreCase` sweep of the `META-INF/` names, and
    /// `SignatureFileVerifier::isSigningRelated` upper-cases the whole entry name before testing
    /// it — so in an archive that spells the directory `meta-inf/`, the JVM still reads the
    /// manifest and still verifies the block. Matching only the specification's spelling here left
    /// precisely the half-a-claim archive this pass exists to prevent: the `.RSA` copied through
    /// because it was not recognised, and the manifest's per-entry digests never stripped for the
    /// same reason, over classes every one of which had just been rewritten.
    fn under_meta_inf(name: &str) -> Option<&str> {
        const META_INF: &str = "META-INF/";
        let (prefix, base) = name.split_at_checked(META_INF.len())?;
        prefix.eq_ignore_ascii_case(META_INF).then_some(base)
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
    /// stripped below — the half-a-claim state the module doc calls worse than keeping neither —
    /// and a member this matches that the JVM would not is an ordinary resource deleted from a jar
    /// that still needs it. So the `SIG-` extension rule is the JDK's: absent, or one to three
    /// ASCII alphanumerics. `META-INF/SIG-config.json` is a resource, and `META-INF/SIG-Foo.class`
    /// is a class — which this predicate is asked about *before* the remapped bytes are collected,
    /// so matching it would have thrown away a class this pass had already rewritten.
    pub(super) fn is_signature_member(name: &str) -> bool {
        let Some(base) = under_meta_inf(name) else {
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
            .any(|extension| has_extension(base, extension))
    }

    /// Whether `name` is the jar manifest: `META-INF/MANIFEST.MF`, at that one depth.
    ///
    /// Case-insensitively in both components (see [`under_meta_inf`]), for the same reason
    /// [`is_signature_member`] is — a jar tool is free not to write the specification's spelling,
    /// and a JVM finds the manifest either way. Matching this one exactly while matching the
    /// signature block loosely is the asymmetry that would drop a jar's `.SF` and keep the manifest
    /// digests saying the same thing about it.
    pub(super) fn is_manifest_member(name: &str) -> bool {
        under_meta_inf(name).is_some_and(|base| base.eq_ignore_ascii_case("MANIFEST.MF"))
    }

    /// Whether a manifest attribute line declares a digest of something.
    ///
    /// Matched on the substring rather than on a fixed list, because the algorithm is part of the
    /// name (`SHA-256-Digest`, `SHA1-Digest`) and a signer may add `-Digest-Manifest` spellings of
    /// its own. On `digest` rather than on `-digest`, because the specification's legacy
    /// `Digest-Algorithms` puts the word first: matching only the hyphenated form would leave a
    /// section saying which algorithms were used and carrying none of them — and, since that line
    /// is not a `Name:`, would keep the whole section alive around it.
    fn is_digest_attribute(line: &str) -> bool {
        const DIGEST: &[u8] = b"digest";
        line.split_once(':').is_some_and(|(name, _)| {
            // Matched without allocating: a signed client jar's manifest holds one section per
            // member, so this runs once per line over megabytes of text.
            name.trim()
                .as_bytes()
                .windows(DIGEST.len())
                .any(|window| window.eq_ignore_ascii_case(DIGEST))
        })
    }

    /// One manifest section with its digest attributes removed, or `None` when nothing but its
    /// `Name:` would be left.
    ///
    /// A continuation line — one that opens with a space — belongs to the attribute above it, so an
    /// attribute is dropped together with its continuations rather than leaving them behind as
    /// syntax nobody can parse.
    fn section_without_digests<'a>(lines: &[&'a str]) -> Option<Vec<&'a str>> {
        let mut kept: Vec<&str> = Vec::new();
        let mut dropping = false;
        for line in lines {
            if line.starts_with(' ') {
                if !dropping {
                    kept.push(line);
                }
                continue;
            }
            dropping = is_digest_attribute(line);
            if !dropping {
                kept.push(line);
            }
        }
        let names_only = kept.iter().all(|line| {
            line.starts_with(' ')
                || line
                    .split_once(':')
                    .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("Name"))
        });
        // `names_only` alone: `all` over an empty `kept` is already `true`, so a section that lost
        // every line is the same answer by the same test.
        if names_only { None } else { Some(kept) }
    }

    /// Drop the per-entry digests a signer wrote into a manifest.
    ///
    /// Removing `META-INF/*.SF` alone is not enough. A signed jar states a digest for every member
    /// in an individual section of `MANIFEST.MF` and the signature file states a digest of those
    /// sections in turn, so a remapped jar that keeps the manifest half carries megabytes of claims
    /// about bytes that no longer exist — and hands them to whoever signs the jar next.
    ///
    /// Only the digests go. An individual section that says something else about its member keeps
    /// saying it, and the main section survives whole in practice, because what a signer writes
    /// there — `Manifest-Version`, `Created-By`, and beside them `Main-Class` and `Multi-Release` —
    /// names no digest; the per-file digests are the individual sections, and the digest *of* those
    /// sections lives in `META-INF/*.SF`, which goes as a member. The rule below is applied to
    /// every section alike rather than to all but the first, so a main-section attribute that did
    /// name a digest would go too.
    pub(super) fn strip_manifest_digests(bytes: &[u8]) -> Vec<u8> {
        let Ok(text) = core::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        // Nothing to strip is the common case — every unsigned jar, and the jars this crate writes
        // itself — and the rebuild below is not the identity on one: it re-terminates every line
        // with the detected terminator and closes every section, so a manifest with no digest in it
        // would come back changed in ways nothing asked for. Answered first, over the same lines
        // and the same predicate the pass below uses, so the two cannot disagree about what a
        // digest is.
        let has_digest = text.split('\n').any(|raw| {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            !line.starts_with(' ') && is_digest_attribute(line)
        });
        if !has_digest {
            return bytes.to_vec();
        }
        // A manifest is written with CRLF. Detected rather than assumed, so a hand-written one that
        // is not stays the way its author left it.
        let terminator = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut kept: Vec<Vec<&str>> = Vec::new();
        let mut section: Vec<&str> = Vec::new();
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                kept.extend(section_without_digests(&section));
                section.clear();
                continue;
            }
            section.push(line);
        }
        kept.extend(section_without_digests(&section));

        let mut out = String::with_capacity(text.len());
        for section in &kept {
            for line in section {
                out.push_str(line);
                out.push_str(terminator);
            }
            // The specification terminates every section with an empty line, the last one included.
            out.push_str(terminator);
        }
        out.into_bytes()
    }

    /// Whether a manifest's main section declares `Multi-Release: true`.
    ///
    /// Only the main section is read, because that is the only place the attribute means anything:
    /// the JVM consults `META-INF/versions/<n>/` for an archive whose *main* attributes say so, and
    /// an individual section saying it says something about one member instead.
    pub(super) fn declares_multi_release(bytes: &[u8]) -> bool {
        let Ok(text) = core::str::from_utf8(bytes) else {
            return false;
        };
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                // The main section ended; whatever follows is about a member.
                return false;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("Multi-Release")
                && value.trim().eq_ignore_ascii_case("true")
            {
                return true;
            }
        }
        false
    }

    /// The same manifest with `Multi-Release: true` in its main section.
    ///
    /// Appended to the end of the main section rather than written in place, because the attribute
    /// may be absent, present as `false`, or present as `true` already — and an append after the
    /// removal below reads the same in all three cases. Order within a section carries no meaning
    /// beyond `Manifest-Version` coming first, which this never displaces.
    pub(super) fn with_multi_release(bytes: &[u8]) -> Vec<u8> {
        let Ok(text) = core::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        let terminator = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut lines: Vec<&str> = text
            .split('\n')
            .map(|raw| raw.strip_suffix('\r').unwrap_or(raw))
            .collect();
        // Every line below is written back with a terminator, so the empty string a trailing
        // terminator leaves behind would become a section break the input did not have.
        if lines.last() == Some(&"") {
            lines.pop();
        }
        let mut out = String::with_capacity(text.len() + 24);
        let mut in_main = true;
        for line in lines {
            if in_main && line.is_empty() {
                out.push_str("Multi-Release: true");
                out.push_str(terminator);
                in_main = false;
            }
            if in_main
                && line
                    .split_once(':')
                    .is_some_and(|(name, _)| name.eq_ignore_ascii_case("Multi-Release"))
            {
                continue;
            }
            out.push_str(line);
            out.push_str(terminator);
        }
        if in_main {
            // A manifest whose main section was never terminated. Close it, rather than leaving the
            // attribute in a section the JVM would read as being about a member.
            out.push_str("Multi-Release: true");
            out.push_str(terminator);
            out.push_str(terminator);
        }
        out.into_bytes()
    }

    /// One physical manifest line without its terminator.
    fn without_terminator(line: &str) -> &str {
        line.trim_end_matches(['\r', '\n'])
    }

    /// The terminator a physical manifest line carried — empty for a final line the archive left
    /// unterminated.
    fn terminator_of(line: &str) -> &str {
        &line[without_terminator(line).len()..]
    }

    /// Append `Main-Class: {value}` to `out`, folded onto continuation lines the way
    /// [`crate::jar::JarPackage`] writes an attribute: every physical line, terminator included,
    /// stays within [`MAX_LINE`] bytes and a continuation opens with exactly one space. Emitting it
    /// as one long line instead would answer a manifest this crate could read with one it could
    /// not, and an over-long line is an invalid manifest rather than a cosmetic matter.
    ///
    /// The manifest's own terminators, not the writer's CRLF: this pass edits an archive somebody
    /// else may have written, and putting one CRLF line into an LF manifest leaves a file whose
    /// terminators disagree. `fold` breaks the intermediate lines and `end` closes the last one, so
    /// a final attribute the archive left unterminated stays that way.
    fn write_main_class(out: &mut String, value: &str, fold: &str, end: &str) {
        let mut attribute = String::with_capacity("Main-Class: ".len() + value.len());
        let _ = write!(attribute, "Main-Class: {value}");
        let mut rest = attribute.as_str();
        // The first physical line spends its whole budget on content; a continuation gives one byte
        // back to the leading space that marks it as one.
        let mut budget = crate::jar::MAX_LINE.saturating_sub(fold.len());
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
            budget = crate::jar::MAX_LINE.saturating_sub(fold.len() + 1);
        }
    }

    /// Rewrite `Main-Class:` in a manifest body when the target maps under `mappings`.
    ///
    /// Read from the *logical* line rather than the physical one. A manifest attribute may be
    /// folded across continuation lines, each opening with a single space, and
    /// [`crate::jar::JarPackage`] is what folds a long `Main-Class` in the first place — its cap is
    /// 72 bytes including the terminator, so any entry point whose name runs past 58 characters
    /// arrives here wrapped. A reader matching physical lines therefore missed exactly the jars
    /// this crate writes: the reobfuscated archive kept the deobfuscated entry-point name and
    /// `java -jar` answered it with `ClassNotFoundException`.
    pub(super) fn rewrite_manifest_main_class(bytes: &[u8], mappings: &Mappings) -> Vec<u8> {
        let Ok(text) = core::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        let mut out = String::with_capacity(text.len());
        let mut physical = text.split_inclusive('\n').peekable();
        while let Some(first) = physical.next() {
            let mut folded: Vec<&str> = vec![first];
            while physical.peek().is_some_and(|next| next.starts_with(' ')) {
                folded.push(physical.next().expect("peeked"));
            }
            // Borrowed for the overwhelming majority — one physical line — so a manifest with a
            // section per member is not one allocation per line to find one attribute.
            let logical: Cow<'_, str> = if folded.len() == 1 {
                Cow::Borrowed(without_terminator(first))
            } else {
                let mut joined = String::new();
                for (index, line) in folded.iter().enumerate() {
                    let body = without_terminator(line);
                    // The one space that marks a continuation is syntax, not value. It is ASCII, so
                    // byte 1 is always a character boundary.
                    joined.push_str(if index == 0 { body } else { &body[1..] });
                }
                Cow::Owned(joined)
            };
            if let Some(value) = logical.strip_prefix("Main-Class:") {
                let internal = value.trim().replace('.', "/");
                if let Some(official) = mappings.remap_class(&internal) {
                    let rewritten = official.replace('/', ".");
                    // An unterminated attribute is the file's last line and cannot have been
                    // folded, so the fallback below is only ever reached when nothing is folded.
                    let fold = match terminator_of(first) {
                        "" => "\r\n",
                        terminator => terminator,
                    };
                    let end = terminator_of(folded[folded.len() - 1]);
                    write_main_class(&mut out, &rewritten, fold, end);
                    continue;
                }
            }
            for line in folded {
                out.push_str(line);
            }
        }
        out.into_bytes()
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
    /// The `META-INF/versions/<n>/` prefix of a multi-release archive member, or `""`.
    pub(super) fn multi_release_prefix(name: &str) -> &str {
        const ROOT: &str = "META-INF/versions/";
        let Some(rest) = name.strip_prefix(ROOT) else {
            return "";
        };
        rest.find('/')
            .map_or("", |end| &name[..=(ROOT.len() + end)])
    }

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
    use alloc::vec::Vec;

    use super::helpers::{
        declares_multi_release, is_manifest_member, is_signature_member, multi_release_prefix,
        rewrite_manifest_main_class, strip_manifest_digests, with_multi_release,
    };
    use crate::mappings::Mappings;
    use crate::{MappingFormat, RemapDirection};

    /// A manifest's logical attributes: continuation lines joined back onto the line above, each
    /// paired with the widest physical line it was written across.
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

    /// A `Main-Class` too long for one manifest line is one attribute in both directions.
    ///
    /// `JarPackage::write_attribute` caps a physical line at 72 bytes including its terminator, so
    /// every entry point whose name runs past 58 characters reaches the remap already folded — and
    /// this crate is what wrote it that way. A pass that matched physical lines read no
    /// `Main-Class` at all there, and the reobfuscated jar shipped naming a class the remap had
    /// just renamed away.
    #[test]
    fn a_main_class_too_long_for_one_line_is_read_and_written_as_one_attribute() {
        const LONG: &str = "com.example.application.launcher.VeryLongApplicationEntryPoint";
        assert!(
            "Main-Class: ".len() + LONG.len() > super::super::jar::MAX_LINE,
            "the fixture has to be long enough to need folding"
        );
        let widened = Mappings::parse(
            &alloc::format!("{LONG} -> a:\n"),
            &MappingFormat::Proguard,
            RemapDirection::Deobfuscate,
        )
        .expect("parses");
        let narrowed = Mappings::parse(
            &alloc::format!("com.example.Short -> {LONG}:\n"),
            &MappingFormat::Proguard,
            RemapDirection::Deobfuscate,
        )
        .expect("parses");

        // Writing: a short name maps to a long one, and the result is folded rather than emitted as
        // one over-long line, which is not a legal manifest.
        let written = rewrite_manifest_main_class(
            b"Manifest-Version: 1.0\r\nMain-Class: a\r\n\r\n",
            &widened,
        );
        let out = attributes(&written);
        assert!(
            out.contains(&(alloc::format!("Main-Class: {LONG}"), 72)),
            "the long name is one attribute folded onto 72-byte lines: {out:?}"
        );
        assert!(
            out.iter()
                .all(|(_, widest)| *widest <= super::super::jar::MAX_LINE),
            "no physical line may exceed the cap: {out:?}"
        );

        // Reading: the folded form this crate writes is what a reobfuscating remap is handed back.
        let read = rewrite_manifest_main_class(&written, &narrowed);
        assert!(
            attributes(&read).contains(&(String::from("Main-Class: com.example.Short"), 31)),
            "a folded `Main-Class` is the attribute the mapping is applied to"
        );
        // Everything else survives, terminators included.
        assert!(
            core::str::from_utf8(&read)
                .expect("utf-8")
                .starts_with("Manifest-Version: 1.0\r\n"),
            "the rest of the manifest is untouched"
        );
    }

    /// A multi-release jar stores the same class twice — once at its plain path and once under
    /// `META-INF/versions/<n>/` — and both copies share a `this_class`. Naming the remapped output
    /// from `this_class` alone collides, which failed the remap of the whole archive.
    #[test]
    fn multi_release_members_keep_their_version_prefix() {
        assert_eq!(
            multi_release_prefix("META-INF/versions/11/foo/Bar.class"),
            "META-INF/versions/11/"
        );
        assert_eq!(
            multi_release_prefix("META-INF/versions/9/Baz.class"),
            "META-INF/versions/9/"
        );
        assert_eq!(multi_release_prefix("foo/Bar.class"), "");
        assert_eq!(multi_release_prefix("META-INF/MANIFEST.MF"), "");
        // A truncated prefix names no version directory, so there is nothing to preserve.
        assert_eq!(multi_release_prefix("META-INF/versions/11"), "");
    }

    /// The block a JVM reads to decide a jar is signed, and nothing that merely looks like it.
    #[test]
    fn a_signature_block_is_recognised_only_directly_under_meta_inf() {
        assert!(is_signature_member("META-INF/MOJANGCS.SF"));
        assert!(is_signature_member("META-INF/MOJANGCS.RSA"));
        assert!(is_signature_member("META-INF/SIGNER.DSA"));
        assert!(is_signature_member("META-INF/SIGNER.EC"));
        // The specification writes them upper-case; a jar tool is free not to.
        assert!(is_signature_member("META-INF/signer.sf"));

        // The JDK's own predicate reads this spelling as signing-related too, extension or not.
        assert!(is_signature_member("META-INF/SIG-BC"));
        assert!(is_signature_member("META-INF/sig-bc.rsa"));

        // …but only with the extension the JDK accepts: absent, or one to three alphanumerics.
        // Matching more than the JVM does deletes a member the archive still needs — and for a
        // `.class` it deletes one this pass had already remapped, since the predicate is asked
        // before the remapped bytes are collected.
        assert!(!is_signature_member("META-INF/SIG-config.json"));
        assert!(!is_signature_member("META-INF/SIG-Foo.class"));
        assert!(!is_signature_member("META-INF/SIG-x.a_b"));

        assert!(!is_signature_member("META-INF/MANIFEST.MF"));
        assert!(!is_signature_member("META-INF/SIGNATURES.TXT"));
        // A member name is whatever the archive says; a prefix test over bytes must not panic on
        // one whose fourth byte falls inside a character.
        assert!(!is_signature_member("META-INF/sé"));
        // Read at one depth only, so a member that happens to share the extension deeper down is
        // an ordinary resource and survives the remap.
        assert!(!is_signature_member("META-INF/services/provider.sf"));
        assert!(!is_signature_member("net/minecraft/Client.sf"));
        assert!(!is_signature_member("META-INF/"));

        // The directory component too, because the JDK upper-cases the whole entry name before
        // testing it. A block this spelling hid would ride through a remap that had already
        // stripped the manifest half of its claim.
        assert!(is_signature_member("meta-inf/SIGNER.RSA"));
        assert!(is_signature_member("Meta-Inf/signer.sf"));
        assert!(!is_signature_member("meta-inf/services/provider.sf"));
    }

    /// `Multi-Release` is read from the main section only, and only as `true`.
    #[test]
    fn multi_release_is_a_main_attribute() {
        assert!(declares_multi_release(
            b"Manifest-Version: 1.0\r\nMulti-Release: true\r\n\r\n"
        ));
        assert!(declares_multi_release(
            b"Manifest-Version: 1.0\r\nmulti-release: TRUE\r\n\r\n"
        ));
        assert!(!declares_multi_release(
            b"Manifest-Version: 1.0\r\nMulti-Release: false\r\n\r\n"
        ));
        assert!(!declares_multi_release(b"Manifest-Version: 1.0\r\n\r\n"));
        // An individual section says it about one member, which is not what the JVM reads.
        assert!(!declares_multi_release(
            b"Manifest-Version: 1.0\r\n\r\nName: a/B.class\r\nMulti-Release: true\r\n\r\n"
        ));
    }

    /// Setting it is idempotent, replaces a `false`, and never lands in an individual section.
    #[test]
    fn multi_release_is_written_into_the_main_section() {
        let plain =
            b"Manifest-Version: 1.0\r\nCreated-By: x\r\n\r\nName: a/B.class\r\nFoo: 1\r\n\r\n";
        let out = with_multi_release(plain);
        let text = core::str::from_utf8(&out).expect("utf-8");
        let expected = concat!(
            "Manifest-Version: 1.0\r\nCreated-By: x\r\nMulti-Release: true\r\n\r\n",
            "Name: a/B.class\r\nFoo: 1\r\n\r\n"
        );
        assert_eq!(text, expected);
        assert!(declares_multi_release(&out));
        // Applying it again changes nothing, and a `false` becomes a `true` rather than both.
        assert_eq!(with_multi_release(&out), out);
        let denied = b"Manifest-Version: 1.0\r\nMulti-Release: false\r\n\r\n";
        let fixed = with_multi_release(denied);
        assert!(declares_multi_release(&fixed));
        assert_eq!(
            core::str::from_utf8(&fixed)
                .expect("utf-8")
                .matches("Multi-Release")
                .count(),
            1
        );
    }

    /// The manifest is found the way the signature block is, or a jar spelling it in lower case
    /// would lose the block and keep the digests — half a claim, which the module doc says is worse
    /// than none.
    #[test]
    fn the_manifest_is_recognised_the_way_a_signature_block_is() {
        assert!(is_manifest_member("META-INF/MANIFEST.MF"));
        assert!(is_manifest_member("META-INF/manifest.mf"));
        // Both components, exactly as `is_signature_member` reads them and as
        // `JarFile::getManEntry`'s `equalsIgnoreCase` sweep finds the manifest.
        assert!(is_manifest_member("meta-inf/MANIFEST.MF"));
        assert!(is_manifest_member("Meta-Inf/Manifest.mf"));

        assert!(!is_manifest_member("META-INF/versions/9/MANIFEST.MF"));
        assert!(!is_manifest_member("MANIFEST.MF"));
        assert!(!is_manifest_member("META-INF/SIGNER.SF"));
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
        let stripped = String::from_utf8(strip_manifest_digests(manifest.as_bytes())).unwrap();
        assert_eq!(
            stripped,
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
        let stripped = String::from_utf8(strip_manifest_digests(manifest.as_bytes())).unwrap();
        assert_eq!(stripped, "Manifest-Version: 1.0\r\n\r\n");
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
        let stripped = String::from_utf8(strip_manifest_digests(manifest.as_bytes())).unwrap();
        assert_eq!(
            stripped,
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
        let stripped = String::from_utf8(strip_manifest_digests(manifest.as_bytes())).unwrap();
        assert_eq!(
            stripped,
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
        ] {
            let stripped = String::from_utf8(strip_manifest_digests(manifest.as_bytes())).unwrap();
            assert_eq!(stripped, manifest);
        }
    }

    /// Not text at all: a manifest this crate cannot read is one it must not rewrite, because the
    /// alternative is replacing a member it does not understand with an empty one.
    #[test]
    fn a_manifest_that_is_not_utf8_passes_through_verbatim() {
        let bytes: Vec<u8> = alloc::vec![0xff, 0xfe, b'M', b'Z'];
        assert_eq!(strip_manifest_digests(&bytes), bytes);
    }
}
