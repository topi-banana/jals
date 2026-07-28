//! Building a class file from scratch through the construction surface, rather than by reading one.
//!
//! The codec's round-trip anchor (`roundtrip.rs`) proves `read(b).write() == b` for bytes `javac`
//! produced. This proves the other direction: a `ClassFile` assembled from nothing survives
//! `write()` → `read()` unchanged, which is what a compiler backend depends on.

use std::fmt::Write as _;
use std::process::{Command, Stdio};

use expect_test::expect;
use jals_classfile::{
    Attribute, AttributeBody, ClassAccessFlags, ClassFile, CodeAttribute, ConstantPool,
    ConstantPoolEntry, Instruction, MethodAccessFlags, MethodInfo,
};

/// Java 25. Chosen to match the committed fixtures, which are pinned to `javac 25`.
const MAJOR_JAVA_25: u16 = 69;

fn read(bytes: &[u8]) -> jals_classfile::Result<ClassFile> {
    jals_exec::block_on_inline(ClassFile::read(bytes))
}

/// Wrap `body` in a `Code` attribute. Neither method below branches, so a `StackMapTable` is not
/// required at any class version — the implicit initial frame is the only one a verifier needs.
fn code_attribute(
    pool: &mut ConstantPool,
    max_stack: u16,
    max_locals: u16,
    body: Vec<Instruction>,
) -> Attribute {
    Attribute {
        name_index: pool.utf8_index("Code").expect("Code"),
        body: AttributeBody::Code(CodeAttribute {
            max_stack,
            max_locals,
            code: body,
            exception_table: Vec::new(),
            attributes: Vec::new(),
        }),
    }
}

/// `public class Empty { public Empty() { super(); } public static void main(String[] a) {} }`,
/// assembled entry by entry.
///
/// It has a `main` so a real JVM can be asked to load, verify, *and run* it — the strongest
/// available statement that the construction surface produces a well-formed class.
fn empty_class() -> ClassFile {
    let mut pool = ConstantPool::new();
    let this_class = pool.class_index("Empty").expect("this");
    let super_class = pool.class_index("java/lang/Object").expect("super");
    let object_init = pool
        .method_ref_index("java/lang/Object", "<init>", "()V")
        .expect("Object.<init>");

    let init_name = pool.utf8_index("<init>").expect("<init>");
    let init_descriptor = pool.utf8_index("()V").expect("()V");
    let init_code = code_attribute(
        &mut pool,
        1,
        1,
        vec![
            Instruction::Aload0,
            Instruction::InvokeSpecial(object_init),
            Instruction::Return,
        ],
    );

    let main_name = pool.utf8_index("main").expect("main");
    let main_descriptor = pool
        .utf8_index("([Ljava/lang/String;)V")
        .expect("main descriptor");
    // No operands are pushed and the one parameter occupies slot 0.
    let main_code = code_attribute(&mut pool, 0, 1, vec![Instruction::Return]);

    let mut class = ClassFile::new(MAJOR_JAVA_25, 0, pool);
    class.access_flags = ClassAccessFlags(ClassAccessFlags::PUBLIC | ClassAccessFlags::SUPER);
    class.this_class = this_class;
    class.super_class = super_class;
    class.methods.push(MethodInfo {
        access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC),
        name_index: init_name,
        descriptor_index: init_descriptor,
        attributes: vec![init_code],
    });
    class.methods.push(MethodInfo {
        access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC | MethodAccessFlags::STATIC),
        name_index: main_name,
        descriptor_index: main_descriptor,
        attributes: vec![main_code],
    });
    class
}

#[test]
fn a_pool_built_from_new_is_one_based() {
    let mut pool = ConstantPool::new();
    assert_eq!(
        pool.next_index(),
        1,
        "index 0 is the sentinel, never an entry"
    );
    assert_eq!(pool.get(0), None);
    assert_eq!(pool.utf8_index("first"), Some(1));
}

#[test]
fn interning_reuses_an_equal_entry() {
    let mut pool = ConstantPool::new();
    let first = pool.utf8_index("java/lang/String").expect("first");
    let after_first = pool.next_index();
    let second = pool.utf8_index("java/lang/String").expect("second");

    assert_eq!(first, second);
    assert_eq!(pool.next_index(), after_first, "the pool did not grow");

    // A `Class` interns its name `Utf8` too, so it reuses the entry already there.
    let class = pool.class_index("java/lang/String").expect("class");
    assert_eq!(
        pool.get(class),
        Some(&ConstantPoolEntry::Class { name_index: first })
    );
}

/// `0.0` and `-0.0` compare equal under `PartialEq` but are different constants, and `NaN` compares
/// unequal to itself. Interning matches bit patterns so neither case misbehaves.
#[test]
fn float_interning_matches_bit_patterns() {
    let mut pool = ConstantPool::new();
    let positive = pool.float_index(0.0).expect("0.0");
    let negative = pool.float_index(-0.0).expect("-0.0");
    assert_ne!(positive, negative, "-0.0 must not reuse 0.0's entry");

    let nan = pool.double_index(f64::NAN).expect("NaN");
    assert_eq!(
        pool.double_index(f64::NAN),
        Some(nan),
        "NaN must intern to itself rather than append every time"
    );
}

/// A `MethodRef` and an `InterfaceMethodRef` for the same member are distinct constants; a JVM
/// rejects the wrong one. They must never collapse into one entry.
#[test]
fn a_method_ref_and_an_interface_method_ref_stay_distinct() {
    let mut pool = ConstantPool::new();
    let class_form = pool
        .method_ref_index("p/Owner", "run", "()V")
        .expect("MethodRef");
    let interface_form = pool
        .interface_method_ref_index("p/Owner", "run", "()V")
        .expect("InterfaceMethodRef");
    assert_ne!(class_form, interface_form);
}

#[test]
fn a_constructed_class_survives_write_then_read() {
    let built = empty_class();
    let bytes = built.write();
    let reparsed = read(&bytes).expect("re-read what we wrote");

    assert_eq!(reparsed, built, "the model changed across write/read");
    assert_eq!(reparsed.write(), bytes, "the bytes are not a fixed point");
    assert_eq!(reparsed.major_version, MAJOR_JAVA_25);
}

/// Whether this host has a `java` that runs. Like the CLI tests, a missing JDK makes the
/// JVM-backed assertion return early rather than be marked `#[ignore]`.
fn java_available() -> bool {
    Command::new("java")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The construction surface's real acceptance test: a real JVM loads, *verifies*, and runs the
/// class. Byte-exact round-tripping only proves this crate agrees with itself; this proves the
/// bytes are a class file by the JVM's own definition.
#[test]
fn a_constructed_class_runs_on_a_real_jvm() {
    if !java_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temp dir");
    std::fs::write(directory.path().join("Empty.class"), empty_class().write()).expect("write");

    let output = Command::new("java")
        .arg("-cp")
        .arg(directory.path())
        .arg("Empty")
        .output()
        .expect("run java");

    assert!(
        output.status.success(),
        "the JVM rejected the constructed class:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The exact pool a minimal class needs, in the order the construction surface emits it. Pinned so
/// a change in interning order — which changes every emitted index — is visible rather than silent.
#[test]
fn the_pool_of_a_minimal_class_is_pinned() {
    let class = empty_class();
    let pool = &class.constant_pool;
    let rendered = (1..pool.next_index()).fold(String::new(), |mut out, index| {
        let entry = match pool.get(index) {
            // Render `Utf8` as its text; the raw modified-UTF8 bytes carry no signal here.
            Some(ConstantPoolEntry::Utf8(_)) => {
                format!("Utf8({:?})", pool.utf8(index).unwrap_or_default())
            }
            Some(entry) => format!("{entry:?}"),
            None => "<gap>".to_owned(),
        };
        // No leading whitespace: `expect!` dedents by the block's minimum indent, so a
        // right-aligned column would shift the whole snapshot when the index count changes.
        writeln!(out, "{index}: {entry}").expect("write to a String");
        out
    });

    expect![[r#"
        1: Utf8("Empty")
        2: Class { name_index: 1 }
        3: Utf8("java/lang/Object")
        4: Class { name_index: 3 }
        5: Utf8("<init>")
        6: Utf8("()V")
        7: NameAndType { name_index: 5, descriptor_index: 6 }
        8: MethodRef { class_index: 4, name_and_type_index: 7 }
        9: Utf8("Code")
        10: Utf8("main")
        11: Utf8("([Ljava/lang/String;)V")
    "#]]
    .assert_eq(&rendered);
}

/// `replace` is the one operation that changes what an index holds, so the intern index it
/// invalidates has to go with it. An intern that handed back a stale index would point a
/// `getstatic` or an `ldc` at whatever the replacement put there.
#[test]
fn replacing_an_entry_invalidates_interning() {
    let mut pool = ConstantPool::new();
    let a = pool.utf8_index("a").expect("a");
    let keep = pool.utf8_index("keep").expect("keep");

    pool.replace(
        a,
        jals_classfile::ConstantPoolEntry::Utf8(ConstantPool::encode_modified_utf8("b")),
    )
    .expect("replace");

    // The index `a` used to have now holds `b`, so interning `a` must not hand it back.
    let again = pool.utf8_index("a").expect("a again");
    assert_ne!(again, a, "`a` must not resolve to the slot `b` took over");
    assert_eq!(pool.utf8(again).as_deref(), Some("a"));
    assert_eq!(pool.utf8(a).as_deref(), Some("b"));
    // Entries the replacement did not touch keep their indices and their contents.
    assert_eq!(pool.utf8(keep).as_deref(), Some("keep"));
}

/// Interning into a pool that was *read* has to find what the file already put there. Handing back
/// a fresh index instead would append a duplicate of an entry existing references already point at
/// — legal, and silently wasteful, which is why it needs a test rather than a reader's attention.
#[test]
fn interning_into_a_read_pool_reuses_its_entries() {
    let original = {
        let mut pool = ConstantPool::new();
        pool.utf8_index("java/lang/Object").expect("utf8");
        pool.class_index("java/lang/Object").expect("class");
        let mut class = jals_classfile::ClassFile::new(MAJOR_JAVA_25, 0, pool);
        class.this_class = 2;
        class.super_class = 2;
        class.write()
    };

    let mut reparsed = read(&original).expect("reparse").constant_pool;
    let before = reparsed.next_index();
    assert_eq!(
        reparsed.class_index("java/lang/Object"),
        Some(2),
        "the `Class` the file already holds"
    );
    assert_eq!(
        reparsed.next_index(),
        before,
        "nothing was appended for an entry that was already there"
    );
}

/// The three constant-pool shapes an `invokedynamic` needs, which nothing could build before.
///
/// A `MethodType` is a descriptor with no name and no owner. A `MethodHandle` wraps a reference whose
/// *kind* has to agree with the entry it points at — an interface method needs an `InterfaceMethodRef`,
/// which is why the caller says which rather than having it inferred. An `InvokeDynamic` names the call
/// site and which `BootstrapMethods` entry computes it, and names the lambda body nowhere at all.
#[test]
fn an_invokedynamic_call_site_can_be_built() {
    let mut pool = ConstantPool::new();
    let ty = pool
        .method_type_index("()Ljava/lang/Object;")
        .expect("type");
    let virtual_handle = pool
        .method_handle_index(5, "p/Owner", "run", "()V", false)
        .expect("virtual handle");
    let interface_handle = pool
        .method_handle_index(9, "p/Iface", "run", "()V", true)
        .expect("interface handle");
    let site = pool
        .invoke_dynamic_index(0, "run", "(I)Lp/Iface;")
        .expect("call site");
    // Four distinct entries, none of them index 0 — which is not a constant-pool index at all.
    let indices = [ty, virtual_handle, interface_handle, site];
    assert!(indices.iter().all(|&index| index > 0), "{indices:?}");
    for (position, &index) in indices.iter().enumerate() {
        assert!(
            !indices[position + 1..].contains(&index),
            "distinct entries: {indices:?}"
        );
    }

    // Writing, reading, and writing again has to give the same bytes: the only proof that each tag and
    // payload survived the binary form rather than merely being accepted by it.
    let mut class = ClassFile::new(69, 0, pool);
    class.this_class = 0;
    let bytes = class.write();
    let read = jals_exec::block_on_inline(ClassFile::read(bytes.as_slice())).expect("read");
    assert_eq!(read.write(), bytes);
}
