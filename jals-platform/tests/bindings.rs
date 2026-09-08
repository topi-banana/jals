//! Every published item, driven — and the numbers this crate's prose states.
//!
//! `cargo hawk check` excludes this crate the way it excludes `jals-native` and `jinja`: its API is
//! what a *host* is offered rather than what one consumer happens to call. So this file is what
//! holds the surface honest instead.

use std::rc::Rc;

use jals_native::{Args, NativeError, NativeHost, NativeValue, RefSlot, Results, SourceKind};
use jals_platform::{CapturedHost, JavaBase, PlatformHost, SilentHost, Stream};

/// A host that answers over one `i32` array, so a binding can be driven with no engine at all.
struct FakeHost {
    array: std::cell::RefCell<Vec<i32>>,
}

impl FakeHost {
    const ARRAY: RefSlot = RefSlot::new(0);

    fn with(values: &[i32]) -> Self {
        Self {
            array: std::cell::RefCell::new(values.to_vec()),
        }
    }

    fn text(&self) -> String {
        String::from_utf16_lossy(
            &self
                .array
                .borrow()
                .iter()
                .map(|value| *value as u16)
                .collect::<Vec<_>>(),
        )
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
        _name: &str,
        _args: &[NativeValue],
        _results: &mut [NativeValue],
    ) -> Result<(), NativeError> {
        Err(NativeError::Call("this fixture instantiates nothing".to_owned()))
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

/// **Twelve.** The crate docs, the README and `bindings.rs`'s own table all state that number, and
/// a number stated in a document and checked by nothing is a number that will be wrong.
#[test]
fn the_platform_binds_exactly_the_twelve_operations_java_cannot_express() {
    let package = JavaBase::package(Rc::new(CapturedHost::new()));
    assert_eq!(package.binding_count(), 12);

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
        ("java/lang/Double", "parseChars([CII)D"),
        ("java/lang/Float", "floatToRawIntBits(F)I"),
        ("java/lang/Float", "intBitsToFloat(I)F"),
        ("java/lang/Float", "toChars(F[C)I"),
        ("java/lang/Float", "parseChars([CII)F"),
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
    assert!(!JavaBase::SOURCES.is_empty());
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

/// `java.lang.Object` and every `java.util` type are **declared and never compiled**.
///
/// The property that used to be prose a reviewer had to enforce: `Object` is the wasm backend's
/// `anyref`, so a compiled one would be a second answer to the same question.
#[test]
fn object_and_java_util_are_declared_and_never_compiled() {
    let signatures: Vec<&str> = JavaBase::SOURCES
        .iter()
        .filter(|source| matches!(source.kind, SourceKind::Signatures))
        .map(|source| source.path.as_ref())
        .collect();
    assert!(signatures.contains(&"java/lang/Object.java"));
    assert!(signatures.contains(&"java/lang/Iterable.java"));
    assert!(signatures.iter().any(|path| path.starts_with("java/util/")));

    let implemented: Vec<&str> = JavaBase::SOURCES
        .iter()
        .filter(|source| matches!(source.kind, SourceKind::Implementation))
        .map(|source| source.path.as_ref())
        .collect();
    assert!(implemented.contains(&"java/lang/String.java"));
    assert!(!implemented.iter().any(|path| path.starts_with("java/util/")));

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

/// A parse reads the text out of the module's array, and refuses what Java refuses.
#[test]
fn a_parse_reads_the_modules_array_and_refuses_what_java_refuses() {
    let mut results = [NativeValue::Null];
    let digits: Vec<i32> = "1.5".encode_utf16().map(i32::from).collect();
    let mut host = FakeHost::with(&digits);
    call(
        "parseChars([CII)D",
        &mut host,
        &[
            NativeValue::Ref(FakeHost::ARRAY),
            NativeValue::I32(0),
            NativeValue::I32(3),
        ],
        &mut results,
    )
    .expect("a parse");
    assert_eq!(results[0], NativeValue::F64(1.5));

    let bad: Vec<i32> = "12x".encode_utf16().map(i32::from).collect();
    let mut host = FakeHost::with(&bad);
    assert!(
        call(
            "parseChars([CII)D",
            &mut host,
            &[
                NativeValue::Ref(FakeHost::ARRAY),
                NativeValue::I32(0),
                NativeValue::I32(3),
            ],
            &mut results,
        )
        .is_err()
    );
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
