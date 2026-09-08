//! Every published item, driven.
//!
//! `cargo hawk check` excludes this crate the way it excludes `jinja`: the API is sized by what a
//! *package author* is offered, not by what the packages in this workspace happen to call. So this
//! file is what holds the surface honest instead — a published item lands with the test that drives
//! it, or it lands unreachable with nothing reporting it.

use std::cell::RefCell;
use std::rc::Rc;

use jals_native::{
    Args, JavaPackage, NativeBindings, NativeError, NativeHost, NativeValue, PackageResolver,
    PackageSelection, Provenance, RefSlot, ResolveError, ResolverChain, Results, SourceKind,
    SourceResolver, StaticResolver, java_package,
};

/// A host that answers over one `i32` array, and records every export it was asked to call.
///
/// The whole `NativeHost` seam, in the smallest thing that can implement it — which is also what a
/// package author writes to test a binding without an engine.
#[derive(Default)]
struct FakeHost {
    array: RefCell<Vec<i32>>,
    calls: RefCell<Vec<String>>,
}

impl FakeHost {
    const ARRAY: RefSlot = RefSlot::new(0);

    fn with(values: &[i32]) -> Self {
        Self {
            array: RefCell::new(values.to_vec()),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl NativeHost for FakeHost {
    fn array_len(&mut self, slot: RefSlot) -> Result<u32, NativeError> {
        if slot != Self::ARRAY {
            return Err(NativeError::NotAnArray);
        }
        Ok(u32::try_from(self.array.borrow().len()).expect("a small fixture"))
    }

    fn array_get(&mut self, slot: RefSlot, index: u32) -> Result<NativeValue, NativeError> {
        let len = self.array_len(slot)?;
        self.array
            .borrow()
            .get(index as usize)
            .map(|value| NativeValue::I32(*value))
            .ok_or(NativeError::OutOfBounds { index, len })
    }

    fn array_set(
        &mut self,
        slot: RefSlot,
        index: u32,
        value: NativeValue,
    ) -> Result<(), NativeError> {
        let len = self.array_len(slot)?;
        let value = value
            .as_i32()
            .ok_or_else(|| NativeError::argument(0, "an i32", value))?;
        *self
            .array
            .borrow_mut()
            .get_mut(index as usize)
            .ok_or(NativeError::OutOfBounds { index, len })? = value;
        Ok(())
    }

    fn call_export(
        &mut self,
        name: &str,
        _args: &[NativeValue],
        results: &mut [NativeValue],
    ) -> Result<(), NativeError> {
        self.calls.borrow_mut().push(name.to_owned());
        for slot in results.iter_mut() {
            *slot = NativeValue::I32(1);
        }
        Ok(())
    }
}

/// The state a declared package's bindings write through.
trait Clock {
    fn millis(&self) -> i64;
}

struct Fixed(i64);

impl Clock for Fixed {
    fn millis(&self) -> i64 {
        self.0
    }
}

java_package! {
    /// A package declared the way a package author declares one: both halves, one invocation.
    pub Demo {
        name: "demo.clock",
        version: 2,
        host: dyn Clock,
        root: "java",
        signatures: ["demo/Marker.java"],
        implementation: ["demo/Clock.java"],
        bind: Demo::install,
    }
}

impl Demo {
    fn install(package: &mut JavaPackage, host: &Rc<dyn Clock>) {
        let clock = Rc::clone(host);
        package.bind("demo/Clock", "now()J", move |_, _, mut out| {
            out.set(0, NativeValue::I64(clock.millis()));
            Ok(())
        });
    }
}

fn demo(millis: i64) -> JavaPackage {
    Demo::package(Rc::new(Fixed(millis)) as Rc<dyn Clock>)
}

/// A package is one value holding both halves: the Java it publishes and the Rust behind that
/// Java's `native` methods, keyed by the two strings the compiler writes into the import section.
#[test]
fn a_package_holds_the_java_and_the_rust_behind_it() {
    let mut package = JavaPackage::new("demo", 3);
    package.source(
        "demo/Answer.java",
        "package demo; public final class Answer {}",
        SourceKind::Implementation,
    );
    package.bind(
        "demo/Answer",
        "compute()I",
        |_, _, mut results: Results<'_>| {
            results.set(0, NativeValue::I32(42));
            Ok(())
        },
    );

    assert_eq!(package.name(), "demo");
    assert_eq!(package.version(), 3);
    assert_eq!(package.sources().len(), 1);
    assert_eq!(package.sources()[0].path, "demo/Answer.java");
    assert_eq!(package.sources()[0].kind, SourceKind::Implementation);
    assert!(package.sources()[0].text.contains("class Answer"));
    assert_eq!(package.binding_count(), 1);

    let keys: Vec<(&str, &str)> = package
        .bindings()
        .map(|(owner, signature, _)| (owner, signature))
        .collect();
    assert_eq!(keys, vec![("demo/Answer", "compute()I")]);
    assert!(format!("{package:?}").contains("demo"));
}

/// The declaration macro puts both halves in one place, and hands back the Java with **no host
/// constructed** — which is what an index needs and what a language server has.
#[test]
fn the_declaration_macro_carries_both_halves_and_a_host_free_source_list() {
    assert_eq!(Demo::NAME, "demo.clock");
    assert_eq!(Demo::VERSION, 2);

    // Reachable as a constant: no `Rc`, no trait object, no state.
    let kinds: Vec<(&str, SourceKind)> = Demo::SOURCES.iter().map(|s| (s.path.as_ref(), s.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            ("demo/Marker.java", SourceKind::Signatures),
            ("demo/Clock.java", SourceKind::Implementation),
        ]
    );
    assert!(Demo::SOURCES[1].text.contains("native long now()"));

    // Signature-tier units come first, whatever order the declaration listed them in, so a
    // consumer's ordering never depends on how somebody wrote the macro.
    assert_eq!(Demo::SOURCES[0].kind, SourceKind::Signatures);

    let package = demo(7);
    assert_eq!(package.name(), Demo::NAME);
    assert_eq!(package.version(), Demo::VERSION);
    assert_eq!(package.sources().len(), Demo::SOURCES.len());
    assert_eq!(package.binding_count(), 1);
}

/// The binding is what runs, and it reads its arguments and writes its results through the same
/// two readers every package uses.
#[test]
fn a_binding_reads_its_arguments_and_writes_its_results() {
    let mut package = JavaPackage::new("demo", 1);
    package.bind(
        "demo/Math",
        "sum([CIJFD)I",
        |host, args: Args<'_>, mut out| {
            assert_eq!(args.len(), 5);
            assert!(!args.is_empty());
            assert_eq!(args.i64(2)?, 7);
            assert!((args.f32(3)? - 1.5).abs() < f32::EPSILON);
            assert!((args.f64(4)? - 2.5).abs() < f64::EPSILON);
            assert_eq!(args.values().len(), 5);
            assert_eq!(args.value(1)?, NativeValue::I32(9));
            let slot = args.reference(0)?;
            let total: i32 = host.array_i32(slot)?.iter().sum::<i32>() + args.i32(1)?;
            assert!(!out.is_empty());
            assert_eq!(out.len(), 1);
            out.set(0, NativeValue::I32(total));
            Ok(())
        },
    );

    let (_, _, binding) = package.bindings().next().expect("the one binding");
    let mut host = FakeHost::with(&[1, 2, 3]);
    let mut results = [NativeValue::Null];
    binding(
        &mut host,
        Args::new(&[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(9),
            NativeValue::I64(7),
            NativeValue::F32(1.5),
            NativeValue::F64(2.5),
        ]),
        Results::new(&mut results),
    )
    .expect("the binding answers");
    assert_eq!(results[0], NativeValue::I32(15));
}

/// The refusals a binding can report, each naming the position it is about.
#[test]
fn an_argument_that_is_not_what_the_binding_reads_is_refused_by_position() {
    let args = [NativeValue::I32(1), NativeValue::Null];
    let args = Args::new(&args);

    let Err(NativeError::Argument {
        position,
        expected,
        found,
    }) = args.i64(0)
    else {
        panic!("an i32 is not an i64");
    };
    assert_eq!((position, expected, found), (0, "an i64", "i32"));

    assert_eq!(args.reference(1), Err(NativeError::Null { position: 1 }));
    assert_eq!(
        args.i32(9),
        Err(NativeError::MissingArgument { position: 9 })
    );
    assert!(args.f32(0).is_err());
    assert!(args.f64(0).is_err());
    assert!(args.value(9).is_err());

    // Every one renders, and says which argument it is about.
    for error in [
        NativeError::argument(0, "an i64", NativeValue::I32(1)),
        NativeError::Null { position: 1 },
        NativeError::MissingArgument { position: 2 },
        NativeError::NotAnArray,
        NativeError::OutOfBounds { index: 3, len: 2 },
        NativeError::Call("boom".to_owned()),
        NativeError::Message("said so".to_owned()),
    ] {
        assert!(!error.to_string().is_empty(), "{error:?}");
    }
}

/// A value says what it is, and answers only for the shape it holds.
#[test]
fn a_value_answers_only_for_the_shape_it_holds() {
    assert_eq!(NativeValue::I32(1).as_i32(), Some(1));
    assert_eq!(NativeValue::I32(1).as_i64(), None);
    assert_eq!(NativeValue::I64(2).as_i64(), Some(2));
    assert_eq!(NativeValue::F32(1.0).as_f32(), Some(1.0));
    assert_eq!(NativeValue::F64(1.0).as_f64(), Some(1.0));
    assert_eq!(
        NativeValue::Ref(RefSlot::new(4))
            .as_ref_slot()
            .map(RefSlot::index),
        Some(4)
    );
    assert_eq!(NativeValue::Null.as_ref_slot(), None);
    assert_eq!(NativeValue::Null.kind(), "null");
    assert_eq!(NativeValue::Ref(RefSlot::new(0)).kind(), "a reference");
    assert_eq!(NativeValue::F64(0.0).kind(), "f64");
    assert_eq!(NativeValue::F32(0.0).kind(), "f32");
    assert_eq!(NativeValue::I64(0).kind(), "i64");
}

/// The provided `NativeHost` methods: an array of `int`s, and a `char[]` read as text.
#[test]
fn the_host_seam_reads_arrays_and_calls_back_into_the_module() {
    let mut host = FakeHost::with(&['h' as i32, 'i' as i32, '!' as i32]);
    assert_eq!(host.array_len(FakeHost::ARRAY).expect("a length"), 3);
    assert_eq!(
        host.array_i32(FakeHost::ARRAY).expect("the elements"),
        vec!['h' as i32, 'i' as i32, '!' as i32]
    );
    assert_eq!(
        host.array_text(FakeHost::ARRAY, 0, 2).expect("the text"),
        "hi"
    );
    // Past the end is a refusal rather than a short read.
    assert!(host.array_text(FakeHost::ARRAY, 2, 9).is_err());
    assert!(host.array_get(FakeHost::ARRAY, 9).is_err());
    assert!(host.array_len(RefSlot::new(1)).is_err());

    host.array_set(FakeHost::ARRAY, 0, NativeValue::I32('H' as i32))
        .expect("a write");
    assert_eq!(
        host.array_text(FakeHost::ARRAY, 0, 3).expect("the text"),
        "Hi!"
    );

    let mut results = [NativeValue::Null];
    host.call_export("factory", &[], &mut results)
        .expect("the call");
    assert_eq!(results[0], NativeValue::I32(1));
    assert_eq!(host.calls.borrow().as_slice(), ["factory"]);
}

/// A resolver is a route; a selection is what one project took through it.
///
/// The two source questions are the point of the type: an index reads everything, a compile reads
/// only what has bodies. `demo/Marker.java` is in one answer and not the other.
#[test]
fn a_selection_separates_what_is_indexed_from_what_is_compiled() {
    let mut builtin = StaticResolver::new("built in");
    builtin.add(demo(1));
    assert_eq!(builtin.offered(), vec!["demo.clock"]);
    assert_eq!(builtin.route(), "built in");
    assert!(builtin.resolve("demo.clock").is_some());
    assert!(builtin.resolve("nope").is_none());

    let chain = ResolverChain::new().push(Box::new(builtin));
    let selection = chain
        .select(&["demo.clock".to_owned()])
        .expect("the package is offered");
    assert!(!selection.is_empty());
    assert_eq!(selection.names().collect::<Vec<_>>(), vec!["demo.clock"]);

    let indexed: Vec<&str> = selection
        .analysis_sources()
        .map(|(package, source)| {
            assert_eq!(package, "demo.clock");
            source.path.as_ref()
        })
        .collect();
    assert_eq!(indexed, vec!["demo/Marker.java", "demo/Clock.java"]);

    // A signature unit is nameable and never compiled. This is the property that keeps a type the
    // backend answers for itself from also being one the module declares.
    let compiled: Vec<&str> = selection.link_sources().map(|(_, s)| s.path.as_ref()).collect();
    assert_eq!(compiled, vec!["demo/Clock.java"]);

    let bindings = selection.bindings();
    assert!(!bindings.is_empty());
    assert_eq!(bindings.keys().collect::<Vec<_>>(), vec![("demo/Clock", "now()J")]);
    assert!(bindings.get("demo/Clock", "now()J").is_some());
    assert!(bindings.get("demo/Clock", "nope()V").is_none());
    assert!(format!("{bindings:?}").contains("now()J"));

    // The empty selection is a value rather than an absence, and answers everything as empty.
    let empty = PackageSelection::empty();
    assert!(empty.is_empty());
    assert_eq!(empty.analysis_sources().count(), 0);
    assert_eq!(empty.link_sources().count(), 0);
    assert!(empty.bindings().is_empty());
    assert!(NativeBindings::new().is_empty());
}

/// A binding runs against a host, and reads the state its closure captured.
#[test]
fn a_declared_packages_binding_answers_from_the_host_it_was_built_over() {
    let package = demo(1_700_000_000_000);
    let (_, _, binding) = package.bindings().next().expect("the one binding");
    let mut host = FakeHost::default();
    let mut results = [NativeValue::Null];
    binding(&mut host, Args::new(&[]), Results::new(&mut results)).expect("the reading");
    assert_eq!(results[0], NativeValue::I64(1_700_000_000_000));
}

/// A name this build does not offer is reported with the names it does — the only evidence a
/// reader gets, since the set is fixed when the binary is built.
#[test]
fn an_unknown_package_names_what_is_offered() {
    let mut builtin = StaticResolver::new("built in");
    builtin.add(demo(1));
    let chain = ResolverChain::new().push(Box::new(builtin));

    let unknown = chain.resolve("demo.net").expect_err("no such package");
    let ResolveError::Unknown { name, available } = &unknown else {
        panic!("an unknown name, not {unknown:?}");
    };
    assert_eq!(name, "demo.net");
    assert_eq!(available, &vec!["demo.clock".to_owned()]);
    assert!(unknown.to_string().contains("demo.net"));
    assert!(unknown.to_string().contains("demo.clock"));
    assert_eq!(chain.offered(), vec!["demo.clock"]);

    // An empty chain says so rather than listing nothing.
    let bare = ResolverChain::new()
        .resolve("anything")
        .expect_err("nothing is offered");
    assert!(bare.to_string().contains("offers none"));
    assert!(ResolverChain::default().offered().is_empty());
}

/// Two routes offering one name is an **error**, not a shadow.
///
/// rhai lets an earlier resolver win; here it cannot, because the two would be one project's
/// analysis and that project's linked module disagreeing about a type with nothing said. It is the
/// rule `jals-config` already applies to a dependency named in two tables: one name denotes one
/// entry wherever it is read.
#[test]
fn a_name_two_routes_offer_is_refused_naming_both() {
    let mut builtin = StaticResolver::new("built in");
    builtin.add(demo(1));
    let mut project = StaticResolver::new("this project");
    project.add(demo(2));

    let chain = ResolverChain::new()
        .push(Box::new(builtin))
        .push(Box::new(project));

    let ambiguous = chain.resolve("demo.clock").expect_err("two routes offer it");
    let ResolveError::Ambiguous { name, routes } = &ambiguous else {
        panic!("an ambiguity, not {ambiguous:?}");
    };
    assert_eq!(name, "demo.clock");
    assert_eq!(routes, &vec!["built in".to_owned(), "this project".to_owned()]);
    assert!(ambiguous.to_string().contains("built in"));
    assert!(ambiguous.to_string().contains("this project"));

    // And `select` refuses for the same reason, rather than quietly taking one.
    assert!(chain.select(&["demo.clock".to_owned()]).is_err());

    // Deduplicated across routes, so the "what is offered" list is not doubled.
    assert_eq!(chain.offered(), vec!["demo.clock"]);
}

/// A selection is ordered by package name, not by the order a manifest listed them, so two
/// manifests selecting the same set produce one cache key and one module layout.
#[test]
fn a_selection_is_ordered_by_name_not_by_the_order_it_was_asked_for() {
    let mut builtin = StaticResolver::new("built in");
    builtin.add(demo(1));
    let mut second = JavaPackage::new("aardvark", 1);
    second.source("a/A.java", "class A {}", SourceKind::Implementation);
    builtin.add(second);
    let chain = ResolverChain::new().push(Box::new(builtin));

    let asked = chain
        .select(&["demo.clock".to_owned(), "aardvark".to_owned()])
        .expect("both are offered");
    let reversed = chain
        .select(&["aardvark".to_owned(), "demo.clock".to_owned()])
        .expect("both are offered");
    assert_eq!(
        asked.names().collect::<Vec<_>>(),
        vec!["aardvark", "demo.clock"]
    );
    assert_eq!(
        asked.names().collect::<Vec<_>>(),
        reversed.names().collect::<Vec<_>>()
    );
    assert_eq!(asked.provenance(), reversed.provenance());

    // Naming one twice is naming it once.
    let twice = chain
        .select(&["demo.clock".to_owned(), "demo.clock".to_owned()])
        .expect("still offered");
    assert_eq!(twice.names().count(), 1);
}

/// Provenance is what a consumer's cache key folds. It has to move when anything a compile
/// observed moved — the source **kind** included, since that decides what is compiled at all.
#[test]
fn provenance_moves_with_the_version_the_java_the_kind_and_the_binding_keys() {
    let build = |version: u32, java: &'static str, signature: &'static str, kind: SourceKind| {
        let mut package = JavaPackage::new("demo", version);
        package.source("demo/A.java", java, kind);
        package.bind("demo/A", signature, |_, _, _| Ok(()));
        let mut resolver = StaticResolver::new("built in");
        resolver.add(package);
        ResolverChain::new()
            .push(Box::new(resolver))
            .select(&["demo".to_owned()])
            .expect("just registered")
            .provenance()
    };
    let implemented = SourceKind::Implementation;

    let base = build(1, "class A {}", "f()V", implemented);
    assert_eq!(base, build(1, "class A {}", "f()V", implemented));
    assert_ne!(base, build(2, "class A {}", "f()V", implemented));
    assert_ne!(base, build(1, "class A { int x; }", "f()V", implemented));
    assert_ne!(base, build(1, "class A {}", "f(I)V", implemented));
    assert_ne!(base, build(1, "class A {}", "f()V", SourceKind::Signatures));
    assert_ne!(base, PackageSelection::empty().provenance());

    // A length-prefixed field, so no value can be confused with a field boundary.
    let mut provenance = Provenance::new();
    provenance.field(b"ab").number(7);
    let mut split = Provenance::new();
    split.field(b"a").field(b"b").number(7);
    assert_ne!(provenance.into_bytes(), split.into_bytes());
}

/// A project can declare a package with **no Rust at all**, and it resolves through the same chain.
///
/// This is the route that makes package definition programmable without writing a crate: a project
/// fills a gap the built-in set leaves by pointing at a directory of its own `.java`.
#[test]
fn a_project_can_declare_a_java_only_package() {
    let mut project = SourceResolver::new("declared by this project");
    assert!(project.is_empty());
    project.declare(
        "acme.util",
        [
            (
                "acme/util/Pair.java".to_owned(),
                "package acme.util; public final class Pair { public int a; public int b; }"
                    .to_owned(),
                SourceKind::Implementation,
            ),
            (
                "acme/util/Marker.java".to_owned(),
                "package acme.util; public interface Marker { String name(); }".to_owned(),
                SourceKind::Signatures,
            ),
        ],
    );
    assert!(!project.is_empty());
    assert_eq!(project.offered(), vec!["acme.util"]);
    assert_eq!(project.route(), "declared by this project");

    let mut builtin = StaticResolver::new("built in");
    builtin.add(demo(1));
    let chain = ResolverChain::new()
        .push(Box::new(builtin))
        .push(Box::new(project));

    let selection = chain
        .select(&["acme.util".to_owned(), "demo.clock".to_owned()])
        .expect("both routes answer");
    assert_eq!(
        selection.names().collect::<Vec<_>>(),
        vec!["acme.util", "demo.clock"]
    );

    // The two source questions answer the same way as for a built-in package: a signature unit is
    // indexed and never compiled.
    let compiled: Vec<&str> = selection
        .link_sources()
        .filter(|(package, _)| *package == "acme.util")
        .map(|(_, source)| source.path.as_ref())
        .collect();
    assert_eq!(compiled, vec!["acme/util/Pair.java"]);

    // It binds nothing, and does not have to: a `native` method in Java nobody wrote Rust for is
    // an unresolved import, refused where every unbound import is.
    assert!(
        selection
            .bindings()
            .keys()
            .all(|(owner, _)| !owner.starts_with("acme/")),
        "a Java-only package binds nothing"
    );

    // Its identity is its text, which is why `declare` takes no version: there are no closures to
    // be unable to observe.
    let mut same = SourceResolver::new("declared by this project");
    same.declare(
        "acme.util",
        [(
            "acme/util/Pair.java".to_owned(),
            "package acme.util; public final class Pair { public int a; }".to_owned(),
            SourceKind::Implementation,
        )],
    );
    let changed = ResolverChain::new()
        .push(Box::new(same))
        .select(&["acme.util".to_owned()])
        .expect("declared")
        .provenance();
    assert_ne!(
        changed,
        ResolverChain::new()
            .push(Box::new({
                let mut r = SourceResolver::new("declared by this project");
                r.declare(
                    "acme.util",
                    [(
                        "acme/util/Pair.java".to_owned(),
                        "package acme.util; public final class Pair { public int b; }".to_owned(),
                        SourceKind::Implementation,
                    )],
                );
                r
            }))
            .select(&["acme.util".to_owned()])
            .expect("declared")
            .provenance(),
        "editing a declared package's Java moves its provenance"
    );
}
