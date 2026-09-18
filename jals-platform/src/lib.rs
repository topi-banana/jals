#![cfg_attr(not(test), no_std)]
//! `jals-platform`: the Java standard library `jals` ships, as packages.
//!
//! `java.lang`, `java.util` and `java.io`, written in Java, behind a handful of host functions. It
//! is a [`jals_native`] package like any other — carrying the same
//! [`JavaSource`](jals_native::JavaSource) values and the same binding keys — and that is
//! deliberate. A standard library privileged into the crate that defines what a package *is* would
//! be a second way to publish Java, and the second way is always the one that drifts.
//!
//! # One text, two readers
//!
//! Every consumer reads the same Java. What differs is how faithfully those declarations are to
//! what will run, and that is a property of the **route**, not of the text:
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
//! wrapper, `Character`, `Math`, `System`, `Class`, `Throwable` and the exception classes, and the
//! annotations the language itself names.
//!
//! `java.io`: `PrintStream`, `Closeable`, and the checked I/O exceptions.
//!
//! `java.util`: the collection interfaces and `ArrayList`, whose `Vec` lives in Rust and whose
//! instances carry a handle the host's table resolves. The containers nobody has implemented —
//! `Map`, `Set`, `Optional` — are declarations with no bodies, never compiled into anything; see
//! below.
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
//! - **Reflection, and a body for `Enum` or `Record`.** A constant's `ordinal()` and a record's
//!   accessors are synthesised per declaration, so there is no single body either could carry. Both
//!   are *declared*, so a program can still name them.
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
//! use jals_platform::{Builtin, CapturedHost, JavaBase};
//!
//! // For analysis, the Java is reachable with no host at all.
//! assert!(JavaBase::SOURCES.iter().any(|s| s.path == "java/lang/String.java"));
//!
//! // For a build that will link and run it, the host supplies the streams and the clock.
//! let host = Rc::new(CapturedHost::at(1_700_000_000_000));
//! let packages = Builtin::packages(host);
//! assert_eq!(packages[0].name(), "java.base");
//! ```

extern crate alloc;

mod bindings;
mod collections;
mod decimal;
mod host;
mod jals_io;
mod sources;

pub use host::{CapturedHost, PlatformHost, SilentHost, Stream};
pub use jals_io::JalsIo;
pub use sources::JavaBase;

use alloc::rc::Rc;
use alloc::vec::Vec;
use jals_native::JavaPackage;

/// The packages this binary ships, built over one host.
///
/// The one place the set is written. Every host calls [`packages`](Self::packages) instead of
/// naming a package of its own, so a package added here is offered by `jals-cli`, the language
/// server and the browser playground at once — and, because the same [`JavaPackage`] values are
/// what an index reads, by `jals lint` and the editor beside it too. Which of them a *project*
/// selects is still the manifest's answer (`[build] platform`, `[build] native-packages`).
pub struct Builtin;

impl Builtin {
    /// Every built-in package, in name order.
    #[must_use]
    pub fn packages(host: Rc<dyn PlatformHost>) -> Vec<JavaPackage> {
        alloc::vec![JavaBase::package(Rc::clone(&host)), JalsIo::package(host)]
    }
}
