//! `jals.io` — text out of a module, and the first package this seam ships.
//!
//! Chosen as the first one because of what it does *not* need. Its Java uses only what the wasm
//! backend already compiles — primitives, arrays, and project-declared classes — so nothing here
//! depends on a lowering rule that does not exist yet, and what it demonstrates is the seam rather
//! than a compiler change hiding behind it.
//!
//! # The sink is the host's
//!
//! This crate is `no_std`: there is no `println!` here and there cannot be one. So the package is
//! *constructed with* the place its output goes, and that is not a workaround — it is the shape a
//! stateful native package has. `jals-cli` passes a sink that writes through its `Shell` (the one
//! thing in that crate allowed to touch a stream), the browser passes one that appends to the
//! page, and a test passes one that appends to a `String`.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::host::NativeHost;
use crate::package::NativePackage;
use crate::value::{Args, NativeError, Results};

/// Where `jals.io`'s output goes.
///
/// `&self` rather than `&mut self`: a sink is shared by three bindings that each hold their own
/// `Rc` of it, and a host that needs interior mutability already has `RefCell` — every runtime
/// here is current-thread, so nothing about this needs to be `Sync`.
pub trait ConsoleSink {
    /// Write `text`, which is one flush's worth of buffered code units, decoded.
    fn write(&self, text: &str);
}

/// The `jals.io` package.
pub struct JalsIo;

impl JalsIo {
    /// The package's name, as `[build] native-packages` spells it.
    pub const NAME: &'static str = "jals.io";

    /// The internal name of the class the bindings belong to.
    const OWNER: &'static str = "jals/io/Out";

    /// The Java this package publishes.
    ///
    /// A real `.java` file rather than a string literal in this module: it is Java, so it is
    /// edited, read and highlighted as Java. `include_str!` makes it a compile-time constant all
    /// the same, so the crate stays pure and needs no I/O to publish it.
    const OUT_JAVA: &'static str = include_str!("../../java/jals/io/Out.java");

    /// Build the package over `sink`.
    ///
    /// Version 1. Bump it when a binding starts answering differently for input that did not
    /// change — a consumer that memoized a compile cannot see a closure body change any other way.
    pub fn package(sink: Rc<dyn ConsoleSink>) -> NativePackage {
        // One buffer behind all three bindings. A code unit is half of a surrogate pair, so a host
        // that decoded each `writeChar` on its own could not join them — the buffer is what makes
        // the pair reachable, not a speed-up.
        let buffer = Rc::new(RefCell::new(Vec::<u16>::new()));

        let mut package = NativePackage::new(Self::NAME, 1);
        package.source("jals/io/Out.java", Self::OUT_JAVA);

        let units = Rc::clone(&buffer);
        package.bind(
            Self::OWNER,
            "writeChar(I)V",
            move |_host: &mut dyn NativeHost, args: Args<'_>, _: Results<'_>| {
                let unit = args.i32(0)?;
                units
                    .borrow_mut()
                    .push(u16::try_from(unit & 0xFFFF).unwrap_or(0));
                Ok(())
            },
        );

        let units = Rc::clone(&buffer);
        package.bind(
            Self::OWNER,
            "writeChars([CII)V",
            move |host: &mut dyn NativeHost, args: Args<'_>, _: Results<'_>| {
                let text = args.reference(0)?;
                let offset = args.i32(1)?;
                let count = args.i32(2)?;
                let (Ok(offset), Ok(count)) = (u32::try_from(offset), u32::try_from(count)) else {
                    // A negative offset or count is a bounds error, not a host defect: the module
                    // passed what its own Java computed, and Java would throw here.
                    let len = host.array_len(text)?;
                    return Err(NativeError::OutOfBounds { index: 0, len });
                };
                let mut buffered = units.borrow_mut();
                let end = offset.saturating_add(count);
                let len = host.array_len(text)?;
                if end > len {
                    return Err(NativeError::OutOfBounds { index: end, len });
                }
                for index in offset..end {
                    let value = host.array_get(text, index)?;
                    let unit = value
                        .as_i32()
                        .ok_or_else(|| NativeError::argument(0, "a char element", value))?;
                    buffered.push(u16::try_from(unit & 0xFFFF).unwrap_or(0));
                }
                Ok(())
            },
        );

        let units = Rc::clone(&buffer);
        package.bind(
            Self::OWNER,
            "flush()V",
            move |_host: &mut dyn NativeHost, _: Args<'_>, _: Results<'_>| {
                let mut buffered = units.borrow_mut();
                if buffered.is_empty() {
                    return Ok(());
                }
                sink.write(&String::from_utf16_lossy(&buffered));
                buffered.clear();
                Ok(())
            },
        );

        package
    }
}

/// A sink that keeps what was written, so a test can assert on it without a console.
///
/// Shipped rather than left to each consumer's test module because it is the only way to drive
/// this package's bindings at all, and a consumer writing its own would be writing the same eight
/// lines to check the same thing.
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
