//! Tests for the embedded `java.lang` stubs, indexed via [`ProjectIndexBuilder::with_stdlib`].
//!
//! They pin the Step-1 contract: core JDK types become real (stub-origin) project items, so a
//! reference to one resolves and its members infer — while the default [`ProjectIndex::builder`] is
//! unchanged (those types stay `external`), and a stub is never offered as a navigation target.

use jals_hir::{
    FileAnalysis, FileId, FileSemantics, ItemOrigin, ProjectIndex, Ty, TypeMismatch,
    TypeResolution, TypedFile,
};
use jals_syntax::SyntaxNode;

/// Parses each source, keeping its `SOURCE_FILE` node alive (rowan nodes are ref-counted).
fn nodes(sources: &[&str]) -> Vec<(FileId, SyntaxNode)> {
    sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(s)).syntax(),
            )
        })
        .collect()
}

/// A single-file project analysed *with the stdlib stubs*. Owns what a binding borrows.
struct Fixture {
    analysis: FileAnalysis,
    index: ProjectIndex,
}

impl Fixture {
    fn new(src: &str) -> Self {
        let node = jals_exec::block_on_inline(jals_syntax::Parse::parse(src)).syntax();
        let analysis = jals_exec::block_on_inline(FileAnalysis::of(&node));
        let index = jals_exec::block_on_inline(
            ProjectIndex::builder(&[(FileId(0), node)])
                .with_stdlib()
                .build(),
        );
        Self { analysis, index }
    }

    const fn semantics(&self) -> FileSemantics<'_> {
        self.analysis.in_project(&self.index, FileId(0))
    }
}

/// The type-mismatch diagnostics for a single-file project analysed *with the stdlib stubs*.
fn mismatches_with_stdlib(src: &str) -> Vec<TypeMismatch> {
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    jals_exec::block_on_inline(semantics.type_mismatches())
}

/// The inferred type of the first definition named `name`.
fn def_ty(typed: TypedFile<'_>, name: &str) -> Ty {
    let def = typed
        .analysis()
        .defs()
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no definition named `{name}`"));
    typed.type_of_def(def.id).clone()
}

#[test]
fn string_resolves_to_a_stdlib_project_item() {
    let src = "class C { void m() { String s = null; } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    let index = &fixture.index;

    let ty = def_ty(typed, "s");
    assert_eq!(ty.to_string(), "String");
    let id = ty
        .project_id()
        .expect("with the stubs indexed, `String` is a project (not external) type");
    assert_eq!(index.item(id).origin, ItemOrigin::Stdlib);
    assert_eq!(index.item(id).fqn.to_string(), "java.lang.String");
}

#[test]
fn string_length_infers_int() {
    // The member call sees through `String` to the stub's `int length()`.
    let src = "class C { void m() { String s = null; var n = s.length(); } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    assert_eq!(def_ty(typed, "n").to_string(), "int");
}

#[test]
fn object_is_a_supertype_of_string() {
    let fixture = Fixture::new("class C { }");
    let index = &fixture.index;
    let string_id = index
        .resolve_type_name(FileId(0), "String", None)
        .project_id()
        .expect("String is indexed");
    let object_id = index
        .resolve_type_name(FileId(0), "Object", None)
        .project_id()
        .expect("Object is indexed");
    assert!(index.is_subtype(string_id, object_id));
    // CharSequence (a stub interface) is a supertype too.
    let cs_id = index
        .resolve_type_name(FileId(0), "CharSequence", None)
        .project_id()
        .expect("CharSequence is indexed");
    assert!(index.is_subtype(string_id, cs_id));
}

#[test]
fn stdlib_symbol_goto_is_none() {
    // A stub type has no host-openable file, so go-to-definition on it yields nothing.
    let src = "class C { String f; }";
    let fixture = Fixture::new(src);
    let offset = src.find("String f").expect("type reference present");
    assert_eq!(fixture.semantics().definition_at(offset), None);
}

#[test]
fn default_build_keeps_string_external() {
    // Regression guard: without the stubs, `String` is external exactly as before.
    let src = "class C { String f; }";
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes(&[src])).build());
    assert_eq!(
        index.resolve_type_name(FileId(0), "String", None),
        TypeResolution::External,
    );
}

// --- Generic java.util containers: type arguments substitute into member types ----------------

#[test]
fn list_element_access_substitutes_the_type_argument() {
    // `List<String>.get(int)` returns `E`, bound to `String` by the receiver's argument.
    let src = "import java.util.List; class C { void m() { List<String> xs = null; var x = xs.get(0); } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    assert_eq!(def_ty(typed, "x").to_string(), "String");
}

#[test]
fn a_raw_container_leaves_its_member_type_a_variable() {
    // A raw `Map` (no arguments) does not bind `V`, so `get` stays the type variable, shown by name.
    let src = "import java.util.Map; class C { void m() { Map m = null; var v = m.get(null); } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    assert_eq!(def_ty(typed, "v").to_string(), "V");
}

#[test]
fn map_value_access_substitutes_the_second_argument() {
    let src = "import java.util.Map; class C { void m() { Map<String, Integer> m = null; var v = m.get(null); } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    assert_eq!(def_ty(typed, "v").to_string(), "Integer");
}

#[test]
fn inherited_generic_member_substitutes_through_the_supertype_chain() {
    // `ArrayList<String>` inherits `get` from `List<E>` (via `implements List<E>`); the argument must
    // propagate `E := String` down the chain.
    let src = "import java.util.ArrayList; class C { void m() { ArrayList<String> xs = null; var x = xs.get(0); } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    assert_eq!(def_ty(typed, "x").to_string(), "String");
}

#[test]
fn optional_unwraps_to_its_argument() {
    let src = "import java.util.Optional; class C { void m() { Optional<String> o = null; var v = o.get(); } }";
    let fixture = Fixture::new(src);
    let semantics = fixture.semantics();
    let typed = jals_exec::block_on_inline(semantics.typed());
    assert_eq!(def_ty(typed, "v").to_string(), "String");
}

#[test]
fn java_util_types_need_an_import_but_then_resolve_to_stubs() {
    // Without an import `List` is unresolved (java.util is not implicitly imported); with one it
    // binds to the stub item.
    let imported = "import java.util.List; class C { List<String> f; }";
    let fixture = Fixture::new(imported);
    let index = &fixture.index;
    let id = index
        .resolve_type_name(FileId(0), "List", None)
        .project_id()
        .expect("List resolves to the stub when imported");
    assert_eq!(index.item(id).fqn.to_string(), "java.util.List");
    assert_eq!(index.item(id).origin, ItemOrigin::Stdlib);
}

// --- Generic invariance: the same nominal type with differing type arguments -----------------

#[test]
fn assigning_a_list_to_a_differently_parameterized_list_is_flagged() {
    // Java generics are invariant: `List<String>` is not a `List<Object>`.
    let src = "import java.util.List; \
               class C { void m() { List<String> a = null; List<Object> b = a; } }";
    let diags = mismatches_with_stdlib(src);
    assert_eq!(diags.len(), 1, "invariant type arguments should be flagged");
}

#[test]
fn assigning_a_list_to_the_same_parameterization_is_ok() {
    let src = "import java.util.List; \
               class C { void m() { List<String> a = null; List<String> b = a; } }";
    assert!(mismatches_with_stdlib(src).is_empty());
}

#[test]
fn assigning_to_a_supertype_with_arguments_stays_lenient() {
    // `List<String>` → `Collection<String>` is a legal widening; a different nominal type stays
    // lenient on its arguments, so nothing is flagged.
    let src = "import java.util.List; import java.util.Collection; \
               class C { void m() { List<String> a = null; Collection<String> c = a; } }";
    assert!(mismatches_with_stdlib(src).is_empty());
}

#[test]
fn a_raw_target_is_lenient_about_arguments() {
    let src = "import java.util.List; \
               class C { void m() { List<String> a = null; List r = a; } }";
    assert!(mismatches_with_stdlib(src).is_empty());
}

#[test]
fn a_user_generic_type_is_invariant_too() {
    // The invariance check is not stdlib-specific: a user `Box<T>` behaves the same way.
    let src = "class Box<T> {} \
               class C { void m() { Box<String> a = null; Box<Integer> b = a; } }";
    let diags = mismatches_with_stdlib(src);
    assert_eq!(diags.len(), 1, "user-generic invariance should be flagged");
}

#[test]
fn nested_type_arguments_are_compared_invariantly() {
    let src = "import java.util.List; \
               class C { void m() { List<List<String>> a = null; List<List<Object>> b = a; } }";
    let diags = mismatches_with_stdlib(src);
    assert_eq!(diags.len(), 1, "nested invariance should be flagged");
}

// --- Stub types are treated leniently in type checking (no false mismatch) -------------------
//
// The stubs carry only a partial hierarchy and member set, so assignment conversion demotes a
// stub-origin type to its external (lenient) form. Inference / hover still use the precise stub
// (see the tests above); only the *checking* must not flag what the partial model cannot disprove.

#[test]
fn autoboxing_to_a_wrapper_is_not_flagged() {
    // `int` → `Integer` is boxing, legal in Java; with `Integer` a stub project type, the demotion
    // keeps the primitive/external boxing rule lenient instead of reporting a mismatch.
    assert!(mismatches_with_stdlib("class C { void m() { Integer n = 1; } }").is_empty());
}

#[test]
fn unboxing_to_a_primitive_is_not_flagged() {
    let src = "class C { void m() { Integer i = null; int n = i; } }";
    assert!(mismatches_with_stdlib(src).is_empty());
}

#[test]
fn assigning_to_a_stub_supertype_is_not_flagged() {
    // `Integer` really is `Comparable`, but the stub `Integer` does not list it, so a precise
    // subtype check would be a false positive. The stub demotion makes it lenient.
    let src = "class C { void m() { Integer i = null; Comparable c = i; } }";
    assert!(mismatches_with_stdlib(src).is_empty());
}

#[test]
fn calling_a_stub_method_with_an_odd_argument_is_not_flagged() {
    // `String.charAt(int)` called with a `String`: a stub parameter type is external (lenient), so
    // no spurious argument / no-overload diagnostic. (Inference of `charAt` still works; see above.)
    let src = "class C { void m() { String s = null; s.charAt(s); } }";
    assert!(mismatches_with_stdlib(src).is_empty());
}

#[test]
fn a_stub_types_method_set_is_never_complete() {
    // A stub carries only the common members, so its overload set is treated as incomplete — the
    // guard that keeps `check_call` from ever concluding "no overload" against a JDK type.
    let fixture = Fixture::new("class C {}");
    let index = &fixture.index;
    let string_id = index
        .resolve_type_name(FileId(0), "String", None)
        .project_id()
        .expect("String is indexed");
    assert!(!index.method_set_complete(string_id, "concat"));
}

#[test]
fn a_real_primitive_narrowing_is_still_flagged() {
    // Demotion must not suppress the precise rules: a `double` into an `int` slot is a genuine,
    // un-rescuable narrowing and is still reported, stubs or not.
    let diags = mismatches_with_stdlib("class C { void m() { int x = 1.0; } }");
    assert_eq!(
        diags.len(),
        1,
        "double → int narrowing should still be flagged"
    );
}

#[test]
fn builder_with_stdlib_never_panics_and_project_items_are_in_bounds() {
    let sources = [
        "",
        "class C { String s; void m() { var n = s.length(); } }",
        "package a; import java.util.List; class D extends Object { }",
        "🦀 class Broken { int (}",
    ];
    let nodes = nodes(&sources);
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_stdlib().build());
    // Every *project* item's name range stays within its source; stub items live at reserved high
    // file ids and are excluded from this host-source bounds check.
    for (_, item) in index
        .items()
        .filter(|(_, it)| it.origin == ItemOrigin::Project)
    {
        let src = sources[item.file.0 as usize];
        assert!(
            item.name_range.end <= src.len(),
            "project item {} out of bounds",
            item.fqn
        );
    }
}

#[test]
fn every_java_lang_stub_resolves_without_the_stubs_too() {
    // Drift guard for the two lists that answer the same question. `ProjectIndex::is_java_lang` is
    // what a *default*-built index resolves an unimported `java.lang` name through; the stubs are
    // what a `with_stdlib` one binds it to. A type the stubs declare and that list omits therefore
    // makes one source report `cannot-resolve` in one build and not the other — which is how
    // `AssertionError` came to resolve with the stubs indexed and not without them.
    let fixture = Fixture::new("class C { }");
    let declared: Vec<String> = fixture
        .index
        .items()
        .filter(|(_, item)| item.origin == ItemOrigin::Stdlib)
        .filter_map(|(_, item)| {
            let fqn = item.fqn.to_string();
            // Top-level `java.lang` types only: a nested one is never named bare.
            let name = fqn.strip_prefix("java.lang.")?;
            (!name.contains('.')).then(|| name.to_owned())
        })
        .collect();
    assert!(
        declared.len() > 30,
        "expected the java.lang stubs to be enumerable: {declared:?}"
    );

    let bare = jals_exec::block_on_inline(ProjectIndex::builder(&nodes(&["class C { }"])).build());
    for name in declared {
        assert_eq!(
            bare.resolve_type_name(FileId(0), &name, None),
            TypeResolution::External,
            "`{name}` is declared by the java.lang stubs but missing from the implicit-import list",
        );
    }
}
