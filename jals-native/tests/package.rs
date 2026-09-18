//! Every published item, driven.
//!
//! `cargo hawk check` excludes this crate the way it excludes `jinja`: the API is sized by what a
//! *package author* is offered, not by what the one shipped package happens to call. So this file
//! is what holds the surface honest instead — a published item lands with the test that drives it,
//! or it lands unreachable with nothing reporting it.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use jals_native::{
    Args, HostId, HostObjects, HostValue, JavaPackage, NativeBindings, NativeClass, NativeError,
    NativeHost, NativeValue, PackageRegistry, PackageSelection, Provenance, RefSlot, Results,
    SourceKind, UnknownPackage, java_package,
};

/// A host that answers over one `i32` array, holds a table of entries, and records every export it
/// was asked to call.
///
/// The whole `NativeHost` seam, in the smallest thing that can implement it — which is also what a
/// package author writes to test a binding without an engine.
#[derive(Default)]
struct FakeHost {
    array: RefCell<Vec<i32>>,
    calls: RefCell<Vec<String>>,
    entries: BTreeMap<u32, Entry>,
    next: u32,
}

/// What the fake table holds: the two kinds the real one holds.
enum Entry {
    Object(Box<dyn Any>),
    Reference(NativeValue),
}

impl FakeHost {
    const ARRAY: RefSlot = RefSlot::new(0);

    fn with(values: &[i32]) -> Self {
        Self {
            array: RefCell::new(values.to_vec()),
            ..Self::default()
        }
    }

    const fn missing(id: HostId) -> NativeError {
        NativeError::UnknownHandle { id: id.raw() }
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

    fn object_store(&mut self, object: Box<dyn Any>) -> Result<HostId, NativeError> {
        let id = HostId::new(self.next);
        self.next += 1;
        self.entries.insert(id.raw(), Entry::Object(object));
        Ok(id)
    }

    fn object_take(&mut self, id: HostId) -> Result<Box<dyn Any>, NativeError> {
        match self.entries.remove(&id.raw()) {
            Some(Entry::Object(object)) => Ok(object),
            Some(entry @ Entry::Reference(_)) => {
                self.entries.insert(id.raw(), entry);
                Err(NativeError::HostKind {
                    expected: "a Rust object",
                    found: "a retained reference",
                })
            }
            None => Err(Self::missing(id)),
        }
    }

    fn object_restore(&mut self, id: HostId, object: Box<dyn Any>) -> Result<(), NativeError> {
        self.entries.insert(id.raw(), Entry::Object(object));
        Ok(())
    }

    fn object_drop(&mut self, id: HostId) -> Result<(), NativeError> {
        self.entries
            .remove(&id.raw())
            .map(|_| ())
            .ok_or_else(|| Self::missing(id))
    }

    fn reference_retain(&mut self, slot: RefSlot) -> Result<HostId, NativeError> {
        let id = HostId::new(self.next);
        self.next += 1;
        self.entries
            .insert(id.raw(), Entry::Reference(NativeValue::Ref(slot)));
        Ok(id)
    }

    fn reference_restore(&mut self, id: HostId) -> Result<RefSlot, NativeError> {
        match self.entries.get(&id.raw()) {
            Some(Entry::Reference(NativeValue::Ref(slot))) => Ok(*slot),
            Some(Entry::Reference(_)) => Err(NativeError::HostKind {
                expected: "a non-null reference",
                found: "a null",
            }),
            Some(Entry::Object(_)) => Err(NativeError::HostKind {
                expected: "a retained reference",
                found: "a Rust object",
            }),
            None => Err(Self::missing(id)),
        }
    }

    fn reference_release(&mut self, id: HostId) -> Result<(), NativeError> {
        self.object_drop(id)
    }
}

/// A package is one value holding both halves: the Java it publishes and the Rust behind that
/// Java's `native` methods, keyed by the two strings the compiler writes into the import section.
#[test]
fn a_package_holds_the_java_and_the_rust_behind_it() {
    let mut package = JavaPackage::new("demo", 3);
    package.signature(
        "demo/Marker.java",
        "package demo; public interface Marker {}",
    );
    package.implementation(
        "demo/Answer.java",
        "package demo; public final class Answer {}",
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
    assert_eq!(package.sources().len(), 2);
    assert_eq!(package.sources()[0].path, "demo/Marker.java");
    assert_eq!(package.sources()[0].kind, SourceKind::Signatures);
    assert_eq!(package.sources()[1].kind, SourceKind::Implementation);
    assert!(package.sources()[1].text.contains("class Answer"));
    assert_eq!(package.binding_count(), 1);

    let keys: Vec<(&str, &str)> = package
        .bindings()
        .map(|(owner, signature, _)| (owner, signature))
        .collect();
    assert_eq!(keys, vec![("demo/Answer", "compute()I")]);
    assert!(format!("{package:?}").contains("demo"));
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

/// The typed registration: a Java class whose instances carry a handle, and Rust closures that
/// receive the object behind it — rhai's `register_type_with_name` / `register_fn`, for Java.
#[test]
fn a_native_class_binds_its_allocator_its_methods_and_its_release() {
    let mut package = JavaPackage::new("demo", 1);
    let class: &mut NativeClass<'_, u32> = &mut package.native_class::<u32>("demo/Counter");
    class
        .allocate("allocate()I", |_| Ok(7u32))
        .method("advance(I)I", |count, _host, args, mut results| {
            *count += u32::try_from(args.i32(0)?).unwrap_or(0);
            results.set(0, NativeValue::I32((*count).cast_signed()));
            Ok(())
        })
        .release("release(I)V");
    assert_eq!(package.binding_count(), 3);

    let binding = |package: &JavaPackage, signature: &str| {
        package
            .bindings()
            .find(|(_, key, _)| *key == signature)
            .map(|(_, _, binding)| Rc::clone(binding))
            .expect("a bound signature")
    };
    let allocate = binding(&package, "allocate()I");
    let advance = binding(&package, "advance(I)I");
    let release = binding(&package, "release(I)V");

    let mut host = FakeHost::default();
    let mut results = [NativeValue::Null];
    allocate(&mut host, Args::new(&[]), Results::new(&mut results)).expect("an allocation");
    let NativeValue::I32(handle) = results[0] else {
        panic!("an allocation answers with the handle");
    };
    assert_eq!(handle, 0);

    advance(
        &mut host,
        Args::new(&[NativeValue::I32(handle), NativeValue::I32(5)]),
        Results::new(&mut results),
    )
    .expect("a method call");
    assert_eq!(results[0], NativeValue::I32(12));

    // The handle names the same object across calls — the table is the run's, not the call's.
    advance(
        &mut host,
        Args::new(&[NativeValue::I32(handle), NativeValue::I32(1)]),
        Results::new(&mut results),
    )
    .expect("a second call");
    assert_eq!(results[0], NativeValue::I32(13));

    // Releasing it is explicit, and a method after that says the handle names nothing.
    let mut none: [NativeValue; 0] = [];
    release(
        &mut host,
        Args::new(&[NativeValue::I32(handle)]),
        Results::new(&mut none),
    )
    .expect("a release");
    assert_eq!(
        advance(
            &mut host,
            Args::new(&[NativeValue::I32(handle), NativeValue::I32(1)]),
            Results::new(&mut results),
        ),
        Err(NativeError::UnknownHandle { id: 0 })
    );

    // A handle that was never issued is refused the same way, and a negative one too.
    assert!(
        advance(
            &mut host,
            Args::new(&[NativeValue::I32(99), NativeValue::I32(1)]),
            Results::new(&mut results),
        )
        .is_err()
    );
    assert!(
        advance(
            &mut host,
            Args::new(&[NativeValue::I32(-1), NativeValue::I32(1)]),
            Results::new(&mut results),
        )
        .is_err()
    );
}

/// The host's table, driven directly: Rust objects and retained references, the typed downcast,
/// and the refusals for a wrong kind or an unknown handle.
#[test]
fn the_host_table_stores_objects_and_roots_references() {
    let mut host = FakeHost::default();

    // A Rust object round-trips, and the typed take is a downcast rather than a reinterpretation:
    // a wrong type is refused, and the object is still there afterwards.
    let id = host.put::<Vec<i32>>(vec![1, 2]).expect("a store");
    assert_eq!(host.take::<Vec<i32>>(id).expect("a take"), vec![1, 2]);
    assert!(matches!(
        host.take::<String>(id),
        Err(NativeError::UnknownHandle { .. })
    ));
    host.put_back(id, vec![3]).expect("a restore");
    assert!(matches!(
        host.take::<String>(id),
        Err(NativeError::HostKind { .. })
    ));
    assert_eq!(host.take::<Vec<i32>>(id).expect("a take"), vec![3]);
    host.put_back(id, vec![3]).expect("a restore");
    host.object_drop(id).expect("a drop");
    assert_eq!(
        host.object_drop(id),
        Err(NativeError::UnknownHandle { id: id.raw() })
    );

    // A reference is retained under an id and restored to a slot.
    let retained = host
        .reference_retain(RefSlot::new(3))
        .expect("a retained reference");
    assert_eq!(
        host.reference_restore(retained).expect("a restore"),
        RefSlot::new(3)
    );
    assert!(matches!(
        host.object_take(retained),
        Err(NativeError::HostKind { .. })
    ));
    host.reference_release(retained).expect("a release");
    assert_eq!(
        host.reference_restore(retained),
        Err(NativeError::UnknownHandle { id: retained.raw() })
    );

    // The values a container stores: numbers and nulls need no slot, a reference does.
    let reference = HostValue::capture(&mut host, NativeValue::Ref(RefSlot::new(4)))
        .expect("a captured reference");
    assert!(matches!(reference, HostValue::Reference(_)));
    assert!(matches!(
        reference.restore(&mut host).expect("a restored reference"),
        NativeValue::Ref(_)
    ));
    assert_eq!(
        HostValue::capture(&mut host, NativeValue::Null).expect("a null"),
        HostValue::Null
    );
    assert_eq!(
        HostValue::capture(&mut host, NativeValue::I32(5)).expect("a number"),
        HostValue::I32(5)
    );
    assert_eq!(HostValue::F64(1.0).kind(), "f64");
    assert_eq!(HostValue::Reference(HostId::new(0)).kind(), "a reference");
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
        NativeError::UnknownHandle { id: 4 },
        NativeError::HostKind {
            expected: "a Rust object",
            found: "a retained reference",
        },
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
    assert_eq!(HostId::new(9).raw(), 9);
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

/// A registry is what a *binary* offers; a selection is what one project took out of it. The two
/// source questions a selection answers are the same text at two tiers.
#[test]
fn a_selection_carries_the_java_the_bindings_and_the_provenance() {
    let mut package = JavaPackage::new("demo", 1);
    package.signature("demo/Marker.java", "interface Marker {}");
    package.implementation("demo/Answer.java", "class Answer {}");
    package.bind("demo/Answer", "compute()I", |_, _, _| Ok(()));

    let mut registry = PackageRegistry::new();
    registry.add(package);
    assert_eq!(registry.names().collect::<Vec<_>>(), vec!["demo"]);

    let selection = registry
        .select(&["demo".to_owned()])
        .expect("the package is offered");
    assert!(!selection.is_empty());
    assert_eq!(selection.names().collect::<Vec<_>>(), vec!["demo"]);
    assert_eq!(selection.analysis_sources().count(), 2);
    assert_eq!(selection.link_sources().count(), 1);
    assert_eq!(
        selection
            .link_sources()
            .map(|(_, source)| source.path.as_ref())
            .collect::<Vec<_>>(),
        vec!["demo/Answer.java"]
    );

    let bindings = selection.bindings();
    assert!(!bindings.is_empty());
    assert_eq!(
        bindings.keys().collect::<Vec<_>>(),
        vec![("demo/Answer", "compute()I")]
    );
    assert!(bindings.get("demo/Answer", "compute()I").is_some());
    assert!(bindings.get("demo/Answer", "nope()V").is_none());
    assert!(format!("{bindings:?}").contains("compute()I"));

    // The empty selection is a value rather than an absence, and answers everything as empty.
    let empty = PackageSelection::empty();
    assert!(empty.is_empty());
    assert_eq!(empty.analysis_sources().count(), 0);
    assert!(empty.bindings().is_empty());
    assert!(NativeBindings::new().is_empty());

    // A selection built straight from packages is ordered by name and deduplicated.
    let mut second = JavaPackage::new("aardvark", 1);
    second.bind("aardvark/A", "f()V", |_, _, _| Ok(()));
    let built = PackageSelection::of([JavaPackage::new("demo", 1), second]);
    assert_eq!(built.names().collect::<Vec<_>>(), vec!["aardvark", "demo"]);
}

/// A name this binary does not offer is reported with the names it does — the only evidence a
/// reader gets, since the set is fixed when the binary is built. Analysis keeps what resolved.
#[test]
fn an_unknown_package_names_what_is_offered() {
    let mut registry = PackageRegistry::new();
    registry.add(JavaPackage::new("demo", 1));

    let unknown = registry
        .select(&["demo.net".to_owned()])
        .expect_err("no such package");
    assert_eq!(unknown.name, "demo.net");
    assert_eq!(unknown.available, vec!["demo".to_owned()]);
    assert!(unknown.to_string().contains("demo.net"));
    assert!(unknown.to_string().contains("demo"));

    // An empty registry says so rather than listing nothing.
    let bare = PackageRegistry::new()
        .select(&["anything".to_owned()])
        .expect_err("nothing is offered");
    assert!(bare.to_string().contains("offers none"));
    let _: &dyn core::error::Error = &bare;

    // One name at a time: what resolved is kept, what did not is reported.
    let (selection, failures) =
        registry.select_reporting(&["demo".to_owned(), "demo.net".to_owned()]);
    assert_eq!(selection.names().collect::<Vec<_>>(), vec!["demo"]);
    assert_eq!(
        failures,
        vec![UnknownPackage {
            name: "demo.net".to_owned(),
            available: vec!["demo".to_owned()],
        }]
    );
}

/// Provenance is what a consumer's cache key folds. It has to move when anything a compile
/// observed moved — and *not* when the same package is selected twice.
#[test]
fn provenance_moves_with_the_version_the_java_the_kind_and_the_binding_keys() {
    let build = |version: u32, java: &'static str, kind: SourceKind, signature: &'static str| {
        let mut package = JavaPackage::new("demo", version);
        package.source("demo/A.java", java, kind);
        package.bind("demo/A", signature, |_, _, _| Ok(()));
        PackageSelection::of([package]).provenance()
    };

    let base = build(1, "class A {}", SourceKind::Implementation, "f()V");
    assert_eq!(
        base,
        build(1, "class A {}", SourceKind::Implementation, "f()V")
    );
    assert_ne!(
        base,
        build(2, "class A {}", SourceKind::Implementation, "f()V")
    );
    assert_ne!(
        base,
        build(1, "class A { int x; }", SourceKind::Implementation, "f()V")
    );
    assert_ne!(base, build(1, "class A {}", SourceKind::Signatures, "f()V"));
    assert_ne!(
        base,
        build(1, "class A {}", SourceKind::Implementation, "f(I)V")
    );
    assert_ne!(base, PackageSelection::empty().provenance());

    // A length-prefixed field, so no value can be confused with a field boundary.
    let mut provenance = Provenance::new();
    provenance.field(b"ab").number(7);
    let mut split = Provenance::new();
    split.field(b"a").field(b"b").number(7);
    assert_ne!(provenance.into_bytes(), split.into_bytes());
}

/// The declaration macro carries both halves in one value, and `SOURCES` is reachable with no host
/// constructed at all — which is what a language server that instantiates nothing needs.
#[test]
fn the_declaration_macro_carries_the_java_and_the_bindings() {
    assert_eq!(Demo::NAME, "demo.clock");
    assert_eq!(Demo::VERSION, 4);

    let kinds: Vec<SourceKind> = Demo::SOURCES.iter().map(|source| source.kind).collect();
    assert_eq!(
        kinds,
        vec![SourceKind::Signatures, SourceKind::Implementation]
    );
    assert_eq!(Demo::SOURCES[0].path, "demo/Marker.java");
    assert_eq!(Demo::SOURCES[1].path, "demo/Clock.java");

    let package = Demo::package(Rc::new(FixedClock { now: 12 }));
    assert_eq!(package.name(), Demo::NAME);
    assert_eq!(package.binding_count(), 1);
    let (_, _, binding) = package.bindings().next().expect("the one binding");
    let mut host = FakeHost::default();
    let mut results = [NativeValue::Null];
    binding(&mut host, Args::new(&[]), Results::new(&mut results)).expect("a call");
    assert_eq!(results[0], NativeValue::I64(12));
}

/// The state the macro-declared package's binding writes through.
trait Clock {
    fn now(&self) -> i64;
}

struct FixedClock {
    now: i64,
}

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.now
    }
}

java_package! {
    /// A package with one host function, declared in one place.
    Demo {
        name: "demo.clock",
        version: 4,
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
        package.bind("demo/Clock", "now()J", move |_host, _args, mut results| {
            results.set(0, NativeValue::I64(clock.now()));
            Ok(())
        });
    }
}
