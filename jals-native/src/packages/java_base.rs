//! `java.base` — the JDK module a WebAssembly host does not have, written as a native package.
//!
//! `jals-hir` ships signature-only stubs for `java.lang` and `java.io` so a reference to `String`
//! or `IOException` resolves. They are *bones*: no bodies, and nothing to run. This package is the
//! other half — the same types with their implementations, compiled into the module the project is
//! compiled into, so `new String(chars)`, `Integer.parseInt(text)`, `throw new
//! IllegalStateException(message)` and `System.out.println(text)` are calls that reach something.
//!
//! The name is the JDK's own module name, and it is deliberate: the wasm backend's diagnostics say
//! "a wasm host has no `java.base` to supply the rest" in so many words. This is that `java.base`,
//! and it is why one package publishes two Java packages — `System.out` is a `java.io.PrintStream`
//! in the JDK and in the stub it shadows, so a `java.lang` that superseded the stub without
//! `java.io` beside it would be a `System` with no `out`.
//!
//! # What is Java and what is Rust
//!
//! Ten host functions, and everything else is Java. They are exactly the operations Java cannot
//! express and this crate can:
//!
//! | binding | why it cannot be Java |
//! | --- | --- |
//! | `Double`/`Float` bit casts | there is no reinterpreting cast in Java |
//! | `Double`/`Float` render and parse | shortest-round-trip decimal is a rounding problem, and `core` already solves it correctly |
//! | `System.currentTimeMillis` / `nanoTime` | a module has no clock |
//! | `PrintStream` write and flush | a module has no console |
//!
//! Everything a program actually calls — `String`, `StringBuilder`, the wrappers, `Math`, the
//! whole `Throwable` hierarchy — is ordinary Java, one file per type under this crate's
//! `java/java` directory, lowered by the same backend that lowers the project's own sources.
//!
//! # What is not here
//!
//! - **`java.lang.Object`.** It is the backend's `anyref` — the top of wasm's reference hierarchy
//!   — and giving it a struct type as well would be one question with two answers. The stub's
//!   `Object` needs no body, so it stays.
//! - **`Enum`, `Record`, `Iterable`, and reflection.** The first two are supertypes the compiler
//!   synthesises and whose members involve facts this target does not carry; `Iterable` needs
//!   `java.util.Iterator`, which is another package's; reflection needs a runtime that reads
//!   metadata, and there is none.
//! - **`Math`'s transcendentals and full Unicode case mapping.** Both would be approximations, and
//!   each class says so where it declares what it *does* answer.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::host::NativeHost;
use crate::package::NativePackage;
use crate::value::{Args, NativeError, NativeValue, Results};

/// Which of the host's two sinks a write is for.
///
/// A value rather than a `bool`, because the two are not "on and off": `jals` sends one to stdout
/// and the other to stderr, and a host that folded them would put a program's output in the same
/// place as its diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// `System.out` — the program's own output.
    Out,
    /// `System.err` — the program's diagnostics.
    Err,
}

/// What `java.base` asks of the host it was built for.
///
/// The console and the clock, which are the two things a WebAssembly module cannot reach on its
/// own. `&self` rather than `&mut self` for the same reason [`ConsoleSink`] takes it: several
/// bindings hold their own `Rc` of one host, and a host that needs interior mutability already has
/// `RefCell` — every runtime in this workspace is current-thread, so nothing here needs `Sync`.
///
/// [`ConsoleSink`]: crate::packages::jals_io::ConsoleSink
pub trait SystemHost {
    /// Write `text`, which is one flush's worth of buffered code units, decoded.
    fn write(&self, stream: Stream, text: &str);

    /// Milliseconds since the Unix epoch.
    ///
    /// Provided, answering zero, because a host without a clock is a real host rather than a
    /// broken one — a language server describing a project never instantiates a module, and a
    /// deterministic test harness may not want one. A program that reads it gets a constant, which
    /// is a wrong time rather than a trap.
    fn current_time_millis(&self) -> i64 {
        0
    }

    /// A monotonic reading in nanoseconds, meaningful only as a difference from another one.
    ///
    /// Provided for the reason [`current_time_millis`](Self::current_time_millis) is.
    fn nano_time(&self) -> i64 {
        0
    }
}

/// The `java.base` package.
pub struct JavaBase;

impl JavaBase {
    /// The package's name, as `[build] native-packages` spells it.
    pub const NAME: &'static str = "java.base";

    /// How many code units a rendered `double` or `float` may take.
    ///
    /// The Java side allocates exactly this, and both sides say so: a shortest round-trip binary64
    /// is at most 17 significant digits, and a sign, a point and `E-324` fit inside the rest.
    const RENDERING_LIMIT: u32 = 32;

    /// Build the package over `host`.
    ///
    /// Version 1. Bump it when a binding starts answering differently for input that did not
    /// change — a consumer that memoized a compile cannot see a closure body change any other way,
    /// and the Java half is already covered because its text is folded into the provenance.
    pub fn package(host: Rc<dyn SystemHost>) -> NativePackage {
        let mut package = NativePackage::new(Self::NAME, 1);
        for (path, text) in SOURCES {
            package.source(path, text);
        }
        Self::bind_streams(&mut package, &host);
        Self::bind_clock(&mut package, host);
        Self::bind_floats(&mut package);
        package
    }

    /// `java.io.PrintStream`'s two host functions.
    ///
    /// One buffer per stream, behind both bindings. The Java side buffers too and hands whole
    /// lines over, so this second buffer is not a speed-up: it is what makes a surrogate pair
    /// reachable no matter how the two halves were split across calls, which is the one thing
    /// neither side can fix alone.
    fn bind_streams(package: &mut NativePackage, host: &Rc<dyn SystemHost>) {
        let pending = Rc::new(RefCell::new(Pending::default()));

        let units = Rc::clone(&pending);
        package.bind(
            "java/io/PrintStream",
            "writeUnits(I[CII)V",
            move |host: &mut dyn NativeHost, args: Args<'_>, _: Results<'_>| {
                let stream = Self::stream_of(args.i32(0)?);
                let text = args.reference(1)?;
                let offset = args.i32(2)?;
                let count = args.i32(3)?;
                let (Ok(offset), Ok(count)) = (u32::try_from(offset), u32::try_from(count)) else {
                    // A negative offset or count is a bounds error, not a host defect: the module
                    // passed what its own Java computed, and Java would throw here.
                    let len = host.array_len(text)?;
                    return Err(NativeError::OutOfBounds { index: 0, len });
                };
                let len = host.array_len(text)?;
                let end = offset.saturating_add(count);
                if end > len {
                    return Err(NativeError::OutOfBounds { index: end, len });
                }
                let mut pending = units.borrow_mut();
                let buffer = pending.of(stream);
                for index in offset..end {
                    let value = host.array_get(text, index)?;
                    let unit = value
                        .as_i32()
                        .ok_or_else(|| NativeError::argument(1, "a char element", value))?;
                    buffer.push(u16::try_from(unit & 0xFFFF).unwrap_or(0));
                }
                Ok(())
            },
        );

        let sink = Rc::clone(host);
        let units = Rc::clone(&pending);
        package.bind(
            "java/io/PrintStream",
            "flushStream(I)V",
            move |_: &mut dyn NativeHost, args: Args<'_>, _: Results<'_>| {
                let stream = Self::stream_of(args.i32(0)?);
                let mut pending = units.borrow_mut();
                let buffer = pending.of(stream);
                if buffer.is_empty() {
                    return Ok(());
                }
                sink.write(stream, &String::from_utf16_lossy(buffer));
                buffer.clear();
                Ok(())
            },
        );
    }

    /// `java.lang.System`'s two clock readings.
    fn bind_clock(package: &mut NativePackage, host: Rc<dyn SystemHost>) {
        let clock = Rc::clone(&host);
        package.bind(
            "java/lang/System",
            "currentTimeMillis()J",
            move |_: &mut dyn NativeHost, _: Args<'_>, mut results: Results<'_>| {
                results.set(0, NativeValue::I64(clock.current_time_millis()));
                Ok(())
            },
        );

        package.bind(
            "java/lang/System",
            "nanoTime()J",
            move |_: &mut dyn NativeHost, _: Args<'_>, mut results: Results<'_>| {
                results.set(0, NativeValue::I64(host.nano_time()));
                Ok(())
            },
        );
    }

    /// The six operations on a floating-point value that Java cannot write.
    fn bind_floats(package: &mut NativePackage) {
        package.bind(
            "java/lang/Double",
            "doubleToRawLongBits(D)J",
            |_: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                results.set(0, NativeValue::I64(args.f64(0)?.to_bits().cast_signed()));
                Ok(())
            },
        );
        package.bind(
            "java/lang/Double",
            "longBitsToDouble(J)D",
            |_: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                results.set(
                    0,
                    NativeValue::F64(f64::from_bits(args.i64(0)?.cast_unsigned())),
                );
                Ok(())
            },
        );
        package.bind(
            "java/lang/Float",
            "floatToRawIntBits(F)I",
            |_: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                results.set(0, NativeValue::I32(args.f32(0)?.to_bits().cast_signed()));
                Ok(())
            },
        );
        package.bind(
            "java/lang/Float",
            "intBitsToFloat(I)F",
            |_: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                results.set(
                    0,
                    NativeValue::F32(f32::from_bits(args.i32(0)?.cast_unsigned())),
                );
                Ok(())
            },
        );
        package.bind(
            "java/lang/Double",
            "toChars(D[C)I",
            |host: &mut dyn NativeHost, args: Args<'_>, results: Results<'_>| {
                let rendered = Decimal::of_f64(args.f64(0)?);
                Self::write_rendering(host, args.reference(1)?, &rendered, results)
            },
        );
        package.bind(
            "java/lang/Float",
            "toChars(F[C)I",
            |host: &mut dyn NativeHost, args: Args<'_>, results: Results<'_>| {
                let rendered = Decimal::of_f32(args.f32(0)?);
                Self::write_rendering(host, args.reference(1)?, &rendered, results)
            },
        );
        package.bind(
            "java/lang/Double",
            "parseChars([CII)D",
            |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                let text = Self::argument_text(host, &args)?;
                let value = Decimal::parse(&text).unwrap_or(f64::NAN);
                results.set(0, NativeValue::F64(value));
                Ok(())
            },
        );
        package.bind(
            "java/lang/Float",
            "parseChars([CII)F",
            |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                let text = Self::argument_text(host, &args)?;
                let value = Decimal::parse_f32(&text).unwrap_or(f32::NAN);
                results.set(0, NativeValue::F32(value));
                Ok(())
            },
        );
    }

    /// `(char[] text, int offset, int count)` at positions 0..3, decoded.
    fn argument_text(host: &mut dyn NativeHost, args: &Args<'_>) -> Result<String, NativeError> {
        let text = args.reference(0)?;
        let offset = args.i32(1)?;
        let count = args.i32(2)?;
        let (Ok(offset), Ok(count)) = (u32::try_from(offset), u32::try_from(count)) else {
            let len = host.array_len(text)?;
            return Err(NativeError::OutOfBounds { index: 0, len });
        };
        host.array_text(text, offset, count)
    }

    /// Write `rendered` into the array in `slot` and report how many code units it took.
    fn write_rendering(
        host: &mut dyn NativeHost,
        slot: crate::value::RefSlot,
        rendered: &str,
        mut results: Results<'_>,
    ) -> Result<(), NativeError> {
        let len = host.array_len(slot)?;
        let mut written = 0_u32;
        for unit in rendered.encode_utf16() {
            if written >= len || written >= Self::RENDERING_LIMIT {
                // Unreachable: the Java side allocates `RENDERING_LIMIT` and no rendering is that
                // long. Reported rather than truncated all the same, because a truncated number is
                // a different number and nothing downstream would notice.
                return Err(NativeError::OutOfBounds {
                    index: written,
                    len,
                });
            }
            host.array_set(slot, written, NativeValue::I32(i32::from(unit)))?;
            written += 1;
        }
        results.set(0, NativeValue::I32(written.cast_signed()));
        Ok(())
    }

    /// The stream a `java.lang.System` identifier names.
    ///
    /// Everything that is not the error stream is the output one, which is what makes
    /// `new PrintStream(n)` total: a `PrintStream` this package did not build still writes
    /// somewhere rather than trapping.
    const fn stream_of(identifier: i32) -> Stream {
        if identifier == 1 {
            Stream::Err
        } else {
            Stream::Out
        }
    }
}

/// What each stream has buffered since its last flush.
#[derive(Debug, Default)]
struct Pending {
    out: Vec<u16>,
    err: Vec<u16>,
}

impl Pending {
    /// The buffer for one stream.
    const fn of(&mut self, stream: Stream) -> &mut Vec<u16> {
        match stream {
            Stream::Out => &mut self.out,
            Stream::Err => &mut self.err,
        }
    }
}

/// Java's decimal rendering of a floating-point value, and its reading of one.
///
/// `core` already answers the hard half — the shortest decimal that reads back as the same value —
/// and what is left is that Java lays those digits out differently from Rust: `1.0` where Rust
/// writes `1`, `1.0E-4` where Rust writes `0.0001`, `Infinity` where Rust writes `inf`. So this
/// takes `core`'s scientific form and re-lays it, rather than producing digits of its own.
struct Decimal;

impl Decimal {
    /// The exponent range Java renders without an `E`: `10^-3 <= |value| < 10^7`.
    const PLAIN: core::ops::RangeInclusive<i32> = -3..=6;

    /// `java.lang.Double.toString(value)`.
    fn of_f64(value: f64) -> String {
        if value.is_nan() {
            return String::from("NaN");
        }
        if value.is_infinite() {
            return String::from(if value < 0.0 { "-Infinity" } else { "Infinity" });
        }
        if value == 0.0 {
            return String::from(if value.is_sign_negative() {
                "-0.0"
            } else {
                "0.0"
            });
        }
        Self::lay_out(&format!("{:e}", value.abs()), value.is_sign_negative())
    }

    /// `java.lang.Float.toString(value)`.
    ///
    /// Formatted from the `f32` and not from its widening, because the shortest decimal that reads
    /// back as the same `float` is not the shortest that reads back as the same `double`:
    /// `0.1f` is `"0.1"` here and `"0.10000000149011612"` there.
    fn of_f32(value: f32) -> String {
        if value.is_nan() {
            return String::from("NaN");
        }
        if value.is_infinite() {
            return String::from(if value < 0.0 { "-Infinity" } else { "Infinity" });
        }
        if value == 0.0 {
            return String::from(if value.is_sign_negative() {
                "-0.0"
            } else {
                "0.0"
            });
        }
        Self::lay_out(&format!("{:e}", value.abs()), value.is_sign_negative())
    }

    /// Java's layout of `core`'s `d.ddde±n` form for a finite, non-zero magnitude.
    fn lay_out(scientific: &str, negative: bool) -> String {
        let Some((mantissa, exponent)) = scientific.split_once('e') else {
            return scientific.to_owned();
        };
        let Ok(exponent) = exponent.parse::<i32>() else {
            return scientific.to_owned();
        };
        let digits: String = mantissa.chars().filter(|unit| *unit != '.').collect();
        let body = if Self::PLAIN.contains(&exponent) {
            Self::plain(&digits, exponent)
        } else {
            let (head, tail) = digits.split_at(1);
            let tail = if tail.is_empty() { "0" } else { tail };
            format!("{head}.{tail}E{exponent}")
        };
        if negative { format!("-{body}") } else { body }
    }

    /// `digits` with a point placed `exponent + 1` in, padded on whichever side is short.
    ///
    /// Java's rendering always has at least one digit on each side of the point, which is the
    /// whole difference from what a naive shift produces: `1e2` is `100.0` and not `100`, and
    /// `1e-3` is `0.001` and not `.001`.
    fn plain(digits: &str, exponent: i32) -> String {
        if exponent < 0 {
            let zeros = usize::try_from(-exponent - 1).unwrap_or(0);
            return format!("0.{:0<width$}{digits}", "", width = zeros);
        }
        let point = usize::try_from(exponent + 1).unwrap_or(0);
        if digits.len() > point {
            let (whole, fraction) = digits.split_at(point);
            return format!("{whole}.{fraction}");
        }
        let zeros = point - digits.len();
        format!("{digits}{:0<width$}.0", "", width = zeros)
    }

    /// `java.lang.Double.parseDouble(text)`, or `None` for text that spells no number.
    ///
    /// Java accepts a trailing type suffix (`1.5d`, `1.5F`) where Rust does not; everything else
    /// the two accept is the same set, `Infinity` and `NaN` included.
    fn parse(text: &str) -> Option<f64> {
        Self::without_suffix(text).parse::<f64>().ok()
    }

    /// [`parse`](Self::parse) at `f32` width, so a `Float.parseFloat` rounds once rather than
    /// twice.
    fn parse_f32(text: &str) -> Option<f32> {
        Self::without_suffix(text).parse::<f32>().ok()
    }

    /// `text` without the `d`/`D`/`f`/`F` a Java literal may end with.
    fn without_suffix(text: &str) -> &str {
        match text.strip_suffix(['d', 'D', 'f', 'F']) {
            // `"inf"` and `"nan"` end in no suffix; `"Inf"` does not either. What does is a number
            // whose last character is a digit or a point once the suffix is off — anything else
            // was a word this must not take a letter from.
            Some(head)
                if head
                    .chars()
                    .next_back()
                    .is_some_and(|unit| unit.is_ascii_digit() || unit == '.') =>
            {
                head
            }
            _ => text,
        }
    }
}

/// A sink that keeps what was written to each stream, so a test can assert on it without a
/// console.
///
/// Shipped for the reason [`CapturedConsole`] is: it is the only way to drive this package's
/// bindings at all, and a consumer writing its own would be writing the same lines to check the
/// same thing.
///
/// [`CapturedConsole`]: crate::packages::jals_io::CapturedConsole
#[derive(Debug, Default)]
pub struct CapturedSystem {
    out: RefCell<String>,
    err: RefCell<String>,
    millis: i64,
}

impl CapturedSystem {
    /// A host that has been written to zero times, whose clock reads zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// A host whose [`current_time_millis`](SystemHost::current_time_millis) answers `millis`.
    pub fn at(millis: i64) -> Self {
        Self {
            millis,
            ..Self::default()
        }
    }

    /// Everything written to `System.out` so far, joined in order.
    pub fn take_out(&self) -> String {
        core::mem::take(&mut self.out.borrow_mut())
    }

    /// Everything written to `System.err` so far, joined in order.
    pub fn take_err(&self) -> String {
        core::mem::take(&mut self.err.borrow_mut())
    }
}

impl SystemHost for CapturedSystem {
    fn write(&self, stream: Stream, text: &str) {
        match stream {
            Stream::Out => self.out.borrow_mut().push_str(text),
            Stream::Err => self.err.borrow_mut().push_str(text),
        }
    }

    fn current_time_millis(&self) -> i64 {
        self.millis
    }

    fn nano_time(&self) -> i64 {
        self.millis.saturating_mul(1_000_000)
    }
}

/// The Java this package publishes, in path order.
///
/// A real `.java` file for each type rather than a string literal, exactly as `jals.io`'s is: it
/// is Java, so it is edited, read and highlighted as Java, and one public type per file is the
/// convention every reader already has. `include_str!` makes each one a compile-time constant, so
/// the crate stays pure and needs no I/O to publish them.
const SOURCES: &[(&str, &str)] = &[
    (
        "java/io/Closeable.java",
        include_str!("../../java/java/io/Closeable.java"),
    ),
    (
        "java/io/FileNotFoundException.java",
        include_str!("../../java/java/io/FileNotFoundException.java"),
    ),
    (
        "java/io/IOException.java",
        include_str!("../../java/java/io/IOException.java"),
    ),
    (
        "java/io/PrintStream.java",
        include_str!("../../java/java/io/PrintStream.java"),
    ),
    (
        "java/io/UncheckedIOException.java",
        include_str!("../../java/java/io/UncheckedIOException.java"),
    ),
    (
        "java/lang/ArithmeticException.java",
        include_str!("../../java/java/lang/ArithmeticException.java"),
    ),
    (
        "java/lang/ArrayIndexOutOfBoundsException.java",
        include_str!("../../java/java/lang/ArrayIndexOutOfBoundsException.java"),
    ),
    (
        "java/lang/AssertionError.java",
        include_str!("../../java/java/lang/AssertionError.java"),
    ),
    (
        "java/lang/AutoCloseable.java",
        include_str!("../../java/java/lang/AutoCloseable.java"),
    ),
    (
        "java/lang/Boolean.java",
        include_str!("../../java/java/lang/Boolean.java"),
    ),
    (
        "java/lang/Byte.java",
        include_str!("../../java/java/lang/Byte.java"),
    ),
    (
        "java/lang/CharSequence.java",
        include_str!("../../java/java/lang/CharSequence.java"),
    ),
    (
        "java/lang/Character.java",
        include_str!("../../java/java/lang/Character.java"),
    ),
    (
        "java/lang/Class.java",
        include_str!("../../java/java/lang/Class.java"),
    ),
    (
        "java/lang/ClassCastException.java",
        include_str!("../../java/java/lang/ClassCastException.java"),
    ),
    (
        "java/lang/ClassNotFoundException.java",
        include_str!("../../java/java/lang/ClassNotFoundException.java"),
    ),
    (
        "java/lang/CloneNotSupportedException.java",
        include_str!("../../java/java/lang/CloneNotSupportedException.java"),
    ),
    (
        "java/lang/Comparable.java",
        include_str!("../../java/java/lang/Comparable.java"),
    ),
    (
        "java/lang/Deprecated.java",
        include_str!("../../java/java/lang/Deprecated.java"),
    ),
    (
        "java/lang/Double.java",
        include_str!("../../java/java/lang/Double.java"),
    ),
    (
        "java/lang/Error.java",
        include_str!("../../java/java/lang/Error.java"),
    ),
    (
        "java/lang/Exception.java",
        include_str!("../../java/java/lang/Exception.java"),
    ),
    (
        "java/lang/Float.java",
        include_str!("../../java/java/lang/Float.java"),
    ),
    (
        "java/lang/FunctionalInterface.java",
        include_str!("../../java/java/lang/FunctionalInterface.java"),
    ),
    (
        "java/lang/IllegalAccessException.java",
        include_str!("../../java/java/lang/IllegalAccessException.java"),
    ),
    (
        "java/lang/IllegalArgumentException.java",
        include_str!("../../java/java/lang/IllegalArgumentException.java"),
    ),
    (
        "java/lang/IllegalStateException.java",
        include_str!("../../java/java/lang/IllegalStateException.java"),
    ),
    (
        "java/lang/IndexOutOfBoundsException.java",
        include_str!("../../java/java/lang/IndexOutOfBoundsException.java"),
    ),
    (
        "java/lang/InstantiationException.java",
        include_str!("../../java/java/lang/InstantiationException.java"),
    ),
    (
        "java/lang/Integer.java",
        include_str!("../../java/java/lang/Integer.java"),
    ),
    (
        "java/lang/InterruptedException.java",
        include_str!("../../java/java/lang/InterruptedException.java"),
    ),
    (
        "java/lang/Long.java",
        include_str!("../../java/java/lang/Long.java"),
    ),
    (
        "java/lang/Math.java",
        include_str!("../../java/java/lang/Math.java"),
    ),
    (
        "java/lang/NegativeArraySizeException.java",
        include_str!("../../java/java/lang/NegativeArraySizeException.java"),
    ),
    (
        "java/lang/NoSuchFieldException.java",
        include_str!("../../java/java/lang/NoSuchFieldException.java"),
    ),
    (
        "java/lang/NoSuchMethodException.java",
        include_str!("../../java/java/lang/NoSuchMethodException.java"),
    ),
    (
        "java/lang/NullPointerException.java",
        include_str!("../../java/java/lang/NullPointerException.java"),
    ),
    (
        "java/lang/Number.java",
        include_str!("../../java/java/lang/Number.java"),
    ),
    (
        "java/lang/NumberFormatException.java",
        include_str!("../../java/java/lang/NumberFormatException.java"),
    ),
    (
        "java/lang/Override.java",
        include_str!("../../java/java/lang/Override.java"),
    ),
    (
        "java/lang/ReflectiveOperationException.java",
        include_str!("../../java/java/lang/ReflectiveOperationException.java"),
    ),
    (
        "java/lang/RuntimeException.java",
        include_str!("../../java/java/lang/RuntimeException.java"),
    ),
    (
        "java/lang/SafeVarargs.java",
        include_str!("../../java/java/lang/SafeVarargs.java"),
    ),
    (
        "java/lang/Short.java",
        include_str!("../../java/java/lang/Short.java"),
    ),
    (
        "java/lang/String.java",
        include_str!("../../java/java/lang/String.java"),
    ),
    (
        "java/lang/StringBuilder.java",
        include_str!("../../java/java/lang/StringBuilder.java"),
    ),
    (
        "java/lang/StringIndexOutOfBoundsException.java",
        include_str!("../../java/java/lang/StringIndexOutOfBoundsException.java"),
    ),
    (
        "java/lang/SuppressWarnings.java",
        include_str!("../../java/java/lang/SuppressWarnings.java"),
    ),
    (
        "java/lang/System.java",
        include_str!("../../java/java/lang/System.java"),
    ),
    (
        "java/lang/Throwable.java",
        include_str!("../../java/java/lang/Throwable.java"),
    ),
    (
        "java/lang/UnsupportedOperationException.java",
        include_str!("../../java/java/lang/UnsupportedOperationException.java"),
    ),
    (
        "java/lang/Void.java",
        include_str!("../../java/java/lang/Void.java"),
    ),
];

#[cfg(test)]
mod tests {
    use super::{Decimal, JavaBase};

    /// Java's own renderings, which are what a program reads back.
    #[test]
    fn a_double_renders_the_way_java_renders_one() {
        for (value, expected) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (-1.0, "-1.0"),
            (100.0, "100.0"),
            (123.456, "123.456"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (9_999_999.0, "9999999.0"),
            (1.0e7, "1.0E7"),
            (1.234e-5, "1.234E-5"),
            (f64::MAX, "1.7976931348623157E308"),
            (f64::MIN_POSITIVE, "2.2250738585072014E-308"),
        ] {
            assert_eq!(Decimal::of_f64(value), expected, "rendering {value}");
        }
        assert_eq!(Decimal::of_f64(f64::NAN), "NaN");
        assert_eq!(Decimal::of_f64(f64::INFINITY), "Infinity");
        assert_eq!(Decimal::of_f64(f64::NEG_INFINITY), "-Infinity");
    }

    /// The one place widening first would have been visible.
    #[test]
    fn a_float_renders_at_float_width() {
        assert_eq!(Decimal::of_f32(0.1_f32), "0.1");
        assert_eq!(Decimal::of_f64(f64::from(0.1_f32)), "0.10000000149011612");
        assert_eq!(Decimal::of_f32(1.0_f32), "1.0");
        assert_eq!(Decimal::of_f32(-2.5e10_f32), "-2.5E10");
    }

    /// Every rendering fits the array the Java side allocates for it.
    #[test]
    fn a_rendering_fits_the_buffer_the_java_side_allocates() {
        for value in [
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            5e-324,
            -5e-324,
            f64::NEG_INFINITY,
        ] {
            let rendered = Decimal::of_f64(value);
            let units = u32::try_from(rendered.encode_utf16().count()).expect("a short rendering");
            assert!(
                units <= JavaBase::RENDERING_LIMIT,
                "`{rendered}` is longer than the {} code units `Double.toChars` is given",
                JavaBase::RENDERING_LIMIT
            );
        }
    }

    /// A Java literal's type suffix comes off; a word that merely ends in one does not.
    #[test]
    fn parsing_accepts_what_java_accepts() {
        assert_eq!(Decimal::parse("1.5"), Some(1.5));
        assert_eq!(Decimal::parse("1.5d"), Some(1.5));
        assert_eq!(Decimal::parse("1.5F"), Some(1.5));
        assert_eq!(Decimal::parse("-2e3"), Some(-2000.0));
        assert_eq!(Decimal::parse("Infinity"), Some(f64::INFINITY));
        assert!(Decimal::parse("NaN").is_some_and(f64::is_nan));
        assert_eq!(Decimal::parse("hello"), None);
        assert_eq!(Decimal::parse(""), None);
        assert_eq!(Decimal::parse_f32("0.1"), Some(0.1_f32));
    }
}
