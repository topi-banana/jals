//! Checks the embedded stdlib stubs against a real JDK's own signatures.
//!
//! `jals-hir` models `java.lang` / `java.util` / `java.io` as hand-written, signature-only Java
//! stubs. That is a deliberate design choice — it keeps the analysis pure and `wasm32`-compatible,
//! with no host file to find — but it changes character the moment a compiler *emits* from them.
//! A stub that says `println(String)` where the JDK says `println(CharSequence)` produces a class
//! file that loads happily and throws `NoSuchMethodError` on the first call, because nothing in the
//! pipeline checks: type errors are the linter's job, and the linter is reading the same wrong stub.
//!
//! So the stubs get an oracle. `$JAVA_HOME/lib/ct.sym` is the signature data `javac --release` reads:
//! an ordinary zip whose entries are ordinary class files with their method bodies stripped. Reading
//! it needs a host path, which is why this lives in a **test** — the product still sees only the
//! stubs, and the `zip` crate is already the workspace's dev-only archive oracle.
//!
//! A missing JDK skips the checks rather than failing, matching the CLI tests' `javac_available()`
//! convention.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use jals_classfile::{ClassFile, MethodDescriptor};
use jals_hir::{DefKind, ItemOrigin, ProjectIndex};
use jals_javac::desc::Descriptor;

/// The running JDK's home directory and specification version, from the JVM itself.
///
/// `$JAVA_HOME` is routinely unset even where a JDK is installed, so it is not consulted; asking
/// the JVM for `java.home` works whenever `java` is on `PATH` at all.
fn jdk() -> Option<(PathBuf, u32)> {
    let output = Command::new("java")
        .args(["-XshowSettings:properties", "-version"])
        .output()
        .ok()?;
    // The settings dump goes to stderr, one `  name = value` per line.
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    let property = |name: &str| {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(key, _)| key.trim() == name)
            .map(|(_, value)| value.trim().to_owned())
    };
    let home = PathBuf::from(property("java.home")?);
    let version = property("java.specification.version")?.parse().ok()?;
    Some((home, version))
}

/// The letter `ct.sym` files a release under: `8` and `9` are themselves, then `A` is 10, `B` is
/// 11, and so on. A directory name is a *set* of these (`MNOP` covers 22 through 25), so an entry
/// belongs to a release when its first path segment contains that release's letter.
const fn release_letter(version: u32) -> Option<char> {
    match version {
        8 | 9 => char::from_digit(version, 10),
        10.. => char::from_u32('A' as u32 + (version - 10)),
        _ => None,
    }
}

/// Every class the JDK ships for `release`, by internal name, as a parsed class file.
///
/// Only the packages the stubs model are read: `ct.sym` holds every release of every module, and
/// parsing all of it to check a few dozen types would dominate the test's runtime.
fn jdk_classes(home: &std::path::Path, release: char) -> Vec<(String, ClassFile)> {
    const PACKAGES: &[&str] = &["java/lang/", "java/util/", "java/io/"];

    let file = std::fs::File::open(home.join("lib").join("ct.sym")).expect("open ct.sym");
    let mut archive = zip::ZipArchive::new(file).expect("ct.sym is a zip");
    let mut out = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("zip entry");
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let name = name.to_string_lossy().replace('\\', "/");
        let Some((releases, rest)) = name.split_once('/') else {
            continue;
        };
        // `<releases>/<module>/<package>/<Class>.sig`
        // `ct.sym` names every signature entry `<Class>.sig`, always lower-case: this is the
        // archive's own naming, not a user-supplied filename, so an exact suffix match is right.
        let Some((_module, path)) = rest.split_once('/') else {
            continue;
        };
        let Some(class_path) = path.strip_suffix(".sig") else {
            continue;
        };
        if !releases.contains(release)
            || !PACKAGES
                .iter()
                .any(|package| class_path.starts_with(package))
        {
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read entry");
        if let Ok(class) = jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())) {
            out.push((class_path.to_owned(), class));
        }
    }
    out
}

/// Every method the class declares, as `name descriptor` — the pair a `Methodref` names, and so
/// exactly what has to exist at run time for an emitted call to resolve.
fn method_signatures(class: &ClassFile) -> BTreeSet<String> {
    class
        .methods
        .iter()
        .filter_map(|method| {
            let name = class.constant_pool.utf8(method.name_index)?;
            let descriptor = class.constant_pool.utf8(method.descriptor_index)?;
            Some(format!("{name} {descriptor}"))
        })
        .collect()
}

/// Every field the class declares, as `name descriptor`.
fn field_signatures(class: &ClassFile) -> BTreeSet<String> {
    class
        .fields
        .iter()
        .filter_map(|field| {
            let name = class.constant_pool.utf8(field.name_index)?;
            let descriptor = class.constant_pool.utf8(field.descriptor_index)?;
            Some(format!("{name} {descriptor}"))
        })
        .collect()
}

/// Every member of the embedded stubs must exist in the JDK with the identical descriptor.
///
/// The direction matters: the stubs are allowed to be a *subset* of the real API — that is the
/// whole point of a stub — but every entry in that subset has to be real, because a compiler emits
/// from it verbatim.
#[test]
fn every_stdlib_stub_member_exists_in_the_real_jdk() {
    // A missing JDK stands the test down. It says so: this is the *only* check that the embedded
    // stubs match the signatures a real JVM will link against, and a signature that drifted would
    // otherwise surface as a `NoSuchMethodError` at run time rather than here.
    let Some((home, version)) = jdk() else {
        eprintln!("note: no JDK on this host; the stdlib stubs went unchecked");
        return;
    };
    let Some(release) = release_letter(version) else {
        eprintln!("note: JDK {version} has no `ct.sym` release letter; the stubs went unchecked");
        return;
    };
    let jdk_classes = jdk_classes(&home, release);
    assert!(
        !jdk_classes.is_empty(),
        "found no java.lang/java.util/java.io signatures for release {release} in ct.sym"
    );

    let index = jals_exec::block_on_inline(ProjectIndex::builder(&[]).with_stdlib().build());
    let mut checked = 0usize;
    let mut wrong = Vec::new();

    for (id, item) in index.items() {
        if item.origin != ItemOrigin::Stdlib {
            continue;
        }
        let internal = Descriptor::internal_name(item.fqn.as_str());
        let Some((_, class)) = jdk_classes.iter().find(|(name, _)| *name == internal) else {
            wrong.push(format!("{internal}: no such class in the JDK"));
            continue;
        };
        let methods = method_signatures(class);
        let fields = field_signatures(class);

        for member_id in index.members_of(id) {
            let member = index.member(member_id);
            // Inherited members are reachable from `members_of` but are declared elsewhere; only
            // this class's own declarations can be looked up in this class file.
            if member.owner != id {
                continue;
            }
            let (name, expected) = match member.kind {
                DefKind::Field => {
                    let Ok(descriptor) = Descriptor::field_descriptor(member_id, &index) else {
                        continue;
                    };
                    (member.name.clone(), (&fields, descriptor.to_string()))
                }
                DefKind::Method | DefKind::Constructor => {
                    let constructor = member.kind == DefKind::Constructor;
                    let Ok(descriptor) =
                        Descriptor::method_descriptor(member_id, &index, constructor)
                    else {
                        continue;
                    };
                    let name = if constructor {
                        "<init>".to_owned()
                    } else {
                        member.name.clone()
                    };
                    (name, (&methods, MethodDescriptor::to_string(&descriptor)))
                }
                // An enum constant is a field of its own type; the stubs declare none.
                _ => continue,
            };
            let (declared, descriptor) = expected;
            checked += 1;
            let signature = format!("{name} {descriptor}");
            if !declared.contains(&signature) {
                wrong.push(format!("{internal}.{signature} is not declared by the JDK"));
            }
        }
    }

    assert!(checked > 50, "only {checked} stub members were checked");
    assert!(
        wrong.is_empty(),
        "{} stub member(s) do not match the JDK:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
