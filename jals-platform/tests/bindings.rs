//! Every published item, driven — and the numbers this crate's prose states.
//!
//! `cargo hawk check` excludes this crate the way it excludes `jals-native` and `jinja`: its API is
//! what a *host* is offered rather than what one consumer happens to call. So this file is what
//! holds the surface honest instead.

use std::any::Any;
use std::collections::BTreeMap;
use std::rc::Rc;

use jals_native::{
    Args, HostId, NativeError, NativeHost, NativeValue, RefSlot, Results, SourceKind,
};
use jals_platform::{CapturedHost, JavaBase, PlatformHost, SilentHost, Stream};

/// A host that answers over two arrays, so a binding can be driven with no engine at all.
///
/// Two, because a binding that produces something wider than one wasm result writes it into an
/// array the *module* allocated — `toChars` into a `char[]`, `parseChars` into a `double[1]`. The
/// second slot is that out-array, and it holds [`NativeValue`]s rather than `i32`s so a decoded
/// `double` reaches a test as the `double` it is.
struct FakeHost {
    array: std::cell::RefCell<Vec<i32>>,
    out: std::cell::RefCell<Vec<NativeValue>>,
    entries: BTreeMap<u32, Box<dyn Any>>,
    /// Retained references, keyed by the id the host handed out. A real host roots these in the
    /// engine's collector; here the slot itself is enough to round-trip a test.
    references: BTreeMap<u32, RefSlot>,
    next: u32,
}

impl FakeHost {
    const ARRAY: RefSlot = RefSlot::new(0);
    /// The one-element out-array a `parseChars` writes its result into.
    const OUT: RefSlot = RefSlot::new(1);

    fn with(values: &[i32]) -> Self {
        Self {
            array: std::cell::RefCell::new(values.to_vec()),
            out: std::cell::RefCell::new(vec![NativeValue::Null]),
            entries: BTreeMap::new(),
            references: BTreeMap::new(),
            next: 0,
        }
    }

    /// What the out-array holds.
    fn decoded(&self) -> NativeValue {
        self.out.borrow()[0]
    }

    fn text(&self) -> String {
        String::from_utf16_lossy(
            &self
                .array
                .borrow()
                .iter()
                .map(|value| u16::try_from(*value).unwrap_or(0xFFFD))
                .collect::<Vec<_>>(),
        )
    }
}

impl NativeHost for FakeHost {
    fn array_len(&mut self, slot: RefSlot) -> Result<u32, NativeError> {
        let len = match slot {
            Self::ARRAY => self.array.borrow().len(),
            Self::OUT => self.out.borrow().len(),
            _ => return Err(NativeError::NotAnArray),
        };
        Ok(u32::try_from(len).expect("a small fixture"))
    }

    fn array_get(&mut self, slot: RefSlot, index: u32) -> Result<NativeValue, NativeError> {
        let len = self.array_len(slot)?;
        if slot == Self::OUT {
            return self
                .out
                .borrow()
                .get(index as usize)
                .copied()
                .ok_or(NativeError::OutOfBounds { index, len });
        }
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
        if slot == Self::OUT {
            *self
                .out
                .borrow_mut()
                .get_mut(index as usize)
                .ok_or(NativeError::OutOfBounds { index, len })? = value;
            return Ok(());
        }
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
        _name: &str,
        _args: &[NativeValue],
        _results: &mut [NativeValue],
    ) -> Result<(), NativeError> {
        Err(NativeError::Call(
            "this fixture instantiates nothing".to_owned(),
        ))
    }

    fn object_store(&mut self, object: Box<dyn Any>) -> Result<HostId, NativeError> {
        let id = HostId::new(self.next);
        self.next += 1;
        self.entries.insert(id.raw(), object);
        Ok(id)
    }

    fn object_take(&mut self, id: HostId) -> Result<Box<dyn Any>, NativeError> {
        self.entries
            .remove(&id.raw())
            .ok_or_else(|| NativeError::UnknownHandle { id: id.raw() })
    }

    fn object_restore(&mut self, id: HostId, object: Box<dyn Any>) -> Result<(), NativeError> {
        self.entries.insert(id.raw(), object);
        Ok(())
    }

    fn object_drop(&mut self, id: HostId) -> Result<(), NativeError> {
        self.entries
            .remove(&id.raw())
            .map(|_| ())
            .ok_or_else(|| NativeError::UnknownHandle { id: id.raw() })
    }

    fn reference_retain(&mut self, slot: RefSlot) -> Result<HostId, NativeError> {
        let id = HostId::new(self.next);
        self.next += 1;
        self.references.insert(id.raw(), slot);
        Ok(id)
    }

    fn reference_restore(&mut self, id: HostId) -> Result<RefSlot, NativeError> {
        self.references
            .get(&id.raw())
            .copied()
            .ok_or_else(|| NativeError::UnknownHandle { id: id.raw() })
    }

    fn reference_release(&mut self, id: HostId) -> Result<(), NativeError> {
        self.references
            .remove(&id.raw())
            .map(|_| ())
            .ok_or_else(|| NativeError::UnknownHandle { id: id.raw() })
    }
}

fn call(
    signature: &str,
    host: &mut FakeHost,
    args: &[NativeValue],
    results: &mut [NativeValue],
) -> Result<(), NativeError> {
    let package = JavaBase::package(Rc::new(CapturedHost::new()));
    let (_, _, binding) = package
        .bindings()
        .find(|(_, key, _)| *key == signature)
        .expect("a bound signature");
    binding(host, Args::new(args), Results::new(results))
}

/// **Nineteen**: the twelve operations Java cannot express, and the seven a `Vec` cannot be asked
/// for from Java. The crate docs, the README and `bindings.rs`'s own table all state the count, and
/// a number stated in a document and checked by nothing is a number that will be wrong.
#[test]
fn the_platform_binds_exactly_the_operations_java_cannot_express() {
    let package = JavaBase::package(Rc::new(CapturedHost::new()));
    assert_eq!(package.binding_count(), 19);

    let keys: Vec<(String, String)> = package
        .bindings()
        .map(|(owner, signature, _)| (owner.to_owned(), signature.to_owned()))
        .collect();
    let mut expected = vec![
        ("java/io/PrintStream", "writeUnits(I[CII)V"),
        ("java/io/PrintStream", "flushStream(I)V"),
        ("java/lang/System", "currentTimeMillis()J"),
        ("java/lang/System", "nanoTime()J"),
        ("java/lang/Double", "doubleToRawLongBits(D)J"),
        ("java/lang/Double", "longBitsToDouble(J)D"),
        ("java/lang/Double", "toChars(D[C)I"),
        ("java/lang/Double", "parseChars([CII[D)Z"),
        ("java/lang/Float", "floatToRawIntBits(F)I"),
        ("java/lang/Float", "intBitsToFloat(I)F"),
        ("java/lang/Float", "toChars(F[C)I"),
        ("java/lang/Float", "parseChars([CII[F)Z"),
        ("java/util/ArrayList", "allocate()I"),
        ("java/util/ArrayList", "sizeOf(I)I"),
        ("java/util/ArrayList", "addElement(ILjava/lang/Object;)Z"),
        (
            "java/util/ArrayList",
            "insertElement(IILjava/lang/Object;)V",
        ),
        ("java/util/ArrayList", "getElement(II)Ljava/lang/Object;"),
        (
            "java/util/ArrayList",
            "setElement(IILjava/lang/Object;)Ljava/lang/Object;",
        ),
        ("java/util/ArrayList", "removeElement(II)Ljava/lang/Object;"),
    ]
    .into_iter()
    .map(|(owner, signature)| (owner.to_owned(), signature.to_owned()))
    .collect::<Vec<_>>();
    expected.sort();
    let mut keys = keys;
    keys.sort();
    assert_eq!(keys, expected);
}

/// The package identifies itself the way a manifest and a cache key read it.
#[test]
fn the_package_carries_its_name_its_version_and_its_java() {
    assert_eq!(JavaBase::NAME, "java.base");
    assert_eq!(JavaBase::VERSION, 1);

    // Reachable with no host constructed at all — what an index needs and a language server has.
    assert_ne!(JavaBase::SOURCES.len(), 0);
    assert!(
        JavaBase::SOURCES
            .iter()
            .any(|source| source.path.as_ref() == "java/lang/String.java")
    );

    let package = JavaBase::package(Rc::new(SilentHost));
    assert_eq!(package.name(), JavaBase::NAME);
    assert_eq!(package.version(), JavaBase::VERSION);
    assert_eq!(package.sources().len(), JavaBase::SOURCES.len());
}

/// `java.lang.Object` and the containers nobody implemented are **declared and never compiled**,
/// while the list family is real code a module links.
///
/// The property that used to be prose a reviewer had to enforce: `Object` is the wasm backend's
/// `anyref`, so a compiled one would be a second answer to the same question. And `ArrayList` is
/// the first container that *is* compiled — its storage is a Rust `Vec`, and the interfaces it
/// implements have to exist as types in the module for a call through one to lower.
#[test]
fn object_is_declared_and_never_compiled_and_the_list_family_is() {
    let signatures: Vec<&str> = JavaBase::SOURCES
        .iter()
        .filter(|source| matches!(source.kind, SourceKind::Signatures))
        .map(|source| source.path.as_ref())
        .collect();
    assert!(signatures.contains(&"java/lang/Object.java"));
    assert!(signatures.contains(&"java/util/Map.java"));
    assert!(signatures.contains(&"java/util/Optional.java"));

    let implemented: Vec<&str> = JavaBase::SOURCES
        .iter()
        .filter(|source| matches!(source.kind, SourceKind::Implementation))
        .map(|source| source.path.as_ref())
        .collect();
    assert!(implemented.contains(&"java/lang/String.java"));
    assert!(implemented.contains(&"java/lang/Iterable.java"));
    assert!(implemented.contains(&"java/util/ArrayList.java"));
    assert!(implemented.contains(&"java/util/List.java"));
    assert!(implemented.contains(&"java/util/Collection.java"));
    assert!(implemented.contains(&"java/util/Iterator.java"));

    // And the tier rule reads the same way through `tiers`, which is what a host folds in.
    let linked: Vec<&str> = JavaBase::tiers(true)
        .into_iter()
        .zip(JavaBase::SOURCES)
        .filter(|((_, running), _)| *running)
        .map(|(_, source)| source.path.as_ref())
        .collect();
    assert_eq!(linked, implemented);
    assert!(
        JavaBase::tiers(false).iter().all(|(_, running)| !running),
        "a build that links nothing reads every unit as a record"
    );
}

/// The two streams stay apart until each is flushed, and a flush is what decodes.
///
/// Buffering is not an optimisation: a `char` is a UTF-16 code unit, and a host decoding each one
/// as it arrived could not join a surrogate pair.
#[test]
fn the_streams_buffer_until_they_flush_and_stay_apart() {
    let host = Rc::new(CapturedHost::new());
    let package = JavaBase::package(Rc::clone(&host) as Rc<dyn PlatformHost>);
    let write = |stream: i32, text: &str, fake: &mut FakeHost| {
        let (_, _, binding) = package
            .bindings()
            .find(|(_, key, _)| *key == "writeUnits(I[CII)V")
            .expect("the write binding");
        let units: Vec<i32> = text.encode_utf16().map(i32::from).collect();
        *fake.array.borrow_mut() = units.clone();
        let mut results: [NativeValue; 0] = [];
        binding(
            fake,
            Args::new(&[
                NativeValue::I32(stream),
                NativeValue::Ref(FakeHost::ARRAY),
                NativeValue::I32(0),
                NativeValue::I32(i32::try_from(units.len()).expect("short")),
            ]),
            Results::new(&mut results),
        )
        .expect("a write");
    };
    let flush = |stream: i32, fake: &mut FakeHost| {
        let (_, _, binding) = package
            .bindings()
            .find(|(_, key, _)| *key == "flushStream(I)V")
            .expect("the flush binding");
        let mut results: [NativeValue; 0] = [];
        binding(
            fake,
            Args::new(&[NativeValue::I32(stream)]),
            Results::new(&mut results),
        )
        .expect("a flush");
    };

    let mut fake = FakeHost::with(&[]);
    write(0, "out", &mut fake);
    write(1, "err", &mut fake);
    // Nothing yet: a code unit is half of a surrogate pair.
    assert_eq!(host.take_out(), "");
    assert_eq!(host.take_err(), "");

    flush(0, &mut fake);
    assert_eq!(host.take_out(), "out");
    assert_eq!(host.take_err(), "", "the other stream is untouched");
    flush(1, &mut fake);
    assert_eq!(host.take_err(), "err");

    // A second flush with nothing buffered writes nothing at all.
    flush(0, &mut fake);
    assert_eq!(host.take_out(), "");
}

/// A rendering is written into the array the **module** allocated, because a binding cannot
/// allocate: a wasm embedder has no `struct.new` of its own.
#[test]
fn a_rendering_is_written_into_the_modules_own_array() {
    let mut host = FakeHost::with(&[0; 32]);
    let mut results = [NativeValue::Null];
    call(
        "toChars(D[C)I",
        &mut host,
        &[NativeValue::F64(1.5), NativeValue::Ref(FakeHost::ARRAY)],
        &mut results,
    )
    .expect("a rendering");
    assert_eq!(results[0], NativeValue::I32(3));
    assert!(host.text().starts_with("1.5"));

    // An array too small to hold the rendering is a refusal, not a truncation: a truncated number
    // is a wrong number, and one that looks right is worse than a refusal.
    let mut small = FakeHost::with(&[0; 2]);
    assert!(
        call(
            "toChars(D[C)I",
            &mut small,
            &[NativeValue::F64(1.5), NativeValue::Ref(FakeHost::ARRAY)],
            &mut results,
        )
        .is_err()
    );
}

/// The bit casts round-trip, and a `float` is rendered at `float` width.
#[test]
fn the_floating_point_seam_round_trips_at_each_width() {
    let mut host = FakeHost::with(&[]);
    let mut results = [NativeValue::Null];

    call(
        "doubleToRawLongBits(D)J",
        &mut host,
        &[NativeValue::F64(1.5)],
        &mut results,
    )
    .expect("the bits");
    let bits = results[0];
    call("longBitsToDouble(J)D", &mut host, &[bits], &mut results).expect("the value");
    assert_eq!(results[0], NativeValue::F64(1.5));

    call(
        "floatToRawIntBits(F)I",
        &mut host,
        &[NativeValue::F32(0.1)],
        &mut results,
    )
    .expect("the bits");
    let bits = results[0];
    call("intBitsToFloat(I)F", &mut host, &[bits], &mut results).expect("the value");
    assert_eq!(results[0], NativeValue::F32(0.1));

    // `0.1f` at `float` width is `0.1`; widened to a `double` first it is `0.10000000149011612`.
    let mut buffer = FakeHost::with(&[0; 32]);
    call(
        "toChars(F[C)I",
        &mut buffer,
        &[NativeValue::F32(0.1), NativeValue::Ref(FakeHost::ARRAY)],
        &mut results,
    )
    .expect("a rendering");
    assert!(buffer.text().starts_with("0.1"));
    assert_eq!(results[0], NativeValue::I32(3));
}

/// A parse reads the text out of the module's array and reports its verdict as a **value**.
///
/// The verdict is the point. A binding that refused would become a trap, and a trap stops the
/// module — so `Double.parseDouble("12x")` would be unrecoverable where `Integer.parseInt("12x")`,
/// which is ordinary Java, throws a `NumberFormatException` a program can catch. Two spellings of
/// the same operation must not answer differently, so the failure crosses as `false` and the Java
/// half raises the exception.
#[test]
fn a_parse_answers_with_a_verdict_rather_than_refusing() {
    let mut results = [NativeValue::Null];
    let digits: Vec<i32> = "1.5".encode_utf16().map(i32::from).collect();
    let mut host = FakeHost::with(&digits);
    call(
        "parseChars([CII[D)Z",
        &mut host,
        &[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(0),
            NativeValue::I32(3),
            NativeValue::Ref(FakeHost::OUT),
        ],
        &mut results,
    )
    .expect("a parse");
    assert_eq!(results[0], NativeValue::I32(1));
    assert_eq!(host.decoded(), NativeValue::F64(1.5));

    let bad: Vec<i32> = "12x".encode_utf16().map(i32::from).collect();
    let mut host = FakeHost::with(&bad);
    call(
        "parseChars([CII[D)Z",
        &mut host,
        &[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(0),
            NativeValue::I32(3),
            NativeValue::Ref(FakeHost::OUT),
        ],
        &mut results,
    )
    .expect("a refusal is not an error");
    assert_eq!(results[0], NativeValue::I32(0));
    // Untouched, so a Java half that ignored the verdict would read `null` rather than a number it
    // could mistake for an answer.
    assert_eq!(host.decoded(), NativeValue::Null);

    // The `float` half answers the same way, at `float` width.
    let digits: Vec<i32> = "0.1".encode_utf16().map(i32::from).collect();
    let mut host = FakeHost::with(&digits);
    call(
        "parseChars([CII[F)Z",
        &mut host,
        &[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(0),
            NativeValue::I32(3),
            NativeValue::Ref(FakeHost::OUT),
        ],
        &mut results,
    )
    .expect("a parse");
    assert_eq!(results[0], NativeValue::I32(1));
    assert_eq!(host.decoded(), NativeValue::F32(0.1));
}

/// The clock is the host's, and a host with none is a real host.
#[test]
fn the_clock_is_the_hosts_and_a_clockless_host_is_a_real_one() {
    let host = Rc::new(CapturedHost::at(1_700_000_000_000));
    let package = JavaBase::package(Rc::clone(&host) as Rc<dyn PlatformHost>);
    let (_, _, binding) = package
        .bindings()
        .find(|(_, key, _)| *key == "currentTimeMillis()J")
        .expect("the clock binding");
    let mut fake = FakeHost::with(&[]);
    let mut results = [NativeValue::Null];
    binding(&mut fake, Args::new(&[]), Results::new(&mut results)).expect("a reading");
    assert_eq!(results[0], NativeValue::I64(1_700_000_000_000));

    // A language server instantiates nothing, so it has no clock to offer and needs none.
    assert_eq!(SilentHost.current_time_millis(), 0);
    assert_eq!(SilentHost.nano_time(), 0);
    SilentHost.write(Stream::Out, "discarded");
    SilentHost.write(Stream::Err, "discarded");
}

/// The opt-in package: `jals.io` writes through the same host stream `System.out` does, and only
/// when it flushes.
#[test]
fn the_opt_in_package_buffers_until_it_is_flushed() {
    let host = Rc::new(CapturedHost::new());
    let package = jals_platform::JalsIo::package(Rc::clone(&host) as Rc<dyn PlatformHost>);
    let binding = |signature: &str| {
        package
            .bindings()
            .find(|(_, key, _)| *key == signature)
            .map(|(_, _, binding)| Rc::clone(binding))
            .expect("a bound signature")
    };
    let write_chars = binding("writeChars([CII)V");
    let flush = binding("flush()V");

    let mut fake = FakeHost::with(&['a' as i32, 'b' as i32, 'c' as i32]);
    let mut none: [NativeValue; 0] = [];
    write_chars(
        &mut fake,
        Args::new(&[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(0),
            NativeValue::I32(3),
        ]),
        Results::new(&mut none),
    )
    .expect("a write");
    assert_eq!(host.take_out(), "", "nothing until a flush");
    flush(&mut fake, Args::new(&[]), Results::new(&mut none)).expect("a flush");
    assert_eq!(host.take_out(), "abc");

    // A range outside the array is a refusal, not a short write.
    assert!(
        write_chars(
            &mut fake,
            Args::new(&[
                NativeValue::Ref(FakeHost::ARRAY),
                NativeValue::I32(2),
                NativeValue::I32(9),
            ]),
            Results::new(&mut none),
        )
        .is_err()
    );
}

/// The built-in set is one value, and the two packages it names are the platform and the opt-in
/// demonstration.
#[test]
fn the_builtin_set_is_the_platform_and_the_opt_in_package() {
    let packages = jals_platform::Builtin::packages(Rc::new(SilentHost));
    let names: Vec<&str> = packages
        .iter()
        .map(jals_native::JavaPackage::name)
        .collect();
    assert_eq!(names, vec!["java.base", jals_platform::JalsIo::NAME]);
}

/// The native list, driven without an engine: a `Vec` in Rust, elements retained across calls,
/// and named refusals for an index that is not one.
#[test]
fn the_native_list_stores_references_in_rust_across_calls() {
    let package = JavaBase::package(Rc::new(SilentHost));
    let binding = |signature: &str| {
        package
            .bindings()
            .find(|(_, key, _)| *key == signature)
            .map(|(_, _, binding)| Rc::clone(binding))
            .expect("a bound signature")
    };
    let allocate = binding("allocate()I");
    let size_of = binding("sizeOf(I)I");
    let add = binding("addElement(ILjava/lang/Object;)Z");
    let insert = binding("insertElement(IILjava/lang/Object;)V");
    let get = binding("getElement(II)Ljava/lang/Object;");
    let set = binding("setElement(IILjava/lang/Object;)Ljava/lang/Object;");
    let remove = binding("removeElement(II)Ljava/lang/Object;");

    let mut fake = FakeHost::with(&[]);
    let mut results = [NativeValue::Null];
    let mut none: [NativeValue; 0] = [];

    allocate(&mut fake, Args::new(&[]), Results::new(&mut results)).expect("an allocation");
    let NativeValue::I32(handle) = results[0] else {
        panic!("an allocation answers with the handle");
    };

    // Two references and a null. The references are retained, so the id survives the call.
    add(
        &mut fake,
        Args::new(&[NativeValue::I32(handle), NativeValue::Ref(RefSlot::new(7))]),
        Results::new(&mut results),
    )
    .expect("an add");
    assert_eq!(results[0], NativeValue::I32(1));
    add(
        &mut fake,
        Args::new(&[NativeValue::I32(handle), NativeValue::Null]),
        Results::new(&mut results),
    )
    .expect("a null add");
    size_of(
        &mut fake,
        Args::new(&[NativeValue::I32(handle)]),
        Results::new(&mut results),
    )
    .expect("a size");
    assert_eq!(results[0], NativeValue::I32(2));

    get(
        &mut fake,
        Args::new(&[NativeValue::I32(handle), NativeValue::I32(0)]),
        Results::new(&mut results),
    )
    .expect("a get");
    assert!(matches!(results[0], NativeValue::Ref(_)));
    get(
        &mut fake,
        Args::new(&[NativeValue::I32(handle), NativeValue::I32(1)]),
        Results::new(&mut results),
    )
    .expect("a null get");
    assert_eq!(results[0], NativeValue::Null);

    // A set returns the element it replaced and keeps the new one.
    set(
        &mut fake,
        Args::new(&[
            NativeValue::I32(handle),
            NativeValue::I32(0),
            NativeValue::Ref(RefSlot::new(8)),
        ]),
        Results::new(&mut results),
    )
    .expect("a set");
    assert!(matches!(results[0], NativeValue::Ref(_)));
    get(
        &mut fake,
        Args::new(&[NativeValue::I32(handle), NativeValue::I32(0)]),
        Results::new(&mut results),
    )
    .expect("a get");
    assert_eq!(results[0], NativeValue::Ref(RefSlot::new(8)));

    // An insert at the end, and a remove from the front.
    insert(
        &mut fake,
        Args::new(&[
            NativeValue::I32(handle),
            NativeValue::I32(2),
            NativeValue::Ref(RefSlot::new(9)),
        ]),
        Results::new(&mut none),
    )
    .expect("an insert");
    remove(
        &mut fake,
        Args::new(&[NativeValue::I32(handle), NativeValue::I32(0)]),
        Results::new(&mut results),
    )
    .expect("a remove");
    assert_eq!(results[0], NativeValue::Ref(RefSlot::new(8)));

    // An index that is not one is a refusal, not an answer from elsewhere.
    assert!(
        get(
            &mut fake,
            Args::new(&[NativeValue::I32(handle), NativeValue::I32(9)]),
            Results::new(&mut results),
        )
        .is_err()
    );
    assert!(
        insert(
            &mut fake,
            Args::new(&[
                NativeValue::I32(handle),
                NativeValue::I32(9),
                NativeValue::Ref(RefSlot::new(1)),
            ]),
            Results::new(&mut none),
        )
        .is_err()
    );
}
