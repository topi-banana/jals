//! Every published item, driven.
//!
//! `cargo hawk check` excludes this crate the way it excludes `jinja`: the API is sized by what a
//! *package author* is offered, not by what the one shipped package happens to call. So this file
//! is what holds the surface honest instead — a published item lands with the test that drives it,
//! or it lands unreachable with nothing reporting it.

use std::cell::RefCell;
use std::rc::Rc;

use jals_native::packages::jals_io::{CapturedConsole, ConsoleSink, JalsIo};
use jals_native::{
    Args, NativeBindings, NativeError, NativeHost, NativePackage, NativePackageSet, NativeRegistry,
    NativeValue, Provenance, RefSlot, Results,
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

/// A package is one value holding both halves: the Java it publishes and the Rust behind that
/// Java's `native` methods, keyed by the two strings the compiler writes into the import section.
#[test]
fn a_package_holds_the_java_and_the_rust_behind_it() {
    let mut package = NativePackage::new("demo", 3);
    package.source(
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
    assert_eq!(package.sources().len(), 1);
    assert_eq!(package.sources()[0].path, "demo/Answer.java");
    assert!(package.sources()[0].text.contains("class Answer"));

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
    let mut package = NativePackage::new("demo", 1);
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

/// A registry is what a *binary* offers; a selection is what one project took out of it.
#[test]
fn a_selection_carries_the_java_the_bindings_and_the_provenance() {
    let console = Rc::new(CapturedConsole::new());
    let mut registry = NativeRegistry::new();
    registry.add(JalsIo::package(Rc::clone(&console) as Rc<dyn ConsoleSink>));
    assert_eq!(registry.names().collect::<Vec<_>>(), vec![JalsIo::NAME]);

    let selection = registry
        .select(&[JalsIo::NAME.to_owned()])
        .expect("the package is offered");
    assert!(!selection.is_empty());
    assert_eq!(selection.names().collect::<Vec<_>>(), vec![JalsIo::NAME]);
    let paths: Vec<&str> = selection
        .sources()
        .map(|(package, source)| {
            assert_eq!(package, JalsIo::NAME);
            source.path
        })
        .collect();
    assert_eq!(paths, vec!["jals/io/Out.java"]);

    let bindings = selection.bindings();
    assert!(!bindings.is_empty());
    let keys: Vec<(&str, &str)> = bindings.keys().collect();
    assert_eq!(
        keys,
        vec![
            ("jals/io/Out", "flush()V"),
            ("jals/io/Out", "writeChar(I)V"),
            ("jals/io/Out", "writeChars([CII)V"),
        ]
    );
    assert!(bindings.get("jals/io/Out", "flush()V").is_some());
    assert!(bindings.get("jals/io/Out", "nope()V").is_none());
    assert!(format!("{bindings:?}").contains("flush()V"));

    // The empty selection is a value rather than an absence, and answers everything as empty.
    let empty = NativePackageSet::empty();
    assert!(empty.is_empty());
    assert_eq!(empty.sources().count(), 0);
    assert!(empty.bindings().is_empty());
    assert!(NativeBindings::new().is_empty());
    assert_eq!(
        format!("{:?}", NativeRegistry::default()),
        "NativeRegistry { packages: {} }"
    );
}

/// A name this binary does not offer is reported with the names it does — the only evidence a
/// reader gets, since the set is fixed when the binary is built.
#[test]
fn an_unknown_package_names_what_is_offered() {
    let console = Rc::new(CapturedConsole::new());
    let mut registry = NativeRegistry::new();
    registry.add(JalsIo::package(console));

    let unknown = registry
        .select(&["jals.net".to_owned()])
        .expect_err("no such package");
    assert_eq!(unknown.name, "jals.net");
    assert_eq!(unknown.available, vec![JalsIo::NAME.to_owned()]);
    assert!(unknown.to_string().contains("jals.net"));
    assert!(unknown.to_string().contains(JalsIo::NAME));

    // An empty registry says so rather than listing nothing.
    let bare = NativeRegistry::new()
        .select(&["anything".to_owned()])
        .expect_err("nothing is offered");
    assert!(bare.to_string().contains("offers none"));
}

/// Provenance is what a consumer's cache key folds. It has to move when anything a compile
/// observed moved — and *not* when the same package is selected twice.
#[test]
fn provenance_moves_with_the_version_the_java_and_the_binding_keys() {
    let build = |version: u32, java: &'static str, signature: &'static str| {
        let mut package = NativePackage::new("demo", version);
        package.source("demo/A.java", java);
        package.bind("demo/A", signature, |_, _, _| Ok(()));
        let mut registry = NativeRegistry::new();
        registry.add(package);
        registry
            .select(&["demo".to_owned()])
            .expect("just registered")
            .provenance()
    };

    let base = build(1, "class A {}", "f()V");
    assert_eq!(base, build(1, "class A {}", "f()V"));
    assert_ne!(base, build(2, "class A {}", "f()V"));
    assert_ne!(base, build(1, "class A { int x; }", "f()V"));
    assert_ne!(base, build(1, "class A {}", "f(I)V"));
    assert_ne!(base, NativePackageSet::empty().provenance());

    // A length-prefixed field, so no value can be confused with a field boundary.
    let mut provenance = Provenance::new();
    provenance.field(b"ab").number(7);
    let mut split = Provenance::new();
    split.field(b"a").field(b"b").number(7);
    assert_ne!(provenance.into_bytes(), split.into_bytes());
}

/// `jals.io`'s three bindings, driven without an engine: what a module writes reaches the sink,
/// and only when it flushes.
#[test]
fn the_shipped_package_buffers_until_it_is_flushed() {
    let console = Rc::new(CapturedConsole::new());
    let package = JalsIo::package(Rc::clone(&console) as Rc<dyn ConsoleSink>);
    let bindings: Vec<(String, jals_native::NativeFn)> = package
        .bindings()
        .map(|(_, signature, binding)| (signature.to_owned(), binding.clone()))
        .collect();
    let call = |signature: &str, host: &mut FakeHost, args: &[NativeValue]| {
        let (_, binding) = bindings
            .iter()
            .find(|(key, _)| key == signature)
            .expect("a bound signature");
        let mut results: [NativeValue; 0] = [];
        binding(host, Args::new(args), Results::new(&mut results))
    };

    let mut host = FakeHost::with(&['b' as i32, 'c' as i32, 'd' as i32]);
    call("writeChar(I)V", &mut host, &[NativeValue::I32('a' as i32)]).expect("a code unit");
    // Nothing yet: a code unit is half of a surrogate pair, so the host cannot decode as it goes.
    assert_eq!(console.take(), "");

    call(
        "writeChars([CII)V",
        &mut host,
        &[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(0),
            NativeValue::I32(2),
        ],
    )
    .expect("two code units");
    call("flush()V", &mut host, &[]).expect("a flush");
    assert_eq!(console.take(), "abc");

    // A second flush with nothing buffered writes nothing at all.
    call("flush()V", &mut host, &[]).expect("an empty flush");
    assert_eq!(console.take(), "");

    // A range outside the array is a refusal, not a short write.
    assert!(
        call(
            "writeChars([CII)V",
            &mut host,
            &[
                NativeValue::Ref(FakeHost::ARRAY),
                NativeValue::I32(2),
                NativeValue::I32(9),
            ],
        )
        .is_err()
    );
    assert!(
        call(
            "writeChars([CII)V",
            &mut host,
            &[
                NativeValue::Ref(FakeHost::ARRAY),
                NativeValue::I32(-1),
                NativeValue::I32(1),
            ],
        )
        .is_err()
    );
}
