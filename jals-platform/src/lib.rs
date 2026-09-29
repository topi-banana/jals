#![no_std]
//! `java.base` for the wasm target, as the package a compiled program links against.
//!
//! Java has been able to name `java.lang.String` since it had a `main`, and until this crate there
//! was nothing behind the name on this target: the standard-library stubs declared the surface so
//! that a program would *check*, and the backend then refused every use, because there was no
//! `String` and no host to ask for one. This crate is the answer. It is a real `java.base`, written
//! in Java, compiled to wasm at build time, and shipped as one artifact.
//!
//! # Why a package and not a `classpath`
//!
//! The classes here are not read by a host JVM at run time; they *are* the run time. Every method
//! is lowered into the module that links them, so the platform is a wasm **library** — exactly the
//! shape [`jals_native::NativePackage`] exists for. A package ships either its Java or a compiled
//! module, and a platform may not ship both without being two platforms, so this one ships the
//! module; the Java it was built from travels inside that module's `jals.library` section, which is
//! what a consumer indexes to resolve `new String(chars)` and the rest.
//!
//! The two halves cannot drift. The module's ABI section states the Java that produced it, so a
//! consumer that runs the module and a reader that resolves against its Java are reading one
//! artifact.
//!
//! # Why the build compiles it
//!
//! The module is produced by [`build.rs`][build] with the workspace's own compiler, and embedded
//! with `include_bytes!`. The alternative — checking the wasm in — is an artifact no reviewer can
//! read and no test can regenerate on the machine that finds the bug. Here the source is
//! [`java`][java], the build compiles it, and the test that links the result runs the same bytes a
//! user gets.
//!
//! [build]: https://doc.rust-lang.org/cargo/reference/build-scripts.html
//! [java]: https://github.com/topi-banana/jals/tree/main/jals-platform/java
//!
//! # Using it
//!
//! [`Platform::package`] is the whole API a host needs: a binary registers the value — built over
//! the sink its own console writes to — and a project links it by naming [`Platform::NAME`] in
//! `[build] native-packages`.

extern crate alloc;

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use core::cmp::Ordering;

use jals_native::console::ConsoleSink;
use jals_native::{Args, NativeError, NativeHost, NativeValue, RefSlot, Results};

mod version;

/// The platform, as the one name a host needs to say it.
///
/// A type rather than a bare pair of constants and a function, because the three say one thing and
/// the association is what keeps them from drifting apart in a caller's code: [`NAME`](Self::NAME)
/// is the link name, [`MODULE`](Self::MODULE) is the artifact, and [`package`](Self::package) is
/// the two of them — and the bindings — in the form the registry takes.
pub struct Platform;

impl Platform {
    /// The link name, which is also the name `[build] native-packages` selects the package by.
    ///
    /// The module name in every import a linked program writes: `java.base` is the Java module the
    /// classes actually live in, and a link name that cannot be confused with a user's `wasm`
    /// dependency.
    pub const NAME: &str = "java.base";

    /// The compiled platform, embedded by the build script that produced it.
    ///
    /// The bytes are the whole package — the code, the ABI it exports, and the Java under `java`
    /// that a consumer indexes — so nothing here is assembled from parts that could disagree. The
    /// version they carry travels beside them into the package below and into the module's ABI
    /// section, so the two cannot be bumped apart.
    pub const MODULE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/java.base.wasm"));

    /// The class the printing binding belongs to, spelled the way the import section spells it.
    const PRINT_OWNER: &'static str = "java/io/PrintStream";

    /// The method the printing binding answers: the name and descriptor the module imports.
    ///
    /// It takes the `char[]` the Java side builds rather than a `String`, because a string's
    /// representation is the backend's own layout and a host that read one would be reading a fact
    /// no declaration states — an array is a first-class object the embedder can read element by
    /// element, and the one shape both halves already agree on.
    const PRINT_SIGNATURE: &'static str = "writeChars([CII)V";

    /// The class whose decimal renderer the host supplies.
    const DOUBLE_OWNER: &'static str = "java/lang/Double";

    /// The renderer's signature: the value, the `char[]` the digits go into, and the `int[]` the
    /// first digit's place goes into; the digit count comes back as the result.
    ///
    /// Nothing but the digits crosses: the platform's Java owns the sign, the zeros, the
    /// infinities, and the JDK's notation, because deciding how a number is *written* is the
    /// platform's business. Producing the exact decimal is the one algorithmic thing this target
    /// asks its host for — almost always the shortest that reads back as the same value, and among
    /// those the nearest, which is exact arithmetic over the raw significand and in Java means a
    /// big-integer core the platform has not built. When it is, this binding and the `native`
    /// declaration beside it retire without any public declaration changing.
    const DOUBLE_DIGITS: &'static str = "writeDigits(D[C[I)I";

    /// The same renderer in half the width.
    ///
    /// It cannot be the `double` one widened or narrowed: shortest-round-trip digits are a
    /// property of the width, so `0.1f` prints `0.1` while the `double` nearest to it prints
    /// `0.10000000149011612`.
    const FLOAT_OWNER: &'static str = "java/lang/Float";

    /// See [`DOUBLE_DIGITS`](Self::DOUBLE_DIGITS).
    const FLOAT_DIGITS: &'static str = "writeDigits(F[C[I)I";

    /// The platform as a binary offers it: the module, under the name its imports are spelled
    /// with, and the bindings its Java cannot supply — where its printed text goes, and the exact
    /// digits of a floating-point value.
    ///
    /// A fresh value per call rather than a `static`: the package carries a sink, and a host that
    /// has one builds the value over it. The module bytes themselves are `'static`, so the value
    /// is cheap.
    #[must_use]
    pub fn package(sink: Rc<dyn ConsoleSink>) -> jals_native::NativePackage {
        let mut package = jals_native::NativePackage::new(Self::NAME, version::VERSION);
        package.library(Self::MODULE);
        package.bind(
            Self::PRINT_OWNER,
            Self::PRINT_SIGNATURE,
            move |host: &mut dyn NativeHost, args: Args<'_>, _: Results<'_>| {
                let chars = args.reference(0)?;
                let offset = args.i32(1)?;
                let count = args.i32(2)?;
                let (Ok(offset), Ok(count)) = (u32::try_from(offset), u32::try_from(count)) else {
                    // A negative offset or count is a bounds error, not a host defect: the module
                    // passed what its own Java computed, and Java would throw here.
                    let len = host.array_len(chars)?;
                    return Err(NativeError::OutOfBounds { index: 0, len });
                };
                let text = host.array_text(chars, offset, count)?;
                if !text.is_empty() {
                    sink.write(&text);
                }
                Ok(())
            },
        );
        package.bind(
            Self::DOUBLE_OWNER,
            Self::DOUBLE_DIGITS,
            |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                let value = args.f64(0)?;
                // A zero, a negative, an infinity, or a NaN — the module's Java answers all four
                // itself, before it calls. One arriving here is a module bug, and a renderer that
                // invented digits for an infinity would hide it.
                if value.is_nan() || value <= 0.0 || value.is_infinite() {
                    return Err(NativeError::Message(String::from(
                        "asked for the digits of a value the platform renders itself",
                    )));
                }
                let out = args.reference(1)?;
                let place = args.reference(2)?;
                let (digits, point) = Self::digits(value);
                let length = Self::write_digits(host, out, place, &digits, point)?;
                results.set(0, NativeValue::I32(length));
                Ok(())
            },
        );
        package.bind(
            Self::FLOAT_OWNER,
            Self::FLOAT_DIGITS,
            |host: &mut dyn NativeHost, args: Args<'_>, mut results: Results<'_>| {
                let value = args.f32(0)?;
                if value.is_nan() || value <= 0.0 || value.is_infinite() {
                    return Err(NativeError::Message(String::from(
                        "asked for the digits of a value the platform renders itself",
                    )));
                }
                let out = args.reference(1)?;
                let place = args.reference(2)?;
                let (digits, point) = Self::digits(value);
                let length = Self::write_digits(host, out, place, &digits, point)?;
                results.set(0, NativeValue::I32(length));
                Ok(())
            },
        );
        package
    }

    /// More significant digits than any `f64` has in its exact decimal expansion: every double's
    /// terminates within 800 digits, and an `f32`'s within 120. Writing a value out at this
    /// precision is therefore writing it out exactly, which the one-digit comparison below needs.
    const EXACT_DIGITS: usize = 1199;

    /// The significant digits of a positive finite `value`, and the power of ten the first one
    /// stands for.
    ///
    /// The JDK's selection rule, which is *almost* "the shortest decimal that reads back": let `p`
    /// be the fewest digits any decimal that rounds to `value` is written with. With `p >= 2` the
    /// answer is the `p`-digit decimal nearest to `value` — a fixed-precision rendering of the
    /// exact value, which rounds ties to even, the same rule as the JDK's "even significand" — and
    /// it necessarily rounds back, because the nearest `p`-digit decimal is at least as near as
    /// the `p`-digit decimal that does. With `p == 1` the JDK is deliberately not shortest: it
    /// admits the two-digit decimals too, and takes whichever of the one- and two-digit candidates
    /// is nearer. That is not an idle corner — the smallest subnormals are where it shows:
    /// `Double.MIN_VALUE` renders `4.9E-324`, not the `5.0E-324` a shortest-only rule produces,
    /// and `Float.MIN_VALUE` renders `1.4E-45`.
    ///
    /// The one-digit case needs the two candidates' *exact* distance, not their parsed values:
    /// each candidate is close enough to `value` to round back to it, so parsing them would
    /// compare the value with itself. The comparison therefore writes `value` out at a precision
    /// past the end of its decimal expansion and compares the two as decimals.
    fn digits<T: core::fmt::LowerExp>(value: T) -> (String, i32) {
        let shortest = alloc::format!("{value:e}");
        let (significant, _) = Self::split_exponential(&shortest);
        if significant.len() >= 2 {
            let exact = alloc::format!("{:.*e}", significant.len() - 1, value);
            return Self::split_exponential(&exact);
        }
        let (one_digits, one_point) = Self::split_exponential(&alloc::format!("{value:.0e}"));
        let (two_digits, two_point) = Self::split_exponential(&alloc::format!("{value:.1e}"));
        if one_digits == two_digits && one_point == two_point {
            // The two-digit candidate is the one-digit one with a zero on the end — every
            // ordinary value with a one-digit shortest form — and the JDK counts it once.
            return (one_digits, one_point);
        }
        let (mid_digits, mid_point) =
            Self::midpoint(&one_digits, one_point, &two_digits, two_point);
        let (full_digits, full_point) =
            Self::split_exponential(&alloc::format!("{:.*e}", Self::EXACT_DIGITS, value));
        let (low, high) = match Self::decimal_cmp(&one_digits, one_point, &two_digits, two_point) {
            Ordering::Less => ((one_digits, one_point), (two_digits, two_point)),
            Ordering::Greater | Ordering::Equal => {
                ((two_digits, two_point), (one_digits, one_point))
            }
        };
        match Self::decimal_cmp(&full_digits, full_point, &mid_digits, mid_point) {
            Ordering::Greater => high,
            Ordering::Equal
                if Self::even_significand(&high.0) && !Self::even_significand(&low.0) =>
            {
                high
            }
            // Below the midpoint, the smaller candidate is the nearer; exactly on it the JDK takes
            // the even significand, and the one-digit candidate unless only the other is even. Both
            // odd cannot happen for a value a `double` can take: an exact tie is the midpoint, a
            // three-digit decimal, and a value whose shortest form is one digit is never one.
            Ordering::Less | Ordering::Equal => low,
        }
    }

    /// The decimal halfway between a one-digit and a two-digit candidate.
    fn midpoint(
        one_digits: &str,
        one_point: i32,
        two_digits: &str,
        two_point: i32,
    ) -> (String, i32) {
        let one_value: u64 = one_digits.parse().unwrap_or(0);
        let one_exponent = one_point - (i32::try_from(one_digits.len()).unwrap_or(0) - 1);
        let two_value: u64 = two_digits.parse().unwrap_or(0);
        let two_exponent = two_point - (i32::try_from(two_digits.len()).unwrap_or(0) - 1);
        let low = one_exponent.min(two_exponent);
        let a = one_value * 10u64.pow(u32::try_from(one_exponent - low).unwrap_or(0));
        let b = two_value * 10u64.pow(u32::try_from(two_exponent - low).unwrap_or(0));
        let sum = a + b;
        // An odd sum cannot be halved as an integer, so the significand is multiplied by five and
        // the exponent dropped one place, which is the same number.
        let (numerator, extra) = if sum.is_multiple_of(2) {
            (sum / 2, 0)
        } else {
            (sum * 5, -1)
        };
        let text = numerator.to_string();
        let point = low + extra + i32::try_from(text.len()).unwrap_or(0) - 1;
        let trimmed = text.trim_end_matches('0');
        if trimmed.is_empty() {
            (String::from("0"), 0)
        } else {
            (String::from(trimmed), point)
        }
    }

    /// Compare two decimals, each a canonical digit string and the place of its first digit.
    fn decimal_cmp(a_digits: &str, a_point: i32, b_digits: &str, b_point: i32) -> Ordering {
        if a_point != b_point {
            return a_point.cmp(&b_point);
        }
        let a = a_digits.as_bytes();
        let b = b_digits.as_bytes();
        for (left, right) in a.iter().zip(b) {
            if left != right {
                return left.cmp(right);
            }
        }
        a.len().cmp(&b.len())
    }

    /// Whether a digit string's significand — the integer it spells — is even.
    fn even_significand(digits: &str) -> bool {
        digits.as_bytes().last().is_some_and(|digit| digit % 2 == 0)
    }

    /// `"1.2345e-7"` into `("12345", -7)`: the digits with no point and no trailing zero, and the
    /// power of ten the first one stands for.
    fn split_exponential(text: &str) -> (String, i32) {
        let (mantissa, exponent) = match text.split_once('e') {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
            None => (text, 0),
        };
        let mantissa = mantissa.strip_prefix('-').unwrap_or(mantissa);
        let (whole, fraction) = match mantissa.split_once('.') {
            Some((whole, fraction)) => (whole, fraction),
            None => (mantissa, ""),
        };
        let mut digits = String::with_capacity(whole.len() + fraction.len());
        digits.push_str(whole);
        digits.push_str(fraction);
        // The exponent names the first digit's place, so dropping trailing zeros leaves it alone.
        let exponent = exponent + i32::try_from(whole.len()).unwrap_or(0) - 1;
        let trimmed = digits.trim_end_matches('0');
        if trimmed.is_empty() {
            // `0e0`: zero has one digit and no place.
            (String::from("0"), 0)
        } else {
            (String::from(trimmed), exponent)
        }
    }

    /// Hand one rendering to the module: the digits into `out`, the first digit's place into
    /// `place[0]`, and the digit count back as the result.
    fn write_digits(
        host: &mut dyn NativeHost,
        out: RefSlot,
        place: RefSlot,
        digits: &str,
        point: i32,
    ) -> Result<i32, NativeError> {
        for (index, digit) in digits.chars().enumerate() {
            host.array_set(
                out,
                u32::try_from(index).unwrap_or(u32::MAX),
                NativeValue::I32(digit as i32),
            )?;
        }
        host.array_set(place, 0, NativeValue::I32(point))?;
        Ok(i32::try_from(digits.len()).unwrap_or(i32::MAX))
    }
}
