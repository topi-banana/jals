//! Jar remapping with Mojang official mappings, plus registered jar merge.
//!
//! [`JarRemap::remap`] turns an obfuscated jar + mapping text into a deobfuscated jar published
//! under `BuildTaskArtifact`. The transform is append-only on every class pool (new Utf8 /
//! `NameAndType` / Class entries are added, refs are rewritten in place) so every external index
//! stays stable while rates of hierarchy-aware member renaming and descriptor/signature
//! rewriting proceed. Non-class members pass through verbatim; `META-INF/MANIFEST.MF`'s
//! `Main-Class` is rewritten when present.
//!
//! [`JarMerge::merge`] unions two jars by member path: the overlay wins on conflicts, the base
//! keeps everything else, both in deterministic input order.

use alloc::borrow::ToOwned;
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
    MethodAccessFlags, MethodDescriptor, MethodInfo, MethodSignature, RecordComponentInfo,
    ReturnType, SimpleClassTypeSignature, ThrowsSignature, TypeAnnotation, TypeArgument,
    TypeParameter, TypeSignature,
};
use jals_exec::Exec;
use jals_storage::{ArtifactCache, CacheBackend, CacheKey, CacheNamespace, ContentDigest};

use crate::DependencyResolver;
use crate::load::{Archive, SourceTreeLimits};
use crate::mappings::Mappings;
use crate::zip::{StoredZip, WriteMember};

/// Hardcoded size budget for a remapped / merged jar input. Matches the task-side
/// `ExtractJava` total (1 GiB) so a Minecraft client/server jar always fits with headroom.
const JAR_LIMITS: SourceTreeLimits = SourceTreeLimits {
    max_files: 200_000,
    max_file_bytes: 64 * 1_048_576,
    max_total_bytes: 1_024 * 1_048_576,
};

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
        let mut provenance = Vec::new();
        provenance.extend_from_slice(parent.provenance().as_bytes());
        provenance.extend_from_slice(parent.content().as_bytes());
        provenance.extend_from_slice(member.as_bytes());
        let key = DependencyResolver::cache_key(
            CacheNamespace::BuildTaskArtifact,
            b"nested-jar\0",
            &provenance,
            bytes,
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
    /// Remap every `.class` member of `jar` per Mojang-format `mappings` text, publishing the
    /// resulting jar under `BuildTaskArtifact`. Provenance includes the source jar key and the
    /// mapping-bytes digest so re-runs are content-addressed.
    pub async fn remap<C: CacheBackend>(
        exec: &Exec,
        cache: &mut ArtifactCache<C>,
        jar: &CacheKey,
        mappings_text: &str,
    ) -> Result<CacheKey, String> {
        let mappings = Mappings::parse(mappings_text)
            .map_err(|error| format!("mappings parse failed: {error}"))?;
        let mappings = Arc::new(mappings);

        let reader = cache
            .open_verified(jar)
            .await
            .map_err(|error| format!("remap jar is invalid: {error:?}"))?
            .ok_or_else(|| "remap jar is not cached".to_owned())?;
        let members = Archive::decode_all_bounded(exec, reader, JAR_LIMITS).await?;

        // Pass 1: parse every class file and build the obfuscated class hierarchy.
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
            parsed.push((position, cf));
        }
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
                if name == "META-INF/MANIFEST.MF" {
                    bytes = helpers::rewrite_manifest_main_class(&bytes, &mappings);
                }
                (name, bytes)
            };
            if !used_names.insert(name.clone()) {
                return Err(format!("duplicate remapped archive member `{name}`"));
            }
            out_members.push(WriteMember { name, bytes });
        }
        let jar_bytes = StoredZip::write(&out_members)?;

        let mut provenance = Vec::new();
        provenance.extend_from_slice(jar.provenance().as_bytes());
        provenance.extend_from_slice(jar.content().as_bytes());
        provenance.extend_from_slice(ContentDigest::of(mappings_text.as_bytes()).as_bytes());
        let key = DependencyResolver::cache_key(
            CacheNamespace::BuildTaskArtifact,
            b"remap-jar\0",
            &provenance,
            &jar_bytes,
        );
        cache
            .publish(&key, &jar_bytes)
            .await
            .map_err(|error| format!("remapped jar publish failed: {error:?}"))?;
        Ok(key)
    }
}

/// Jar merge namespace.
pub struct JarMerge;

impl JarMerge {
    /// Merge two cached jars. Members of `overlay` win on path conflicts; everything else comes
    /// from `base` in its original order, followed by any `overlay`-only members in overlay order.
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

        let mut overlay_map: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut overlay_order: Vec<String> = Vec::new();
        for (name, outcome) in overlay_members {
            let bytes = outcome
                .map_err(|error| format!("failed to read overlay member `{name}`: {error}"))?;
            if overlay_map.insert(name.clone(), bytes).is_none() {
                overlay_order.push(name);
            }
        }

        // Walking the base consumes every overlay member that shadows one, so whatever is still in
        // `overlay_map` afterwards is exactly the overlay-only set.
        let mut out_members = Vec::new();
        for (name, outcome) in base_members {
            let bytes = match overlay_map.remove(&name) {
                Some(overlay_bytes) => overlay_bytes,
                None => outcome
                    .map_err(|error| format!("failed to read base member `{name}`: {error}"))?,
            };
            out_members.push(WriteMember { name, bytes });
        }
        for name in overlay_order {
            if let Some(bytes) = overlay_map.remove(&name) {
                out_members.push(WriteMember { name, bytes });
            }
        }

        let jar_bytes = StoredZip::write(&out_members)?;
        let mut provenance = Vec::new();
        provenance.extend_from_slice(base.provenance().as_bytes());
        provenance.extend_from_slice(base.content().as_bytes());
        provenance.extend_from_slice(overlay.provenance().as_bytes());
        provenance.extend_from_slice(overlay.content().as_bytes());
        let key = DependencyResolver::cache_key(
            CacheNamespace::BuildTaskArtifact,
            b"merge-jars\0",
            &provenance,
            &jar_bytes,
        );
        cache
            .publish(&key, &jar_bytes)
            .await
            .map_err(|error| format!("merged jar publish failed: {error:?}"))?;
        Ok(key)
    }
}

#[allow(clippy::wildcard_imports)]
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

    /// Rewrite `Main-Class:` in a manifest body when the target maps under `mappings`.
    pub(super) fn rewrite_manifest_main_class(bytes: &[u8], mappings: &Mappings) -> Vec<u8> {
        let Ok(text) = core::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if let Some(value) = trimmed
                .strip_prefix("Main-Class:")
                .or_else(|| trimmed.strip_prefix("Main-Class: "))
            {
                let dotted = value.trim();
                let internal = dotted.replace('.', "/");
                if let Some(official) = mappings.remap_class(&internal) {
                    let rewritten = official.replace('/', ".");
                    let _ = writeln!(out, "Main-Class: {rewritten}");
                    continue;
                }
            }
            out.push_str(line);
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
                    let new_desc = remap_descriptor(&desc, mappings);
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
        let new_desc = remap_descriptor(&desc_obf, mappings);
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
        let new_desc = remap_descriptor(&desc_obf, mappings);
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
    pub(super) fn remap_class_constant(raw: &str, mappings: &Mappings) -> String {
        if raw.starts_with('[') {
            return remap_descriptor(raw, mappings);
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
        let new_desc = remap_descriptor(&desc_obf, mappings);
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
        let new_desc = remap_descriptor(&desc_obf, mappings);
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

    fn remap_descriptor(desc: &str, mappings: &Mappings) -> String {
        if let Ok(md) = MethodDescriptor::parse(desc) {
            let params: Vec<_> = md
                .params
                .into_iter()
                .map(|p| remap_field_type(p, mappings))
                .collect();
            let ret = match md.return_type {
                ReturnType::Void => ReturnType::Void,
                ReturnType::Type(t) => ReturnType::Type(remap_field_type(t, mappings)),
            };
            return MethodDescriptor {
                params,
                return_type: ret,
            }
            .to_string();
        }
        if let Ok(ft) = FieldType::parse(desc) {
            return remap_field_type(ft, mappings).to_string();
        }
        desc.to_owned()
    }

    fn remap_field_type(ft: FieldType, mappings: &Mappings) -> FieldType {
        match ft {
            FieldType::Base(b) => FieldType::Base(b),
            FieldType::Object(name) => FieldType::Object(
                mappings
                    .remap_class(&name)
                    .map(str::to_owned)
                    .unwrap_or(name),
            ),
            FieldType::Array(inner) => {
                FieldType::Array(alloc::boxed::Box::new(remap_field_type(*inner, mappings)))
            }
        }
    }

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

    pub(super) fn remap_class_type_sig(
        mut c: ClassTypeSignature,
        mappings: &Mappings,
    ) -> ClassTypeSignature {
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

    pub(super) fn remap_class_signature(
        mut s: ClassSignature,
        mappings: &Mappings,
    ) -> ClassSignature {
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
                        let new = remap_descriptor(&desc, mappings);
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
        let new_desc = remap_descriptor(&desc, mappings);
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
                let new_desc = remap_descriptor(&desc, mappings);
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
                    remap_descriptor(&desc, mappings)
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
        let new_desc = remap_descriptor(&desc_obf, mappings);
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
    use super::helpers::multi_release_prefix;

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
}
