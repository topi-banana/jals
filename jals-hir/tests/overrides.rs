//! The override relation, and the field lookup a layout walks the superclass chain for.
//!
//! These are not checking questions. Nothing here decides whether a program is legal — it decides
//! whether a bridge method is written and which function a virtual call reaches, for a program
//! already assumed to be. A wrong answer is emitted output that loads and runs, not a message.
//!
//! Every case below is a defect that was fixed once already, in a backend, where the only coverage
//! was an end-to-end run against a real JVM or wasm engine. Those runs stand down on a host with no
//! JDK and in CI's `wasm32-wasip1` cell, which is why the rule now lives on the index and is
//! verified here instead, with no host in reach.

use jals_hir::{DefKind, FileId, ItemId, MemberId, MemberType, Overrides, ProjectIndex};
use jals_syntax::SyntaxNode;

/// The platform library at **signature** fidelity — what every host but a linking wasm build
/// indexes, and what the embedded stubs used to be.
///
/// One text, read as a record: the real JDK behind a `javac` build is a superset of it, so a
/// member it omits is a gap in the record rather than an absence in the program.
fn platform() -> Vec<jals_hir::LibraryFile> {
    jals_exec::block_on_inline(jals_hir::LibraryFile::parse_tiers(
        &jals_platform::JavaBase::tiers(false),
    ))
}

/// Parses each source (keeping the `SOURCE_FILE` nodes alive) and builds a [`ProjectIndex`].
///
/// The embedded stdlib stubs are folded in because `java.lang.Object` has to be an *indexed* type
/// for the questions here: `equals(Object)` is the inherited member several cases override, and the
/// implicit `Object` edge is what makes `is_subtype` reach it. Without them those tests would pass
/// for the wrong reason — no `Object.equals` found at all.
fn build(sources: &[&str]) -> (Vec<(FileId, SyntaxNode)>, ProjectIndex) {
    let nodes: Vec<(FileId, SyntaxNode)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                FileId(u32::try_from(i).unwrap()),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(s)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&nodes).with_library(&platform()).build());
    (nodes, index)
}

/// The [`ItemId`] of the top-level type named `name`.
///
/// By fully-qualified name rather than by declaration offset: every fixture here declares its types
/// in the default package, so the name *is* the FQN, and a generic declaration (`class C<U extends
/// Number>`) has no offset a test can spell without restating the type-parameter list.
fn item(index: &ProjectIndex, name: &str) -> ItemId {
    index
        .item_by_fqn(name)
        .unwrap_or_else(|| panic!("no indexed type named `{name}`"))
}

/// The spelling of a declared type, as a test writes it: `int`, `Leaf`, `Leaf[]`, `T`.
///
/// Matching on the spelling rather than on a resolved type is deliberate — it is what lets a test
/// address one of two same-arity overloads, which is the case the old name-and-arity rule could not
/// see and half of this file exists for.
fn spelling(ty: &MemberType) -> String {
    let (base, dims) = match ty {
        MemberType::Void => return "void".to_owned(),
        MemberType::Unknown => return "?".to_owned(),
        MemberType::Primitive { keyword, dims } => (keyword.clone(), *dims),
        MemberType::Named { name, dims, .. } => (name.clone(), *dims),
    };
    let mut out = base;
    for _ in 0..dims {
        out.push_str("[]");
    }
    out
}

/// The method `owner` *declares* named `name` whose parameters are spelled `params`.
fn method(index: &ProjectIndex, owner: ItemId, name: &str, params: &[&str]) -> MemberId {
    index
        .own_members(owner)
        .iter()
        .copied()
        .find(|&id| {
            let m = index.member(id);
            m.kind == DefKind::Method
                && m.name == name
                && m.params.len() == params.len()
                && m.params
                    .iter()
                    .zip(params)
                    .all(|(p, want)| spelling(&p.ty) == *want)
        })
        .unwrap_or_else(|| panic!("no `{name}({})` declared on the type", params.join(", ")))
}

/// `java.lang.Object`'s `equals(Object)`, from the folded-in stubs.
fn object_equals(index: &ProjectIndex) -> MemberId {
    let object = index
        .item_by_fqn("java.lang.Object")
        .expect("the stdlib stubs index java.lang.Object");
    method(index, object, "equals", &["Object"])
}

// ---------------------------------------------------------------------------------------------
// The relation
// ---------------------------------------------------------------------------------------------

/// JLS §8.4.8.1. The two questions differ, and only one of them is the one a dispatch asks.
///
/// `Base` is no subtype of `I`, so asking about `Base`'s own declaring type answers `No` — correctly,
/// and about the wrong thing. The implementation `C` supplies for `I.f` *is* `Base.f`. A backend read
/// the `No` as "nothing in this module implements it" and emitted a trap against a receiver whose
/// body was one function away.
#[test]
fn an_implementation_inherited_from_a_third_type_is_the_override() {
    let sources = [
        "interface I { int f(); } class Base { public int f() { return 1; } } class C extends Base implements I {}",
    ];
    let (_nodes, index) = build(&sources);
    let i = item(&index, "I");
    let base = item(&index, "Base");
    let c = item(&index, "C");

    let i_f = method(&index, i, "f", &[]);
    let base_f = method(&index, base, "f", &[]);

    assert_eq!(index.implements_for(c, base_f, i_f), Overrides::Yes);
    // The same pair, asked about `Base` instead of `C`: still `No`, and still the wrong question.
    assert_eq!(index.overrides(base_f, i_f), Overrides::No);
}

/// The case name-and-arity could not see. `Holder<Leaf>` binds `T := Leaf`, so `put(Leaf)` overrides
/// and `put(int)` does not — two arity-1 methods of one name, indistinguishable without the
/// substitution.
#[test]
fn a_same_arity_overload_is_not_an_override() {
    let sources = [
        "class Leaf {} interface Holder<T> { void put(T x); } class Box implements Holder<Leaf> { public void put(Leaf x) {} public void put(int x) {} }",
    ];
    let (_nodes, index) = build(&sources);
    let holder = item(&index, "Holder");
    let box_ = item(&index, "Box");
    let put_t = method(&index, holder, "put", &["T"]);

    assert_eq!(
        index.overrides(method(&index, box_, "put", &["Leaf"]), put_t),
        Overrides::Yes
    );
    assert_eq!(
        index.overrides(method(&index, box_, "put", &["int"]), put_t),
        Overrides::No
    );
}

/// A bounded type variable is not the parameter a concrete type is.
///
/// `C<T extends Number>.equals(T)` is an *overload* of `Object.equals(Object)`, and javac says so by
/// emitting no bridge. Answering otherwise wrote one that cast, so
/// `((Object) new C<Integer>()).equals("hello")` threw `ClassCastException` where javac returns
/// `false`.
#[test]
fn a_bounded_type_variable_parameter_is_an_overload() {
    let sources = [
        "class Number {} class C<T extends Number> { public boolean equals(T o) { return true; } }",
    ];
    let (_nodes, index) = build(&sources);
    let c = item(&index, "C");
    let own = method(&index, c, "equals", &["T"]);

    assert_eq!(index.overrides(own, object_equals(&index)), Overrides::No);
}

/// A type variable threaded through a supertype *is* the same parameter.
///
/// `I<U>` binds `I`'s `T` to `C`'s own `U`, and `C.f(U)` is written with that same variable — so the
/// two are one parameter and this is a definite override.
///
/// **This answer improved with the move.** The backend copy converted the substituted `U` through
/// its own private lowering, which resolved the *name* `U` against the project, found nothing, and
/// produced an unindexed external type — so the comparison saw a variable against a name and had to
/// stay lenient (`Unknown`) rather than drop a bridge the override genuinely needs. This crate's
/// converter yields the variable itself, so the pair is now decidable. The strict consumer (virtual
/// dispatch) therefore finds an overrider it previously missed, which is the same family of defect
/// as [`an_implementation_inherited_from_a_third_type_is_the_override`].
#[test]
fn a_type_variable_threaded_through_a_supertype_is_the_same_parameter() {
    let sources = [
        "class Number {} interface I<T> { void f(T x); } class C<U extends Number> implements I<U> { public void f(U x) {} }",
    ];
    let (_nodes, index) = build(&sources);
    let i = item(&index, "I");
    let c = item(&index, "C");

    assert_eq!(
        index.overrides(
            method(&index, c, "f", &["U"]),
            method(&index, i, "f", &["T"])
        ),
        Overrides::Yes
    );
}

/// The substitution recurses into array element types: `T[]` with `T := Leaf` is `Leaf[]`.
#[test]
fn an_array_parameter_is_compared_element_by_element() {
    let sources = [
        "class Leaf {} class Other {} interface Slot<T> { void take(T[] k); } class Cell implements Slot<Leaf> { public void take(Leaf[] k) {} public void take(Other[] k) {} }",
    ];
    let (_nodes, index) = build(&sources);
    let slot = item(&index, "Slot");
    let cell = item(&index, "Cell");
    let take_t = method(&index, slot, "take", &["T[]"]);

    assert_eq!(
        index.overrides(method(&index, cell, "take", &["Leaf[]"]), take_t),
        Overrides::Yes
    );
    assert_eq!(
        index.overrides(method(&index, cell, "take", &["Other[]"]), take_t),
        Overrides::No
    );
}

/// The substitution composes along the whole path, not just the first edge: `B<Leaf>` binds `B`'s
/// `U`, which `B extends A<U>` carries into `A`'s `T`.
#[test]
fn a_substitution_composes_along_the_path() {
    let sources = [
        "class Leaf {} interface A<T> { void f(T x); } interface B<U> extends A<U> {} class C implements B<Leaf> { public void f(Leaf x) {} }",
    ];
    let (_nodes, index) = build(&sources);
    let a = item(&index, "A");
    let c = item(&index, "C");

    assert_eq!(
        index.overrides(
            method(&index, c, "f", &["Leaf"]),
            method(&index, a, "f", &["T"])
        ),
        Overrides::Yes
    );
}

/// The three shapes the declaration itself rules out, before any type is compared.
///
/// A `static` method *hides* rather than overrides (JLS §8.4.8.2), a `private` one is not inherited
/// at all even though the member walk still lists it, and two members of one owner are an overload —
/// which is precisely the case the old name-and-arity rule accepted.
#[test]
fn the_declaration_shapes_that_rule_an_override_out() {
    let sources = [
        "class A { static void s() {} private void p() {} void q() {} void q(int x) {} }
         class B extends A { static void s() {} void p() {} }",
    ];
    let (_nodes, index) = build(&sources);
    let a = item(&index, "A");
    let b = item(&index, "B");

    // `static` on either side hides.
    assert_eq!(
        index.overrides(method(&index, b, "s", &[]), method(&index, a, "s", &[])),
        Overrides::No
    );
    // A `private` inherited member is not inherited.
    assert_eq!(
        index.overrides(method(&index, b, "p", &[]), method(&index, a, "p", &[])),
        Overrides::No
    );
    // Two members of one owner are an overload, whatever their parameters say.
    assert_eq!(
        index.overrides(
            method(&index, a, "q", &[]),
            method(&index, a, "q", &["int"])
        ),
        Overrides::No
    );
}

/// A raw supertype supplies no arguments, so its parameters stay type variables.
///
/// Against a reference type nothing is decidable — the variable could yet be instantiated to it.
/// Against a **primitive** it is decidable and the answer is `No`: no instantiation of a type
/// parameter is `int`, so `put(int)` cannot override a raw `Holder`'s `put`, whose erasure is
/// `put(Object)`.
///
/// The backend copy bound every parameter of a raw supertype to "unreadable" instead of leaving it a
/// variable, which lost that second answer and wrote a bridge javac does not write. It is the only
/// place the move changes an answer, and this is the direction it changes in.
#[test]
fn a_raw_supertype_leaves_its_parameters_unbound() {
    let sources = [
        "class Leaf {} interface Holder<T> { void put(T x); } class C implements Holder { public void put(int x) {} public void put(Leaf x) {} }",
    ];
    let (_nodes, index) = build(&sources);
    let holder = item(&index, "Holder");
    let c = item(&index, "C");
    let put_t = method(&index, holder, "put", &["T"]);

    assert_eq!(
        index.overrides(method(&index, c, "put", &["int"]), put_t),
        Overrides::No
    );
    assert_eq!(
        index.overrides(method(&index, c, "put", &["Leaf"]), put_t),
        Overrides::Unknown
    );
}

/// The two policies, named. Comparing against a variant instead is how a fourth answer added later
/// would land on one side of each with nothing failing to compile.
#[test]
fn the_three_answers_collapse_oppositely() {
    assert!(Overrides::Yes.is_certain() && Overrides::Yes.is_possible());
    assert!(!Overrides::No.is_certain() && !Overrides::No.is_possible());
    // The whole reason the fact has three answers rather than two.
    assert!(!Overrides::Unknown.is_certain() && Overrides::Unknown.is_possible());
}

// ---------------------------------------------------------------------------------------------
// The field lookup a layout walks the superclass chain for
// ---------------------------------------------------------------------------------------------

/// Nearest-first, so a shadowing field wins — which is also the order the slot layout relies on.
#[test]
fn a_shadowing_field_wins_over_the_one_it_hides() {
    let sources = ["class A { int x; } class B extends A { int x; } class C extends B {}"];
    let (_nodes, index) = build(&sources);
    let b = item(&index, "B");
    let c = item(&index, "C");

    let found = index
        .inherited_field(c, "x")
        .expect("`x` is reachable from C");
    assert_eq!(index.member(found).owner, b);
}

/// Interface edges are not followed. An interface's `static final` constant is reached on the
/// interface and occupies no slot, so answering with it would hand a layout a member that has no
/// place in it.
#[test]
fn an_interface_constant_is_not_reached_through_the_superclass_chain() {
    let sources = ["interface K { int c = 1; } class D implements K {}"];
    let (_nodes, index) = build(&sources);
    let d = item(&index, "D");

    assert_eq!(index.inherited_field(d, "c"), None);
}

/// A supertype cycle parses and indexes, so the walk has to terminate on one. It used to be bounded
/// by an arbitrary depth in the backend; visiting each type once is the same guarantee without the
/// number, and answers for a chain deeper than that number besides.
#[test]
fn a_cyclic_superclass_chain_terminates() {
    let sources = ["class A extends B {} class B extends A {}"];
    let (_nodes, index) = build(&sources);
    let a = item(&index, "A");

    assert_eq!(index.inherited_field(a, "missing"), None);
}

/// A cycle in the *substitution* walk terminates too, and for the same reason.
#[test]
fn a_cyclic_hierarchy_still_answers_the_relation() {
    let sources =
        ["class A extends B { public void f() {} } class B extends A { public void f() {} }"];
    let (_nodes, index) = build(&sources);
    let a = item(&index, "A");
    let b = item(&index, "B");

    // The answer itself is uninteresting on a malformed hierarchy; not hanging is the assertion.
    let _ = index.overrides(method(&index, a, "f", &[]), method(&index, b, "f", &[]));
}

// ---------------------------------------------------------------------------
// The hierarchy walk the rules above are built on, and the two edge answers it
// is distinct from. Published because five consumers wrote this visited set
// themselves; two shipped without one, and they wedged the editor and aborted
// the process on input that parses and indexes perfectly.
// ---------------------------------------------------------------------------

/// The chain is nearest-first and does not lead with its own start.
///
/// Pinned as a sequence, not as membership: the order is what makes a shadowing field win in
/// `inherited_field` and what makes a multi-catch join answer the *nearest* shared class, and until
/// now nothing asserted it.
#[test]
fn the_superclass_chain_is_nearest_first_and_excludes_its_start() {
    let sources = ["class Base {} class Mid extends Base {} class Leaf extends Mid {}"];
    let (_nodes, index) = build(&sources);
    let object = item(&index, "java.lang.Object");

    let chain: Vec<ItemId> = index.superclasses(item(&index, "Leaf")).collect();
    assert_eq!(
        chain,
        [item(&index, "Mid"), item(&index, "Base"), object],
        "nearest first, `Leaf` itself absent, ending at the indexed `Object`"
    );
}

/// A supertype cycle parses and indexes, so the walk has to terminate on one — by visiting each type
/// once, which also answers for a chain deeper than any fixed bound.
#[test]
fn a_cyclic_superclass_chain_yields_each_type_once() {
    let sources = ["class A extends B {} class B extends A {}"];
    let (_nodes, index) = build(&sources);

    assert_eq!(
        index.superclasses(item(&index, "A")).collect::<Vec<_>>(),
        [item(&index, "B")]
    );
    assert_eq!(
        index.superclasses(item(&index, "B")).collect::<Vec<_>>(),
        [item(&index, "A")]
    );
}

/// The one input on which the step and the walk disagree, asserted together so the divergence is
/// documented where a reader of either will meet it.
///
/// `ClassFile.super_class` emits the edge the source wrote, cycle or not, so the step must keep
/// answering `Some(C)`. A chain that re-entered its own start would report `C` as its own ancestor.
#[test]
fn a_self_referential_superclass_is_not_its_own_superclass() {
    let sources = ["class C extends C {}"];
    let (_nodes, index) = build(&sources);
    let c = item(&index, "C");

    assert_eq!(index.direct_superclass(c), Some(c), "the written edge");
    assert!(
        index.superclasses(c).next().is_none(),
        "the chain does not re-enter its start"
    );
}

/// A type's own field is found before the chain is walked at all.
///
/// `inherited_field` leads with `from` because `superclasses` excludes its start, and nothing
/// asserted that: every shadowing case in this file declares the field on two *ancestors* and asks
/// from a third, so the answer comes from the chain either way and dropping the head passes.
#[test]
fn a_type_finds_its_own_field_before_any_inherited_one() {
    let sources = ["class Base { int x; } class Sub extends Base { int x; }"];
    let (_nodes, index) = build(&sources);
    let (base, sub) = (item(&index, "Base"), item(&index, "Sub"));

    let found = index.inherited_field(sub, "x").expect("`Sub` declares `x`");
    assert_eq!(
        index.member(found).owner,
        sub,
        "`Sub`'s own `x` shadows `Base`'s"
    );
    assert_eq!(
        index
            .member(
                index
                    .inherited_field(base, "x")
                    .expect("`Base` declares `x`")
            )
            .owner,
        base,
        "and `Base` still answers with its own"
    );
}

/// The chain follows one kind of edge where `is_subtype` follows every kind.
///
/// An interface is a supertype and is not a superclass; a walk that conflated the two would put a
/// `catch` type's interfaces into a multi-catch join and a layout's slots onto an interface.
#[test]
fn the_chain_follows_no_interface() {
    let sources = ["interface I {} class B {} class C extends B implements I {}"];
    let (_nodes, index) = build(&sources);
    let (c, i) = (item(&index, "C"), item(&index, "I"));

    assert!(index.is_subtype(c, i), "C is a subtype of I");
    assert!(
        !index.superclasses(c).any(|id| id == i),
        "and I is still not on C's superclass chain"
    );
}

/// An `@interface` is an interface (JLS §9.6), and both edge answers have to agree about that.
///
/// The regression this pins: the JVM lowering selected interfaces *negatively* (`kind != Interface`),
/// so it skipped an `AnnotationType` edge, while the positive superclass filter did not claim it
/// either — the edge went missing from the emitted class file entirely.
#[test]
fn an_annotation_type_is_a_direct_interface_and_not_a_superclass() {
    let sources = ["@interface Marker {} class C implements Marker {}"];
    let (_nodes, index) = build(&sources);
    let c = item(&index, "C");

    assert_eq!(
        index.direct_interfaces(c).collect::<Vec<_>>(),
        [item(&index, "Marker")]
    );
    assert_eq!(
        index.direct_superclass(c),
        Some(item(&index, "java.lang.Object"))
    );
}

/// An `@interface` is a reference type, so it carries the implicit `java.lang.Object` edge that
/// every other type-declaration kind does.
///
/// Leaving `DefKind::AnnotationType` out of the builder's kind list was the same silent
/// reclassification the `kind != Interface` filters were, one layer down: without the edge
/// `Object o = someAnnotation;` is a mismatch and `someAnnotation.toString()` an unresolved name,
/// on legal Java, while the backend emits the type with `ACC_INTERFACE` and the `Annotation`
/// superinterface.
#[test]
fn an_annotation_type_is_a_subtype_of_object_like_every_other_reference_type() {
    let sources = ["@interface Marker {} interface I {}"];
    let (_nodes, index) = build(&sources);
    let (marker, iface) = (item(&index, "Marker"), item(&index, "I"));
    let object = item(&index, "java.lang.Object");

    for (name, id) in [("Marker", marker), ("I", iface)] {
        assert!(index.is_subtype(id, object), "{name} is an Object");
        assert_eq!(
            index.superclasses(id).collect::<Vec<_>>(),
            [object],
            "{name}'s chain is the implicit Object edge"
        );
        assert!(
            index
                .resolve_member(id, "toString", jals_hir::Namespace::Method)
                .is_some(),
            "{name} inherits Object's public instance methods"
        );
    }
}

/// `ClassFile.interfaces` is emitted from this in sequence, and a generic type's `Signature` is
/// written against the same sequence — so the order is contract, not incidental.
#[test]
fn direct_interfaces_keeps_the_order_the_source_wrote() {
    let sources = ["interface A {} interface B {} class C implements B, A {}"];
    let (_nodes, index) = build(&sources);

    assert_eq!(
        index
            .direct_interfaces(item(&index, "C"))
            .collect::<Vec<_>>(),
        [item(&index, "B"), item(&index, "A")],
        "declaration order, not resolution order"
    );
}

/// Every supertype the source *wrote* is claimed by exactly one of the two edge answers, and an
/// `@interface` by the interface half — which is the property both filters are written positively
/// for.
///
/// Deliberately not called a partition of the type's edges: it is not one. `direct_superclass` is a
/// `.find`, so `C`'s implicit `java.lang.Object` edge — which it carries beside the written
/// `extends B` — is claimed by neither answer. `ProjectIndex::direct_superclass`'s doc says so, and
/// the edge list it is a claim about is crate-private, so the whole-edge-set assertion lives beside
/// the builder in `project.rs` instead.
#[test]
fn the_two_edge_answers_claim_every_written_supertype() {
    let sources = [
        "@interface Marker {} interface I {} class B {} class C extends B implements I, Marker {}",
    ];
    let (_nodes, index) = build(&sources);
    let c = item(&index, "C");

    let mut edges: Vec<ItemId> = index.direct_superclass(c).into_iter().collect();
    edges.extend(index.direct_interfaces(c));
    edges.sort_unstable();
    let mut expected = vec![item(&index, "B"), item(&index, "I"), item(&index, "Marker")];
    expected.sort_unstable();
    assert_eq!(
        edges, expected,
        "every written edge is claimed exactly once"
    );
}

/// The join answers the nearest class every entry reaches, and says nothing when it cannot.
#[test]
fn common_superclass_answers_the_nearest_shared_class() {
    let sources = ["class Base {} class L extends Base {} class R extends Base {}"];
    let (_nodes, index) = build(&sources);
    let (base, l, r) = (item(&index, "Base"), item(&index, "L"), item(&index, "R"));

    assert_eq!(index.common_superclass(&[l, r]), Some(base));
    assert_eq!(
        index.common_superclass(&[l]),
        Some(l),
        "a single entry is itself"
    );
    assert_eq!(index.common_superclass(&[]), None);
}

/// A shared *interface* is not the answer, even when it is the only thing both entries have in
/// common besides `Object`.
///
/// A class file writes the erasure of a multi-catch's lub, and that erasure is a class — no
/// descriptor records the interfaces. Nothing asserted this before, in either of the two places
/// that computed it.
#[test]
fn common_superclass_ignores_a_shared_interface() {
    let sources = ["interface I {} class L implements I {} class R implements I {}"];
    let (_nodes, index) = build(&sources);

    assert_eq!(
        index.common_superclass(&[item(&index, "L"), item(&index, "R")]),
        Some(item(&index, "java.lang.Object")),
        "the shared class, not the shared interface"
    );
}

/// The join terminates on a cycle rather than oscillating — the defect that hung the editor when
/// this existed twice and neither copy had a visited set.
#[test]
fn common_superclass_terminates_on_a_cycle() {
    let sources = ["class A extends B {} class B extends A {} class Z {}"];
    let (_nodes, index) = build(&sources);

    // The answer on a malformed hierarchy is uninteresting; returning at all is the assertion.
    let _ = index.common_superclass(&[item(&index, "A"), item(&index, "Z")]);
}

/// `ClassTy::Project::name` is the **simple** name, which is the fact three hand-written copies of
/// this construction each re-derived from the FQN.
#[test]
fn item_ty_names_the_type_by_its_simple_name() {
    let sources = ["class Outer { class Inner {} }"];
    let (_nodes, index) = build(&sources);

    let ty = index.item_ty(item(&index, "Outer.Inner"));
    assert_eq!(ty.to_string(), "Inner", "not the qualified `Outer.Inner`");
}
