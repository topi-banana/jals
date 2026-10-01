//! Where a package's text goes.
//!
//! A package that prints is `no_std`, so it cannot write to a stream itself: the place its output
//! goes is a value the *host* supplies, and that value is [`ConsoleSink`]. Two packages in this
//! workspace print — `jals.io`, and `java.base`'s `System.out` — and a host that offers both
//! builds one sink and passes it to each, so the two write through the same console and in the
//! order the program wrote them.
//!
//! [`CapturedConsole`] is the sink a test passes when it wants to assert on the text: it keeps
//! everything written and hands it back on [`take`](CapturedConsole::take).

use alloc::string::String;
use core::cell::RefCell;

/// Where a package's output goes.
///
/// `&self` rather than `&mut self`: a sink is shared by the bindings of every package that prints,
/// each holding its own `Rc` of it, and a host that needs interior mutability already has
/// `RefCell` — every runtime here is current-thread, so nothing about this needs to be `Sync`.
pub trait ConsoleSink {
    /// Write `text`, which is one write's worth of buffered code units, decoded.
    fn write(&self, text: &str);
}

/// A sink that keeps what was written, so a test can assert on it without a console.
///
/// Shipped rather than left to each consumer's test module because it is the only way to drive a
/// printing binding at all, and a consumer writing its own would be writing the same few lines to
/// check the same thing.
#[derive(Debug, Default)]
pub struct CapturedConsole {
    written: RefCell<String>,
}

impl CapturedConsole {
    /// A console that has been written to zero times.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything written so far, joined in order.
    pub fn take(&self) -> String {
        core::mem::take(&mut self.written.borrow_mut())
    }
}

impl ConsoleSink for CapturedConsole {
    fn write(&self, text: &str) {
        self.written.borrow_mut().push_str(text);
    }
}
