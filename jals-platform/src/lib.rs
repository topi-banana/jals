#![cfg_attr(not(test), no_std)]
//! `jals-platform`: the Java standard library `jals` ships, as a package.
//!
//! `java.lang` and `java.io`, written in Java, behind twelve host functions. It is a
//! [`jals_native`] package like any other — found through the same resolver chain a third party's
//! is, carrying the same [`JavaSource`](jals_native::JavaSource) values — and that is deliberate.
//! A standard library privileged into the crate that defines what a package *is* would be a second
//! way to publish Java, and the second way is always the one that drifts.
//!
//! # One text, two tiers
//!
//! Every consumer reads the same Java. What differs is how faithful those declarations are to what
//! will run, and that is a property of the **route**, not of the text:
//!
//! | consumer | reads | as |
//! | --- | --- | --- |
//! | a `jals-wasm` build linking this package | every unit | `Complete` — what it does not declare, the program does not have |
//! | a `javac` build, an editor session, `jals lint` | every unit | `Signatures` — the real JDK is a superset |
//! | any build's compile step | [`SourceKind::Implementation`](jals_native::SourceKind) only | the Java it lowers |
//!
//! What this replaces is a hand-written signature copy of the same API sitting beside the
//! implementation, with a test diffing their member sets and a comment asking maintainers not to
//! edit one to match the other. There is one member set now, so there is nothing to keep in step.
//!
//! # What is here
//!
//! `java.lang`: `String` (a `char[]`, no interning, no shared backing), `StringBuilder`, every
//! wrapper, `Character`, `Math`, `System`, `Class`, `Throwable` and twenty-three exception classes,
//! and the five annotations the language itself names.
//!
//! `java.io`: `PrintStream`, `Closeable`, and the checked I/O exceptions.
//!
//! `java.util` and `java.lang.Object` are here too, at signature fidelity: declarations with no
//! bodies, never compiled into anything. See below.
//!
//! # What is deliberately not
//!
//! Each because of what the target is, and each stated in the class that would have carried it:
//!
//! - **`java.lang.Object` has no implementation, and must not.** It *is* the wasm backend's
//!   `anyref`, answered for before the backend consults its struct table — so a declared `Object`
//!   with fields would be one question with two answers, a field present on some instances and not
//!   others. It is a signature unit, and the tier is what enforces it: a compile takes only
//!   implementation units, so there is no way to hand it that file. Nothing has to remember a rule.
//! - **`java.util` is declarations only.** A `List` nobody has implemented is a type a program can
//!   still *name*, which is what an editor needs; it is not a type a module can call.
//! - **Reflection, `Enum`, `Record`.** Each needs metadata the backend does not emit.
//! - **`Math`'s transcendentals.** `sin`, `exp` and `log` need either a polynomial table this
//!   package would have to be trusted about or a host binding each. `sqrt` is exact and is here.
//! - **Full Unicode case mapping.** `Character` and `String` map ASCII and say so, rather than
//!   shipping a half-Unicode answer that looks general.
//!
//! # Using it
//!
//! ```
//! # extern crate alloc;
//! # use alloc::rc::Rc;
//! use jals_platform::{CapturedHost, JavaBase};
//!
//! // For analysis, the Java is reachable with no host at all.
//! assert!(JavaBase::SOURCES.iter().any(|s| s.path == "java/lang/String.java"));
//!
//! // For a build that will link and run it, the host supplies the streams and the clock.
//! let host = Rc::new(CapturedHost::at(1_700_000_000_000));
//! let package = JavaBase::package(host);
//! assert_eq!(package.name(), "java.base");
//! ```

extern crate alloc;

mod bindings;
mod decimal;
mod host;
mod sources;

pub use host::{CapturedHost, PlatformHost, SilentHost, Stream};
pub use sources::JavaBase;
