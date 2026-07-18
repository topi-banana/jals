//! Synthesizing `.java` **skeletons** from compiled `.class` files.
//!
//! When a `[dependencies]` jar ships no `sources` jar (and no `git`/`path` source dependency provides
//! its `.java`), an editor has nowhere to land a go-to-definition on one of its types. This module
//! renders a *skeleton* `.java` for each such class straight from its [`jals_classfile`] model — every
//! type and member declaration, with member details and method bodies decompiled through
//! [`jals_decompile`] (`ConstantValue` initializers, declared `throws`, real parameter names, and
//! straight-line method bodies reconstructed from bytecode; a body that cannot be reconstructed falls
//! back to a placeholder suited to the method's shape). The host writes these to disk via
//! [`SkeletonGroup::synthesize`] and
//! registers them as navigation files, so the existing source-location overlay points a classpath
//! type/member at its synthesized declaration and jump-to-definition works even without library
//! source. The output is always valid Java — an un-reconstructable member falls back to a safe form.
//!
//! The rendering is pure (driven entirely off [`jals_classfile`]'s public model); the public entry
//! point [`SkeletonGroup::synthesize`] publishes verified artifacts. One file is emitted per
//! **top-level** type; nested types are
//! inlined into their enclosing type's body so the dotted FQN the overlay keys on (`a.b.Outer.Inner`)
//! lines up. Anonymous / local / synthetic / module classes are skipped.

use alloc::borrow::{Cow, ToOwned};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use jals_exec::LocalBoxFuture;

use jals_classfile::{
    Attribute, ClassAccessFlags, ClassFile, ClassSignature, ConstantPool, FieldAccessFlags,
    FieldType, MethodAccessFlags, MethodDescriptor, MethodInfo, MethodSignature, ResultSignature,
    ReturnType, TypeParameter, TypeSignature,
};
use jals_decompile::{Attrs, JavaType, MethodBody};
use jals_storage::{ArtifactCache, CacheBackend, CacheNamespace, RelativePath};

use crate::{DependencyResolver, LibrarySource, Warning, WarningOrigin};

/// Skeleton generation is resilient: invalid class names or cache failures are diagnosed and skipped.
#[derive(Debug, Default)]
pub struct Skeletons {
    pub sources: Vec<LibrarySource>,
    pub warnings: Vec<Warning>,
}

/// One top-level type's worth of class files, ready to render into a single `.java`.
///
/// Its output path plus the members (the top-level type and its inlined nested types). Grouping and
/// rendering are separate internal passes so one cache artifact corresponds to one top-level type.
pub struct SkeletonGroup<'a> {
    /// The package, dotted (`a.b`); empty for the default package.
    package: String,
    /// The top-level type's simple name.
    top: String,
    /// The `$`-split nested path → class file for the top-level type and every nested type under it.
    members: BTreeMap<Vec<String>, &'a ClassFile>,
}

impl SkeletonGroup<'_> {
    /// Render and publish one source artifact per top-level type. Rendering is CPU-bound, so a
    /// cooperative tick runs per class group; publishes go through the async cache seam.
    pub async fn synthesize<C: CacheBackend>(
        cache: &mut ArtifactCache<C>,
        classes: &[ClassFile],
    ) -> Skeletons {
        let mut out = Skeletons::default();
        let mut yielder = jals_exec::Yielder::every(1);
        for group in Self::groups(classes) {
            yielder.tick().await;
            let rel = group.rel_path();
            let Ok(path) = RelativePath::parse(&rel) else {
                out.warnings.push(Warning::new(
                    WarningOrigin::Skeleton,
                    format!("generated source path is not portable: {rel}"),
                ));
                continue;
            };
            let bytes = group.render().await.into_bytes();
            let key = DependencyResolver::cache_key(
                CacheNamespace::Skeleton,
                b"skeleton\0",
                path.to_string().as_bytes(),
                &bytes,
            );
            match cache.publish(&key, &bytes).await {
                Ok(()) => out.sources.push(LibrarySource { path, key }),
                Err(error) => out.warnings.push(Warning::new(
                    WarningOrigin::Skeleton,
                    format!("failed to publish generated source `{path}`: {error:?}"),
                )),
            }
        }
        out
    }

    /// The typed-artifact display path: package segments plus `<TopLevel>.java`.
    fn rel_path(&self) -> String {
        let mut path = String::new();
        if !self.package.is_empty() {
            // The package is dotted (`a.b`); the on-disk layout is `/`-separated (`a/b/`).
            path.push_str(&self.package.replace('.', "/"));
            path.push('/');
        }
        path.push_str(&self.top);
        path.push_str(".java");
        path
    }

    /// Render this group's `.java` text: every type/member declaration, with M0 bodies.
    async fn render(&self) -> String {
        let mut text = String::new();
        if !self.package.is_empty() {
            let _ = writeln!(text, "package {};\n", self.package);
        }
        Self::render_type(
            &mut text,
            &self.members,
            core::slice::from_ref(&self.top),
            0,
        )
        .await;
        text
    }

    /// Group `classes` into one [`SkeletonGroup`] per top-level type — the cheap planning pass, with
    /// no bodies rendered (the caller renders each group on demand, skipping any already on disk).
    ///
    /// Classes are grouped by `(package, top-level simple name)`; each group's nested types render
    /// inline so their dotted FQNs are well-formed. A class with no present top-level enclosing type,
    /// or a module / anonymous / local / synthetic class, contributes nothing.
    fn groups(classes: &[ClassFile]) -> Vec<SkeletonGroup<'_>> {
        // group key (package, top-level name) -> nested-path -> class file.
        let mut groups: BTreeMap<(String, String), BTreeMap<Vec<String>, &ClassFile>> =
            BTreeMap::new();
        for cf in classes {
            let Some(entry) = ClassEntry::classify(cf) else {
                continue;
            };
            let key = (entry.package.clone(), entry.nested_path[0].clone());
            groups.entry(key).or_default().insert(entry.nested_path, cf);
        }

        groups
            .into_iter()
            // Only keep a group whose top-level type itself is present, so every nested FQN nests under
            // a real declaration (an orphan inner whose outer was not loaded would otherwise get a wrong
            // FQN).
            .filter(|((_, top), members)| members.contains_key(core::slice::from_ref(top)))
            .map(|((package, top), members)| SkeletonGroup {
                package,
                top,
                members,
            })
            .collect()
    }

    /// Render the type at `path` (and, recursively, every nested type one level under it) into
    /// `out`. Boxed because the nested-type recursion makes the future self-referential.
    fn render_type<'a>(
        out: &'a mut String,
        group: &'a BTreeMap<Vec<String>, &ClassFile>,
        path: &'a [String],
        indent: usize,
    ) -> LocalBoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(cf) = group.get(path) else {
                return;
            };
            let pad = "    ".repeat(indent);
            let simple = path.last().map(String::as_str).unwrap_or_default();
            let class_sig = Self::class_signature(cf);

            let _ = writeln!(
                out,
                "{pad}{} {{",
                Self::type_header(cf, simple, class_sig.as_ref(), indent == 0)
            );
            Self::render_members(out, cf, simple, indent + 1).await;

            // Nested types: the group's entries whose path extends `path` by exactly one segment.
            let child_len = path.len() + 1;
            for child_path in group.keys() {
                if child_path.len() == child_len && child_path.starts_with(path) {
                    out.push('\n');
                    Self::render_type(out, group, child_path, indent + 1).await;
                }
            }
            let _ = writeln!(out, "{pad}}}");
        })
    }

    /// The declaration header up to (not including) the opening brace: modifiers, keyword, name, type
    /// parameters, and the `extends` / `implements` clause.
    fn type_header(
        cf: &ClassFile,
        simple: &str,
        sig: Option<&ClassSignature>,
        top_level: bool,
    ) -> String {
        let flags = cf.access_flags;
        let mut header = Self::tokens_prefix(&Self::class_modifiers(flags, top_level));
        header.push_str(Self::class_keyword(cf));
        header.push(' ');
        header.push_str(simple);
        if let Some(sig) = sig {
            header.push_str(&Self::render_type_params(&sig.type_parameters));
        }
        if !flags.is_annotation() {
            let (superclass, interfaces) = Self::supertypes(cf, sig);
            // Only a class spells out a non-implicit superclass; an interface's superclass is always
            // Object, so it never reaches this line.
            if !flags.is_interface()
                && let Some(sc) = superclass.filter(|sc| !Self::is_implicit_super(sc))
            {
                let _ = write!(header, " extends {sc}");
            }
            if !interfaces.is_empty() {
                // An interface lists its parent interfaces with `extends`, a class with `implements`.
                let kw = if flags.is_interface() {
                    "extends"
                } else {
                    "implements"
                };
                let _ = write!(header, " {kw} {}", interfaces.join(", "));
            }
        }
        header
    }

    /// The class-kind keyword. A record is rendered as a plain `class` — the `.class` stays
    /// authoritative for typing, and the skeleton is navigation-only, so the record component syntax is
    /// unnecessary.
    const fn class_keyword(cf: &ClassFile) -> &'static str {
        let flags = cf.access_flags;
        if flags.is_annotation() {
            "@interface"
        } else if flags.is_interface() {
            "interface"
        } else if flags.is_enum() {
            "enum"
        } else {
            "class"
        }
    }

    /// The keyword modifiers of a type declaration, in canonical order. `abstract`/`final` are emitted
    /// only for a plain class (they are implied or illegal for an interface / enum / annotation), and a
    /// nested type is marked `static` (the skeleton flattens any enclosing-instance capture).
    fn class_modifiers(flags: ClassAccessFlags, top_level: bool) -> Vec<&'static str> {
        let plain_class = !(flags.is_interface() || flags.is_annotation() || flags.is_enum());
        let mut mods = Vec::new();
        if flags.contains(ClassAccessFlags::PUBLIC) {
            mods.push("public");
        }
        // A nested interface / enum / annotation is implicitly static; only a nested *class* spells it.
        if !top_level && plain_class {
            mods.push("static");
        }
        if plain_class {
            if flags.is_abstract() {
                mods.push("abstract");
            }
            if flags.contains(ClassAccessFlags::FINAL) {
                mods.push("final");
            }
        }
        mods
    }

    /// The rendered superclass (if any) and interfaces of a class, from its generic `Signature` when
    /// present (so type arguments survive), else from its raw `super_class` / `interfaces` descriptors.
    fn supertypes(cf: &ClassFile, sig: Option<&ClassSignature>) -> (Option<String>, Vec<String>) {
        if let Some(sig) = sig {
            let superclass = JavaType::render_class_type_sig(&sig.superclass);
            let interfaces = sig
                .superinterfaces
                .iter()
                .map(JavaType::render_class_type_sig)
                .collect();
            return (Some(superclass), interfaces);
        }
        let pool = &cf.constant_pool;
        let superclass = (cf.super_class != 0)
            .then(|| pool.class_name(cf.super_class))
            .flatten()
            .map(|name| JavaType::internal_to_java(&name));
        let interfaces = cf
            .interfaces
            .iter()
            .filter_map(|&i| pool.class_name(i))
            .map(|name| JavaType::internal_to_java(&name))
            .collect();
        (superclass, interfaces)
    }

    /// Whether a rendered superclass is one Java supplies implicitly (so the skeleton omits it).
    fn is_implicit_super(rendered: &str) -> bool {
        let base = rendered.split('<').next().unwrap_or(rendered);
        matches!(
            base,
            "java.lang.Object" | "java.lang.Enum" | "java.lang.Record"
        )
    }

    /// Render a type declaration's body members: enum constants, then fields, then constructors/methods.
    async fn render_members(out: &mut String, cf: &ClassFile, simple: &str, indent: usize) {
        let pool = &cf.constant_pool;
        let pad = "    ".repeat(indent);

        if cf.access_flags.is_enum() {
            let constants: Vec<String> = cf
                .fields
                .iter()
                .filter(|f| f.access_flags.is_enum())
                .filter_map(|f| pool.utf8(f.name_index).map(Cow::into_owned))
                .collect();
            if !constants.is_empty() {
                let _ = writeln!(out, "{pad}{};", constants.join(", "));
            }
        }

        for field in &cf.fields {
            let flags = field.access_flags;
            if flags.is_enum() || flags.contains(FieldAccessFlags::SYNTHETIC) {
                continue;
            }
            let Some(name) = pool.utf8(field.name_index).map(Cow::into_owned) else {
                continue;
            };
            let ty = Self::field_type_java(&field.attributes, field.descriptor_index, pool);
            // A `static final` field's compile-time constant becomes its initializer (`= 42`), so a
            // navigated declaration shows the value.
            let init = Attrs::constant_value_initializer(field, pool)
                .map(|value| format!(" = {value}"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{pad}{}{ty} {name}{init};",
                Self::tokens_prefix(&Self::field_modifiers(flags))
            );
        }

        for method in &cf.methods {
            let flags = method.access_flags;
            if flags.contains(MethodAccessFlags::SYNTHETIC)
                || flags.contains(MethodAccessFlags::BRIDGE)
            {
                continue;
            }
            let Some(raw_name) = pool.utf8(method.name_index).map(Cow::into_owned) else {
                continue;
            };
            if raw_name == "<clinit>" {
                continue;
            }
            Self::render_method(out, method, cf, &raw_name, simple, &pad).await;
        }
    }

    /// Render one method or constructor declaration. The signature is followed by its body: `;` for an
    /// `abstract`/`native` method (which holds none), else the method's decompiled body when
    /// reconstructable ([`jals_decompile::MethodBody::decompile`]), else a safe placeholder
    /// ([`Self::safe_body`]). Recovered source parameter names are used when available.
    async fn render_method(
        out: &mut String,
        method: &MethodInfo,
        cf: &ClassFile,
        raw_name: &str,
        simple: &str,
        pad: &str,
    ) {
        let pool = &cf.constant_pool;
        let flags = method.access_flags;
        let pieces = MethodPieces::of(method, pool);
        let names = Attrs::parameter_names(method, pool, flags.is_static(), pieces.params.len())
            .unwrap_or_else(|| {
                (0..pieces.params.len())
                    .map(|i| format!("arg{i}"))
                    .collect()
            });
        let params = Self::render_params(&pieces.params, &names, flags.is_varargs());
        let throws = if pieces.throws.is_empty() {
            String::new()
        } else {
            format!(" throws {}", pieces.throws.join(", "))
        };
        let mods = Self::tokens_prefix(&Self::method_modifiers(flags));
        let type_params = Self::space_suffix(Self::render_type_params(&pieces.type_params));
        let is_ctor = raw_name == "<init>";
        let head = if is_ctor {
            // A constructor has no return type; its source name is the class's simple name.
            format!("{pad}{mods}{type_params}{simple}({params}){throws}")
        } else {
            let ret = pieces.ret.as_deref().unwrap_or("void");
            format!("{pad}{mods}{type_params}{ret} {raw_name}({params}){throws}")
        };
        let body = Self::method_body(method, cf, &names, is_ctor, pieces.ret.is_some(), pad).await;
        let _ = writeln!(out, "{head}{body}");
    }

    /// The body to place after a rendered signature. An `abstract`/`native` method holds none (`;`).
    /// Otherwise, prefer the decompiled body (using the same parameter `names` the signature renders);
    /// if it cannot be reconstructed, fall back to a safe placeholder — `{}` for a `void` method /
    /// constructor, `{ throw new RuntimeException(); }` for a value-returning one (valid for any return
    /// type, so the output always parses).
    async fn method_body(
        method: &MethodInfo,
        cf: &ClassFile,
        names: &[String],
        is_ctor: bool,
        returns_value: bool,
        pad: &str,
    ) -> String {
        let flags = method.access_flags;
        if flags.is_abstract() || flags.contains(MethodAccessFlags::NATIVE) {
            return ";".to_owned();
        }
        if let Some(stmts) = MethodBody::decompile(method, cf, names).await {
            return Self::render_body_block(&stmts, pad);
        }
        Self::safe_body(is_ctor, returns_value).to_owned()
    }

    /// The safe placeholder body used when a method's real body cannot be decompiled.
    const fn safe_body(is_ctor: bool, returns_value: bool) -> &'static str {
        if is_ctor || !returns_value {
            " {}"
        } else {
            " { throw new RuntimeException(); }"
        }
    }

    /// Wrap decompiled statement lines in a block, indented one level past `pad`. An empty body renders
    /// inline as ` {}`.
    fn render_body_block(stmts: &[String], pad: &str) -> String {
        if stmts.is_empty() {
            return " {}".to_owned();
        }
        let mut block = String::from(" {\n");
        for stmt in stmts {
            let _ = writeln!(block, "{pad}    {stmt}");
        }
        block.push_str(pad);
        block.push('}');
        block
    }

    /// Render a parameter list, naming each parameter from `names` (its recovered source name, or an
    /// `argN` fallback the caller supplies). A trailing array becomes `...` for a varargs method.
    fn render_params(params: &[String], names: &[String], varargs: bool) -> String {
        params
            .iter()
            .zip(names)
            .enumerate()
            .map(|(i, (ty, name))| {
                let last = i + 1 == params.len();
                let ty = if varargs && last && ty.ends_with("[]") {
                    format!("{}...", &ty[..ty.len() - 2])
                } else {
                    ty.clone()
                };
                format!("{ty} {name}")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A field's type, from its generic `Signature` when present, else its descriptor;
    /// `java.lang.Object` if neither parses (never happens for a well-formed class file).
    fn field_type_java(attrs: &[Attribute], descriptor_index: u16, pool: &ConstantPool) -> String {
        if let Some(sig) = Attrs::signature_string(attrs, pool)
            && let Ok(ts) = TypeSignature::parse(&sig)
        {
            return JavaType::render_type_sig(&ts);
        }
        if let Some(desc) = pool.utf8(descriptor_index)
            && let Ok(ft) = FieldType::parse(&desc)
        {
            return JavaType::render_field_type(&ft);
        }
        "java.lang.Object".to_owned()
    }

    /// The keyword modifiers of a field, in canonical order.
    fn field_modifiers(flags: FieldAccessFlags) -> Vec<&'static str> {
        let mut mods = Vec::new();
        if flags.is_public() {
            mods.push("public");
        } else if flags.contains(FieldAccessFlags::PROTECTED) {
            mods.push("protected");
        } else if flags.contains(FieldAccessFlags::PRIVATE) {
            mods.push("private");
        }
        if flags.is_static() {
            mods.push("static");
        }
        if flags.contains(FieldAccessFlags::FINAL) {
            mods.push("final");
        }
        if flags.contains(FieldAccessFlags::VOLATILE) {
            mods.push("volatile");
        }
        if flags.contains(FieldAccessFlags::TRANSIENT) {
            mods.push("transient");
        }
        mods
    }

    /// The keyword modifiers of a method, in canonical order.
    fn method_modifiers(flags: MethodAccessFlags) -> Vec<&'static str> {
        let mut mods = Vec::new();
        if flags.is_public() {
            mods.push("public");
        } else if flags.contains(MethodAccessFlags::PROTECTED) {
            mods.push("protected");
        } else if flags.contains(MethodAccessFlags::PRIVATE) {
            mods.push("private");
        }
        if flags.is_static() {
            mods.push("static");
        }
        if flags.is_abstract() {
            mods.push("abstract");
        }
        if flags.contains(MethodAccessFlags::FINAL) {
            mods.push("final");
        }
        if flags.contains(MethodAccessFlags::SYNCHRONIZED) {
            mods.push("synchronized");
        }
        if flags.contains(MethodAccessFlags::NATIVE) {
            mods.push("native");
        }
        mods
    }

    /// `"tok1 tok2 "` for a non-empty token list, `""` for an empty one — a space-terminated modifier
    /// prefix that glues cleanly onto whatever follows.
    fn tokens_prefix(tokens: &[&str]) -> String {
        Self::space_suffix(tokens.join(" "))
    }

    /// Append a single trailing space to a non-empty fragment, leaving an empty one untouched — so an
    /// optional prefix (modifiers, type parameters) glues cleanly onto whatever follows.
    fn space_suffix(fragment: String) -> String {
        if fragment.is_empty() {
            fragment
        } else {
            format!("{fragment} ")
        }
    }

    /// Render a class declaration's / method's type parameters: `<T, K extends Bound & Other>`. An
    /// empty list renders to the empty string. An implicit `Object` class bound contributes no listed
    /// bound.
    fn render_type_params(params: &[TypeParameter]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let rendered: Vec<String> = params
            .iter()
            .map(|p| {
                let mut bounds = Vec::new();
                if let Some(class_bound) = &p.class_bound
                    && !class_bound.is_java_lang_object()
                {
                    bounds.push(JavaType::render_type_sig(class_bound));
                }
                bounds.extend(p.interface_bounds.iter().map(JavaType::render_type_sig));
                if bounds.is_empty() {
                    p.name.clone()
                } else {
                    format!("{} extends {}", p.name, bounds.join(" & "))
                }
            })
            .collect();
        format!("<{}>", rendered.join(", "))
    }

    /// The class's generic signature, if it has a parseable `Signature` attribute.
    fn class_signature(cf: &ClassFile) -> Option<ClassSignature> {
        ClassSignature::parse(&Attrs::signature_string(&cf.attributes, &cf.constant_pool)?).ok()
    }
}

/// A renderable class's identity within its top-level group.
struct ClassEntry {
    /// The package, dotted (`a.b`); empty for the default package.
    package: String,
    /// The `$`-split simple-name path under the package (`Outer$Inner` → `["Outer", "Inner"]`).
    nested_path: Vec<String>,
}

impl ClassEntry {
    /// Classify a class file for rendering, or `None` to skip it (a module / anonymous / local /
    /// synthetic / `module-info` / `package-info` class is not a navigable named type).
    fn classify(cf: &ClassFile) -> Option<Self> {
        let flags = cf.access_flags;
        if flags.is_module() || flags.contains(ClassAccessFlags::SYNTHETIC) {
            return None;
        }
        let internal = cf.constant_pool.class_name(cf.this_class)?;
        let (package, simple_internal) = match internal.rsplit_once('/') {
            Some((pkg, simple)) => (pkg.replace('/', "."), simple.to_owned()),
            None => (String::new(), internal.into_owned()),
        };
        if simple_internal == "module-info" || simple_internal == "package-info" {
            return None;
        }
        let nested_path: Vec<String> = simple_internal.split('$').map(str::to_owned).collect();
        // Skip anonymous / local classes: any `$`-segment after the first that is empty or begins with
        // a digit (`Foo$1`, `Foo$1Local`) is compiler-generated and not a navigable source name.
        if nested_path
            .iter()
            .skip(1)
            .any(|seg| seg.is_empty() || seg.starts_with(|c: char| c.is_ascii_digit()))
        {
            return None;
        }
        Some(Self {
            package,
            nested_path,
        })
    }
}

/// The rendered signature pieces of a method: from its generic `Signature` when present, else its
/// descriptor.
#[derive(Default)]
struct MethodPieces {
    type_params: Vec<TypeParameter>,
    params: Vec<String>,
    /// The return type, or `None` for `void`.
    ret: Option<String>,
    throws: Vec<String>,
}

impl MethodPieces {
    /// The rendered pieces of `method`, filling in a non-generic `throws` clause the descriptor /
    /// generic signature omits.
    fn of(method: &MethodInfo, pool: &ConstantPool) -> Self {
        let mut pieces = Self::from_signature_or_descriptor(method, pool);
        // A non-generic `throws` clause lives in the `Exceptions` attribute, not the descriptor — and a
        // generic `Signature` omits its throws entirely when no thrown type is generic — so fill it in.
        if pieces.throws.is_empty() {
            pieces.throws = Attrs::declared_throws(method, pool);
        }
        pieces
    }

    /// The rendered pieces from `method`'s generic `Signature` when present, else its descriptor.
    fn from_signature_or_descriptor(method: &MethodInfo, pool: &ConstantPool) -> Self {
        if let Some(sig) = Attrs::signature_string(&method.attributes, pool)
            && let Ok(ms) = MethodSignature::parse(&sig)
        {
            return Self {
                type_params: ms.type_parameters,
                params: ms
                    .parameters
                    .iter()
                    .map(JavaType::render_type_sig)
                    .collect(),
                ret: match &ms.result {
                    ResultSignature::Void => None,
                    ResultSignature::Type(t) => Some(JavaType::render_type_sig(t)),
                },
                throws: ms.throws.iter().map(JavaType::render_throws).collect(),
            };
        }
        if let Some(desc) = pool.utf8(method.descriptor_index)
            && let Ok(md) = MethodDescriptor::parse(&desc)
        {
            return Self {
                type_params: Vec::new(),
                params: md.params.iter().map(JavaType::render_field_type).collect(),
                ret: match &md.return_type {
                    ReturnType::Void => None,
                    ReturnType::Type(ft) => Some(JavaType::render_field_type(ft)),
                },
                throws: Vec::new(),
            };
        }
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_class() -> ClassFile {
        jals_exec::block_on_inline(jals_classfile::ClassFile::read(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/Box.class"
            ))
            .as_slice(),
        ))
        .expect("parse Box.class")
    }

    #[test]
    fn renders_generic_class_with_members() {
        let classes = [box_class()];
        let groups = SkeletonGroup::groups(&classes);
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        // `Box` is in the default package, so it is `Box.java` at the root.
        assert_eq!(group.rel_path(), "Box.java");
        let text = jals_exec::block_on_inline(group.render());
        // The generic type, its field, and its methods — each with its decompiled body (Box.class
        // carries no debug info, so parameters keep their `argN` fallback names).
        assert!(text.contains("public class Box<T> {"), "{text}");
        assert!(text.contains("private T value;"), "{text}");
        assert!(text.contains("public T get() {"), "{text}");
        assert!(text.contains("return this.value;"), "{text}");
        assert!(text.contains("public void set(T arg0) {"), "{text}");
        assert!(text.contains("this.value = arg0;"), "{text}");
        assert!(text.contains("public Box() {}"), "{text}");
    }

    #[test]
    fn skips_synthetic_and_module_classes() {
        // The same bytes render fine as a plain class...
        assert_eq!(SkeletonGroup::groups(&[box_class()]).len(), 1);

        // ...but a synthetic or module class is compiler-generated, not a navigable named type, so it
        // contributes no skeleton group.
        let mut synthetic = box_class();
        synthetic.access_flags.0 |= ClassAccessFlags::SYNTHETIC;
        assert!(SkeletonGroup::groups(&[synthetic]).is_empty());

        let mut module = box_class();
        module.access_flags.0 |= ClassAccessFlags::MODULE;
        assert!(SkeletonGroup::groups(&[module]).is_empty());
    }
}
