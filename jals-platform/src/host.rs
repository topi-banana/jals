//! The state a host supplies to the platform's bindings.
//!
//! Ten of the twelve bindings are pure — a bit cast, a decimal rendering, a decimal parse — and
//! need nothing from anybody. The other two write text, and one pair reads a clock, and neither is
//! something a `no_std` crate with no dependencies can do on its own. So the host supplies them,
//! exactly as `jinja`'s `Object` lets a consumer supply a value the engine has never heard of.
//!
//! # Two streams, not one
//!
//! [`Stream`] is an enum rather than a `bool` because a host that folded the two would put a
//! program's output in the same place as its diagnostics. `jals run` sends one to stdout and the
//! other to stderr; the playground has one pane and joins them *deliberately*, which is a decision
//! it makes rather than one this crate makes for it.
//!
//! # A host with no clock is a real host
//!
//! Both clock methods default to zero. A language server resolves this package's Java and never
//! instantiates a module, so it has no clock to offer and needs none; requiring it to invent one
//! would be requiring it to lie. A program that reads the clock on such a host sees time standing
//! still, which is the honest answer.

use alloc::string::String;
use core::cell::RefCell;

/// Which standard stream a write is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// A program's own output — `System.out`.
    Out,
    /// A program's diagnostics — `System.err`.
    Err,
}

impl Stream {
    /// The identifier the Java half passes across the seam.
    ///
    /// `java.lang.System` names these `OUT_STREAM` and `ERR_STREAM`, and the two spellings meet
    /// here and nowhere else. Total in the other direction — an identifier the Java half never
    /// sends is read as [`Out`](Self::Out) rather than refused — because a stream number is not
    /// input a program controls, so a refusal would report a defect in this package as a defect in
    /// the program.
    pub(crate) const fn of(identifier: i32) -> Self {
        if identifier == 1 {
            Self::Err
        } else {
            Self::Out
        }
    }
}

/// What the platform's `System` and `PrintStream` reach out to.
pub trait PlatformHost {
    /// Write `text`, which is one flush's worth of buffered code units, decoded.
    ///
    /// Called once per flush and never per character: the Java half buffers until a `println` or an
    /// explicit `flush`, so a surrogate pair arrives whole. Nothing here adds a line break — the
    /// module decides where its own lines end.
    fn write(&self, stream: Stream, text: &str);

    /// Milliseconds since the Unix epoch.
    fn current_time_millis(&self) -> i64 {
        0
    }

    /// A monotonic reading in nanoseconds, meaningful only as a difference from another.
    fn nano_time(&self) -> i64 {
        0
    }
}

/// A host that discards every write and has no clock.
///
/// What a language server takes. It resolves the package's Java for analysis and instantiates
/// nothing, so there is no stream for a write to reach and no run for a clock to time.
#[derive(Debug, Default, Clone, Copy)]
pub struct SilentHost;

impl PlatformHost for SilentHost {
    fn write(&self, _stream: Stream, _text: &str) {}
}

/// A host that keeps both streams apart in memory, with a clock the caller sets.
///
/// What tests and the browser playground take. The two streams stay separate here even for a
/// consumer that will join them, because joining is a decision with a place to be made and this is
/// not it.
#[derive(Debug, Default)]
pub struct CapturedHost {
    out: RefCell<String>,
    err: RefCell<String>,
    millis: i64,
}

impl CapturedHost {
    /// A host whose clock reads zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A host whose clock reads `millis` milliseconds since the epoch, and whose monotonic reading
    /// is that same instant in nanoseconds — so a test can assert an exact number.
    #[must_use]
    pub fn at(millis: i64) -> Self {
        Self {
            millis,
            ..Self::default()
        }
    }

    /// Everything written to `System.out` so far, leaving the buffer empty.
    pub fn take_out(&self) -> String {
        core::mem::take(&mut self.out.borrow_mut())
    }

    /// Everything written to `System.err` so far, leaving the buffer empty.
    pub fn take_err(&self) -> String {
        core::mem::take(&mut self.err.borrow_mut())
    }
}

impl PlatformHost for CapturedHost {
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
