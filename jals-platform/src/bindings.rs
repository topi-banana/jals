//! The Rust half: twelve host functions, and why it is exactly twelve.
//!
//! Every one is an operation **Java cannot express** and a dependency-free `no_std` crate can.
//! Nothing here is a convenience or a shortcut past Java that was inconvenient to write — a thing
//! this package could do in Java, it does in Java, which is why `Math.sqrt` and every text
//! conversion of an integer are on the other side of the seam.
//!
//! | count | what | why Java cannot |
//! | --- | --- | --- |
//! | 2 | `PrintStream.writeUnits`, `flushStream` | there is no output on this target but the host's |
//! | 2 | `System.currentTimeMillis`, `nanoTime` | there is no clock on this target but the host's |
//! | 4 | `Double`/`Float` bit casts, both directions | a reinterpretation is not an arithmetic operation |
//! | 4 | `Double`/`Float` render and parse | the shortest round-tripping decimal is a hard problem `core` already solves |
//!
//! The count is stated in this crate's prose, and `tests/bindings.rs` asserts it against
//! [`JavaPackage::binding_count`](jals_native::JavaPackage::binding_count). A number in a document
//! that nothing checks is a number that will be wrong.
//!
//! # A binding cannot allocate
//!
//! A wasm embedder has no `struct.new` of its own, so nothing here can hand back a Java object. The
//! two that produce text therefore write into an array *the module* allocated and return how many
//! characters they wrote — which is why `Double.toChars` takes a `char[]` and `Double.toString`
//! is the Java wrapper around it.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use jals_native::{Args, JavaPackage, NativeError, NativeHost, NativeValue, Results};

use crate::decimal::Decimal;
use crate::host::{PlatformHost, Stream};

/// How many code units a rendered `double` or `float` may take.
///
/// The same number `java.lang.Double.RENDERING_LIMIT` allocates. Past it the binding **refuses**
/// rather than truncating: a truncated number is a wrong number, and one that looks right is worse
/// than a refusal. `decimal.rs` pins the bound against the widest values there are.
const RENDERING_LIMIT: u32 = 32;

/// The code units each stream has been handed but not yet told to complete.
///
/// One buffer per stream, on the Rust side, because a flush is what tells the host a piece of text
/// is whole — see `java.io.PrintStream` for why the Java half buffers *characters* and this half
/// buffers the decoded result.
#[derive(Default)]
struct Pending {
    out: Vec<u16>,
    err: Vec<u16>,
}

impl Pending {
    /// The buffer for one stream.
    fn of(&mut self, stream: Stream) -> &mut Vec<u16> {
        match stream {
            Stream::Out => &mut self.out,
            Stream::Err => &mut self.err,
        }
    }
}

/// Installs the platform's bindings onto a package.
pub struct Bindings;

impl Bindings {
    /// Bind every `native` method the platform's Java declares.
    ///
    /// The three groups are separate functions because they need different things: the streams need
    /// shared mutable state, the clock needs the host and nothing else, and the floating-point
    /// twelve — eight of them — need neither.
    pub fn install(package: &mut JavaPackage, host: &Rc<dyn PlatformHost>) {
        Self::bind_streams(package, host);
        Self::bind_clock(package, host);
        Self::bind_floats(package);
    }

    /// `PrintStream`'s two: append code units, and decode-and-hand-over on a flush.
    fn bind_streams(package: &mut JavaPackage, host: &Rc<dyn PlatformHost>) {
        let pending = Rc::new(RefCell::new(Pending::default()));

        let units = Rc::clone(&pending);
        package.bind(
            "java/io/PrintStream",
            "writeUnits(I[CII)V",
            move |host_ref, args, _| {
                let stream = Stream::of(args.i32(0)?);
                let slot = args.reference(1)?;
                let offset = args.i32(2)?;
                let count = args.i32(3)?;
                let (offset, count) = Self::bounds(host_ref, slot, offset, count)?;
                let mut pending = units.borrow_mut();
                let buffer = pending.of(stream);
                for index in offset..offset + count {
                    let value = host_ref.array_get(slot, index)?;
                    // A `char` crosses as an `i32`: wasm has no sixteen-bit value type, so the
                    // low half is the code unit and the rest is the sign extension of nothing.
                    buffer.push(value.as_i32().unwrap_or(0) as u16);
                }
                Ok(())
            },
        );

        let sink = Rc::clone(host);
        let flushed = Rc::clone(&pending);
        package.bind("java/io/PrintStream", "flushStream(I)V", move |_, args, _| {
            let stream = Stream::of(args.i32(0)?);
            let mut pending = flushed.borrow_mut();
            let buffer = pending.of(stream);
            if buffer.is_empty() {
                return Ok(());
            }
            // Lossy, and the loss is a real one: an unpaired surrogate becomes U+FFFD. That is what
            // a program wrote, though — the pairing happened or it did not, and the buffering above
            // is what makes a *paired* one arrive whole.
            sink.write(stream, &String::from_utf16_lossy(buffer));
            buffer.clear();
            Ok(())
        });
    }

    /// `System`'s two clock readings.
    fn bind_clock(package: &mut JavaPackage, host: &Rc<dyn PlatformHost>) {
        let wall = Rc::clone(host);
        package.bind(
            "java/lang/System",
            "currentTimeMillis()J",
            move |_, _, mut out| {
                out.set(0, NativeValue::I64(wall.current_time_millis()));
                Ok(())
            },
        );

        let monotonic = Rc::clone(host);
        package.bind("java/lang/System", "nanoTime()J", move |_, _, mut out| {
            out.set(0, NativeValue::I64(monotonic.nano_time()));
            Ok(())
        });
    }

    /// The eight on a floating-point value: four bit casts, two renderings, two parses.
    fn bind_floats(package: &mut JavaPackage) {
        package.bind(
            "java/lang/Double",
            "doubleToRawLongBits(D)J",
            |_, args, mut out| {
                out.set(0, NativeValue::I64(args.f64(0)?.to_bits() as i64));
                Ok(())
            },
        );
        package.bind(
            "java/lang/Double",
            "longBitsToDouble(J)D",
            |_, args, mut out| {
                out.set(0, NativeValue::F64(f64::from_bits(args.i64(0)? as u64)));
                Ok(())
            },
        );
        package.bind(
            "java/lang/Float",
            "floatToRawIntBits(F)I",
            |_, args, mut out| {
                out.set(0, NativeValue::I32(args.f32(0)?.to_bits() as i32));
                Ok(())
            },
        );
        package.bind("java/lang/Float", "intBitsToFloat(I)F", |_, args, mut out| {
            out.set(0, NativeValue::F32(f32::from_bits(args.i32(0)? as u32)));
            Ok(())
        });

        package.bind("java/lang/Double", "toChars(D[C)I", |host, args, out| {
            let rendered = Decimal::of_f64(args.f64(0)?);
            Self::write_rendering(host, args.reference(1)?, &rendered, out)
        });
        package.bind("java/lang/Float", "toChars(F[C)I", |host, args, out| {
            let rendered = Decimal::of_f32(args.f32(0)?);
            Self::write_rendering(host, args.reference(1)?, &rendered, out)
        });

        package.bind(
            "java/lang/Double",
            "parseChars([CII)D",
            |host, args, mut out| {
                let text = Self::argument_text(host, &args)?;
                let value = Decimal::parse(&text)
                    .ok_or_else(|| NativeError::Message(alloc::format!("not a double: {text}")))?;
                out.set(0, NativeValue::F64(value));
                Ok(())
            },
        );
        package.bind(
            "java/lang/Float",
            "parseChars([CII)F",
            |host, args, mut out| {
                let text = Self::argument_text(host, &args)?;
                let value = Decimal::parse_f32(&text)
                    .ok_or_else(|| NativeError::Message(alloc::format!("not a float: {text}")))?;
                out.set(0, NativeValue::F32(value));
                Ok(())
            },
        );
    }

    /// The `(offset, count)` window a `char[]` argument names, refused if it leaves the array.
    ///
    /// Negative values reach here as negative `i32`s and must be refused before the conversion to
    /// `u32`, which would turn `-1` into four billion.
    fn bounds(
        host: &mut dyn NativeHost,
        slot: jals_native::RefSlot,
        offset: i32,
        count: i32,
    ) -> Result<(u32, u32), NativeError> {
        let length = host.array_len(slot)?;
        let (Ok(offset), Ok(count)) = (u32::try_from(offset), u32::try_from(count)) else {
            return Err(NativeError::OutOfBounds {
                index: offset.unsigned_abs(),
                len: length,
            });
        };
        if offset.saturating_add(count) > length {
            return Err(NativeError::OutOfBounds {
                index: offset.saturating_add(count),
                len: length,
            });
        }
        Ok((offset, count))
    }

    /// The text a `([CII)` argument triple names.
    fn argument_text(host: &mut dyn NativeHost, args: &Args<'_>) -> Result<String, NativeError> {
        let slot = args.reference(0)?;
        let (offset, count) = Self::bounds(host, slot, args.i32(1)?, args.i32(2)?)?;
        host.array_text(slot, offset, count)
    }

    /// Write `rendered` into the array the module allocated, and report how much was written.
    fn write_rendering(
        host: &mut dyn NativeHost,
        slot: jals_native::RefSlot,
        rendered: &str,
        mut out: Results<'_>,
    ) -> Result<(), NativeError> {
        let units: Vec<u16> = rendered.encode_utf16().collect();
        let written = u32::try_from(units.len()).unwrap_or(u32::MAX);
        let capacity = host.array_len(slot)?;
        if written > capacity || written > RENDERING_LIMIT {
            return Err(NativeError::OutOfBounds {
                index: written,
                len: capacity,
            });
        }
        for (index, unit) in units.into_iter().enumerate() {
            host.array_set(
                slot,
                u32::try_from(index).unwrap_or(u32::MAX),
                NativeValue::I32(i32::from(unit)),
            )?;
        }
        out.set(0, NativeValue::I32(written as i32));
        Ok(())
    }
}
